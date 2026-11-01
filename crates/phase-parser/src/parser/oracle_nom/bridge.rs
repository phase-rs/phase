//! Bridge utilities for connecting nom combinators to the Oracle text parser's
//! mixed-case architecture.
//!
//! Oracle text is mixed-case, but nom `tag()` requires exact matching. These
//! bridge functions run nom combinators on lowercase input and map the consumed
//! byte count back to the original-case text to produce correct remainders.
//!
//! Shared across all parser branch files — extracted here to avoid per-file
//! duplication of the same bridging pattern.

use super::error::OracleResult;

/// Run a nom combinator on lowercase input and map the result back to original-case text.
///
/// The parser operates on `lower` (pre-lowercased). On success, the consumed byte count
/// is applied to `text` (original case) to produce the correct remainder.
///
/// Returns `Some((result, original_case_remainder))` on success, `None` on parse failure.
pub fn nom_on_lower<'a, T, F>(text: &'a str, lower: &str, mut parser: F) -> Option<(T, &'a str)>
where
    F: FnMut(&str) -> OracleResult<'_, T>,
{
    let (rest, result) = parser(lower).ok()?;
    let consumed = lower.len() - rest.len();
    Some((result, &text[consumed..]))
}

/// Like [`nom_on_lower`], but returns `Result` for contexts where error propagation is needed.
///
/// On parse failure, returns the nom error converted to a `String` for diagnostic reporting.
pub fn nom_on_lower_required<'a, T, F>(
    text: &'a str,
    lower: &str,
    mut parser: F,
) -> Result<(T, &'a str), String>
where
    F: FnMut(&str) -> OracleResult<'_, T>,
{
    match parser(lower) {
        Ok((rest, result)) => {
            let consumed = lower.len() - rest.len();
            Ok((result, &text[consumed..]))
        }
        Err(e) => Err(format!("{e}")),
    }
}

/// Split `text` on the first case-insensitive occurrence of `sep` (given as lowercase),
/// returning `(before, after)` both in the **original case** of `text`.
///
/// Eliminates the manual `text.len() - suffix.len()` offset idiom by composing
/// `nom_on_lower` with `take_until` + `tag` internally.
///
/// `lower` must be the pre-lowercased version of `text` (same byte length).
/// `sep` must be lowercase.
///
/// # Example
/// ```ignore
/// let text = "You may Exert it. When you do, Draw a card";
/// let lower = text.to_lowercase();
/// let (before, after) = split_once_on_lower(text, &lower, ". when you do, ").unwrap();
/// assert_eq!(before, "You may Exert it");
/// assert_eq!(after, "Draw a card");
/// ```
pub fn split_once_on_lower<'a>(
    text: &'a str,
    lower: &str,
    sep: &str,
) -> Option<(&'a str, &'a str)> {
    // Find `sep` in the lowercase text to get the split position, then map both
    // sides back to the original-case `text`. This is a structural position lookup,
    // not parsing dispatch — `.find()` is permitted for structural boundary detection.
    let pos = lower.find(sep)?;
    Some((&text[..pos], &text[pos + sep.len()..]))
}

/// Run a nom combinator directly on lowercase text, discarding the remainder.
///
/// Useful when the caller only needs the parsed value and the remainder is handled
/// separately (e.g., when the caller already tracks position via byte offsets).
pub fn nom_parse_lower<T, F>(lower: &str, mut parser: F) -> Option<T>
where
    F: FnMut(&str) -> OracleResult<'_, T>,
{
    parser(lower).ok().map(|(_, result)| result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nom::bytes::complete::tag;
    use nom::combinator::value;
    use nom::Parser;

    #[test]
    fn nom_on_lower_maps_remainder_to_original_case() {
        let text = "Exile Target Creature";
        let lower = text.to_lowercase();
        let result = nom_on_lower(text, &lower, |input| {
            value("exile", tag("exile ")).parse(input)
        });
        assert_eq!(result, Some(("exile", "Target Creature")));
    }

    #[test]
    fn nom_on_lower_returns_none_on_mismatch() {
        let text = "Draw a card";
        let lower = text.to_lowercase();
        let result = nom_on_lower(text, &lower, |input| {
            value("exile", tag("exile ")).parse(input)
        });
        assert!(result.is_none());
    }

    #[test]
    fn nom_on_lower_required_returns_error_on_mismatch() {
        let text = "Draw a card";
        let lower = text.to_lowercase();
        let result = nom_on_lower_required(text, &lower, |input| {
            value("exile", tag("exile ")).parse(input)
        });
        assert!(result.is_err());
    }

    #[test]
    fn nom_parse_lower_extracts_value() {
        let result = nom_parse_lower("exile target creature", |input| {
            value("exile", tag("exile ")).parse(input)
        });
        assert_eq!(result, Some("exile"));
    }
}
