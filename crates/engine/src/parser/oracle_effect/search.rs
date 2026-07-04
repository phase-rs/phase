use crate::parser::oracle_nom::error::{OracleError, OracleResult};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_till1, take_until};
use nom::character::complete::space1;
use nom::combinator::{map, opt, peek, value};
use nom::multi::separated_list1;
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::super::oracle_nom::bridge::nom_on_lower;
use super::super::oracle_nom::filter as nom_filter;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_quantity;
use super::super::oracle_target::{
    distribute_properties_to_or, parse_mana_value_suffix, parse_shared_quality_clause,
    parse_target, parse_type_phrase, parse_zone_word,
};
use super::super::oracle_util::{
    contains_possessive, infer_core_type_for_subtype, split_around, strip_after,
};
use super::sequence::{parse_choice_partition_destination, parse_rest_cards_reference};
use super::{capitalize, scan_contains_phrase, ParseContext};
use crate::parser::oracle_ir::ast::{SearchLibraryDetails, SeekDetails};
use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
use crate::types::ability::{
    AggregateFunction, Comparator, ControllerRef, FilterProp, ObjectProperty, ObjectScope,
    QuantityExpr, QuantityRef, SearchDestinationSplit, SearchSelectionConstraint, SharedQuality,
    SharedQualityRelation, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card_type::{CoreType, Supertype};
#[cfg(test)]
use crate::types::counter::CounterType;
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

    // Extract count from "up to N" / "up to X" / "up to that many" (must be done
    // before filter extraction since "for up to five creature cards" needs to skip
    // the count to find the type).
    // CR 107.3a + CR 601.2b: X resolves to the caster's announced value at cast time.
    // CR 608.2c: "up to that many" is a back-reference to a count produced by an
    // earlier instruction in the same resolution (e.g. Scapeshift's sacrifice) —
    // `parse_quantity_ref` maps "that many"/"that much" to `EventContextAmount`,
    // which resolves through `state.last_effect_count` at runtime.
    let up_to_match = scan_preceded(lower, "up to ", |input| {
        alt((
            map(nom_quantity::parse_quantity_ref, |qty| QuantityExpr::Ref {
                qty,
            }),
            nom_quantity::parse_quantity_expr_number,
        ))
        .parse(input)
    });

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

    // CR 608.2c + CR 701.23: "with different names" / "with different powers"
    // and "don't share ..." are printed-text restrictions on the chosen set,
    // not filters on individual library cards.
    let selection_constraint = if let Some(constraint) = scan_total_mana_value_constraint(lower) {
        constraint
    } else if let Some(constraint) = scan_distinct_qualities_constraint(lower) {
        constraint
    } else {
        SearchSelectionConstraint::None
    };

    // CR 701.23a + CR 608.2c: Detect cultivate-class split destinations ("put
    // one onto the battlefield tapped and the other into your hand"). Only the
    // single-filter case carries a split; multi-filter chains handle their own
    // destinations via the interleaved-ChangeZone lowering. Scan the full effect
    // chain when available so Final Parting's destination clause in a sibling
    // chunk still populates `split` on the search effect.
    let split_scan = ctx.effect_chain_full_lower.as_deref().unwrap_or(lower);
    let split = if extra_filters.is_empty() {
        detect_search_split_destination(split_scan)
    } else {
        None
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
        split,
        // CR 701.23a: Library-only unless the text names a multi-zone set
        // ("graveyard, hand, and/or library").
        source_zones: parse_multi_search_zones(lower).unwrap_or_else(|| vec![Zone::Library]),
    }
}

fn scan_total_mana_value_constraint(lower: &str) -> Option<SearchSelectionConstraint> {
    scan_preceded(
        lower,
        "with total mana value ",
        parse_total_mana_value_constraint,
    )
    .map(|(constraint, _)| constraint)
}

/// CR 202.3: Shared combinator for the `"<N> or less" / "<N> or greater"`
/// mana-value bound that follows a "total mana value" phrase. Parses the number
/// token (the parser treats `X` as `0` here) followed by the comparator suffix.
///
/// Used by both the search-set constraint (`SearchSelectionConstraint::TotalManaValue`,
/// LE/GE) and the target-set constraint detection/strip on the put-from-graveyard
/// path (target side accepts LE only — see `validate_target_constraints`).
pub(crate) fn parse_total_mana_value_comparator(
    input: &str,
) -> OracleResult<'_, (Comparator, i32)> {
    // `parse_number_or_x` (X → 0) rather than `parse_number`: the where-X target
    // form (Ancient Brass Dragon: "with total mana value X or less") uses the
    // literal `X` token here. On the search side the value is always a literal
    // digit, so accepting X is a harmless superset (X → 0); on the target side
    // the parsed value is discarded — the cap is carried as `Variable("X")` and
    // rebound to the die result on the lowering path.
    let (rest, amount) = nom_primitives::parse_number_or_x.parse(input)?;
    let (rest, comparator) = alt((
        value(Comparator::LE, tag::<_, _, OracleError<'_>>(" or less")),
        value(Comparator::GE, tag(" or greater")),
    ))
    .parse(rest)?;
    Ok((rest, (comparator, amount as i32)))
}

fn parse_total_mana_value_constraint(
    input: &str,
) -> Result<(&str, SearchSelectionConstraint), nom::Err<OracleError<'_>>> {
    let (rest, (comparator, value)) = parse_total_mana_value_comparator(input)?;
    Ok((
        rest,
        SearchSelectionConstraint::TotalManaValue { comparator, value },
    ))
}

/// CR 608.2c + CR 701.23: Detect selected-set distinct-quality restrictions
/// at any word boundary in the clause. This covers both "with different names"
/// and "that don't share a mana value, power, toughness, or card type with
/// each other" without treating either as an individual-card filter suffix.
fn scan_distinct_qualities_constraint(lower: &str) -> Option<SearchSelectionConstraint> {
    scan_preceded(lower, "with different ", parse_quality_list_constraint)
        .or_else(|| scan_preceded(lower, "that have different ", parse_quality_list_constraint))
        .or_else(|| {
            scan_preceded(
                lower,
                "that each have different ",
                parse_quality_list_constraint,
            )
        })
        .or_else(|| {
            scan_preceded(
                lower,
                "that don't share ",
                parse_each_other_quality_constraint,
            )
        })
        .or_else(|| {
            scan_preceded(
                lower,
                "that do not share ",
                parse_each_other_quality_constraint,
            )
        })
        .map(|(constraint, _)| constraint)
}

fn parse_quality_list_constraint(
    input: &str,
) -> Result<(&str, SearchSelectionConstraint), nom::Err<OracleError<'_>>> {
    map(parse_search_selection_quality_list, |qualities| {
        SearchSelectionConstraint::DistinctQualities { qualities }
    })
    .parse(input)
}

fn parse_each_other_quality_constraint(
    input: &str,
) -> Result<(&str, SearchSelectionConstraint), nom::Err<OracleError<'_>>> {
    let (rest, qualities) = terminated(
        parse_search_selection_quality_list,
        tag::<_, _, OracleError<'_>>(" with each other"),
    )
    .parse(input)?;
    Ok((
        rest,
        SearchSelectionConstraint::DistinctQualities { qualities },
    ))
}

fn parse_distinct_quality_suffix(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    alt((
        value(
            (),
            preceded(
                tag::<_, _, OracleError<'_>>("with different "),
                parse_search_selection_quality_list,
            ),
        ),
        value(
            (),
            preceded(
                tag("that have different "),
                parse_search_selection_quality_list,
            ),
        ),
        value(
            (),
            preceded(
                tag("that each have different "),
                parse_search_selection_quality_list,
            ),
        ),
        value(
            (),
            preceded(
                tag("that don't share "),
                parse_each_other_quality_constraint,
            ),
        ),
        value(
            (),
            preceded(
                tag("that do not share "),
                parse_each_other_quality_constraint,
            ),
        ),
    ))
    .parse(input)
}

fn parse_search_selection_quality_list(
    input: &str,
) -> Result<(&str, Vec<SharedQuality>), nom::Err<OracleError<'_>>> {
    separated_list1(
        parse_search_selection_quality_separator,
        parse_search_selection_quality,
    )
    .parse(input)
}

fn parse_search_selection_quality_separator(
    input: &str,
) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>(", or "),
            tag(", and "),
            tag(", "),
            tag(" or "),
            tag(" and "),
        )),
    )
    .parse(input)
}

fn parse_search_selection_quality(
    input: &str,
) -> Result<(&str, SharedQuality), nom::Err<OracleError<'_>>> {
    preceded(
        opt(alt((
            tag::<_, _, OracleError<'_>>("a "),
            tag::<_, _, OracleError<'_>>("an "),
        ))),
        alt((
            value(
                SharedQuality::ManaValue,
                alt((tag("mana values"), tag("mana value"))),
            ),
            value(SharedQuality::Power, alt((tag("powers"), tag("power")))),
            value(
                SharedQuality::Toughness,
                alt((tag("toughnesses"), tag("toughness"))),
            ),
            value(
                SharedQuality::TotalPowerToughness,
                tag("total power and toughness"),
            ),
            value(
                SharedQuality::CardType,
                alt((tag("card types"), tag("card type"))),
            ),
            value(SharedQuality::Name, alt((tag("names"), tag("name")))),
        )),
    )
    .parse(input)
}

/// CR 701.23a + CR 107.1: Split a search filter tail on conjunction boundaries
/// (`"<primary> and a <secondary>"`, `"... and an ..."`, `"... and basic ..."`)
/// so each filter phrase parses independently. Returns the primary filter and
/// a list of extra filters; the list is empty in the common single-filter case.
///
/// The conjunction scan ends at the first action-clause comma or sentence
/// boundary (e.g., `"..., put them onto the battlefield tapped, then shuffle"`)
/// because anything after that belongs to the destination / action chain — not
/// to the filter expression. Serial-list commas stay in the filter region.
fn parse_search_filter_with_extras(
    tail: &str,
    ctx: &mut ParseContext,
) -> (TargetFilter, Vec<TargetFilter>) {
    // structural: not dispatch — bound the filter region at the first action
    // clause or sentence terminator before running the conjunction combinator,
    // so `" and "` inside e.g. `"put it onto the battlefield, then ..."` can't
    // pollute the filter split.
    let filter_region = search_filter_region(tail);

    if let Some(filters) = parse_each_basic_land_type_search_filters(filter_region) {
        return filters;
    }

    // CR 701.23a: A disjunctive series ("a X card, a Y card, or a Z card")
    // describes ONE card matching any listed property — try the disjunction
    // split before the conjunction split so it isn't mis-classified as N separate
    // cards (count + MatchEachFilter). parse_search_filter_disjunction returns
    // None for <2 disjunction segments, so "and"-lists and single filters fall
    // through to the conjunction path unchanged.
    if let Some(or_filter) = parse_search_filter_disjunction(tail, ctx) {
        return (or_filter, Vec::new());
    }

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

fn parse_each_basic_land_type_search_filters(
    filter_region: &str,
) -> Option<(TargetFilter, Vec<TargetFilter>)> {
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("land card of each basic land type"),
        tag("land cards of each basic land type"),
    ))
    .parse(filter_region.trim())
    .ok()?;
    rest.is_empty().then(|| {
        let mut filters = ["Plains", "Island", "Swamp", "Mountain", "Forest"]
            .into_iter()
            .map(land_subtype_filter);
        let primary = filters.next().expect("basic land type list is non-empty");
        (primary, filters.collect())
    })
}

fn land_subtype_filter(subtype: &str) -> TargetFilter {
    TargetFilter::Typed(TypedFilter::land().subtype(subtype.to_string()))
}

fn search_filter_region(text: &str) -> &str {
    parse_filter_region_terminator(text).unwrap_or(text)
}

fn parse_filter_region_terminator(input: &str) -> Option<&str> {
    [
        ". ",
        ".",
        ", put ",
        ", reveal ",
        ", then ",
        ", shuffle ",
        ", exile ",
        " and exile ",
        " and reveal ",
        " with different names",
        " with different powers",
        " that have different ",
        " that each have different ",
        " that don't share ",
        " that do not share ",
    ]
    .into_iter()
    .filter_map(|delimiter| parse_filter_region_delimiter(input, delimiter))
    .min_by_key(|before| before.len())
}

fn parse_filter_region_delimiter<'a>(input: &'a str, delimiter: &'static str) -> Option<&'a str> {
    let mut scan = (
        take_until::<_, _, OracleError<'_>>(delimiter),
        tag(delimiter),
    );
    let Ok((_, (before, _))) = scan.parse(input) else {
        return None;
    };
    Some(before)
}

// (nom `alt` arm that consumes the conjunction, amount pushed back onto
// the remainder so the "basic" supertype stays on the following segment)
#[derive(Clone, Copy)]
enum Conjunction {
    AndA,
    AndAn,
    AndBasic,
    CommaA,
    CommaAn,
    CommaAndA,
    CommaAndAn,
    CommaBasic,
    CommaAndBasic,
}

/// Split a filter-region string (no action chain) on article/basic
/// conjunctions using a nom `take_until` scan. For basic variants the supertype
/// stays attached to the following segment by re-prepending `"basic "` to the
/// remainder after consuming the shared delimiter prefix. Returns a
/// single-segment vector when no conjunction matches.
fn split_filter_conjunctions(filter_region: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut remaining = filter_region;
    loop {
        let Some((rest, before, conj)) = parse_next_filter_conjunction(remaining) else {
            segments.push(remaining.trim());
            break;
        };
        segments.push(before.trim());
        remaining = match conj {
            Conjunction::AndA
            | Conjunction::AndAn
            | Conjunction::CommaA
            | Conjunction::CommaAn
            | Conjunction::CommaAndA
            | Conjunction::CommaAndAn => rest,
            // Keep the "basic " supertype attached to the following segment.
            // SAFETY: `rest` is a suffix of `remaining`, so stepping back
            // "basic ".len() bytes yields a well-aligned slice that begins with
            // "basic …".
            Conjunction::AndBasic | Conjunction::CommaBasic | Conjunction::CommaAndBasic => {
                let start = remaining.len() - rest.len() - "basic ".len();
                &remaining[start..]
            }
        };
    }
    segments
}

fn parse_next_filter_conjunction(input: &str) -> Option<(&str, &str, Conjunction)> {
    [
        (", and basic ", Conjunction::CommaAndBasic),
        (", and an ", Conjunction::CommaAndAn),
        (", and a ", Conjunction::CommaAndA),
        (", basic ", Conjunction::CommaBasic),
        (", an ", Conjunction::CommaAn),
        (", a ", Conjunction::CommaA),
        (" and basic ", Conjunction::AndBasic),
        (" and an ", Conjunction::AndAn),
        (" and a ", Conjunction::AndA),
    ]
    .into_iter()
    .filter_map(|(delimiter, conjunction)| {
        parse_filter_conjunction_delimiter(input, delimiter, conjunction)
    })
    .min_by_key(|(_, before, _)| before.len())
}

fn parse_filter_conjunction_delimiter<'a>(
    input: &'a str,
    delimiter: &'static str,
    conjunction: Conjunction,
) -> Option<(&'a str, &'a str, Conjunction)> {
    let mut scan = (
        take_until::<_, _, OracleError<'_>>(delimiter),
        tag(delimiter),
    );
    let Ok((rest, (before, _))) = scan.parse(input) else {
        return None;
    };
    Some((rest, before, conjunction))
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

    // CR 701.23a: The possessive determiner identifies the searched player; the
    // zone(s) that follow ("library" for a single-zone tutor, or "graveyard,
    // hand, and library" for a multi-zone exile like Ancient Vendetta) do not
    // change WHO is searched. Match the determiner alone so multi-zone opponent
    // searches don't silently drop the target player.
    let (filter, _rest) = nom_on_lower(lower, lower, |i| {
        preceded(
            tag("search "),
            alt((
                value(
                    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                    tag("target opponent's "),
                ),
                value(
                    TargetFilter::Typed(TypedFilter::default()),
                    tag("target player's "),
                ),
                value(
                    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                    tag("an opponent's "),
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
    let (filter_text, dynamic_count) = split_seek_for_each_count_suffix(filter_text)
        .map_or((filter_text, None), |(remaining, count)| {
            (remaining, Some(count))
        });

    // Extract count: "two nonland cards" → (2, "nonland cards"); "x cards" → (X, "cards").
    // CR 107.3a + CR 601.2b: X resolves to the caster's announced value at cast time.
    let (count, remaining) = if let Some(expr) = dynamic_count {
        (expr, filter_text)
    } else if let Ok((rest, expr)) = nom_quantity::parse_quantity_expr_number(filter_text) {
        (expr, rest.trim_start())
    } else {
        (QuantityExpr::Fixed { value: 1 }, filter_text)
    };

    // Strip leading article "a "/"an "
    let remaining = nom_primitives::parse_article
        .parse(remaining)
        .map(|(rest, _)| rest)
        .unwrap_or(remaining);

    let (filter, extra_filters) = parse_search_filter_with_extras(remaining, ctx);

    SeekDetails {
        filter,
        count,
        from_top,
        destination,
        enter_tapped,
        extra_filters,
    }
}

fn split_seek_for_each_count_suffix(filter_text: &str) -> Option<(&str, QuantityExpr)> {
    let (suffix, remaining) = take_until::<_, _, OracleError<'_>>(" for each ")
        .parse(filter_text)
        .ok()?;
    let (clause, _) = tag::<_, _, OracleError<'_>>(" for each ")
        .parse(suffix)
        .ok()?;
    let count = oracle_quantity::parse_for_each_clause_expr(clause.trim())?;
    Some((remaining.trim_end(), count))
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
pub(crate) fn parse_search_filter(text: &str, ctx: &mut ParseContext) -> TargetFilter {
    let type_text = search_filter_region(text).trim();

    if let Some(filter) = parse_search_filter_color_disjunction(type_text, ctx) {
        return filter;
    }

    if let Some(filter) = parse_search_filter_disjunction(type_text, ctx) {
        return filter;
    }

    if let Some(filter) = parse_search_filter_leading_property_stack(type_text, ctx) {
        return filter;
    }

    if let Ok((rest, filter)) = parse_card_with_highest_mana_value_library_filter(type_text) {
        if rest.trim().is_empty() {
            return filter;
        }
    }

    // CR 201.2 + CR 701.23a: "a card named X" / "a [type] card named X" — a
    // name filter (God-Pharaoh's-Gift-class tutors, Lost Legacy, etc.). Parse
    // before the type-phrase attempt so "card" isn't mistaken for a type word
    // and the name isn't dropped by the fallback path.
    if let Some(filter) = parse_search_named_filter(type_text) {
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

/// CR 201.2 + CR 701.23a: Parse a "card named X" search filter (e.g. the filter
/// region of "search ... for a card named God-Pharaoh's Gift"), returning a
/// `FilterProp::Named` filter (name match is case-insensitive at runtime).
///
/// Anchored on the `"card named "` template (the leading article was already
/// stripped by the caller). Anchoring is deliberate: it bails on the negated
/// "... not named X" form (owned by `parse_not_named_suffix`) and on descriptive
/// clauses like "a card with a name noted as you drafted cards named X" (Aether
/// Searcher), where "named" is not the search-target template — neither begins
/// with "card named ".
///
/// Card names can contain commas (Altanak, the Thrice-Called) AND the word
/// "and" (Sword of Fire and Ice, Gisa and Geralf), so the name is never split
/// on punctuation, and a bare " and " is NOT a boundary — only a clause-joining
/// conjunction terminates it (see [`parse_name_terminator`]).
///
/// Kept separate from the name extractors in `oracle_target.rs` / `condition.rs`
/// on purpose: those split on `,`/`.`, which would truncate comma-bearing names.
fn parse_search_named_filter(text: &str) -> Option<TargetFilter> {
    let (after, _) = tag::<_, _, OracleError<'_>>("card named ")
        .parse(text)
        .ok()?;
    // CR 201.2: The name runs to the earliest *clause-joining* terminator. Scan
    // at word boundaries (every terminator begins with a space) and stop at the
    // first position where `parse_name_terminator` matches, so a " and " that is
    // part of the name ("Fire and Ice") is preserved.
    let name_end = after
        .char_indices()
        .filter(|&(_, c)| c == ' ')
        .find(|&(idx, _)| parse_name_terminator(&after[idx..]).is_ok())
        .map_or(after.len(), |(idx, _)| idx);
    let name = after[..name_end].trim_end_matches('.').trim();
    (!name.is_empty()).then(|| {
        TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Named {
            name: name.to_string(),
        }]))
    })
}

/// CR 201.2 + CR 701.18a: Match a clause-joining terminator that ends a card
/// name in "card named X …". A bare " and " is NOT a terminator (it may be part
/// of the name — "Fire and Ice", "Gisa and Geralf"); " and " only ends the name
/// when it introduces a follow-up *action* (" and put/reveal/shuffle/…"). The
/// disjunction (" and/or ") and sequence (" then ") connectives always end it.
fn parse_name_terminator(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    alt((
        value((), tag(" and/or ")),
        value((), tag(" then ")),
        value(
            (),
            (
                tag(" and "),
                alt((
                    tag("put"),
                    tag("reveal"),
                    tag("shuffle"),
                    tag("exile"),
                    tag("play"),
                    tag("cast"),
                    tag("attach"),
                    tag("return"),
                )),
            ),
        ),
    ))
    .parse(input)
}

/// CR 701.23a: Match a separator between zones in a zone list — handles commas,
/// "and", "or", and the "and/or" conjunction in any combination. Longer forms
/// are tried first so the list parser consumes the whole connective.
fn parse_search_zone_separator(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    value(
        (),
        alt((
            tag(", and/or "),
            tag(", and "),
            tag(", or "),
            tag(" and/or "),
            tag(" and "),
            tag(" or "),
            tag(", "),
        )),
    )
    .parse(input)
}

/// CR 701.23a: Detect a multi-zone search ("search your graveyard, hand, and/or
/// library for ...") and return the deduplicated zone set in canonical order
/// (Graveyard, Hand, Library). Returns `None` for the ordinary single-zone
/// library search so the caller falls back to the library-only default.
pub(super) fn parse_multi_search_zones(lower: &str) -> Option<Vec<Zone>> {
    fn run(input: &str) -> Result<Vec<Zone>, nom::Err<OracleError<'_>>> {
        let (input, _) = take_until::<_, _, OracleError<'_>>("search ").parse(input)?;
        let (input, _) = tag("search ").parse(input)?;
        // Strip the possessive that precedes the zone list. Multi-zone tutors are
        // always controller-owned ("your"); the opponent-search forms remain
        // single-zone and never reach here.
        let (input, _) = opt(alt((
            tag("your "),
            tag("their "),
            tag("target player's "),
            tag("target opponent's "),
            tag("an opponent's "),
        )))
        .parse(input)?;
        // `take_until` yields `(remaining, consumed_before)` — the zone list is
        // the consumed-before output, not the remainder.
        let (_, region) = take_until(" for ").parse(input)?;
        // Reuse the canonical zone-word combinator (handles plurals + the full
        // zone vocabulary); the canonicalize step below keeps only the three
        // tutoring zones.
        let (_, zones) =
            separated_list1(parse_search_zone_separator, parse_zone_word).parse(region)?;
        Ok(zones)
    }
    let zones = run(lower).ok()?;
    // CR 701.23a: Canonicalize and dedupe; only treat as multi-zone when 2+
    // distinct zones are named (a lone "library" is the ordinary tutor).
    let set: Vec<Zone> = [Zone::Graveyard, Zone::Hand, Zone::Library]
        .into_iter()
        .filter(|z| zones.contains(z))
        .collect();
    (set.len() >= 2).then_some(set)
}

fn parse_search_filter_color_disjunction(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<TargetFilter> {
    let (rest, first_color) = nom_primitives::parse_color.parse(text).ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" or ").parse(rest).ok()?;
    let (rest, second_color) = nom_primitives::parse_color.parse(rest).ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" ").parse(rest).ok()?;

    let base_filter = parse_search_filter(rest, ctx);
    if !search_filter_has_meaningful_content(&base_filter) {
        return None;
    }

    let filters = [first_color, second_color]
        .into_iter()
        .map(|color| {
            apply_search_suffix_constraints(
                base_filter.clone(),
                &SearchSuffixConstraints {
                    properties: vec![FilterProp::HasColor { color }],
                    type_filters: Vec::new(),
                    filters: Vec::new(),
                },
            )
        })
        .collect();
    Some(TargetFilter::Or { filters })
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
                filters: Vec::new(),
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
        .flat_map(|s| match parse_search_filter(s, ctx) {
            TargetFilter::Or { filters } => filters,
            filter => vec![filter],
        })
        .filter(search_filter_has_meaningful_content)
        .collect();
    (filters.len() >= 2).then(|| {
        let filter = normalize_search_filter(TargetFilter::Or { filters });
        let filter = apply_shared_leading_search_properties(filter_region, filter);
        // CR 701.23a: each comma/or disjunct is parsed independently, so a
        // trailing "with mana value N" suffix lands only on the final leg
        // ("creature, instant, or sorcery card with mana value N", #2892).
        // Distribute that trailing predicate back onto the earlier `Typed`
        // legs via the shared leg-locality authority, which keeps inherently
        // leg-local props (keyword/name/adjective) on their originating leg.
        distribute_properties_to_or(filter)
    })
}

fn apply_shared_leading_search_properties(
    filter_region: &str,
    filter: TargetFilter,
) -> TargetFilter {
    if !filter_region.as_bytes().contains(&b',') {
        return filter;
    }

    let suffix = leading_search_properties(filter_region);
    if suffix.properties.is_empty() {
        return filter;
    }

    if search_filter_all_land_subtype_branches(&filter) {
        apply_search_suffix_constraints(filter, &suffix)
    } else {
        filter
    }
}

fn leading_search_properties(filter_region: &str) -> SearchSuffixConstraints {
    let mut suffix = SearchSuffixConstraints::default();
    let mut remaining = filter_region;
    while let Ok((rest, property)) = parse_search_leading_filter_property(remaining) {
        suffix.properties.push(property);
        remaining = rest;
    }
    suffix
}

fn search_filter_all_land_subtype_branches(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed.type_filters.iter().any(|type_filter| {
            matches!(type_filter, TypeFilter::Subtype(subtype)
                if infer_core_type_for_subtype(subtype) == Some(CoreType::Land))
        }),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().all(search_filter_all_land_subtype_branches)
        }
        _ => false,
    }
}

/// The two structural axes of a search-filter disjunction. Replaces the
/// flat 7-variant `Disjunction` cluster — every consumer site checks
/// `connector` (Or vs AndOr) and `leading` (article shape) independently,
/// which is what the parameterized form exposes directly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Connector {
    Or,
    AndOr,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Leading {
    A,
    An,
    Basic,
    None,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Disjunction {
    connector: Connector,
    leading: Leading,
}

// Leading-article dispatch on a single alt() — also reused by the
// comma-member peel below to recover the article-stripped (or supertype)
// form of each enumerated member.
fn parse_leading_article(i: &str) -> nom::IResult<&str, Leading, OracleError<'_>> {
    alt((
        value(Leading::A, tag::<_, _, OracleError<'_>>("a ")),
        value(Leading::An, tag::<_, _, OracleError<'_>>("an ")),
        value(Leading::Basic, tag::<_, _, OracleError<'_>>("basic ")),
        value(Leading::None, tag::<_, _, OracleError<'_>>("")),
    ))
    .parse(i)
}

// CR 701.23a: recover one co-equal disjunctive member into its own segment.
// Strips a leading article ("a"/"an") but preserves "basic" as a supertype.
fn push_peeled_member<'a>(member: &'a str, segments: &mut Vec<&'a str>) {
    let m = member.trim().trim_end_matches(',').trim_end();
    if m.is_empty() {
        return;
    }
    let cleaned = strip_search_member_leading(m);
    if !cleaned.is_empty() {
        segments.push(cleaned);
    }
}

fn strip_search_member_leading(member: &str) -> &str {
    match parse_leading_article(member) {
        Ok((rest, Leading::Basic)) => {
            let start = member.len() - rest.len() - "basic ".len();
            &member[start..]
        }
        Ok((rest, _)) => rest,
        Err(_) => member,
    }
}

fn parse_search_named_member(member: &str) -> Option<TargetFilter> {
    parse_search_named_filter(strip_search_member_leading(member.trim()))
}

fn parse_comma_member_start(input: &str) -> OracleResult<'_, ()> {
    let input = input.trim_start();
    let input = strip_search_member_leading(input);
    alt((
        value((), tag::<_, _, OracleError<'_>>("card named ")),
        value((), parse_bare_search_disjunction_right),
    ))
    .parse(input)
}

fn split_next_comma_member(region: &str) -> Option<(&str, &str)> {
    region.match_indices(',').find_map(|(idx, _)| {
        let after_comma = &region[idx + 1..];
        parse_comma_member_start(after_comma)
            .is_ok()
            .then(|| (&region[..idx], after_comma.trim_start()))
    })
}

// CR 701.23a: a left segment may itself enumerate co-members the upstream comma
// split did not break out. Peel each delimiter comma into its own flat segment
// without shredding comma-bearing card names in "card named X" members.
fn peel_comma_members<'a>(region: &'a str, segments: &mut Vec<&'a str>) {
    let mut remaining = region.trim();
    while !remaining.is_empty() {
        if let Some((member, rest)) = split_next_comma_member(remaining) {
            push_peeled_member(member, segments);
            remaining = rest;
        } else {
            push_peeled_member(remaining, segments);
            break;
        }
    }
}

// CR 202.3: comparator words following a bare " or " form a numeric bound
// ("3 or less", "X or higher"), never a disjunction terminator. Negative-
// lookahead guard for split_terminal_or.
fn parse_comparator_word(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>("less"),
            tag("greater"),
            tag("more"),
            tag("fewer"),
            tag("higher"),
            tag("lower"),
        )),
    )
    .parse(input)
}

// CR 701.23a: split a disjunctive series at its TERMINAL connector, returning
// the left side, the final member, and which connector matched. " and/or "
// takes precedence over a bare " or "; a bare " or " introducing a comparator
// word ("3 or less") is skipped so it never terminates the series.
fn split_terminal_or(region: &str) -> Option<(&str, &str, Connector)> {
    let mut last: Option<(&str, &str, Connector)> = None;
    // rightmost " and/or "
    let mut cursor = region;
    while let Ok((after, _)) = take_until::<_, _, OracleError<'_>>(" and/or ").parse(cursor) {
        let after_conn = &after[" and/or ".len()..];
        let before_final = &region[..region.len() - after.len()];
        let final_member = &region[region.len() - after_conn.len()..];
        last = Some((before_final, final_member, Connector::AndOr));
        cursor = after_conn;
    }
    // rightmost NON-comparator bare " or "
    let mut cursor = region;
    while let Ok((after, _)) = take_until::<_, _, OracleError<'_>>(" or ").parse(cursor) {
        let after_conn = &after[" or ".len()..];
        if peek(parse_comparator_word).parse(after_conn).is_err() {
            let before_final = &region[..region.len() - after.len()];
            let final_member = &region[region.len() - after_conn.len()..];
            match last {
                Some((_, _, Connector::AndOr)) => {}
                Some((prev_before, _, _)) if prev_before.len() >= before_final.len() => {}
                _ => last = Some((before_final, final_member, Connector::Or)),
            }
        }
        cursor = after_conn;
    }
    last.map(|(b, f, c)| (b.trim(), f.trim(), c))
}

// CR 701.23a: front-gate for the comma-series disjunction class ("a X, a Y, or
// a Z" — count 1, one choice among co-equal filters). Fires deterministically
// BEFORE the greedy bare-or loop so an intra-member union or a comma-bearing
// card name is split correctly. Returns None (defer to the loop) for simple
// non-comma 2-way disjunctions and for "and/or" comma enumerations.
fn detect_comma_series_or(filter_region: &str) -> Option<Vec<&str>> {
    // Scope to the comma-series class; simple 2-way disjunctions stay on the loop.
    // structural: not dispatch (comma-presence scope gate)
    if !filter_region.as_bytes().contains(&b',') {
        return None;
    }
    let (before_final, final_member, connector) = split_terminal_or(filter_region)?;

    // Article-tolerant card-head guard: the final member must be a co-equal card
    // filter — either "card named X" (anchored, no article) or, after stripping a
    // leading article, a bare "<head> card(s)". "basic" is preserved (supertype).
    // This strip MUST match push_peeled_member's strip so guard and peeler agree.
    let head = strip_search_member_leading(final_member);
    if parse_search_named_member(final_member).is_none()
        && parse_bare_search_disjunction_right(head).is_err()
    {
        return None;
    }

    // and/or-comma defer: "X, Y, and/or Z" enumerations are handled by the loop's
    // existing and/or gate; keep the per-type comparator form on the type path.
    if connector == Connector::AndOr && before_final.as_bytes().contains(&b',') {
        // structural: not dispatch (comma-in-left enumeration)
        return None;
    }

    let mut segments = Vec::new();
    // CR 201.2: a named left segment's comma belongs to the card name — keep whole.
    if parse_search_named_member(before_final).is_some() {
        push_peeled_member(before_final, &mut segments);
    } else {
        peel_comma_members(before_final, &mut segments);
    }
    push_peeled_member(final_member, &mut segments);

    (segments.len() >= 2).then_some(segments)
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
    // CR 701.23a: comma-series disjunctions are split deterministically here, ahead
    // of the greedy bare-or loop, so intra-member unions and comma-bearing card
    // names are not mis-split into a multi-card conjunction.
    if let Some(segments) = detect_comma_series_or(filter_region) {
        return segments;
    }

    // Sub-combinator that dispatches the leading-article axis on a single
    // alt() — shared between the and/or and or scans so future leading
    // variants (e.g., AndOrBasic) require one arm, not one per connector.
    fn parse_leading<'a>(
        connector: Connector,
        connector_tag: &'static str,
    ) -> impl Parser<&'a str, Output = Disjunction, Error = OracleError<'a>> {
        move |i: &'a str| {
            let (i, _) = tag::<_, _, OracleError<'a>>(connector_tag).parse(i)?;
            let (i, leading) = parse_leading_article(i)?;
            Ok((i, Disjunction { connector, leading }))
        }
    }

    let mut segments = Vec::new();
    let mut remaining = filter_region;
    loop {
        let mut and_or_scan = (
            take_until::<_, _, OracleError<'_>>(" and/or "),
            parse_leading(Connector::AndOr, " and/or "),
        );
        let parsed = if let Ok(found) = and_or_scan.parse(remaining) {
            Some(found)
        } else {
            let mut or_scan = (
                take_until::<_, _, OracleError<'_>>(" or "),
                parse_leading(Connector::Or, " or "),
            );
            or_scan.parse(remaining).ok()
        };

        let Some((rest, (before, disjunction))) = parsed else {
            segments.push(remaining.trim());
            break;
        };

        // Bare " or " gates: only fires with no article, and the right side
        // must look like a card-bearing alternative for the current grammar
        // (otherwise comparator suffixes "or less" would split incorrectly).
        if disjunction.connector == Connector::Or
            && disjunction.leading == Leading::None
            && !bare_search_disjunction_allowed(before.trim(), rest.trim_start())
        {
            if segments.is_empty() {
                return vec![filter_region.trim()];
            }
            segments.push(remaining.trim());
            break;
        }

        // Bare " and/or " gate: a comma in the left segment indicates an
        // enumeration ("X, Y, and/or Z") that the upstream split has already
        // mishandled — bail rather than over-split.
        if disjunction.connector == Connector::AndOr
            && disjunction.leading == Leading::None
            && before.as_bytes().contains(&b',')
        // structural: not dispatch (comma presence check)
        {
            if segments.is_empty() {
                return vec![filter_region.trim()];
            }
            segments.push(remaining.trim());
            break;
        }

        // CR 201.2: a named member's comma belongs to the card name
        // ("Halvar, God of Battle") — never peel it. Otherwise peel co-members.
        if parse_search_named_member(before).is_some() {
            push_peeled_member(before, &mut segments);
        } else {
            peel_comma_members(before, &mut segments);
        }
        remaining = if disjunction.leading == Leading::Basic {
            // "basic" is a supertype, not an article — recover it into the
            // right segment so the type-phrase parser sees "basic <type>".
            let start = filter_region.len() - rest.len() - "basic ".len();
            &filter_region[start..]
        } else {
            rest
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
    filters: Vec<TargetFilter>,
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
    if suffix.properties.is_empty() && suffix.type_filters.is_empty() && suffix.filters.is_empty() {
        return filter;
    }

    let branch_suffix = SearchSuffixConstraints {
        properties: suffix.properties.clone(),
        type_filters: suffix.type_filters.clone(),
        filters: Vec::new(),
    };

    let filter = match filter {
        TargetFilter::Any => {
            TargetFilter::Typed(apply_search_suffix_to_typed(TypedFilter::default(), suffix))
        }
        TargetFilter::Typed(typed_filter) => {
            TargetFilter::Typed(apply_search_suffix_to_typed(typed_filter, suffix))
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|branch| apply_search_suffix_constraints(branch, &branch_suffix))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|branch| apply_search_suffix_constraints(branch, &branch_suffix))
                .collect(),
        },
        other => other,
    };

    if suffix.filters.is_empty() {
        filter
    } else {
        let mut filters = vec![filter];
        for suffix_filter in &suffix.filters {
            if !filters.contains(suffix_filter) {
                filters.push(suffix_filter.clone());
            }
        }
        TargetFilter::And { filters }
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

pub(crate) fn parse_search_name_reference_suffix(
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

fn parse_chosen_name_reference_suffix(
    input: &str,
) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    let (rest, _) = alt((
        tag("which have the same name as the chosen "),
        tag("which has the same name as the chosen "),
        tag("that have the same name as the chosen "),
        tag("that has the same name as the chosen "),
        tag("with the same name as the chosen "),
    ))
    .parse(input)?;
    let (rest, _) =
        take_till1::<_, _, OracleError<'_>>(|c: char| c == ',' || c == '.').parse(rest)?;
    Ok((rest, ()))
}

fn parse_noted_name_search_suffix(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("with a name noted as "),
        tag("with a name you noted for "),
    ))
    .parse(input)?;
    let (rest, _) =
        take_till1::<_, _, OracleError<'_>>(|c: char| c == ',' || c == '.').parse(rest)?;
    Ok((rest, ()))
}

fn parse_not_named_suffix(input: &str) -> Result<(&str, FilterProp), nom::Err<OracleError<'_>>> {
    let (rest, _) = tag("not named ").parse(input)?;
    let (after_name, name) = if let Ok((after_name, (name, _))) = (
        take_until::<_, _, OracleError<'_>>(" that "),
        peek(tag::<_, _, OracleError<'_>>(" that ")),
    )
        .parse(rest)
    {
        (after_name, name)
    } else {
        take_till1::<_, _, OracleError<'_>>(|c: char| c == ',' || c == '.').parse(rest)?
    };

    Ok((
        after_name,
        FilterProp::SharesQuality {
            quality: SharedQuality::Name,
            reference: Some(Box::new(TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::Named {
                    name: name.trim().to_string(),
                }]),
            ))),
            relation: SharedQualityRelation::DoesNotShare,
        },
    ))
}

fn parse_same_total_power_toughness_suffix(
    input: &str,
) -> Result<(&str, FilterProp), nom::Err<OracleError<'_>>> {
    let (rest, _) = tag("with the same total power and toughness").parse(input)?;
    Ok((
        rest,
        FilterProp::SharesQuality {
            quality: SharedQuality::TotalPowerToughness,
            reference: Some(Box::new(TargetFilter::TriggeringSource)),
            relation: SharedQualityRelation::Shares,
        },
    ))
}

fn parse_highest_mana_value_library_suffix(
    input: &str,
) -> Result<(&str, Vec<FilterProp>), nom::Err<OracleError<'_>>> {
    let (rest, _) = tag("with the highest mana value among cards in your library with mana value ")
        .parse(input)?;
    let (rest, threshold) =
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("x or less, where x is ").parse(rest) {
            let (_, qty) = nom_quantity::parse_quantity_ref_complete(rest)?;
            ("", QuantityExpr::Ref { qty })
        } else {
            let (rest, _) = tag("less than or equal to ").parse(rest)?;
            let (_, qty) = nom_quantity::parse_quantity_ref_complete(rest)?;
            ("", QuantityExpr::Ref { qty })
        };

    let eligible_filter = TargetFilter::Typed(
        TypedFilter::card()
            .controller(ControllerRef::You)
            .properties(vec![
                FilterProp::InZone {
                    zone: Zone::Library,
                },
                FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: threshold.clone(),
                },
            ]),
    );

    Ok((
        rest,
        vec![
            FilterProp::Cmc {
                comparator: Comparator::LE,
                value: threshold,
            },
            FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate {
                        function: AggregateFunction::Max,
                        property: ObjectProperty::ManaValue,
                        filter: eligible_filter,
                    },
                },
            },
        ],
    ))
}

fn parse_card_with_highest_mana_value_library_filter(
    input: &str,
) -> Result<(&str, TargetFilter), nom::Err<OracleError<'_>>> {
    let (rest, _) = tag("card ").parse(input)?;
    let (rest, properties) = parse_highest_mana_value_library_suffix(rest)?;
    Ok((
        rest,
        TargetFilter::Typed(TypedFilter::card().properties(properties)),
    ))
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
        // CR 608.2c: Negation wraps the inner prop's zone reference — recurse (mirrors AnyOf).
        FilterProp::Not { prop } => filter_prop_is_zone(prop),
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

/// CR 701.23a: Parse a possessive-scoped zone phrase (`"your library"`,
/// `"an opponent's library"`, etc.) into the typed `(Zone, ControllerRef)`
/// pair the engine consumes. Composes from `tag()` arms over the connector
/// × zone axes so future graveyard / hand variants add a single combinator
/// arm rather than a new sibling on every consumer.
fn parse_possessive_zone(
    input: &str,
) -> nom::IResult<&str, (Zone, ControllerRef), OracleError<'_>> {
    alt((
        value(
            (Zone::Library, ControllerRef::You),
            tag::<_, _, OracleError<'_>>("your library"),
        ),
        value(
            (Zone::Library, ControllerRef::Opponent),
            tag("an opponent's library"),
        ),
        value(
            (Zone::Library, ControllerRef::Opponent),
            tag("target opponent's library"),
        ),
        value(
            (Zone::Library, ControllerRef::Opponent),
            tag("opponent's library"),
        ),
    ))
    .parse(input)
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
            || tag::<_, _, OracleError<'_>>("exile ")
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

        // CR 201.2 + CR 608.2c: "with that/the chosen name" in search filters
        // refers to a resolving card-name choice stored on the source, not the
        // source object's own name.
        if let Ok((rest, _)) = alt((
            tag::<_, _, OracleError<'_>>("with that name"),
            tag("with the chosen name"),
        ))
        .parse(remaining)
        {
            suffix.filters.push(TargetFilter::HasChosenName);
            remaining = rest.trim_start();
            continue;
        }

        // Draft-note search filters (Aether Searcher / Smuggler Captain) are
        // already unsupported by their draft-note abilities. Consume the suffix
        // here so the search filter does not add a misleading target-fallback
        // warning on top of the real unsupported draft mechanic.
        if let Ok((rest, _)) = parse_noted_name_search_suffix(remaining) {
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

        if let Ok((rest, prop)) = parse_not_named_suffix(remaining) {
            suffix.properties.push(prop);
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, _)) = parse_chosen_name_reference_suffix(remaining) {
            suffix.properties.push(FilterProp::SameNameAsParentTarget);
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

        // CR 205.3m + CR 701.23a: "of the most prevalent creature type in
        // <possessive zone>". Parameterized over `(Zone, ControllerRef)` so
        // future opponent's-library / graveyard variants reuse this slot
        // instead of spawning a sibling tag arm per phrasing.
        if let Ok((rest, (zone, scope))) = preceded(
            tag::<_, _, OracleError<'_>>("of the most prevalent creature type in "),
            parse_possessive_zone,
        )
        .parse(remaining)
        {
            suffix
                .properties
                .push(FilterProp::MostPrevalentCreatureTypeIn { zone, scope });
            remaining = rest.trim_start();
            continue;
        }

        // CR 608.2c: distinct-quality suffixes constrain the chosen set, not
        // individual cards. The constraint is already encoded upstream via
        // `scan_distinct_qualities_constraint`; this arm only consumes the marker.
        if let Ok((rest, _)) = parse_distinct_quality_suffix(remaining) {
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, prop)) = parse_shared_quality_clause(remaining, &ParseContext::default()) {
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

        if let Ok((rest, prop)) = parse_same_total_power_toughness_suffix(remaining) {
            remaining = rest.trim_start();
            suffix.properties.push(prop);
            continue;
        }

        if let Ok((rest, props)) = parse_highest_mana_value_library_suffix(remaining) {
            remaining = rest.trim_start();
            suffix.properties.extend(props);
            continue;
        }

        if let Ok((rest, _)) = alt((
            tag::<_, _, OracleError<'_>>("with a basic land type"),
            tag::<_, _, OracleError<'_>>("that have a basic land type"),
            tag::<_, _, OracleError<'_>>("that each have a basic land type"),
        ))
        .parse(remaining)
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

        if let Ok((rest, prop)) = preceded(
            (tag::<_, _, OracleError<'_>>("with"), space1),
            nom_filter::parse_no_abilities,
        )
        .parse(remaining)
        {
            suffix.properties.push(prop);
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

        if let Some((prop, consumed)) =
            parse_mana_value_suffix(remaining, &mut ParseContext::default())
        {
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
    let (rest, semantic_can_enchant) = alt((
        value(false, tag("with enchant ")),
        value(true, tag("that could enchant ")),
    ))
    .parse(input)?;
    let (after_target, target_text) =
        take_till1::<_, _, OracleError<'_>>(|c: char| c == ',' || c == '.').parse(rest)?;
    let (target, remainder) = {
        let (target, remainder) = parse_target(target_text.trim());
        if matches!(target, TargetFilter::Any) {
            parse_type_phrase(target_text.trim())
        } else {
            (target, remainder)
        }
    };
    if !remainder.trim().is_empty() || !search_filter_has_meaningful_content(&target) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let prop = if semantic_can_enchant
        && matches!(
            target,
            TargetFilter::ParentTarget | TargetFilter::ParentTargetSlot { .. }
        ) {
        FilterProp::CanEnchant {
            target: Box::new(target),
        }
    } else {
        FilterProp::WithKeyword {
            value: Keyword::Enchant(target),
        }
    };
    Ok((after_target, prop))
}

/// Parse the destination zone from search Oracle text.
/// Looks for "put it into your hand", "put it onto the battlefield", etc.
/// CR 701.23a + CR 608.2c: Parse the tail of a cultivate-class split-destination
/// clause — the portion AFTER the leading "put " — into a
/// [`SearchDestinationSplit`]. Expects e.g. "one onto the battlefield tapped and
/// the other into your hand". The primary destination is the battlefield (the
/// only primary in the A/B/C cluster); the count is the literal integer in
/// "put N"; `primary_enter_tapped` reflects the optional "tapped" modifier; the
/// rest zone is parsed from the "...and the rest/other into <zone>" tail.
///
/// All-nom; consumes via `scan_preceded`, which strips the leading "put ".
fn parse_search_split_destination(input: &str) -> OracleResult<'_, SearchDestinationSplit> {
    let (input, primary_count) = nom_primitives::parse_number(input)?;
    // Primary is always the battlefield for the A/B/C split cluster.
    let (input, _) =
        alt((tag(" onto the battlefield"), tag(" to the battlefield"))).parse(input)?;
    let (input, tapped) = opt(tag(" tapped")).parse(input)?;
    let (input, _) = tag(" and ").parse(input)?;
    let (input, _) = opt(tag("put ")).parse(input)?;
    let (input, _) = parse_rest_cards_reference(input)?;
    let (input, rest_destination) = parse_choice_partition_destination(input)?;
    Ok((
        input,
        SearchDestinationSplit {
            primary_destination: Zone::Battlefield,
            primary_count,
            primary_enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(
                tapped.is_some(),
            ),
            rest_destination,
        },
    ))
}

fn parse_zone_pair_search_split(input: &str) -> OracleResult<'_, SearchDestinationSplit> {
    let (input, primary_count) = nom_primitives::parse_number(input)?;
    let (input, primary_destination) = parse_choice_partition_destination(input)?;
    let (input, _) = tag(" and ").parse(input)?;
    let (input, _) = opt(tag("put ")).parse(input)?;
    let (input, _) = parse_rest_cards_reference(input)?;
    let (input, rest_destination) = parse_choice_partition_destination(input)?;
    Ok((
        input,
        SearchDestinationSplit {
            primary_destination,
            primary_count,
            primary_enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(false),
            rest_destination,
        },
    ))
}

/// Detector: scan `lower` at word boundaries for the "put N onto the battlefield
/// ... and the rest/other into <zone>" split clause. Returns the parsed split
/// when the cultivate-class grammar matches, else `None` so the caller falls
/// back to single-zone destination handling.
fn detect_search_split_destination(lower: &str) -> Option<SearchDestinationSplit> {
    scan_preceded(lower, "put ", parse_search_split_destination)
        .or_else(|| scan_preceded(lower, "put ", parse_zone_pair_search_split))
        .map(|(split, _)| split)
}

/// Whether `lower` is a standalone put-destination clause already handled by a
/// preceding `SearchLibrary { split: Some(_) }` effect (Final Parting class).
pub(super) fn is_zone_pair_search_split_clause(lower: &str) -> bool {
    scan_preceded(lower, "put ", parse_zone_pair_search_split).is_some()
}

pub(super) fn parse_search_destination(lower: &str) -> Zone {
    if scan_contains_phrase(lower, "onto the battlefield") {
        Zone::Battlefield
    } else if scan_contains_phrase(lower, "exile it")
        || scan_contains_phrase(lower, "exile them")
        || scan_contains_phrase(lower, "exile that card")
        || scan_contains_phrase(lower, "exile the card")
    {
        Zone::Exile
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
    use crate::types::keywords::{Keyword, KeywordKind};
    use crate::types::mana::{ManaColor, ManaCost};

    #[test]
    fn named_filter_anchors_on_card_named_and_stops_at_conjunction() {
        // CR 201.2: "card named X" → Named X, with internal commas preserved.
        let f = parse_search_named_filter("card named altanak, the thrice-called");
        assert!(
            matches!(&f, Some(TargetFilter::Typed(t)) if t.properties.iter().any(|p| matches!(p, FilterProp::Named { name } if name == "altanak, the thrice-called"))),
            "comma-bearing name must be preserved, got {f:?}"
        );

        // Agency Outfitter: "named X and/or a card named Y" must not over-consume
        // across the conjunction into one bogus filter.
        let f = parse_search_named_filter(
            "card named magnifying glass and/or a card named thinking cap",
        );
        assert!(
            matches!(&f, Some(TargetFilter::Typed(t)) if t.properties.iter().any(|p| matches!(p, FilterProp::Named { name } if name == "magnifying glass"))),
            "must stop at and/or, got {f:?}"
        );

        // "and put …" destination is a terminator.
        let f = parse_search_named_filter(
            "card named god-pharaoh's gift and put it onto the battlefield",
        );
        assert!(
            matches!(&f, Some(TargetFilter::Typed(t)) if t.properties.iter().any(|p| matches!(p, FilterProp::Named { name } if name == "god-pharaoh's gift"))),
            "got {f:?}"
        );

        // Card names containing "and" must NOT be truncated — a bare " and " is
        // a terminator only when it introduces a follow-up action.
        for (input, want) in [
            ("card named sword of fire and ice", "sword of fire and ice"),
            ("card named gisa and geralf", "gisa and geralf"),
            (
                "card named sword of fire and ice and put it onto the battlefield",
                "sword of fire and ice",
            ),
        ] {
            let f = parse_search_named_filter(input);
            assert!(
                matches!(&f, Some(TargetFilter::Typed(t)) if t.properties.iter().any(|p| matches!(p, FilterProp::Named { name } if name == want))),
                "name with 'and' mishandled for {input:?}: got {f:?}"
            );
        }

        // Aether Searcher: "card with a name noted ... cards named X" is not a
        // name-equality template — must bail (not anchored on "card named ").
        assert_eq!(
            parse_search_named_filter(
                "card with a name noted as you drafted cards named aether searcher"
            ),
            None
        );

        // A plain type filter is not a named filter.
        assert_eq!(parse_search_named_filter("creature card"), None);
    }

    #[test]
    fn multi_zone_tutor_detection_and_named_filter() {
        // CR 701.23a: God-Pharaoh's-Gift-class tutors search graveyard + hand +
        // library for a named card. The zone list and the "named X" filter must
        // both parse, regardless of comma-separated zone ordering / "and/or".
        let mut ctx = ParseContext::default();
        for lower in [
            "search your graveyard, hand, and/or library for a card named god-pharaoh's gift",
            "search your graveyard, hand, and/or library for a card named altanak, the thrice-called",
            "search your graveyard, hand, and/or library for an aura card",
        ] {
            let details = parse_search_library_details(lower, &mut ctx);
            assert_eq!(
                details.source_zones,
                vec![Zone::Graveyard, Zone::Hand, Zone::Library],
                "multi-zone detection failed for {lower:?}"
            );
        }

        // Named filter preserves the full card name, including internal commas.
        let details = parse_search_library_details(
            "search your graveyard, hand, and/or library for a card named altanak, the thrice-called",
            &mut ctx,
        );
        assert!(
            matches!(&details.filter, TargetFilter::Typed(t) if t.properties.iter().any(|p| matches!(p, FilterProp::Named { name } if name == "altanak, the thrice-called"))),
            "expected Named filter with full name, got {:?}",
            details.filter
        );

        // Ordinary single-zone library tutor stays library-only.
        let single =
            parse_search_library_details("search your library for a creature card", &mut ctx);
        assert_eq!(single.source_zones, vec![Zone::Library]);
    }

    #[test]
    fn multi_zone_chosen_name_exile_search_has_exile_destination() {
        // CR 201.2 + CR 701.23a + CR 701.18a: Unmoored Ego / The Stone Brain
        // search multiple hidden zones for cards matching the chosen name, then
        // exile the found cards. "and exile them" is the continuation action,
        // not an unmatched search-filter suffix.
        let lower = "choose a card name. search target opponent's graveyard, hand, and library for up to four cards with that name and exile them";
        let mut ctx = ParseContext::default();
        let details = parse_search_library_details(lower, &mut ctx);

        assert_eq!(
            details.source_zones,
            vec![Zone::Graveyard, Zone::Hand, Zone::Library]
        );
        assert!(details.up_to);
        assert_eq!(details.count, QuantityExpr::Fixed { value: 4 });
        assert_filter_contains(&details.filter, &TargetFilter::HasChosenName);
        assert_eq!(parse_search_destination(lower), Zone::Exile);
        assert!(ctx.diagnostics.iter().all(|diagnostic| !matches!(
            diagnostic,
            OracleDiagnostic::TargetFallback { context, .. }
                if context == "search-filter-suffix unmatched"
        )));
    }

    #[test]
    fn cultivate_lowers_to_split_destination() {
        // CR 701.23a + CR 608.2c: Cultivate's "put one onto the battlefield
        // tapped and the other into your hand" must populate the split, not
        // collapse to a single battlefield destination.
        let details = parse_search_library_details(
            "search your library for up to two basic land cards, reveal those cards, put one onto the battlefield tapped and the other into your hand, then shuffle",
            &mut ParseContext::default(),
        );
        let split = details
            .split
            .expect("cultivate must populate a SearchDestinationSplit");
        assert_eq!(split.primary_destination, Zone::Battlefield);
        assert_eq!(split.primary_count, 1);
        assert!(split.primary_enter_tapped.is_tapped());
        assert_eq!(split.rest_destination, Zone::Hand);
    }

    #[test]
    fn viewpoint_synchronization_primary_count_two() {
        // Pattern C: "put two onto the battlefield tapped and the other into
        // your hand" — proves primary_count is the literal integer, not 1.
        let details = parse_search_library_details(
            "search your library for up to three basic land cards, reveal those cards, put two onto the battlefield tapped and the other into your hand, then shuffle",
            &mut ParseContext::default(),
        );
        let split = details
            .split
            .expect("pattern C must populate a SearchDestinationSplit");
        assert_eq!(split.primary_count, 2);
        assert_eq!(split.primary_destination, Zone::Battlefield);
        assert_eq!(split.rest_destination, Zone::Hand);
    }

    #[test]
    fn final_parting_lowers_to_hand_graveyard_split() {
        let details = parse_search_library_details(
            "search your library for two cards. put one into your hand and the other into your graveyard. then shuffle",
            &mut ParseContext::default(),
        );
        let split = details
            .split
            .expect("Final Parting must populate a SearchDestinationSplit");
        assert_eq!(split.primary_destination, Zone::Hand);
        assert_eq!(split.primary_count, 1);
        assert!(!split.primary_enter_tapped.is_tapped());
        assert_eq!(split.rest_destination, Zone::Graveyard);
        assert_eq!(details.count, QuantityExpr::Fixed { value: 2 });
    }

    #[test]
    fn zone_pair_split_lowers_to_exile_and_library_bottom() {
        let details = parse_search_library_details(
            "search your library for two cards. put one into exile and the other on the bottom of your library. then shuffle",
            &mut ParseContext::default(),
        );
        let split = details
            .split
            .expect("zone-pair split must populate a SearchDestinationSplit");
        assert_eq!(split.primary_destination, Zone::Exile);
        assert_eq!(split.primary_count, 1);
        assert!(!split.primary_enter_tapped.is_tapped());
        assert_eq!(split.rest_destination, Zone::Library);
        assert_eq!(details.count, QuantityExpr::Fixed { value: 2 });
    }

    #[test]
    fn single_zone_search_has_no_split() {
        // A plain tutor must NOT be misread as a split.
        let details = parse_search_library_details(
            "search your library for a basic land card, put it onto the battlefield tapped, then shuffle",
            &mut ParseContext::default(),
        );
        assert!(details.split.is_none());
    }

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

    /// CR 110.2a: "put that card onto the battlefield under your control" must
    /// thread `enters_under = Some(You)` all the way onto the chained
    /// `Effect::ChangeZone`. Bribery routes the pre-chained path (the
    /// `SearchLibrary` clause is `defs.last()` when the destination continuation
    /// applies). Revert-failing: before the fix `enters_under` is `None`.
    #[test]
    fn search_put_onto_battlefield_under_your_control_sets_enters_under() {
        use crate::types::ability::Effect;
        let def = super::super::parse_effect_chain(
            "Search target opponent's library for a creature card and put that card onto the battlefield under your control. Then that player shuffles.",
            crate::types::ability::AbilityKind::Spell,
        );
        let enters_under = find_battlefield_change_zone_enters_under(&def)
            .expect("chain should contain a ChangeZone to the battlefield");
        assert_eq!(
            enters_under,
            Some(ControllerRef::You),
            "under-your-control tutor must route the found card to the controller"
        );
        // Sanity: the search itself was recognized.
        assert!(
            matches!(&*def.effect, Effect::SearchLibrary { .. }),
            "head of chain should be the SearchLibrary"
        );
    }

    /// Negative sibling: a search-to-battlefield tutor WITHOUT "under your
    /// control" must leave `enters_under = None` (the scan must not over-fire).
    #[test]
    fn search_put_onto_battlefield_without_control_clause_leaves_enters_under_none() {
        let def = super::super::parse_effect_chain(
            "Search your library for a creature card, put it onto the battlefield, then shuffle.",
            crate::types::ability::AbilityKind::Spell,
        );
        let enters_under = find_battlefield_change_zone_enters_under(&def)
            .expect("chain should contain a ChangeZone to the battlefield");
        assert_eq!(
            enters_under, None,
            "no control clause -> default owner's control (None)"
        );
    }

    /// Walk the `sub_ability` chain and return the `enters_under` of the first
    /// `ChangeZone` whose destination is the battlefield.
    fn find_battlefield_change_zone_enters_under(
        def: &crate::types::ability::AbilityDefinition,
    ) -> Option<Option<ControllerRef>> {
        use crate::types::ability::Effect;
        let mut cursor = Some(def);
        while let Some(node) = cursor {
            if let Effect::ChangeZone {
                destination: Zone::Battlefield,
                enters_under,
                ..
            } = &*node.effect
            {
                return Some(enters_under.clone());
            }
            cursor = node.sub_ability.as_deref();
        }
        None
    }

    #[test]
    fn search_target_player_library() {
        let details = parse_search_library_details(
            "search target player's library for a card and exile it",
            &mut ParseContext::default(),
        );
        assert!(details.target_player.is_some());
        let TargetFilter::Typed(target_player) = details.target_player.unwrap() else {
            panic!("expected typed target-player library owner");
        };
        assert_eq!(target_player.controller, None);
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
    fn search_that_players_library_without_context_does_not_surface_player_target() {
        let details = parse_search_library_details(
            "search that player's library for a card with the same name as that permanent",
            &mut ParseContext::default(),
        );
        assert_eq!(details.target_player, None);
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
    fn build_search_suffix_constraints_includes_basic_and_chosen_name() {
        let suffix =
            build_search_suffix_constraints(" with that name", true, &mut ParseContext::default());
        assert!(suffix.properties.iter().any(|property| matches!(
            property,
            FilterProp::HasSupertype {
                value: crate::types::card_type::Supertype::Basic
            }
        )));
        assert!(suffix.filters.contains(&TargetFilter::HasChosenName));
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
    fn build_search_suffix_constraints_handles_basic_land_type_variants() {
        for suffix_text in [
            " with a basic land type",
            " that have a basic land type",
            " that each have a basic land type",
        ] {
            let suffix =
                build_search_suffix_constraints(suffix_text, false, &mut ParseContext::default());
            assert!(suffix
                .type_filters
                .iter()
                .any(|filter| matches!(filter, TypeFilter::AnyOf(_))));
        }
    }

    #[test]
    fn parse_search_filter_fallback_handles_basic_card_same_name() {
        let filter = parse_search_filter_fallback(
            "card",
            " with that name",
            true,
            &mut ParseContext::default(),
        );
        let TargetFilter::And { filters } = filter else {
            panic!("expected And filter, got {filter:?}");
        };
        assert!(filters.iter().any(|filter| matches!(
            filter,
            TargetFilter::Typed(typed)
                if typed.properties.iter().any(|property| matches!(
                    property,
                    FilterProp::HasSupertype {
                        value: crate::types::card_type::Supertype::Basic
                    }
                ))
        )));
        assert!(filters.contains(&TargetFilter::HasChosenName));
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
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 0,
            }
        )));
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
    fn parse_search_filter_handles_could_enchant_parent_reference_suffix() {
        let filter = parse_search_filter(
            "aura card that could enchant that creature, put it onto the battlefield attached to that creature",
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
            FilterProp::CanEnchant {
                target
            } if matches!(target.as_ref(), TargetFilter::ParentTarget)
        )));
    }

    #[test]
    fn parse_search_filter_keeps_plain_could_enchant_type_as_keyword_filter() {
        let filter = parse_search_filter(
            "aura card that could enchant creature, reveal it",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::WithKeyword {
                value: Keyword::Enchant(TargetFilter::Typed(target))
            } if target.type_filters.contains(&TypeFilter::Creature)
        )));
    }

    #[test]
    fn parse_search_filter_handles_keyword_kind_suffix() {
        let mut ctx = ParseContext::default();
        let filter = parse_search_filter("card with augment, reveal it", &mut ctx);
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::HasKeywordKind {
                value: KeywordKind::Augment
            }
        )));
        assert!(ctx.diagnostics.is_empty());
    }

    #[test]
    fn parse_search_filter_keeps_unimplemented_combine_after_augment_visible() {
        use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;

        let mut ctx = ParseContext::default();
        let filter = parse_search_filter(
            "card with augment and combine it with target host you control",
            &mut ctx,
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::HasKeywordKind {
                value: KeywordKind::Augment
            }
        )));
        assert!(
            ctx.diagnostics
                .iter()
                .any(|d| matches!(d, OracleDiagnostic::TargetFallback { text, .. } if text == "combine it with target host you control")),
            "combine continuation should remain visible until runtime combine is implemented: {:?}",
            ctx.diagnostics
        );
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
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::ColorCount {
                comparator: Comparator::GE,
                count: 2,
            }
        )));
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
    fn search_filter_color_disjunction_shares_type_and_suffixes() {
        let filter = parse_search_filter(
            "red or white instant card with mana value 4 or less",
            &mut ParseContext::default(),
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);

        for (filter, expected_color) in filters.iter().zip([
            crate::types::mana::ManaColor::Red,
            crate::types::mana::ManaColor::White,
        ]) {
            let TargetFilter::Typed(typed) = filter else {
                panic!("expected typed branch, got {filter:?}");
            };
            assert!(typed.type_filters.contains(&TypeFilter::Instant));
            assert!(typed
                .properties
                .iter()
                .any(|property| matches!(property, FilterProp::HasColor { color } if *color == expected_color)));
            assert!(typed.properties.iter().any(|property| matches!(
                property,
                FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 4 }
                }
            )));
        }
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
    fn parse_search_filter_handles_bare_and_or_subtype_variant() {
        let mut ctx = ParseContext::default();
        let filter = parse_search_filter(
            "basic land cards and/or town cards with different names",
            &mut ctx,
        );
        assert!(ctx.diagnostics.is_empty());
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
                value: Supertype::Basic
            }
        )));

        let TargetFilter::Typed(town) = &filters[1] else {
            panic!("expected typed Town branch, got {:?}", filters[1]);
        };
        assert_eq!(town.get_subtype(), Some("Town"));
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
    fn parse_search_filter_applies_shared_basic_to_land_subtype_list() {
        let filter = parse_search_filter(
            "basic swamp, forest, or island card",
            &mut ParseContext::default(),
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 3);

        for (filter, subtype) in filters.iter().zip(["Swamp", "Forest", "Island"]) {
            let TargetFilter::Typed(typed) = filter else {
                panic!("expected typed {subtype} branch, got {filter:?}");
            };
            assert!(typed.type_filters.contains(&TypeFilter::Land));
            assert_eq!(typed.get_subtype(), Some(subtype));
            assert!(typed.properties.iter().any(|property| matches!(
                property,
                FilterProp::HasSupertype {
                    value: Supertype::Basic
                }
            )));
        }
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
    fn parse_search_filter_same_name_as_chosen_object() {
        let mut ctx = ParseContext::default();
        let filter = parse_search_filter(
            "basic land cards which have the same name as the chosen land",
            &mut ctx,
        );
        assert!(ctx.diagnostics.is_empty());
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(filter.type_filters.contains(&TypeFilter::Land));
        assert!(filter
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::SameNameAsParentTarget)));
    }

    #[test]
    fn parse_search_filter_not_named_preserves_trailing_distinct_names_suffix() {
        let mut ctx = ParseContext::default();
        let filter = parse_search_filter(
            "dragon cards not named tiamat that each have different names",
            &mut ctx,
        );
        assert!(ctx.diagnostics.is_empty());
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(filter.type_filters.iter().any(
            |type_filter| matches!(type_filter, TypeFilter::Subtype(subtype) if subtype == "Dragon")
        ));
        assert!(filter.properties.iter().any(|property| matches!(
            property,
            FilterProp::SharesQuality {
                quality: SharedQuality::Name,
                reference: Some(reference),
                relation: SharedQualityRelation::DoesNotShare,
            } if matches!(
                reference.as_ref(),
                TargetFilter::Typed(TypedFilter {
                    properties,
                    ..
                }) if properties.iter().any(|property| matches!(property, FilterProp::Named { name } if name == "tiamat"))
            )
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

    /// CR 701.23a: an intra-member union ("instant or sorcery") inside a
    /// comma-series disjunction must flatten into the single co-equal `Or` — one
    /// choice among {Instant ∨ Sorcery ∨ Legendary ∨ Saga}, count 1. A non-empty
    /// `extra_filters` here is the count:2 MatchEachFilter deadlock this fix
    /// removes (demanding one instant/sorcery AND one legendary/saga card).
    #[test]
    fn search_intra_member_union_flattens_to_single_choice_or() {
        let details = parse_search_library_details(
            "search your library for an instant or sorcery card, a legendary card, or a saga card, reveal it, put it into your hand, then shuffle",
            &mut ParseContext::default(),
        );
        assert!(
            details.extra_filters.is_empty(),
            "intra-member union must not produce a multi-card conjunction: {:?}",
            details.extra_filters
        );
        assert_eq!(details.count, QuantityExpr::Fixed { value: 1 });
        assert_eq!(
            details.selection_constraint,
            SearchSelectionConstraint::None
        );
        let TargetFilter::Or { filters } = &details.filter else {
            panic!("expected Or filter, got {:?}", details.filter);
        };
        assert_eq!(
            filters.len(),
            4,
            "intra-member union must flatten to 4 co-equal branches: {filters:?}"
        );
        assert!(
            filters
                .iter()
                .all(|f| !matches!(f, TargetFilter::Or { .. })),
            "every branch must be flat (no nested Or): {filters:?}"
        );

        let has_type = |ty: &TypeFilter| {
            filters.iter().any(|f| {
                matches!(
                    f,
                    TargetFilter::Typed(typed) if typed.type_filters.contains(ty)
                )
            })
        };
        assert!(
            has_type(&TypeFilter::Instant),
            "missing Instant: {filters:?}"
        );
        assert!(
            has_type(&TypeFilter::Sorcery),
            "missing Sorcery: {filters:?}"
        );
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(typed) if typed.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::HasSupertype { value: Supertype::Legendary }
                ))
            )),
            "missing Legendary supertype branch: {filters:?}"
        );
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(typed) if typed.get_subtype() == Some("Saga")
            )),
            "missing Saga subtype branch: {filters:?}"
        );
    }

    /// CR 201.2: a comma-bearing card name ("Halvar, God of Battle") in a
    /// disjunctive series must NOT be shredded on its internal comma — the name
    /// stays intact and the series resolves to exactly two co-equal branches.
    #[test]
    fn search_named_member_with_comma_in_name_not_shredded() {
        let details = parse_search_library_details(
            "search your library for a card named Halvar, God of Battle or an Equipment card, reveal it, put it into your hand, then shuffle",
            &mut ParseContext::default(),
        );
        assert!(
            details.extra_filters.is_empty(),
            "named disjunction must not produce a multi-card conjunction: {:?}",
            details.extra_filters
        );
        assert_eq!(details.count, QuantityExpr::Fixed { value: 1 });
        let TargetFilter::Or { filters } = &details.filter else {
            panic!("expected Or filter, got {:?}", details.filter);
        };
        assert_eq!(
            filters.len(),
            2,
            "named disjunction must be exactly two branches (name not shredded): {filters:?}"
        );
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(typed) if typed.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Named { name } if name == "Halvar, God of Battle"
                ))
            )),
            "expected intact \"Halvar, God of Battle\" name branch: {filters:?}"
        );
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(typed) if typed.get_subtype() == Some("Equipment")
            )),
            "expected Equipment branch: {filters:?}"
        );
    }

    /// CR 201.2 + CR 701.23a: a comma-bearing named card can appear in the middle
    /// of a comma-series disjunction. Only delimiter commas split the series; the
    /// comma inside the card name remains part of the `Named` filter.
    #[test]
    fn search_middle_named_member_with_comma_in_name_not_shredded() {
        let details = parse_search_library_details(
            "search your library for a legendary card, a card named Halvar, God of Battle, or an Equipment card, reveal it, put it into your hand, then shuffle",
            &mut ParseContext::default(),
        );
        assert!(
            details.extra_filters.is_empty(),
            "comma-series disjunction must stay one choice, not required extras: {:?}",
            details.extra_filters
        );
        assert_eq!(details.count, QuantityExpr::Fixed { value: 1 });
        let TargetFilter::Or { filters } = &details.filter else {
            panic!("expected Or filter, got {:?}", details.filter);
        };
        assert_eq!(
            filters.len(),
            3,
            "expected three co-equal branches without name shredding: {filters:?}"
        );
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(typed) if typed.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::HasSupertype { value: Supertype::Legendary }
                ))
            )),
            "expected Legendary branch: {filters:?}"
        );
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(typed) if typed.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Named { name } if name == "Halvar, God of Battle"
                ))
            )),
            "expected intact named branch: {filters:?}"
        );
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(typed) if typed.get_subtype() == Some("Equipment")
            )),
            "expected Equipment branch: {filters:?}"
        );
    }

    /// CR 201.2 + CR 701.23a: the comma-series front gate must recognize a final
    /// named member even when it has a leading article.
    #[test]
    fn search_final_named_member_with_leading_article_splits_as_disjunction() {
        let details = parse_search_library_details(
            "search your library for an Equipment card, or a card named Halvar, God of Battle, reveal it, put it into your hand, then shuffle",
            &mut ParseContext::default(),
        );
        assert!(details.extra_filters.is_empty());
        let TargetFilter::Or { filters } = &details.filter else {
            panic!("expected Or filter, got {:?}", details.filter);
        };
        assert_eq!(
            filters.len(),
            2,
            "expected Equipment or named card: {filters:?}"
        );
        assert!(filters.iter().any(|f| matches!(
            f,
            TargetFilter::Typed(typed) if typed.get_subtype() == Some("Equipment")
        )));
        assert!(filters.iter().any(|f| matches!(
            f,
            TargetFilter::Typed(typed) if typed.properties.iter().any(|p| matches!(
                p,
                FilterProp::Named { name } if name == "Halvar, God of Battle"
            ))
        )));
    }

    /// CR 202.3: the "X, Y, and/or Z ... with mana value N or less" form must
    /// DEFER from the comma-series front-gate (an `and/or` enumeration with a
    /// comma in the left side) and the bare " or less" must never terminate the
    /// series. This guards the defer path, not the front-gate: the load-bearing
    /// invariant is that nothing is shredded into a garbage `less` filter or a
    /// spurious required `extra_filters` entry.
    #[test]
    fn search_andor_comparator_series_defers_without_shredding() {
        let details = parse_search_library_details(
            "search your library for artifact, creature, and/or enchantment cards with mana value 1 or less, reveal it, put it into your hand, then shuffle",
            &mut ParseContext::default(),
        );
        assert!(
            details.extra_filters.is_empty(),
            "and/or comparator series must not produce required extra filters: {:?}",
            details.extra_filters
        );
        assert_eq!(details.count, QuantityExpr::Fixed { value: 1 });
        // No branch may be a bare comparator-word garbage filter (the bug this
        // guards): assert no subtype or Named branch is the comparator word.
        if let TargetFilter::Or { filters } = &details.filter {
            assert!(
                filters.iter().all(|f| !matches!(
                    f,
                    TargetFilter::Typed(typed) if typed.get_subtype() == Some("less")
                        || typed.properties.iter().any(|p| matches!(
                            p,
                            FilterProp::Named { name } if name == "less"
                        ))
                )),
                "comparator word must not become a garbage filter branch: {filters:?}"
            );
        }
    }

    // Issue #458: "search ... for up to that many land cards" — Scapeshift.
    // CR 608.2c: "that many" back-references the count produced by the earlier
    // sacrifice instruction in the same resolution. `parse_quantity_ref` maps
    // it to `QuantityRef::EventContextAmount`.
    #[test]
    fn search_up_to_that_many_emits_event_context_back_reference() {
        let details = parse_search_library_details(
            "search your library for up to that many land cards, put them onto the battlefield tapped, then shuffle",
            &mut ParseContext::default(),
        );
        assert!(details.up_to, "\"up to that many\" should set up_to=true");
        assert_eq!(
            details.count,
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            "\"that many\" must back-reference the prior effect's count"
        );
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

    /// CR 701.23a + CR 107.1: Lotuslight Dancers-style serial filters —
    /// "a black card, a green card, and a blue card" — are three independent
    /// search filters, not one black filter with swallowed comma text.
    #[test]
    fn search_serial_color_filters_extracts_all_colors() {
        let details = parse_search_library_details(
            "search your library for a black card, a green card, and a blue card. put those cards into your graveyard, then shuffle",
            &mut ParseContext::default(),
        );
        assert_eq!(details.extra_filters.len(), 2);
        assert_filter_has_color(&details.filter, ManaColor::Black);
        assert_filter_has_color(&details.extra_filters[0], ManaColor::Green);
        assert_filter_has_color(&details.extra_filters[1], ManaColor::Blue);
        assert_eq!(details.multi_destination, Zone::Graveyard);
    }

    /// CR 701.23a: Search for Glory — "a snow permanent card, a legendary card,
    /// or a Saga card" is a single choice among three co-equal filters, NOT
    /// three required picks. Must lower to count 1 + `Or` of three branches with
    /// no `MatchEachFilter` selection constraint and no extra filters.
    #[test]
    fn search_for_glory_disjunctive_series_is_single_choice_or() {
        let details = parse_search_library_details(
            "search your library for a snow permanent card, a legendary card, or a saga card, reveal it, put it into your hand, then shuffle",
            &mut ParseContext::default(),
        );
        assert!(
            details.extra_filters.is_empty(),
            "disjunctive series must not produce extra (required) filters: {:?}",
            details.extra_filters
        );
        assert_eq!(details.count, QuantityExpr::Fixed { value: 1 });
        assert_eq!(
            details.selection_constraint,
            SearchSelectionConstraint::None
        );
        let TargetFilter::Or { filters } = &details.filter else {
            panic!("expected Or filter, got {:?}", details.filter);
        };
        assert_eq!(
            filters.len(),
            3,
            "expected 3 disjunctive branches: {filters:?}"
        );

        // Branch 0: snow permanent.
        let TargetFilter::Typed(snow) = &filters[0] else {
            panic!("expected typed snow branch, got {:?}", filters[0]);
        };
        assert!(
            snow.properties.iter().any(|property| matches!(
                property,
                FilterProp::HasSupertype {
                    value: Supertype::Snow
                }
            )),
            "first branch should carry Snow supertype: {snow:?}"
        );

        // Branch 1: legendary.
        let TargetFilter::Typed(legendary) = &filters[1] else {
            panic!("expected typed legendary branch, got {:?}", filters[1]);
        };
        assert!(
            legendary.properties.iter().any(|property| matches!(
                property,
                FilterProp::HasSupertype {
                    value: Supertype::Legendary
                }
            )),
            "second branch should carry Legendary supertype: {legendary:?}"
        );

        // Branch 2: Saga subtype.
        let TargetFilter::Typed(saga) = &filters[2] else {
            panic!("expected typed Saga branch, got {:?}", filters[2]);
        };
        assert_eq!(saga.get_subtype(), Some("Saga"));
    }

    /// CR 701.23a (GAP2 coverage): the comma-member peel must recover the
    /// `basic` supertype on a non-land mixed series — "a basic land card, a
    /// plains card, or a saga card" lowers to an `Or` of three branches with the
    /// first carrying `Basic` + `Land`.
    #[test]
    fn search_basic_leading_disjunctive_series_recovers_supertype() {
        let details = parse_search_library_details(
            "search your library for a basic land card, a plains card, or a saga card, reveal it, put it into your hand, then shuffle",
            &mut ParseContext::default(),
        );
        assert!(details.extra_filters.is_empty());
        assert_eq!(details.count, QuantityExpr::Fixed { value: 1 });
        let TargetFilter::Or { filters } = &details.filter else {
            panic!("expected Or filter, got {:?}", details.filter);
        };
        assert_eq!(
            filters.len(),
            3,
            "expected 3 disjunctive branches: {filters:?}"
        );

        let TargetFilter::Typed(basic_land) = &filters[0] else {
            panic!("expected typed basic land branch, got {:?}", filters[0]);
        };
        assert!(basic_land.type_filters.contains(&TypeFilter::Land));
        assert!(
            basic_land.properties.iter().any(|property| matches!(
                property,
                FilterProp::HasSupertype {
                    value: Supertype::Basic
                }
            )),
            "first branch should carry Basic supertype: {basic_land:?}"
        );
    }

    /// CR 701.23a + CR 205.3i: "a land card of each basic land type" is a
    /// multi-filter search: one land card with each of the five basic land
    /// subtypes. It reuses the existing chained `SearchLibrary` lowering path
    /// used by "a Forest card and a Plains card" instead of adding a special
    /// resolver.
    #[test]
    fn search_land_card_of_each_basic_land_type_extracts_five_filters() {
        let details = parse_search_library_details(
            "search your library for a land card of each basic land type, put those cards onto the battlefield, then shuffle",
            &mut ParseContext::default(),
        );
        assert_eq!(details.extra_filters.len(), 4);
        for (filter, subtype) in [&details.filter]
            .into_iter()
            .chain(details.extra_filters.iter())
            .zip(["Plains", "Island", "Swamp", "Mountain", "Forest"])
        {
            match filter {
                TargetFilter::Typed(typed) => {
                    assert!(typed.type_filters.contains(&TypeFilter::Land));
                    assert_eq!(typed.get_subtype(), Some(subtype));
                }
                other => panic!("expected typed land filter for {subtype}, got {other:?}"),
            }
        }
        assert_eq!(details.multi_destination, Zone::Battlefield);
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

    fn assert_filter_has_color(filter: &TargetFilter, expected: ManaColor) {
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(
            tf.properties.iter().any(
                |property| matches!(property, FilterProp::HasColor { color } if *color == expected)
            ),
            "expected {expected:?} color filter, got {tf:?}"
        );
    }

    fn assert_filter_contains(filter: &TargetFilter, expected: &TargetFilter) {
        match filter {
            TargetFilter::Or { filters } | TargetFilter::And { filters } => {
                assert!(
                    filters
                        .iter()
                        .any(|filter| filter == expected || filter_contains(filter, expected)),
                    "expected {expected:?} in {filter:?}"
                );
            }
            other => assert_eq!(other, expected),
        }
    }

    fn filter_contains(filter: &TargetFilter, expected: &TargetFilter) -> bool {
        match filter {
            TargetFilter::Or { filters } | TargetFilter::And { filters } => filters
                .iter()
                .any(|filter| filter == expected || filter_contains(filter, expected)),
            other => other == expected,
        }
    }

    /// CR 608.2c + CR 701.23: Gifts Ungiven — "search your library for up to
    /// four cards with different names". The "with different names" clause
    /// must surface as `SearchSelectionConstraint::DistinctQualities` rather than
    /// silently degrading the per-card filter.
    #[test]
    fn search_with_different_names_emits_distinct_names_constraint() {
        let details = parse_search_library_details(
            "search your library for up to four cards with different names, reveal those cards, and put them into your graveyard",
            &mut ParseContext::default(),
        );
        assert_eq!(
            details.selection_constraint,
            SearchSelectionConstraint::DistinctQualities {
                qualities: vec![SharedQuality::Name],
            }
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
            SearchSelectionConstraint::DistinctQualities {
                qualities: vec![SharedQuality::Name],
            }
        );
        assert!(details.up_to);
        assert_eq!(details.count, QuantityExpr::Fixed { value: 5 });
    }

    #[test]
    fn search_that_each_have_different_names_emits_distinct_names_constraint() {
        let details = parse_search_library_details(
            "search your library for up to five dragon cards that each have different names, reveal them, put them into your hand, then shuffle",
            &mut ParseContext::default(),
        );
        assert_eq!(
            details.selection_constraint,
            SearchSelectionConstraint::DistinctQualities {
                qualities: vec![SharedQuality::Name],
            }
        );
        assert!(details.up_to);
        assert_eq!(details.count, QuantityExpr::Fixed { value: 5 });
    }

    #[test]
    fn search_with_different_powers_emits_distinct_quality_constraint() {
        let mut ctx = ParseContext::default();
        let details = parse_search_library_details(
            "search your library for up to four creature cards with different powers and reveal them",
            &mut ctx,
        );
        assert_eq!(
            details.selection_constraint,
            SearchSelectionConstraint::DistinctQualities {
                qualities: vec![SharedQuality::Power],
            }
        );
        assert!(ctx.diagnostics.iter().all(|diagnostic| !matches!(
            diagnostic,
            OracleDiagnostic::TargetFallback { context, .. }
                if context == "search-filter-suffix unmatched"
        )));
    }

    #[test]
    fn search_that_dont_share_quality_list_emits_distinct_quality_constraint() {
        let mut ctx = ParseContext::default();
        let details = parse_search_library_details(
            "search your library for up to four cards that don't share a mana value, power, toughness, or card type with each other",
            &mut ctx,
        );
        assert_eq!(
            details.selection_constraint,
            SearchSelectionConstraint::DistinctQualities {
                qualities: vec![
                    SharedQuality::ManaValue,
                    SharedQuality::Power,
                    SharedQuality::Toughness,
                    SharedQuality::CardType,
                ],
            }
        );
        assert_eq!(details.filter, TargetFilter::Typed(TypedFilter::card()));
        assert!(ctx.diagnostics.iter().all(|diagnostic| !matches!(
            diagnostic,
            OracleDiagnostic::TargetFallback { context, .. }
                if context == "search-filter-suffix unmatched"
        )));
    }

    #[test]
    fn search_same_total_power_toughness_emits_trigger_source_quality_filter() {
        let mut ctx = ParseContext::default();
        let details = parse_search_library_details(
            "search your library for a creature card with the same total power and toughness",
            &mut ctx,
        );
        let TargetFilter::Typed(filter) = details.filter else {
            panic!("expected typed filter, got {:?}", details.filter);
        };
        assert!(matches!(
            filter.type_filters.as_slice(),
            [TypeFilter::Creature]
        ));
        assert!(filter.properties.iter().any(|prop| matches!(
            prop,
            FilterProp::SharesQuality {
                quality: SharedQuality::TotalPowerToughness,
                reference: Some(reference),
                relation: SharedQualityRelation::Shares,
            } if matches!(reference.as_ref(), TargetFilter::TriggeringSource)
        )));
        assert!(ctx.diagnostics.iter().all(|diagnostic| !matches!(
            diagnostic,
            OracleDiagnostic::TargetFallback { context, .. }
                if context == "search-filter-suffix unmatched"
        )));
    }

    #[test]
    fn seek_most_prevalent_creature_type_emits_library_prevalence_filter() {
        let mut ctx = ParseContext::default();
        let details = parse_seek_details(
            "seek a creature card of the most prevalent creature type in your library",
            &mut ctx,
        );
        let TargetFilter::Typed(filter) = details.filter else {
            panic!("expected typed filter, got {:?}", details.filter);
        };
        assert!(matches!(
            filter.type_filters.as_slice(),
            [TypeFilter::Creature]
        ));
        assert!(filter
            .properties
            .contains(&FilterProp::MostPrevalentCreatureTypeIn {
                zone: Zone::Library,
                scope: ControllerRef::You,
            }));
        assert!(ctx.diagnostics.iter().all(|diagnostic| !matches!(
            diagnostic,
            OracleDiagnostic::TargetFallback { context, .. }
                if context == "search-filter-suffix unmatched"
        )));
    }

    /// CR 205.3m + CR 701.23a: L1 lift regression — opponent-scoped
    /// possessive must dispatch into the same parameterized filter without
    /// spawning a new variant. The test pins the (Zone, ControllerRef) axis
    /// combination proving the parser composes both scopes from one
    /// combinator.
    #[test]
    fn seek_most_prevalent_creature_type_in_opponents_library_uses_opponent_scope() {
        let mut ctx = ParseContext::default();
        let details = parse_seek_details(
            "seek a creature card of the most prevalent creature type in an opponent's library",
            &mut ctx,
        );
        let TargetFilter::Typed(filter) = details.filter else {
            panic!("expected typed filter, got {:?}", details.filter);
        };
        assert!(filter
            .properties
            .contains(&FilterProp::MostPrevalentCreatureTypeIn {
                zone: Zone::Library,
                scope: ControllerRef::Opponent,
            }));
    }

    #[test]
    fn seek_highest_mana_value_under_life_gained_threshold_emits_aggregate_filter() {
        let mut ctx = ParseContext::default();
        let details = parse_seek_details(
            "seek a card with the highest mana value among cards in your library with mana value less than or equal to the amount of life you gained this turn",
            &mut ctx,
        );
        let TargetFilter::Typed(filter) = details.filter else {
            panic!("expected typed filter, got {:?}", details.filter);
        };
        assert!(filter.properties.iter().any(|prop| matches!(
            prop,
            FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn { .. },
                },
            }
        )));
        assert!(filter.properties.iter().any(|prop| matches!(
            prop,
            FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate {
                        function: AggregateFunction::Max,
                        property: ObjectProperty::ManaValue,
                        ..
                    },
                },
            }
        )));
        assert!(ctx.diagnostics.iter().all(|diagnostic| !matches!(
            diagnostic,
            OracleDiagnostic::TargetFallback { context, .. }
                if context == "search-filter-suffix unmatched"
        )));
    }

    #[test]
    fn seek_highest_mana_value_under_counter_threshold_emits_source_counter_filter() {
        let mut ctx = ParseContext::default();
        let details = parse_seek_details(
            "seek a card with the highest mana value among cards in your library with mana value x or less, where x is the number of charge counters on ~",
            &mut ctx,
        );
        let TargetFilter::Typed(filter) = details.filter else {
            panic!("expected typed filter, got {:?}", details.filter);
        };
        assert!(filter.properties.iter().any(|prop| matches!(
            prop,
            FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: Some(counter_type),
                    },
                },
            } if *counter_type == CounterType::Generic("charge".to_string())
        )));
        assert!(filter.properties.iter().any(|prop| matches!(
            prop,
            FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate {
                        function: AggregateFunction::Max,
                        property: ObjectProperty::ManaValue,
                        ..
                    },
                },
            }
        )));
        assert!(ctx.diagnostics.iter().all(|diagnostic| !matches!(
            diagnostic,
            OracleDiagnostic::TargetFallback { context, .. }
                if context == "search-filter-suffix unmatched"
        )));
    }

    #[test]
    fn highest_mana_value_library_suffix_rejects_partial_counter_threshold_tail() {
        assert!(
            parse_highest_mana_value_library_suffix(
                "with the highest mana value among cards in your library with mana value x or less, where x is the number of charge counters on ~ plus one",
            )
            .is_err()
        );
    }

    #[test]
    fn highest_mana_value_library_suffix_rejects_partial_life_gained_threshold_tail() {
        assert!(
            parse_highest_mana_value_library_suffix(
                "with the highest mana value among cards in your library with mana value less than or equal to the amount of life you gained this turn plus one",
            )
            .is_err()
        );
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

    #[test]
    fn shared_total_mana_value_comparator_parses_le_and_ge() {
        // CR 202.3: shared combinator used by both search-set and target-set
        // mana-value bounds.
        assert_eq!(
            parse_total_mana_value_comparator("3 or less").map(|(_, v)| v),
            Ok((Comparator::LE, 3))
        );
        assert_eq!(
            parse_total_mana_value_comparator("3 or greater").map(|(_, v)| v),
            Ok((Comparator::GE, 3))
        );
        // X (lowercase, as the parser lowercases dispatch text) resolves to 0 at
        // parse time; the where-X binding rebinds it to the die result later.
        assert_eq!(
            parse_total_mana_value_comparator("x or less").map(|(_, v)| v),
            Ok((Comparator::LE, 0))
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

    // M6 regression: each old `Disjunction` variant maps to an observable
    // split outcome from `split_filter_disjunctions`. Covers all 7 cluster
    // members through their observable split behavior, proving the
    // parameterized `connector × leading` axis combination still drives the
    // same segmentation. Bare " or " requires a card-bearing right side
    // (covered by `bare_search_disjunction_allowed`); bare " and/or " runs
    // unconditionally.

    // M6 regression: each old `Disjunction` variant maps to an observable
    // split outcome from `split_filter_disjunctions`. The leading article is
    // consumed by the connector parser (matching the legacy tag form) so the
    // right segment is the article-less remainder; only the `Basic` axis
    // recovers the supertype word into the right segment for downstream
    // type-phrase parsing.

    #[test]
    fn split_disjunction_or_a_recovers_article_less_right_segment() {
        // Old Disjunction::OrA → Or × A
        let segments = split_filter_disjunctions("creature card or a noncreature card");
        assert_eq!(segments, vec!["creature card", "noncreature card"]);
    }

    #[test]
    fn split_disjunction_or_an_recovers_article_less_right_segment() {
        // Old Disjunction::OrAn → Or × An
        let segments = split_filter_disjunctions("creature card or an artifact card");
        assert_eq!(segments, vec!["creature card", "artifact card"]);
    }

    #[test]
    fn split_disjunction_or_basic_recovers_basic_into_right_segment() {
        // Old Disjunction::OrBasic → Or × Basic — "basic" must stay on the
        // right segment so the type-phrase parser sees "basic <type>".
        let segments = split_filter_disjunctions("Mountain or basic Forest card");
        assert_eq!(segments, vec!["Mountain", "basic Forest card"]);
    }

    #[test]
    fn split_disjunction_and_or_a_recovers_article_less_right_segment() {
        // Old Disjunction::AndOrA → AndOr × A
        let segments = split_filter_disjunctions("creature card and/or a sorcery card");
        assert_eq!(segments, vec!["creature card", "sorcery card"]);
    }

    #[test]
    fn split_disjunction_and_or_an_recovers_article_less_right_segment() {
        // Old Disjunction::AndOrAn → AndOr × An
        let segments = split_filter_disjunctions("creature card and/or an artifact card");
        assert_eq!(segments, vec!["creature card", "artifact card"]);
    }

    #[test]
    fn split_disjunction_and_or_bare_recovers_left_and_right_segments() {
        // Old Disjunction::AndOrBare → AndOr × None
        let segments = split_filter_disjunctions("creatures and/or planeswalkers");
        assert_eq!(segments, vec!["creatures", "planeswalkers"]);
    }

    #[test]
    fn split_disjunction_bare_or_with_card_bearing_right_splits_segments() {
        // Old Disjunction::BareOr → Or × None (gated by
        // `bare_search_disjunction_allowed` — right must look like a card).
        let segments = split_filter_disjunctions("Mountain or Cave card");
        assert_eq!(segments, vec!["Mountain", "Cave card"]);
    }

    #[test]
    fn split_terminal_or_preserves_and_or_precedence() {
        let (before, final_member, connector) =
            split_terminal_or("artifact card and/or creature card or enchantment card")
                .expect("terminal connector");
        assert_eq!(connector, Connector::AndOr);
        assert_eq!(before, "artifact card");
        assert_eq!(final_member, "creature card or enchantment card");
    }

    #[test]
    fn split_comma_series_middle_named_member_keeps_name_comma() {
        let segments = split_filter_disjunctions(
            "legendary card, a card named Halvar, God of Battle, or Equipment card",
        );
        assert_eq!(
            segments,
            vec![
                "legendary card",
                "card named Halvar, God of Battle",
                "Equipment card",
            ]
        );
    }

    #[test]
    fn split_comma_series_final_named_member_strips_article() {
        let segments =
            split_filter_disjunctions("Equipment card, or a card named Halvar, God of Battle");
        assert_eq!(
            segments,
            vec!["Equipment card", "card named Halvar, God of Battle"]
        );
    }

    /// M7 backward-compat: a serialized JSON snapshot using the legacy
    /// `ObjectCountDistinctNames` tag (single `filter` field, no `qualities`)
    /// must deserialize to the new parameterized `ObjectCountDistinct` shape
    /// with `qualities = vec![SharedQuality::Name]`. Mirrors Batch 5's
    /// approach for forward-compatible enum lifts.
    #[test]
    fn legacy_object_count_distinct_names_json_deserializes_with_default_qualities() {
        let legacy_json = serde_json::json!({
            "type": "ObjectCountDistinctNames",
            "filter": { "type": "Any" }
        });
        let qty: QuantityRef =
            serde_json::from_value(legacy_json).expect("legacy tag deserializes");
        match qty {
            QuantityRef::ObjectCountDistinct {
                filter: _,
                qualities,
            } => {
                assert_eq!(qualities, vec![SharedQuality::Name]);
            }
            other => panic!("expected ObjectCountDistinct, got {other:?}"),
        }
    }

    /// M7 backward-compat: legacy `MostPrevalentCreatureTypeInLibrary` tag
    /// deserializes to the new parameterized variant with default
    /// `zone = Library` and `scope = You`.
    #[test]
    fn legacy_most_prevalent_creature_type_json_deserializes_with_default_axes() {
        let legacy_json = serde_json::json!({
            "type": "MostPrevalentCreatureTypeInLibrary"
        });
        let prop: FilterProp =
            serde_json::from_value(legacy_json).expect("legacy tag deserializes");
        match prop {
            FilterProp::MostPrevalentCreatureTypeIn { zone, scope } => {
                assert_eq!(zone, Zone::Library);
                assert_eq!(scope, ControllerRef::You);
            }
            other => panic!("expected MostPrevalentCreatureTypeIn, got {other:?}"),
        }
    }

    /// Counts how many `Typed` legs of an `Or` carry a `FilterProp` of the given
    /// discriminant. Building-block assertion over the leg-locality distribution.
    fn legs_with_prop(
        filter: &TargetFilter,
        predicate: impl Fn(&FilterProp) -> bool,
    ) -> (usize, usize) {
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        let mut typed = 0;
        let mut matching = 0;
        for f in filters {
            if let TargetFilter::Typed(t) = f {
                typed += 1;
                if t.properties.iter().any(&predicate) {
                    matching += 1;
                }
            }
        }
        (matching, typed)
    }

    /// #2892 — CR 701.23a + CR 202.3: Bring to Light's "creature, instant, or
    /// sorcery card with mana value less than or equal to N" parses each comma/or
    /// disjunct independently, so the trailing mana-value predicate must be
    /// distributed back across ALL three type legs. Pre-fix only the final
    /// (Sorcery) leg carried `Cmc`, leaving the Creature/Instant legs
    /// unconstrained (a MV-6 creature was wrongly findable).
    #[test]
    fn search_disjunction_distributes_trailing_mana_value_to_all_legs() {
        let details = parse_search_library_details(
            "search your library for a creature, instant, or sorcery card with mana value less than or equal to the number of colors of mana spent to cast this spell",
            &mut ParseContext::default(),
        );
        let (with_cmc, typed) = legs_with_prop(&details.filter, |p| {
            matches!(
                p,
                FilterProp::Cmc {
                    comparator: Comparator::LE,
                    ..
                }
            )
        });
        assert_eq!(typed, 3, "expected 3 type legs, got {:?}", details.filter);
        assert_eq!(
            with_cmc, 3,
            "every leg must carry the trailing Cmc<=N predicate, got {:?}",
            details.filter
        );
    }

    /// CR 115.1: anti-regression — adjective/keyword-suffix props that bind to a
    /// single disjunct ("creature, artifact, or enchantment with flying") must
    /// stay leg-local. Only the creature leg may carry `WithKeyword(Flying)`.
    #[test]
    fn search_disjunction_keeps_with_keyword_leg_local() {
        let details = parse_search_library_details(
            "search your library for a creature, artifact, or enchantment with flying",
            &mut ParseContext::default(),
        );
        let (with_flying, _typed) = legs_with_prop(&details.filter, |p| {
            matches!(
                p,
                FilterProp::WithKeyword {
                    value: Keyword::Flying
                }
            )
        });
        assert_eq!(
            with_flying, 1,
            "WithKeyword(Flying) must remain on its originating leg only, got {:?}",
            details.filter
        );
    }

    /// #2892 anti-regression — Clever Combo: "a host card or a card with augment".
    /// CR 702.1: the augment keyword-kind predicate must stay on its own
    /// disjunct; distributing it onto the host leg ("host card with augment")
    /// would empty that leg's match set.
    #[test]
    fn search_disjunction_keeps_keyword_kind_leg_local() {
        let details = parse_search_library_details(
            "search your library for a host card or a card with augment",
            &mut ParseContext::default(),
        );
        let (with_augment, _typed) = legs_with_prop(&details.filter, |p| {
            matches!(
                p,
                FilterProp::HasKeywordKind {
                    value: KeywordKind::Augment
                }
            )
        });
        assert_eq!(
            with_augment, 1,
            "HasKeywordKind(Augment) must remain on the non-host leg only, got {:?}",
            details.filter
        );
    }

    /// #2892 building-block guard — CR 201.2 / CR 201.2a (card name) +
    /// CR 202.3 (mana value): asserts the leg-locality registry directly on the
    /// distributor, independent of any card's parse path. `FilterProp::Named` is
    /// inherently leg-local (a name predicate binds only to its own disjunct,
    /// same class as `HasKeywordKind`/`WithKeyword`), so it must NOT distribute
    /// across an `Or`; `FilterProp::Cmc` is a trailing-suffix predicate and MUST.
    ///
    /// Constructing the `Or` AST directly is deliberate: no current card routes a
    /// `Named` leg through a real `" or "`/`" and/or "` disjunction with a
    /// non-`Named` earlier leg (name-disjunction cards either use bare "and",
    /// which takes the dual-filter `MatchEachFilter` path and never reaches this
    /// distributor, or carry `Named` on every leg and are deduped by
    /// `same_kind`). The exclusion is defense-in-depth; this test guards it.
    ///
    /// The `Cmc` positive control is the discriminator: if the `Named` exclusion
    /// were removed, `Named` would wrongly land on the first leg and the first
    /// assertion would fail — while the `Cmc` assertions prove the test does not
    /// merely block all distribution.
    #[test]
    fn distribute_or_keeps_named_leg_local_but_distributes_cmc() {
        // Or { Creature [], Card [Named "jiang yanggu", Cmc<=3] }
        let filter = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Card],
                    controller: None,
                    properties: vec![
                        FilterProp::Named {
                            name: "jiang yanggu".to_string(),
                        },
                        FilterProp::Cmc {
                            comparator: Comparator::LE,
                            value: QuantityExpr::Fixed { value: 3 },
                        },
                    ],
                }),
            ],
        };

        let out = distribute_properties_to_or(filter);
        let TargetFilter::Or { filters } = &out else {
            panic!("expected Or filter, got {out:?}");
        };
        let TargetFilter::Typed(first) = &filters[0] else {
            panic!("expected first leg Typed, got {:?}", filters[0]);
        };
        let TargetFilter::Typed(second) = &filters[1] else {
            panic!("expected second leg Typed, got {:?}", filters[1]);
        };

        // Named stayed leg-local: the Creature leg did NOT receive it. This is
        // the assertion that flips if the `Named` exclusion is removed from
        // `is_adjective_prefix_prop`.
        assert!(
            !first
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::Named { .. })),
            "Named must NOT distribute to the earlier (Creature) leg, got {first:?}"
        );
        // ...but the originating (Card) leg still carries its own Named.
        assert!(
            second
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::Named { name } if name == "jiang yanggu")),
            "Named must remain on its originating (Card) leg, got {second:?}"
        );

        // Positive control: the trailing Cmc<=N predicate DID distribute back to
        // the earlier leg, proving the guard doesn't wrongly suppress everything.
        assert!(
            first.properties.iter().any(|p| matches!(
                p,
                FilterProp::Cmc {
                    comparator: Comparator::LE,
                    ..
                }
            )),
            "Cmc<=N must distribute to the earlier (Creature) leg, got {first:?}"
        );
    }
}
