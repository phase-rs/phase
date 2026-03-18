use crate::types::ability::TargetFilter;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};

/// Strip reminder text (parenthesized) from a line.
pub fn strip_reminder_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut depth = 0u32;
    for ch in text.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
            }
            _ if depth == 0 => result.push(ch),
            _ => {}
        }
    }
    result.trim().to_string()
}

/// Replace "~" and "CARDNAME" with the actual card name, then lowercase for matching.
pub fn self_ref(text: &str, card_name: &str) -> String {
    text.replace('~', card_name).replace("CARDNAME", card_name)
}

/// Parse an English number word or digit at the start of text.
/// Returns (value, remaining_text) or None.
pub fn parse_number(text: &str) -> Option<(u32, &str)> {
    let text = text.trim_start();
    // Try digit(s) first
    let digit_end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    if digit_end > 0 {
        if let Ok(n) = text[..digit_end].parse::<u32>() {
            return Some((n, text[digit_end..].trim_start()));
        }
    }
    // English words
    let words: &[(&str, u32)] = &[
        ("twenty", 20),
        ("nineteen", 19),
        ("eighteen", 18),
        ("seventeen", 17),
        ("sixteen", 16),
        ("fifteen", 15),
        ("fourteen", 14),
        ("thirteen", 13),
        ("twelve", 12),
        ("eleven", 11),
        ("ten", 10),
        ("nine", 9),
        ("eight", 8),
        ("seven", 7),
        ("six", 6),
        ("five", 5),
        ("four", 4),
        ("three", 3),
        ("two", 2),
        ("one", 1),
        ("an", 1),
        ("a", 1),
    ];
    let lower = text.to_lowercase();
    for &(word, val) in words {
        if lower.starts_with(word) {
            let rest = &text[word.len()..];
            // "a" and "an" must be followed by space or end
            if word.len() <= 2 && !rest.starts_with(|c: char| c.is_whitespace()) && !rest.is_empty()
            {
                continue;
            }
            return Some((val, rest.trim_start()));
        }
    }
    // "X" → 0 (caller should check for "X" and use DamageAmount::Variable where applicable)
    if lower.starts_with('x') {
        let rest = &text[1..];
        if rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace()) {
            return Some((0, rest.trim_start()));
        }
    }
    None
}

/// Parse an English ordinal number word at the start of text.
/// Returns (value, remaining_text) or None.
/// Handles "second" = 2, "third" = 3, "fourth" = 4, etc.
pub fn parse_ordinal(text: &str) -> Option<(u32, &str)> {
    let text = text.trim_start();
    let ordinals: &[(&str, u32)] = &[
        ("twentieth", 20),
        ("nineteenth", 19),
        ("eighteenth", 18),
        ("seventeenth", 17),
        ("sixteenth", 16),
        ("fifteenth", 15),
        ("fourteenth", 14),
        ("thirteenth", 13),
        ("twelfth", 12),
        ("eleventh", 11),
        ("tenth", 10),
        ("ninth", 9),
        ("eighth", 8),
        ("seventh", 7),
        ("sixth", 6),
        ("fifth", 5),
        ("fourth", 4),
        ("third", 3),
        ("second", 2),
        ("first", 1),
    ];
    let lower = text.to_lowercase();
    for &(word, val) in ordinals {
        if lower.starts_with(word) {
            let rest = &text[word.len()..];
            return Some((val, rest.trim_start()));
        }
    }
    None
}

/// Parse mana symbols like `{2}{W}{U}` at the start of text.
/// Returns (ManaCost, remaining_text) or None.
pub fn parse_mana_symbols(text: &str) -> Option<(ManaCost, &str)> {
    let text = text.trim_start();
    if !text.starts_with('{') {
        return None;
    }

    let mut generic: u32 = 0;
    let mut shards = Vec::new();
    let mut pos = 0;
    let mut parsed_any = false;

    while pos < text.len() && text[pos..].starts_with('{') {
        let end = text[pos..].find('}')? + pos;
        let symbol = &text[pos + 1..end];
        pos = end + 1;
        parsed_any = true;

        match symbol {
            "W" => shards.push(ManaCostShard::White),
            "U" => shards.push(ManaCostShard::Blue),
            "B" => shards.push(ManaCostShard::Black),
            "R" => shards.push(ManaCostShard::Red),
            "G" => shards.push(ManaCostShard::Green),
            "C" => shards.push(ManaCostShard::Colorless),
            "S" => shards.push(ManaCostShard::Snow),
            "X" => shards.push(ManaCostShard::X),
            "W/U" => shards.push(ManaCostShard::WhiteBlue),
            "W/B" => shards.push(ManaCostShard::WhiteBlack),
            "U/B" => shards.push(ManaCostShard::BlueBlack),
            "U/R" => shards.push(ManaCostShard::BlueRed),
            "B/R" => shards.push(ManaCostShard::BlackRed),
            "B/G" => shards.push(ManaCostShard::BlackGreen),
            "R/W" => shards.push(ManaCostShard::RedWhite),
            "R/G" => shards.push(ManaCostShard::RedGreen),
            "G/W" => shards.push(ManaCostShard::GreenWhite),
            "G/U" => shards.push(ManaCostShard::GreenBlue),
            "2/W" => shards.push(ManaCostShard::TwoWhite),
            "2/U" => shards.push(ManaCostShard::TwoBlue),
            "2/B" => shards.push(ManaCostShard::TwoBlack),
            "2/R" => shards.push(ManaCostShard::TwoRed),
            "2/G" => shards.push(ManaCostShard::TwoGreen),
            "W/P" => shards.push(ManaCostShard::PhyrexianWhite),
            "U/P" => shards.push(ManaCostShard::PhyrexianBlue),
            "B/P" => shards.push(ManaCostShard::PhyrexianBlack),
            "R/P" => shards.push(ManaCostShard::PhyrexianRed),
            "G/P" => shards.push(ManaCostShard::PhyrexianGreen),
            other => {
                if let Ok(n) = other.parse::<u32>() {
                    generic += n;
                } else {
                    // Unknown symbol — stop parsing
                    pos = pos - symbol.len() - 2; // rewind
                    break;
                }
            }
        }
    }

    if !parsed_any {
        return None;
    }

    let cost = ManaCost::Cost { shards, generic };
    Some((cost, &text[pos..]))
}

/// Possessive variants used in MTG Oracle text ("your library", "their hand", etc.).
const POSSESSIVES: &[&str] = &["your", "their", "its owner's", "that player's"];

/// Object pronouns in MTG Oracle text that refer to previously-mentioned objects.
/// Used in anaphoric references like "shuffle it into", "put them onto", "exile that card".
pub const OBJECT_PRONOUNS: &[&str] = &["it", "them", "that card", "those cards"];

/// Test whether `text` matches `"{prefix} {word} {suffix}"` for any word in `variants`,
/// using the given match strategy.
fn match_phrase_variants(
    text: &str,
    prefix: &str,
    suffix: &str,
    variants: &[&str],
    strategy: fn(&str, &str) -> bool,
) -> bool {
    variants.iter().any(|word| {
        let mut needle = String::with_capacity(prefix.len() + word.len() + suffix.len() + 2);
        needle.push_str(prefix);
        needle.push(' ');
        needle.push_str(word);
        needle.push(' ');
        needle.push_str(suffix);
        strategy(text, &needle)
    })
}

/// Check if `text` contains `"{prefix} {possessive} {suffix}"` for any possessive variant.
///
/// Useful for matching zone references like "into your hand" / "into their hand" without
/// enumerating every possessive form at each call site.
pub fn contains_possessive(text: &str, prefix: &str, suffix: &str) -> bool {
    match_phrase_variants(text, prefix, suffix, POSSESSIVES, |hay, needle| {
        hay.contains(needle)
    })
}

/// Like `contains_possessive`, but checks if `text` starts with the phrase.
pub fn starts_with_possessive(text: &str, prefix: &str, suffix: &str) -> bool {
    match_phrase_variants(text, prefix, suffix, POSSESSIVES, |hay, needle| {
        hay.starts_with(needle)
    })
}

/// Check if `text` contains `"{prefix} {pronoun} {suffix}"` for any object pronoun variant.
///
/// Matches anaphoric references like "shuffle it into", "put them onto", "exile that card from".
pub fn contains_object_pronoun(text: &str, prefix: &str, suffix: &str) -> bool {
    match_phrase_variants(text, prefix, suffix, OBJECT_PRONOUNS, |hay, needle| {
        hay.contains(needle)
    })
}

/// Parse mana production symbols like `{G}` into Vec<ManaColor>.
pub fn parse_mana_production(text: &str) -> Option<(Vec<ManaColor>, &str)> {
    let text = text.trim_start();
    if !text.starts_with('{') {
        return None;
    }

    let mut colors = Vec::new();
    let mut pos = 0;

    while pos < text.len() && text[pos..].starts_with('{') {
        let end = match text[pos..].find('}') {
            Some(e) => e + pos,
            None => break,
        };
        let symbol = &text[pos + 1..end];
        pos = end + 1;

        match symbol {
            "W" => colors.push(ManaColor::White),
            "U" => colors.push(ManaColor::Blue),
            "B" => colors.push(ManaColor::Black),
            "R" => colors.push(ManaColor::Red),
            "G" => colors.push(ManaColor::Green),
            _ => {
                pos = pos - symbol.len() - 2;
                break;
            }
        }
    }

    if colors.is_empty() {
        return None;
    }
    Some((colors, &text[pos..]))
}

/// Merge two filters into an Or, flattening nested Or branches.
pub fn merge_or_filters(a: TargetFilter, b: TargetFilter) -> TargetFilter {
    let mut filters = Vec::new();
    match a {
        TargetFilter::Or { filters: af } => filters.extend(af),
        other => filters.push(other),
    }
    match b {
        TargetFilter::Or { filters: bf } => filters.extend(bf),
        other => filters.push(other),
    }
    TargetFilter::Or { filters }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_number_digits() {
        assert_eq!(parse_number("3 damage"), Some((3, "damage")));
        assert_eq!(parse_number("10 life"), Some((10, "life")));
    }

    #[test]
    fn parse_number_words() {
        assert_eq!(parse_number("two cards"), Some((2, "cards")));
        assert_eq!(parse_number("a card"), Some((1, "card")));
        assert_eq!(parse_number("an opponent"), Some((1, "opponent")));
        assert_eq!(parse_number("three"), Some((3, "")));
    }

    #[test]
    fn parse_number_a_not_greedy() {
        // "a" should not match inside "attacking"
        assert_eq!(parse_number("attacking"), None);
        assert_eq!(parse_number("another"), None);
    }

    #[test]
    fn parse_number_none() {
        assert_eq!(parse_number("target creature"), None);
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn strip_reminder_text_basic() {
        assert_eq!(
            strip_reminder_text(
                "Flying (This creature can't be blocked except by creatures with flying.)"
            ),
            "Flying"
        );
    }

    #[test]
    fn strip_reminder_text_nested() {
        assert_eq!(
            strip_reminder_text("Ward {1} (Whenever this becomes the target)"),
            "Ward {1}"
        );
    }

    #[test]
    fn strip_reminder_text_no_parens() {
        assert_eq!(
            strip_reminder_text("Destroy target creature."),
            "Destroy target creature."
        );
    }

    #[test]
    fn self_ref_replaces_tilde() {
        assert_eq!(
            self_ref("~ deals 3 damage", "Lightning Bolt"),
            "Lightning Bolt deals 3 damage"
        );
    }

    #[test]
    fn parse_mana_symbols_basic() {
        let (cost, rest) = parse_mana_symbols("{2}{W}").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::White]
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_mana_symbols_hybrid() {
        let (cost, _) = parse_mana_symbols("{G/W}").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::GreenWhite]
            }
        );
    }

    #[test]
    fn parse_mana_symbols_zero() {
        let (cost, rest) = parse_mana_symbols("{0}").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                generic: 0,
                shards: vec![],
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_mana_production_basic() {
        let (colors, _) = parse_mana_production("{G}").unwrap();
        assert_eq!(colors, vec![ManaColor::Green]);
    }

    #[test]
    fn parse_mana_production_multi() {
        let (colors, _) = parse_mana_production("{W}{W}").unwrap();
        assert_eq!(colors, vec![ManaColor::White, ManaColor::White]);
    }

    #[test]
    fn contains_possessive_matches_all_variants() {
        assert!(contains_possessive("into your hand", "into", "hand"));
        assert!(contains_possessive("into their hand", "into", "hand"));
        assert!(contains_possessive("into its owner's hand", "into", "hand"));
        assert!(contains_possessive(
            "into that player's hand",
            "into",
            "hand"
        ));
        assert!(!contains_possessive("into a hand", "into", "hand"));
    }

    #[test]
    fn starts_with_possessive_checks_prefix() {
        assert!(starts_with_possessive(
            "search your library for a card",
            "search",
            "library"
        ));
        assert!(starts_with_possessive(
            "search their library for a card",
            "search",
            "library"
        ));
        assert!(!starts_with_possessive(
            "then search your library",
            "search",
            "library"
        ));
    }

    #[test]
    fn contains_object_pronoun_matches_variants() {
        assert!(contains_object_pronoun(
            "shuffle it into",
            "shuffle",
            "into"
        ));
        assert!(contains_object_pronoun(
            "shuffle them into",
            "shuffle",
            "into"
        ));
        assert!(contains_object_pronoun(
            "shuffle that card into",
            "shuffle",
            "into"
        ));
        assert!(contains_object_pronoun(
            "put those cards onto the battlefield",
            "put",
            "onto"
        ));
        assert!(!contains_object_pronoun(
            "shuffle your into",
            "shuffle",
            "into"
        ));
    }
}
