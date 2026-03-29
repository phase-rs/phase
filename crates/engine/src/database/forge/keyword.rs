use crate::types::keywords::Keyword;

/// Translate a Forge `K:` line into a `Keyword`.
///
/// Forge keyword format: `"Flying"`, `"Cycling 2"`, `"Equip 3"`,
/// `"Protection from red"`, `"Disturb:4 W"`.
///
/// Strategy:
/// 1. Try full line with `from_str()` directly (handles `Flying`, `Haste`)
/// 2. For parameterized keywords: identify keyword prefix, extract cost,
///    reformat as `"Name:cost"` → `from_str()`
/// 3. `Unknown(_)` → return None (don't override Oracle-parsed keywords)
pub(crate) fn translate_keyword(kw_line: &str) -> Option<Keyword> {
    let kw_line = kw_line.trim();
    if kw_line.is_empty() {
        return None;
    }

    // 1. Try direct parse (works for simple keywords + colon-delimited)
    let kw: Keyword = kw_line.parse().unwrap();
    if !matches!(kw, Keyword::Unknown(_)) {
        return Some(kw);
    }

    // 2. Try space-delimited parameterized format: "Cycling 2" → "Cycling:2"
    if let Some((name, cost)) = kw_line.split_once(' ') {
        // Don't transform "Protection from red" — that's multi-word, not parameterized.
        // Protection is handled by step 1 already.
        let reformatted = format!("{name}:{cost}");
        let kw: Keyword = reformatted.parse().unwrap();
        if !matches!(kw, Keyword::Unknown(_)) {
            return Some(kw);
        }
    }

    // 3. Unknown keyword — skip
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_keywords() {
        assert_eq!(translate_keyword("Flying"), Some(Keyword::Flying));
        assert_eq!(translate_keyword("Haste"), Some(Keyword::Haste));
        assert_eq!(translate_keyword("Deathtouch"), Some(Keyword::Deathtouch));
        assert_eq!(
            translate_keyword("First Strike"),
            Some(Keyword::FirstStrike)
        );
    }

    #[test]
    fn test_unknown_returns_none() {
        assert_eq!(translate_keyword("SomeUnknownKeyword123"), None);
    }

    #[test]
    fn test_empty_returns_none() {
        assert_eq!(translate_keyword(""), None);
    }
}
