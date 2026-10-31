//! CR 614.1 + CR 614.12 + CR 303.4 + CR 303.4a + CR 303.4g + CR 613.1d +
//! CR 613.1f + CR 113.10 + CR 604.1 + CR 702.5a: detect and parse the
//! return-as-Aura sentence form used by Old-Growth Troll (KHM),
//! Bronzehide Lion (THB), and Harold and Bob, First Numens (FIN-precon).
//!
//! Grammar (post-chunk-split):
//!
//! ```text
//! "It's an Aura enchantment with enchant " <enchant_filter>
//!   " and \"" <body> "\""
//!   [ ( ". " | " " ) "~ loses all other abilities" ]
//!   [ "." ]
//! ```
//!
//! Bronzehide's `it loses all other abilities` is NOT matched here — the
//! chunk-splitter at `parser/oracle_effect/sequence.rs::starts_bare_and_clause`
//! pre-splits that clause into a separate `GenericEffect` sibling, which the
//! caller's `try_fold_loses_other_sibling` consumes (see
//! `parser/oracle_effect/mod.rs`).
//!
//! Body closure detection consumes up to the first closing `"`. This works for
//! the three-card class because no member nests inner double quotes;
//! single-quoted segments inside the body are fine. If a future card nests
//! `"..."` inside the outer block, the body parser must track double-quote
//! depth.
//!
//! Returns `(TargetFilter, Vec<ContinuousModification>, bool /* loses_other */)`.

use nom::branch::alt;
use nom::bytes::complete::{tag, take_till1};
use nom::combinator::value;
use nom::Parser;

use super::bridge::{nom_on_lower, split_once_on_lower};
use super::enchant::parse_enchant_target_full;
use super::error::OracleResult;
use crate::types::ability::{ContinuousModification, TargetFilter};

/// Detect the return-as-Aura sentence form on a chunked Oracle text body.
///
/// `text` is the original-case chunk; `lower` is the pre-lowercased view
/// (same byte length — Oracle text after upstream normalization is ASCII).
///
/// Returns `Some((enchant_filter, grants, loses_other))` on success.
/// Returns `None` if any of the following are false:
///   * prefix `it's an aura enchantment with enchant ` is missing,
///   * the enchant filter does not parse,
///   * no body opener `\" and \"` follows the filter,
///   * the body is empty or has no closing `\"`.
///
/// The `loses_other` flag is `true` only for Harold-shape inputs where the
/// suffix `[. ]~ loses all other abilities` follows the closing `\"`.
/// Bronzehide-shape `and it loses all other abilities` arrives as a separate
/// sibling clause (the chunk-splitter pre-splits at the bare ` and ` connector)
/// and is folded into `grants` by `try_fold_loses_other_sibling` at the IR
/// layer; it does NOT round-trip through this combinator.
pub fn try_parse(
    text: &str,
    lower: &str,
) -> Option<(TargetFilter, Vec<ContinuousModification>, bool)> {
    // Step 1: run the lowercase prefix detector to extract the enchant filter.
    // `nom_on_lower` runs the combinator on the lowercase view and validates
    // structural shape; the returned `TargetFilter` is owned (no borrow back
    // into `lower`) so the lifetime constraints on `nom_on_lower`'s parser
    // closure are trivially satisfied.
    let (enchant_filter, _) = nom_on_lower(text, lower, parse_prefix_and_filter)?;

    // Step 2: split the original-case `text` on ` and "` to find the body
    // opener. `split_once_on_lower` returns both halves in original case.
    let (_, after_open_quote) = split_once_on_lower(text, lower, " and \"")?;

    // Step 3: consume the quoted body in original-case `text`. BLOCKER E: body
    // must be non-empty.
    let (after_close_quote, body_original) = parse_quoted_body(after_open_quote).ok()?;
    if body_original.trim().is_empty() {
        return None;
    }

    // Step 4: optional loses-other-abilities suffix (Harold-shape only;
    // Bronzehide-shape is consumed by `try_fold_loses_other_sibling` at the IR
    // layer). The tail may end with a trailing period; strip it for matching.
    let tail_lower = after_close_quote.trim().to_ascii_lowercase();
    let tail_no_period = tail_lower.trim_end_matches('.').trim();
    let loses_other = parse_loses_clause(tail_no_period).is_ok();

    // Step 5: classify the original-case body via the canonical helper. The
    // original-case slice preserves "Enchanted Forest" so the inner static
    // parser's subtype lookup matches.
    let grants = crate::parser::oracle_static::classify_quoted_inner(body_original.trim());

    Some((enchant_filter, grants, loses_other))
}

/// Lowercase-only combinator: consume the fixed prefix and the enchant filter.
/// Returns the owned `TargetFilter`; `nom_on_lower` derives the consumed-byte
/// count from the returned remainder slice for offset bookkeeping.
fn parse_prefix_and_filter(input: &str) -> OracleResult<'_, TargetFilter> {
    let (input, _) = tag("it's an aura enchantment with enchant ").parse(input)?;
    let (input, filter) = parse_enchant_target_full(input)?;
    Ok((input, filter))
}

fn parse_quoted_body(input: &str) -> OracleResult<'_, &str> {
    let (input, body) = take_till1(|c| c == '"').parse(input)?;
    let (input, _) = tag("\"").parse(input)?;
    Ok((input, body))
}

/// Loses-clause combinator — Harold-shape only.
/// Bronzehide's "and it loses all other abilities" is handled by the IR-layer
/// fold (`try_fold_loses_other_sibling`) because the chunk-splitter pre-splits
/// the bare ` and ` connector.
fn parse_loses_clause(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag(". ~ loses all other abilities"),
            tag("~ loses all other abilities"),
        )),
    )
    .parse(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ControllerRef, Duration, TypeFilter, TypedFilter};

    fn lower(s: &str) -> String {
        s.to_ascii_lowercase()
    }

    #[test]
    fn try_parse_old_growth_troll_chunk() {
        let text = "It's an Aura enchantment with enchant Forest you control and \"Enchanted Forest has '{T}: Add {G}{G}' and '{1}, {T}, Sacrifice ~: Create a tapped 4/4 green Troll Warrior creature token with trample.'\"";
        let l = lower(text);
        let (filter, grants, loses_other) = try_parse(text, &l).expect("must parse");
        match &filter {
            TargetFilter::Typed(TypedFilter {
                type_filters,
                controller,
                ..
            }) => {
                assert_eq!(type_filters.len(), 1);
                assert_eq!(type_filters[0], TypeFilter::Subtype("Forest".to_string()));
                assert_eq!(*controller, Some(ControllerRef::You));
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
        assert!(!grants.is_empty(), "expected at least one grant, got 0");
        assert!(!loses_other);
    }

    #[test]
    fn try_parse_bronzehide_lion_chunk() {
        // Bronzehide Lion: chunk #2 — the loses-other-abilities clause is a
        // separate sibling sub_ability and is NOT present in this chunk.
        let text = "It's an Aura enchantment with enchant creature you control and \"{G}{W}: Enchanted creature gains indestructible until end of turn,\"";
        let l = lower(text);
        let (filter, grants, loses_other) = try_parse(text, &l).expect("must parse");
        match &filter {
            TargetFilter::Typed(TypedFilter {
                type_filters,
                controller,
                ..
            }) => {
                assert_eq!(type_filters, &vec![TypeFilter::Creature]);
                assert_eq!(*controller, Some(ControllerRef::You));
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
        assert!(!grants.is_empty());
        assert!(!loses_other);
        // #5681: the granted ability's "until end of turn" is carried with the
        // enclosing sentence's comma INSIDE the closing quote ("...until end of
        // turn,"). That trailing comma must be normalized away before the inner
        // sub-parse, so the duration survives as a typed `UntilEndOfTurn` rather
        // than being dropped into prose. Assert on the typed duration, not the
        // debug representation.
        let ContinuousModification::GrantAbility { definition } = grants
            .iter()
            .find(|grant| matches!(grant, ContinuousModification::GrantAbility { .. }))
            .expect("expected the quoted activated ability to be granted")
        else {
            unreachable!()
        };
        assert_eq!(definition.duration, Some(Duration::UntilEndOfTurn));
    }

    #[test]
    fn try_parse_harold_and_bob_chunk() {
        // Harold: loses-clause attached to the same chunk (no ` and ` connector).
        let text = "It's an Aura enchantment with enchant Forest you control and \"Enchanted Forest has '{T}: Add three mana of any one color. You get two rad counters.'\" ~ loses all other abilities";
        let l = lower(text);
        let (filter, grants, loses_other) = try_parse(text, &l).expect("must parse");
        match &filter {
            TargetFilter::Typed(TypedFilter {
                type_filters,
                controller,
                ..
            }) => {
                assert_eq!(
                    type_filters,
                    &vec![TypeFilter::Subtype("Forest".to_string())]
                );
                assert_eq!(*controller, Some(ControllerRef::You));
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
        assert!(!grants.is_empty());
        assert!(loses_other, "expected loses_other = true for Harold");
    }

    #[test]
    fn try_parse_rejects_pure_filter_no_body() {
        // No ` and "` body opener — must reject (BLOCKER E).
        let text = "It's an Aura enchantment with enchant Forest you control.";
        let l = lower(text);
        assert!(try_parse(text, &l).is_none());
    }

    #[test]
    fn try_parse_rejects_unrelated_chunk() {
        let text = "Return target creature to its owner's hand.";
        let l = lower(text);
        assert!(try_parse(text, &l).is_none());
    }

    #[test]
    fn try_parse_rejects_empty_body() {
        // Empty body: prefix + filter + ` and ""`. Must reject (BLOCKER E).
        let text = "It's an Aura enchantment with enchant Forest you control and \"\"";
        let l = lower(text);
        assert!(try_parse(text, &l).is_none());
    }
}
