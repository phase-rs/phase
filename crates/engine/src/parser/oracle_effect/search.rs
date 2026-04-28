use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::Parser;
use nom_language::error::VerboseError;

use super::super::oracle_nom::bridge::nom_on_lower;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_target::{parse_mana_value_suffix, parse_type_phrase};
use super::super::oracle_util::{
    contains_possessive, infer_core_type_for_subtype, split_around, strip_after,
};
use super::types::{SearchLibraryDetails, SeekDetails};
use super::{capitalize, scan_contains_phrase};
use crate::parser::oracle_warnings::push_warning;
use crate::types::ability::{
    ControllerRef, FilterProp, QuantityExpr, SearchSelectionConstraint, TargetFilter, TypeFilter,
    TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::zones::Zone;

/// Scan `lower` at word boundaries for `tag_prefix`, then apply `combinator` to the
/// remainder. Returns `(parsed_value, byte_offset_in_lower_of_tail)` on first match.
///
/// Prefer this over `strip_after` + nom for composable multi-position parsing —
/// matches start-of-string, spaces, commas, or semicolons as word boundaries.
fn scan_preceded<'a, T>(
    lower: &'a str,
    tag_prefix: &'static str,
    mut combinator: impl FnMut(&'a str) -> Result<(&'a str, T), nom::Err<VerboseError<&'a str>>>,
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

pub(super) fn parse_search_library_details(lower: &str) -> SearchLibraryDetails {
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
        (parse_search_filter(&lower[type_start..]), Vec::new())
    } else if let Some(after_for) = strip_after(lower, "for a ") {
        parse_search_filter_with_extras(after_for)
    } else if let Some(after_for) = strip_after(lower, "for an ") {
        parse_search_filter_with_extras(after_for)
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
    let selection_constraint = if scan_distinct_names_clause(lower) {
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
        extra_filters,
        multi_destination,
        multi_enter_tapped,
    }
}

/// CR 608.2c + CR 701.23: Detect the "with different names" printed-text
/// restriction at any word boundary in the clause. Composable nom scan that
/// matches both the canonical phrasing and the trivial pluralization variants
/// without committing to a fixed prefix.
fn scan_distinct_names_clause(lower: &str) -> bool {
    use nom::branch::alt;
    use nom::combinator::value;

    scan_preceded(lower, "with ", |i| {
        alt((
            value((), tag::<_, _, VerboseError<&str>>("different names")),
            value((), tag::<_, _, VerboseError<&str>>("different name")),
        ))
        .parse(i)
    })
    .is_some()
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
fn parse_search_filter_with_extras(tail: &str) -> (TargetFilter, Vec<TargetFilter>) {
    // structural: not dispatch — bound the filter region at the first clause
    // terminator (comma / period) before running the conjunction combinator,
    // so `" and "` inside e.g. `"put it onto the battlefield, then ..."` can't
    // pollute the filter split.
    let filter_region_end = tail
        .find(',')
        .or_else(|| tail.find('.'))
        .unwrap_or(tail.len());
    let filter_region = &tail[..filter_region_end];

    // Split on `" and a "` / `" and an "` / `" and basic "` at filter-region
    // boundaries only. The "and basic" branch preserves the supertype prefix so
    // the downstream filter parser sees e.g. `"basic plains card"` intact.
    let segments = split_filter_conjunctions(filter_region);
    if segments.len() < 2 {
        return (parse_search_filter(tail), Vec::new());
    }

    let primary = parse_search_filter(segments[0]);
    let extras: Vec<TargetFilter> = segments[1..]
        .iter()
        .map(|segment| parse_search_filter(segment))
        .collect();
    (primary, extras)
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
            take_until::<_, _, VerboseError<&str>>(" and "),
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
pub(super) fn parse_seek_details(lower: &str) -> SeekDetails {
    let after_seek = tag::<_, _, VerboseError<&str>>("seek ")
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

    let filter = parse_search_filter(remaining);

    SeekDetails {
        filter,
        count,
        destination,
        enter_tapped,
    }
}

/// Parse the card type filter from search text like "basic land card, ..."
/// or "creature card with ..." into a TargetFilter.
pub(super) fn parse_search_filter(text: &str) -> TargetFilter {
    let type_text = text.trim();

    let (parsed_filter, remainder) = parse_type_phrase(type_text);
    if !matches!(parsed_filter, TargetFilter::Any) {
        let mut suffix = SearchSuffixConstraints::default();
        parse_search_filter_suffixes(remainder, &mut suffix);
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

    parse_search_filter_fallback(type_word, suffix_text, is_basic)
}

fn parse_search_filter_fallback(
    type_word: &str,
    suffix_text: &str,
    is_basic: bool,
) -> TargetFilter {
    let suffix = build_search_suffix_constraints(suffix_text, is_basic);
    let filter = parse_search_builtin_type_word(type_word)
        .unwrap_or_else(|| parse_search_specialized_type_word(type_word));
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
            tag::<_, _, VerboseError<&str>>("instant or sorcery"),
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

fn parse_search_specialized_type_word(type_word: &str) -> TargetFilter {
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

    push_warning(format!(
        "target-fallback: unrecognized search filter '{}'",
        type_word
    ));
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

fn build_search_suffix_constraints(suffix_text: &str, is_basic: bool) -> SearchSuffixConstraints {
    let mut suffix = SearchSuffixConstraints::default();
    if is_basic {
        suffix.properties.push(FilterProp::HasSupertype {
            value: crate::types::card_type::Supertype::Basic,
        });
    }
    parse_search_filter_suffixes(suffix_text, &mut suffix);
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

/// Parse property suffixes from search filter text ("with mana value ...", "with a different name ...").
/// Reuses the existing suffix parsers from oracle_target.
fn parse_search_filter_suffixes(text: &str, suffix: &mut SearchSuffixConstraints) {
    let lower = text.to_lowercase();
    let mut remaining = lower.as_str();

    while !remaining.is_empty() {
        remaining = remaining.trim_start();

        // Consume redundant "card(s)" re-declaration left by parse_type_phrase.
        // parse_type_phrase extracts only the type word (e.g. "creature"), so the
        // literal " card" / " cards" token remains and carries no filter meaning.
        if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("cards").parse(remaining) {
            remaining = rest.trim_start();
        } else if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("card").parse(remaining) {
            remaining = rest.trim_start();
        }

        // End-of-filter sentinel: punctuation, "then …", "reveal …", or "put …"
        // means the search filter has ended and what follows is the action chain
        // handled by the downstream sequence parser. Not a filter-suffix gap — break
        // without warning.
        if remaining.is_empty()
            || tag::<_, _, VerboseError<&str>>(",")
                .parse(remaining)
                .is_ok()
            || tag::<_, _, VerboseError<&str>>(".")
                .parse(remaining)
                .is_ok()
            || tag::<_, _, VerboseError<&str>>("then ")
                .parse(remaining)
                .is_ok()
            || tag::<_, _, VerboseError<&str>>("reveal ")
                .parse(remaining)
                .is_ok()
            || tag::<_, _, VerboseError<&str>>("put ")
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
        if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("and ").parse(remaining) {
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("with that name").parse(remaining) {
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
            tag::<_, _, VerboseError<&str>>("with the same name as that ").parse(remaining)
        {
            // Consume the demonstrative subject noun and any trailing modifier
            // ("nontoken creature", "creature", "card") up to the next sentinel
            // (',', '.') via `take_till` — drop the consumed noun and continue
            // the dispatch loop at the sentinel position.
            let (after_noun, _consumed_noun) =
                nom::bytes::complete::take_till::<_, _, VerboseError<&str>>(|c: char| {
                    c == ',' || c == '.'
                })
                .parse(rest)
                .unwrap_or((rest, ""));
            suffix.properties.push(FilterProp::SameNameAsParentTarget);
            remaining = after_noun.trim_start();
            continue;
        }

        // CR 608.2c: "with different names" / "with different name" — distinct-names
        // constraint is already encoded on the search details upstream via
        // `scan_distinct_names_clause`. The chomping arm here exists solely to
        // consume the marker text so the dispatch loop does not emit a spurious
        // "search-filter-suffix unmatched" warning. No FilterProp emitted.
        if let Ok((rest, _)) = nom::sequence::pair(
            tag::<_, _, VerboseError<&str>>("with different name"),
            nom::combinator::opt(tag::<_, _, VerboseError<&str>>("s")),
        )
        .parse(remaining)
        {
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, _)) =
            tag::<_, _, VerboseError<&str>>("with a basic land type").parse(remaining)
        {
            suffix.type_filters.push(basic_land_type_any_of());
            remaining = rest.trim_start();
            continue;
        }

        if let Some((prop, consumed)) = parse_mana_value_suffix(remaining) {
            suffix.properties.push(prop);
            remaining = remaining[consumed..].trim_start();
            continue;
        }

        if let Ok((rest, _)) =
            tag::<_, _, VerboseError<&str>>("with a different name than each ").parse(remaining)
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
                    push_warning(format!(
                        "target-fallback: unrecognized inner type '{}' in different-name filter",
                        inner_type
                    ));
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
        push_warning(format!(
            "target-fallback: search-filter-suffix unmatched: '{}'",
            remaining
        ));
        break;
    }
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

    #[test]
    fn search_target_opponent_library() {
        let details = parse_search_library_details(
            "search target opponent's library for a creature card and put that card onto the battlefield under your control",
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
        let details =
            parse_search_library_details("search target player's library for a card and exile it");
        assert!(details.target_player.is_some());
        assert_eq!(details.target_player.unwrap(), TargetFilter::Player);
    }

    #[test]
    fn search_target_player_library_for_three() {
        // Jester's Cap: "search target player's library for three cards and exile them"
        let details = parse_search_library_details(
            "search target player's library for three cards and exile them",
        );
        assert!(details.target_player.is_some());
        assert_eq!(details.count, QuantityExpr::Fixed { value: 3 });
    }

    #[test]
    fn search_your_library_no_target_player() {
        let details = parse_search_library_details(
            "search your library for a basic land card, reveal it, put it into your hand",
        );
        assert!(details.target_player.is_none());
        assert!(details.reveal);
    }

    #[test]
    fn search_up_to_x_cards_emits_variable_count() {
        // CR 107.3a + CR 601.2b: `up to X` emits `QuantityRef::Variable` so the
        // resolver can pick up the caster's announced X at effect time.
        use crate::types::ability::QuantityRef;
        let details =
            parse_search_library_details("search your library for up to x creature cards");
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
        let details =
            parse_search_library_details("search your library for three cards and exile them");
        assert_eq!(details.count, QuantityExpr::Fixed { value: 3 });
    }

    #[test]
    fn action_chain_continuation_does_not_warn() {
        // Regression: filter parser must not emit "search-filter-suffix unmatched"
        // for legitimate action-chain continuations. The filter is already
        // extracted by parse_type_phrase; what follows the filter clause
        // (", put it onto the battlefield, then shuffle") is handled by the
        // downstream sequence parser — not a filter-suffix gap.
        use crate::parser::oracle_warnings::{clear_warnings, take_warnings};
        for text in [
            "creature card, put it onto the battlefield, then shuffle",
            "land card, reveal it, put it into your hand, then shuffle",
            "card, put it onto the battlefield tapped",
            "creature card. exile it",
        ] {
            clear_warnings();
            let _ = parse_search_filter(text);
            let warnings = take_warnings();
            assert!(
                !warnings
                    .iter()
                    .any(|w| w.contains("search-filter-suffix unmatched")),
                "unexpected filter-suffix warning for {text:?}: {warnings:?}"
            );
        }
    }

    #[test]
    fn genuine_filter_suffix_gap_still_warns() {
        // Diagnostic preserved: when the suffix parser is handed text that
        // doesn't match any known filter-suffix pattern AND doesn't look like an
        // action-chain continuation (no leading comma / period / "then"), a
        // warning must still fire so coverage reports surface parser gaps.
        use crate::parser::oracle_warnings::{clear_warnings, take_warnings};
        clear_warnings();
        let mut suffix = SearchSuffixConstraints::default();
        // Invented suffix that won't hit any existing filter-suffix pattern.
        parse_search_filter_suffixes(" with unrecognized flibbertigibbet suffix", &mut suffix);
        let warnings = take_warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("search-filter-suffix unmatched")),
            "expected filter-suffix warning for novel grammar, got {warnings:?}"
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
        let suffix = build_search_suffix_constraints(" with that name", true);
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
    fn parse_search_filter_fallback_handles_basic_card_same_name() {
        let filter = parse_search_filter_fallback("card", " with that name", true);
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
        let filter = parse_search_filter("land card with a basic land type");
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
    fn search_any_number_of_dragon_creature_cards_sets_up_to_and_filter() {
        // CR 107.1c: Sarkhan, Dragonsoul [-9]: "Search your library for any number
        // of Dragon creature cards, put them onto the battlefield, then shuffle."
        let details = parse_search_library_details(
            "search your library for any number of dragon creature cards, put them onto the battlefield, then shuffle",
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
        );
        assert!(!details.up_to, "exact-count search should not set up_to");
        assert_eq!(details.count, QuantityExpr::Fixed { value: 1 });
    }

    #[test]
    fn parse_search_specialized_type_word_handles_unknown_alphabetic_subtype() {
        let filter = parse_search_specialized_type_word("elf");
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
        );
        assert_eq!(
            details.selection_constraint,
            SearchSelectionConstraint::DistinctNames
        );
        assert!(details.up_to);
        assert_eq!(details.count, QuantityExpr::Fixed { value: 4 });
    }

    /// Regression: searches without the "different names" clause stay on the
    /// `None` constraint and don't pick up a spurious restriction.
    #[test]
    fn search_without_different_names_keeps_none_constraint() {
        let details = parse_search_library_details(
            "search your library for a creature card, put it onto the battlefield",
        );
        assert_eq!(
            details.selection_constraint,
            SearchSelectionConstraint::None
        );
    }
}
