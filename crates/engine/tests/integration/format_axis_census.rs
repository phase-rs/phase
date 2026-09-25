//! Structural guards for the three format-axis methods `GameFormat` gained:
//! `card_pool`, `commander_pairing`, and `deck_size_subject`. Each
//! guard scans production-scope source text
//! through `include_str!`, so a moved or renamed file is a compile error, not
//! a silently empty scan.
//!
//! Shares `cfg_test_scoped_lines` (the `#[cfg(test)]` scope classifier) and
//! `source_census::code` (the comment-stripping authority) with the sibling
//! censuses in this binary rather than re-deriving either.

use engine::types::format::{CardPool, GameFormat};
use strum::IntoEnumIterator;

use super::loop_shortcut_offer_writer_census::cfg_test_scoped_lines;
use super::source_census::code;

const FORMAT_RS: &str = include_str!("../../src/types/format.rs");
const DECK_VALIDATION_RS: &str = include_str!("../../src/game/deck_validation.rs");
const CLIENT_TYPES_TS: &str = include_str!("../../../../client/src/adapter/types.ts");
const CLIENT_FORMAT_REGISTRY_TS: &str =
    include_str!("../../../../client/src/data/formatRegistry.ts");

/// How many of `src`'s production-scope (non-`#[cfg(test)]`), code-half lines
/// contain `needle`.
fn production_count(src: &str, needle: &str) -> usize {
    let scoped = cfg_test_scoped_lines(src);
    src.lines()
        .enumerate()
        .filter(|(i, line)| !scoped[*i] && code(line).contains(needle))
        .count()
}

/// How many of `text`'s own lines contain `needle`, on the code half. Used on
/// an already-extracted span, whose lines are never inside `#[cfg(test)]`
/// for any header this file passes to [`fn_span`].
fn count_needle(text: &str, needle: &str) -> usize {
    text.lines()
        .filter(|line| code(line).contains(needle))
        .count()
}

/// The source text of the brace-delimited declaration whose header line's
/// code half, trimmed of leading whitespace, starts with `header` — from that
/// line through the line whose code half is exactly the closing brace at the
/// header's own indentation. Callers pass `fn ` headers and
/// a `pub struct ` header, so `header` is the literal opening text
/// of a declaration and not a function name. Comment halves are stripped by
/// `source_census::code` before either test is applied, so a brace inside a
/// doc comment cannot close a span. Panics unless exactly one line matches
/// `header`: an ambiguous header would otherwise silently select the first
/// match.
pub(super) fn fn_span<'a>(src: &'a str, header: &str) -> &'a str {
    let mut offsets: Vec<(usize, &str)> = Vec::new();
    let mut pos = 0usize;
    for line in src.split_inclusive('\n') {
        let stripped = line.strip_suffix('\n').unwrap_or(line);
        offsets.push((pos, stripped));
        pos += line.len();
    }

    let starts: Vec<usize> = offsets
        .iter()
        .enumerate()
        .filter(|(_, (_, line))| code(line).trim_start().starts_with(header))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        starts.len(),
        1,
        "expected exactly one line starting with {header:?}, found {}",
        starts.len()
    );
    let start_idx = starts[0];
    let (start_byte, start_line) = offsets[start_idx];
    let indent = start_line.len() - start_line.trim_start().len();
    let closing = format!("{}}}", " ".repeat(indent));

    let mut end_idx = start_idx;
    for (idx, (_, line)) in offsets.iter().enumerate().skip(start_idx + 1) {
        if code(line).trim_end() == closing {
            end_idx = idx;
            break;
        }
    }
    let end_byte = offsets
        .get(end_idx + 1)
        .map(|(byte, _)| *byte)
        .unwrap_or(src.len());
    &src[start_byte..end_byte]
}

/// Asserts that the exhaustive `match` inside `GameFormat`'s axis method whose
/// header is `header` has gained no `_ =>` wildcard arm.
///
/// Non-vacuity is mandatory here: this assertion is
/// an ABSENCE, so a failed extraction yields an empty span, which trivially
/// contains no `_ =>` and would pass. Each span must be non-empty and mention
/// `GameFormat::` at least once before the wildcard check is trusted.
fn assert_axis_method_has_no_wildcard_arm(header: &str) {
    let span = fn_span(FORMAT_RS, header);
    assert!(
        !span.is_empty(),
        "non-vacuity: the extracted span for {header:?} must not be empty"
    );
    assert!(
        count_needle(span, "GameFormat::") >= 1,
        "non-vacuity: the extracted span for {header:?} must mention GameFormat::"
    );
    assert_eq!(
        count_needle(span, "_ =>"),
        0,
        "{header:?}'s match must stay exhaustive with no wildcard arm"
    );
}

/// Zero occurrences of
/// `legality_format()` chained straight into `.unwrap()`/`.expect(` in
/// production scope, across both files.
///
/// Positive reach-guard: the same scan must still find at least one BARE
/// `legality_format()` call — `evaluate_commander_with_format`,
/// `quick_commander_check` and `max_deck_copies` already respect the
/// `Option` and are deliberately not migrated, so a scan finding zero
/// bare calls would mean the scan itself is broken, not that every caller was
/// migrated.
#[test]
fn no_caller_unwraps_legality_format() {
    let mut bare = 0usize;
    let mut chained = 0usize;
    for src in [FORMAT_RS, DECK_VALIDATION_RS] {
        let scoped = cfg_test_scoped_lines(src);
        for (i, line) in src.lines().enumerate() {
            if scoped[i] {
                continue;
            }
            let half = code(line);
            if half.contains("legality_format().unwrap()")
                || half.contains("legality_format().expect(")
            {
                chained += 1;
            } else if half.contains("legality_format()") {
                bare += 1;
            }
        }
    }
    assert_eq!(
        chained, 0,
        "a production caller chains legality_format() straight into .unwrap()/.expect( — \
         it must go through CardPoolAuthority::for_format instead"
    );
    assert!(
        bare >= 1,
        "reach guard: no bare legality_format() call survives in production scope"
    );
}

/// Zero occurrences of the four commander-count spellings,
/// in production scope. The fourth spelling is
/// brace-anchored by measurement, not by taste: the bare form also matches
/// `evaluate_commander_with_format`'s post-admission consequence gate
/// (`… && request.commander.len() <= 2`).
///
/// This is a ratchet against the four spellings, not a
/// proof that no caller re-derives a commander count some other way.
///
/// Positive reach-guard: the consequence gates and arithmetic
/// must still be found — at least six `commander_pairing()`
/// calls and at least one `request.commander.len()` read.
#[test]
fn no_caller_reintroduces_a_commander_count_literal() {
    const BANNED: [&str; 4] = [
        "commander.len() > 2",
        "commander.len() != 1",
        "commander.is_empty() ||",
        "!request.commander.is_empty() {",
    ];
    for spelling in BANNED {
        assert_eq!(
            production_count(DECK_VALIDATION_RS, spelling),
            0,
            "a production caller re-derives the commander count with {spelling:?} — \
             it must go through GameFormat::commander_pairing() instead"
        );
    }
    assert!(
        production_count(DECK_VALIDATION_RS, "commander_pairing()") >= 6,
        "reach guard: fewer than 6 commander_pairing() calls survive in production scope"
    );
    assert!(
        production_count(DECK_VALIDATION_RS, "request.commander.len()") >= 1,
        "reach guard: no request.commander.len() read survives in production scope — \
         the surviving consequence gates and arithmetic should still read it"
    );
}

/// No construction outside `for_format`.
///
/// A textual count cannot tell a `match` PATTERN naming `LegalityTable` from
/// a CONSTRUCTION of one, so this subtracts the one known consumption site
/// (`CardPoolAuthority::status`'s own match arm) by span rather than by a
/// flat count. Three count equalities, each against a non-zero expectation,
/// so a failed span extraction fails the assertion instead of passing
/// vacuously.
#[test]
fn card_pool_authority_tables_are_built_in_one_place() {
    const NEEDLE: &str = "CardPoolAuthority::LegalityTable(";

    assert!(
        DECK_VALIDATION_RS.contains("CardPoolAuthority::for_format("),
        "reach guard: no CardPoolAuthority::for_format( call found"
    );

    let for_format_span = fn_span(DECK_VALIDATION_RS, "fn for_format(");
    let status_span = fn_span(DECK_VALIDATION_RS, "fn status(self,");
    let for_format_count = count_needle(for_format_span, NEEDLE);
    let status_count = count_needle(status_span, NEEDLE);
    let total_production = production_count(DECK_VALIDATION_RS, NEEDLE);
    let remainder = total_production
        .checked_sub(for_format_count)
        .and_then(|n| n.checked_sub(status_count))
        .expect("the two spans' counts must not exceed the file's total production count");

    assert_eq!(
        for_format_count, 1,
        "CardPoolAuthority::for_format must construct LegalityTable exactly once"
    );
    assert_eq!(
        status_count, 1,
        "CardPoolAuthority::status's own match arm is the one disclosed consumption \
         site"
    );
    assert_eq!(
        remainder, 1,
        "exactly one LegalityTable( construction should survive outside for_format and \
         status — evaluate_standard's disclosed, format-independent reference column"
    );
}

/// Secondary instrument: `E0004` catches a deleted arm, but not a future
/// `_ => CardPool::NoEngineAuthority` silencing the compiler and re-creating
/// the implied default.
#[test]
fn format_axis_methods_carry_no_wildcard_arm() {
    assert_axis_method_has_no_wildcard_arm("pub fn card_pool(");
    assert_axis_method_has_no_wildcard_arm("pub fn commander_pairing(");
    assert_axis_method_has_no_wildcard_arm("pub fn deck_size_subject(");
}

/// What is pinned is the
/// DECLARATION — which built-ins positively declare `CardPool::Unrestricted`
/// — and not an inference from how permissive a format is.
/// [`CardPool::Unrestricted`] draws that line itself: "Silence is not such a
/// declaration", and a format that states no deck-construction rule at all
/// stays `NoEngineAuthority` however little it restricts. Declaring
/// `Unrestricted` is a deliberate act; this list is what keeps it one.
#[test]
fn unrestricted_card_pool_declarations_match_the_committed_list() {
    let declared: Vec<GameFormat> = GameFormat::iter()
        .filter(|format| format.card_pool() == CardPool::Unrestricted)
        .collect();
    assert_eq!(
        declared,
        vec![GameFormat::Freeform, GameFormat::FreeformCommander]
    );
}

/// No format axis reaches either client mirror.
/// These axes are `GameFormat` methods, not `FormatConfig`/`FormatMetadata`
/// fields, so they have no wire surface by design; this asserts the absence
/// the design promises.
///
/// Reach-guard: both files must still carry the existing `sideboard_policy`
/// key, so a failed extraction (reading the wrong file) cannot pass by
/// finding nothing.
#[test]
fn no_format_axis_key_reaches_the_client_mirror() {
    for (label, src) in [
        ("client/src/adapter/types.ts", CLIENT_TYPES_TS),
        (
            "client/src/data/formatRegistry.ts",
            CLIENT_FORMAT_REGISTRY_TS,
        ),
    ] {
        assert!(
            src.contains("sideboard_policy"),
            "reach guard: {label} must still declare sideboard_policy"
        );
        for key in ["card_pool", "commander_pairing", "deck_size_subject"] {
            assert!(
                !src.contains(key),
                "{label} must not gain a `{key}` key — this axis has no wire surface"
            );
        }
    }
}

/// `FormatMetadata`'s field list is exactly its
/// committed shape. A presence assertion against a non-empty expected list,
/// so a failed extraction fails rather than passing vacuously — no separate
/// non-vacuity premise is needed.
#[test]
fn format_metadata_declares_exactly_its_committed_field_list() {
    let span = fn_span(FORMAT_RS, "pub struct FormatMetadata {");
    let mut fields = Vec::new();
    for line in span.lines().skip(1) {
        let trimmed = code(line).trim();
        if trimmed == "}" {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("pub ") {
            if let Some(name) = rest.split(':').next() {
                fields.push(name.trim().to_string());
            }
        }
    }
    assert_eq!(
        fields,
        vec![
            "format",
            "label",
            "short_label",
            "description",
            "group",
            "legality_key",
            "default_config",
        ],
        "FormatMetadata's field list must match its committed shape exactly"
    );
}
