//! Standardized result type and error conversion for Oracle text parser combinators.
//!
//! Provides the shared `OracleResult` type alias and the `parse_or_unimplemented`
//! boundary function that converts nom parse results into `Effect::Unimplemented`
//! with diagnostic traces per D-12 (error trace on failed fragments) and D-13
//! (partial parses become Unimplemented).

use nom::IResult;
use nom_language::error::VerboseError;

use crate::types::ability::Effect;

/// Standard result type for all Oracle text parser combinators.
/// Uses `VerboseError` for error chain accumulation across branches,
/// enabling detailed diagnostics when a combinator chain fails.
pub type OracleResult<'a, O> = IResult<&'a str, O, VerboseError<&'a str>>;

/// Convert a nom parse attempt into an `Effect`, returning `Effect::Unimplemented`
/// with error trace when all branches fail (D-12) or when input is not fully
/// consumed (D-13: no partial parses).
///
/// This is the boundary function between nom combinators and the Oracle text
/// dispatcher. Every line that goes through nom dispatch should pass through
/// this function to ensure consistent error reporting.
pub fn parse_or_unimplemented<'a, F>(input: &'a str, mut parser: F) -> Effect
where
    F: FnMut(&'a str) -> OracleResult<'a, Effect>,
{
    match parser(input) {
        Ok(("", effect)) => effect, // Fully consumed — success
        Ok((rest, _)) => {
            // D-13: partial parse = entire line is Unimplemented
            Effect::Unimplemented {
                name: "partial_parse".into(),
                description: Some(format!("Unparsed remainder: {}", truncate(rest, 80))),
            }
        }
        Err(nom::Err::Error(e) | nom::Err::Failure(e)) => {
            // D-12: emit Unimplemented with VerboseError trace
            Effect::Unimplemented {
                name: "parse_failed".into(),
                description: Some(format_verbose_error(input, &e)),
            }
        }
        Err(nom::Err::Incomplete(_)) => Effect::Unimplemented {
            name: "incomplete".into(),
            description: Some(format!("Incomplete input: {}", truncate(input, 80))),
        },
    }
}

/// Format a `VerboseError` into a human-readable string showing which branches
/// were attempted and where they failed.
fn format_verbose_error(input: &str, error: &VerboseError<&str>) -> String {
    nom_language::error::convert_error(input, error.clone())
}

/// Adapt an `Option`-returning parser into a nom combinator for use in `alt()` chains.
///
/// When the parser returns `Some(effect)`, this produces `Ok(("", effect))` (full consumption).
/// When it returns `None`, this produces a nom `Error` with a context label so that
/// `VerboseError` traces show which branch was attempted.
pub fn option_to_nom<'a>(label: &'static str, result: Option<Effect>) -> OracleResult<'a, Effect> {
    match result {
        Some(effect) => Ok(("", effect)),
        None => Err(nom::Err::Error(VerboseError {
            errors: vec![("", nom_language::error::VerboseErrorKind::Context(label))],
        })),
    }
}

/// Truncate a string for error messages, respecting UTF-8 char boundaries.
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        // Find the last char boundary at or before max_len
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nom::bytes::complete::tag;
    use nom::Parser;

    use crate::types::ability::QuantityExpr;

    fn draw_one() -> Effect {
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: crate::types::ability::TargetFilter::Controller,
            up_to: false,
        }
    }

    fn draw_parser(input: &str) -> OracleResult<'_, Effect> {
        tag("draw a card").map(|_: &str| draw_one()).parse(input)
    }

    #[test]
    fn test_successful_full_parse() {
        let effect = parse_or_unimplemented("draw a card", draw_parser);
        assert!(matches!(effect, Effect::Draw { .. }));
    }

    #[test]
    fn test_partial_parse_returns_unimplemented() {
        let effect = parse_or_unimplemented("draw a card and more", draw_parser);
        match effect {
            Effect::Unimplemented { name, description } => {
                assert_eq!(name, "partial_parse");
                assert!(description
                    .as_deref()
                    .unwrap_or("")
                    .contains("Unparsed remainder"));
            }
            _ => panic!("Expected Unimplemented, got {effect:?}"),
        }
    }

    #[test]
    fn test_failed_parse_returns_error_trace() {
        let effect = parse_or_unimplemented("xyz unknown text", draw_parser);
        match effect {
            Effect::Unimplemented { name, description } => {
                assert_eq!(name, "parse_failed");
                // VerboseError trace should contain diagnostic info
                let desc = description.as_deref().unwrap_or("");
                assert!(!desc.is_empty(), "Error trace should not be empty");
            }
            _ => panic!("Expected Unimplemented, got {effect:?}"),
        }
    }

    #[test]
    fn test_truncate_respects_char_boundaries() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello");
        // Multi-byte: ensure no panic on UTF-8 boundary
        let s = "cafe\u{0301}"; // "café" with combining accent
        let t = truncate(s, 4);
        assert!(t.len() <= 4);
    }
}
