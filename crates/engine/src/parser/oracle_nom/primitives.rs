//! Atomic parsing combinators for numbers, mana symbols, colors, counters, and P/T modifiers.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::{char, digit1, space0};
use nom::combinator::{map, map_res, opt, value};
use nom::multi::many1;
use nom::sequence::{delimited, preceded};
use nom::Parser;

use super::error::OracleResult;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};

/// Parse a number from Oracle text: digit string OR English words (one through twenty).
///
/// Mirrors `oracle_util::parse_number` but as a nom combinator.
pub fn parse_number(input: &str) -> OracleResult<'_, u32> {
    alt((parse_digit_number, parse_english_number)).parse(input)
}

/// Parse one or more ASCII digits into a u32.
fn parse_digit_number(input: &str) -> OracleResult<'_, u32> {
    map_res(digit1, |s: &str| s.parse::<u32>()).parse(input)
}

/// Parse an English number word (one through twenty, plus "a"/"an").
///
/// "a"/"an" require a word boundary after the match (whitespace, punctuation, or
/// end-of-input) to prevent false matches on words like "another" or "anyone".
fn parse_english_number(input: &str) -> OracleResult<'_, u32> {
    // Longest-match-first ordering within shared prefixes (e.g. "fourteen" before "four").
    // Split into two alt groups to stay within nom's 21-element tuple limit.
    alt((
        value(20u32, tag("twenty")),
        value(19, tag("nineteen")),
        value(18, tag("eighteen")),
        value(17, tag("seventeen")),
        value(16, tag("sixteen")),
        value(15, tag("fifteen")),
        value(14, tag("fourteen")),
        value(13, tag("thirteen")),
        value(12, tag("twelve")),
        value(11, tag("eleven")),
        value(10, tag("ten")),
    ))
    .or(alt((
        value(9u32, tag("nine")),
        value(8, tag("eight")),
        value(7, tag("seven")),
        value(6, tag("six")),
        value(5, tag("five")),
        value(4, tag("four")),
        value(3, tag("three")),
        value(2, tag("two")),
        value(1, tag("one")),
        parse_article_number,
    )))
    .parse(input)
}

/// Parse "a" or "an" as 1, requiring a word boundary after the match.
///
/// Prevents false matches where "a" greedily consumes the start of words like
/// "another", "anyone", "among". The boundary check requires the next character
/// to be non-alphanumeric (whitespace, punctuation) or end-of-input.
fn parse_article_number(input: &str) -> OracleResult<'_, u32> {
    // Try "an" before "a" (longest match first).
    let (rest, _) = alt((tag("an"), tag("a"))).parse(input)?;
    match rest.chars().next() {
        None | Some(' ' | ',' | ';' | '.' | ':' | ')' | '/' | '-' | '\'' | '"') => Ok((rest, 1)),
        _ => Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Context("article requires word boundary"),
            )],
        })),
    }
}

/// Parse a number OR "x" (as 0). Use for costs, P/T, counter amounts where
/// X represents a variable that resolves to 0 at parse time.
///
/// For effect quantities where X should remain as `Variable("X")`, use
/// [`parse_number`] instead — it does not match "x".
pub fn parse_number_or_x(input: &str) -> OracleResult<'_, u32> {
    alt((parse_number, value(0u32, tag("x")))).parse(input)
}

/// Parse a single mana symbol: `{W}`, `{U}`, `{B}`, `{R}`, `{G}`, `{C}`, `{S}`, `{X}`,
/// hybrid symbols `{W/U}`, phyrexian `{W/P}`, two-generic hybrid `{2/W}`,
/// or generic `{N}` (digit inside braces).
pub fn parse_mana_symbol(input: &str) -> OracleResult<'_, ManaCostShard> {
    delimited(char('{'), parse_mana_symbol_inner, char('}')).parse(input)
}

/// Parse the inner content of a mana symbol (between `{` and `}`).
fn parse_mana_symbol_inner(input: &str) -> OracleResult<'_, ManaCostShard> {
    alt((
        // Hybrid symbols (longest match first)
        value(ManaCostShard::WhiteBlue, tag("W/U")),
        value(ManaCostShard::WhiteBlack, tag("W/B")),
        value(ManaCostShard::PhyrexianWhite, tag("W/P")),
        value(ManaCostShard::BlueBlack, tag("U/B")),
        value(ManaCostShard::BlueRed, tag("U/R")),
        value(ManaCostShard::PhyrexianBlue, tag("U/P")),
        value(ManaCostShard::BlackRed, tag("B/R")),
        value(ManaCostShard::BlackGreen, tag("B/G")),
        value(ManaCostShard::PhyrexianBlack, tag("B/P")),
        value(ManaCostShard::RedWhite, tag("R/W")),
        value(ManaCostShard::RedGreen, tag("R/G")),
        value(ManaCostShard::PhyrexianRed, tag("R/P")),
        value(ManaCostShard::GreenWhite, tag("G/W")),
        value(ManaCostShard::GreenBlue, tag("G/U")),
        value(ManaCostShard::PhyrexianGreen, tag("G/P")),
        value(ManaCostShard::TwoWhite, tag("2/W")),
        value(ManaCostShard::TwoBlue, tag("2/U")),
        value(ManaCostShard::TwoBlack, tag("2/B")),
        value(ManaCostShard::TwoRed, tag("2/R")),
        value(ManaCostShard::TwoGreen, tag("2/G")),
        // Basic colored and special
        value(ManaCostShard::White, tag("W")),
    ))
    .or(alt((
        value(ManaCostShard::Blue, tag("U")),
        value(ManaCostShard::Black, tag("B")),
        value(ManaCostShard::Red, tag("R")),
        value(ManaCostShard::Green, tag("G")),
        value(ManaCostShard::Colorless, tag("C")),
        value(ManaCostShard::Snow, tag("S")),
        value(ManaCostShard::X, tag("X")),
        // Generic mana: digit(s) inside braces → Colorless placeholder
        // Note: generic mana is accumulated as u32 in ManaCost, not as shards.
        // For the symbol-level combinator we return Colorless as a sentinel;
        // parse_mana_cost handles proper generic accumulation.
        map_res(digit1, |_s: &str| -> Result<ManaCostShard, &str> {
            Ok(ManaCostShard::Colorless)
        }),
    )))
    .parse(input)
}

/// Parse a sequence of mana symbols into a `ManaCost`.
///
/// Handles generic mana accumulation: `{1}{W}{U}` -> ManaCost::Cost { shards: [W, U], generic: 1 }.
pub fn parse_mana_cost(input: &str) -> OracleResult<'_, ManaCost> {
    let (rest, symbols) = many1(parse_mana_cost_element).parse(input)?;
    let mut shards = Vec::new();
    let mut generic = 0u32;
    for elem in symbols {
        match elem {
            ManaElement::Shard(s) => shards.push(s),
            ManaElement::Generic(n) => generic += n,
        }
    }
    Ok((rest, ManaCost::Cost { shards, generic }))
}

/// Internal enum to distinguish shards from generic mana during accumulation.
enum ManaElement {
    Shard(ManaCostShard),
    Generic(u32),
}

/// Parse a single mana cost element (shard or generic number).
fn parse_mana_cost_element(input: &str) -> OracleResult<'_, ManaElement> {
    delimited(char('{'), parse_mana_cost_inner, char('}')).parse(input)
}

/// Parse inner mana cost element, properly distinguishing generic numbers from shards.
fn parse_mana_cost_inner(input: &str) -> OracleResult<'_, ManaElement> {
    alt((
        // Try generic number first (before shard parsing eats digits)
        map(
            map_res(digit1, |s: &str| s.parse::<u32>()),
            ManaElement::Generic,
        ),
        map(parse_mana_symbol_inner, ManaElement::Shard),
    ))
    .parse(input)
}

/// Parse a color word: "white", "blue", "black", "red", "green".
pub fn parse_color(input: &str) -> OracleResult<'_, ManaColor> {
    alt((
        value(ManaColor::White, tag("white")),
        value(ManaColor::Blue, tag("blue")),
        value(ManaColor::Black, tag("black")),
        value(ManaColor::Red, tag("red")),
        value(ManaColor::Green, tag("green")),
    ))
    .parse(input)
}

/// Parse a counter type: "+1/+1", "-1/-1", or a named counter type.
pub fn parse_counter_type(input: &str) -> OracleResult<'_, String> {
    alt((
        map(tag("+1/+1"), |_| "+1/+1".to_string()),
        map(tag("-1/-1"), |_| "-1/-1".to_string()),
        parse_named_counter_type,
    ))
    .parse(input)
}

/// Parse a named counter type: "loyalty", "charge", "lore", "defense", etc.
fn parse_named_counter_type(input: &str) -> OracleResult<'_, String> {
    alt((
        map(tag("loyalty"), |s: &str| s.to_string()),
        map(tag("charge"), |s: &str| s.to_string()),
        map(tag("lore"), |s: &str| s.to_string()),
        map(tag("defense"), |s: &str| s.to_string()),
        map(tag("time"), |s: &str| s.to_string()),
        map(tag("quest"), |s: &str| s.to_string()),
        map(tag("energy"), |s: &str| s.to_string()),
        map(tag("valor"), |s: &str| s.to_string()),
        map(tag("verse"), |s: &str| s.to_string()),
        map(tag("level"), |s: &str| s.to_string()),
        map(tag("vitality"), |s: &str| s.to_string()),
        map(tag("vigilance"), |s: &str| s.to_string()),
        map(tag("bounty"), |s: &str| s.to_string()),
    ))
    .parse(input)
}

/// Parse a P/T modifier: "+N/+M", "-N/-M", or mixed signs like "+N/-M".
pub fn parse_pt_modifier(input: &str) -> OracleResult<'_, (i32, i32)> {
    (parse_signed_number, char('/'), parse_signed_number)
        .map(|(power, _, toughness)| (power, toughness))
        .parse(input)
}

/// Parse a signed integer: "+N" or "-N".
fn parse_signed_number(input: &str) -> OracleResult<'_, i32> {
    alt((
        preceded(char('+'), map(parse_digit_number, |n| n as i32)),
        preceded(char('-'), map(parse_digit_number, |n| -(n as i32))),
    ))
    .parse(input)
}

/// Parse a roman numeral (I through XX) from Oracle text.
///
/// Consumes one or more roman numeral characters (I, V, X) and converts to u32.
/// Used by saga chapter headers and class level parsing.
pub fn parse_roman_numeral(input: &str) -> OracleResult<'_, u32> {
    let end = input
        .find(|c: char| !matches!(c.to_ascii_uppercase(), 'I' | 'V' | 'X'))
        .unwrap_or(input.len());
    if end == 0 {
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Context("roman numeral"),
            )],
        }));
    }
    let roman_str = &input[..end];
    let upper = roman_str.to_uppercase();
    let mut total: u32 = 0;
    let mut prev = 0u32;
    for ch in upper.chars().rev() {
        let val = match ch {
            'I' => 1,
            'V' => 5,
            'X' => 10,
            _ => {
                return Err(nom::Err::Error(nom_language::error::VerboseError {
                    errors: vec![(
                        input,
                        nom_language::error::VerboseErrorKind::Context("roman numeral"),
                    )],
                }));
            }
        };
        if val < prev {
            total = match total.checked_sub(val) {
                Some(t) => t,
                None => {
                    return Err(nom::Err::Error(nom_language::error::VerboseError {
                        errors: vec![(
                            input,
                            nom_language::error::VerboseErrorKind::Context("roman numeral"),
                        )],
                    }));
                }
            };
        } else {
            total += val;
        }
        prev = val;
    }
    if total == 0 {
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Context("roman numeral"),
            )],
        }));
    }
    Ok((&input[end..], total))
}

/// Parse optional whitespace, consuming zero or more spaces/tabs.
pub fn ws(input: &str) -> OracleResult<'_, &str> {
    space0.parse(input)
}

/// Parse optional whitespace then a specific tag.
pub fn ws_tag(
    t: &str,
) -> impl Parser<&str, Output = &str, Error = nom_language::error::VerboseError<&str>> {
    preceded(opt(space0), tag(t))
}

/// Parse an evergreen keyword name from Oracle text.
///
/// Uses a table lookup (longest-match-first within shared prefixes) to avoid
/// deep nom `alt` nesting which causes stack overflow in debug builds.
/// Returns the keyword string as matched (lowercase).
pub fn parse_keyword_name(input: &str) -> OracleResult<'_, &str> {
    // Longest-match-first within shared prefixes (e.g. "first strike" before "flash").
    static KEYWORDS: &[&str] = &[
        "first strike",
        "double strike",
        "trample over planeswalkers",
        "trample",
        "flying",
        "deathtouch",
        "lifelink",
        "vigilance",
        "haste",
        "reach",
        "defender",
        "menace",
        "indestructible",
        "hexproof",
        "shroud",
        "flash",
        "fear",
        "intimidate",
        "skulk",
        "shadow",
        "horsemanship",
        "wither",
        "infect",
        "prowess",
        "undying",
        "persist",
        "cascade",
        "exalted",
        "flanking",
        "evolve",
        "extort",
        "exploit",
        "explore",
        "ascend",
        "convoke",
        "delve",
        "devoid",
        "changeling",
        "phasing",
        "decayed",
        "unleash",
        "riot",
        "ward",
        "protection",
        "landwalk",
        "islandwalk",
        "swampwalk",
        "mountainwalk",
        "forestwalk",
        "plainswalk",
    ];

    for &kw in KEYWORDS {
        if let Some(rest) = input.strip_prefix(kw) {
            // Require word boundary after keyword
            match rest.chars().next() {
                None | Some(' ' | ',' | ';' | '.' | ':' | ')' | '/' | '\'' | '"' | '\n') => {
                    return Ok((rest, kw));
                }
                _ => continue,
            }
        }
    }

    Err(nom::Err::Error(nom_language::error::VerboseError {
        errors: vec![(
            input,
            nom_language::error::VerboseErrorKind::Context("keyword name"),
        )],
    }))
}

/// Parse an imperative verb from Oracle text.
///
/// Matches common Oracle text action verbs: "destroy", "exile", "draw",
/// "create", "sacrifice", "discard", "return", "put", "counter", "gain",
/// "lose", "deal", "tap", "untap", "search", "shuffle", "reveal", "mill",
/// "scry", "surveil", "fight".
/// Returns the matched verb as a string slice.
pub fn parse_verb(input: &str) -> OracleResult<'_, &str> {
    static VERBS: &[&str] = &[
        // Longest-match-first within shared prefixes
        "destroys",
        "destroy",
        "exiles",
        "exile",
        "draws",
        "draw",
        "creates",
        "create",
        "sacrifices",
        "sacrifice",
        "discards",
        "discard",
        "returns",
        "return",
        "puts",
        "put",
        "counters",
        "counter",
        "gains",
        "gain",
        "loses",
        "lose",
        "deals",
        "deal",
        "taps",
        "tap",
        "untaps",
        "untap",
        "searches",
        "search",
        "shuffles",
        "shuffle",
        "reveals",
        "reveal",
        "mills",
        "mill",
        "scry",
        "surveil",
        "fights",
        "fight",
        "prevents",
        "prevent",
        "regenerate",
        "attach",
        "detach",
        "transform",
        "investigate",
        "populate",
        "proliferate",
        "bolster",
        "explore",
        "adapt",
    ];

    for &verb in VERBS {
        if let Some(rest) = input.strip_prefix(verb) {
            // Require word boundary after verb
            match rest.chars().next() {
                None | Some(' ' | ',' | ';' | '.' | ':' | ')' | '/' | '\'' | '"' | '\n') => {
                    return Ok((rest, verb));
                }
                _ => continue,
            }
        }
    }

    Err(nom::Err::Error(nom_language::error::VerboseError {
        errors: vec![(
            input,
            nom_language::error::VerboseErrorKind::Context("verb"),
        )],
    }))
}

/// Parse common Oracle phrase fragments.
///
/// Matches "you may", "choose one", "choose two", "up to", "each",
/// "each player", "each opponent", "target player".
pub fn parse_phrase_fragment(input: &str) -> OracleResult<'_, &str> {
    static FRAGMENTS: &[&str] = &[
        "you may",
        "choose one",
        "choose two",
        "choose three",
        "choose one or more",
        "choose one or both",
        "up to",
        "each player",
        "each opponent",
        "target player",
        "target opponent",
        "any target",
    ];

    for &frag in FRAGMENTS {
        if let Some(rest) = input.strip_prefix(frag) {
            return Ok((rest, frag));
        }
    }

    Err(nom::Err::Error(nom_language::error::VerboseError {
        errors: vec![(
            input,
            nom_language::error::VerboseErrorKind::Context("phrase fragment"),
        )],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_number_digits() {
        let (rest, n) = parse_number("42 damage").unwrap();
        assert_eq!(n, 42);
        assert_eq!(rest, " damage");
    }

    #[test]
    fn test_parse_number_english() {
        let (rest, n) = parse_number("three creatures").unwrap();
        assert_eq!(n, 3);
        assert_eq!(rest, " creatures");
    }

    #[test]
    fn test_parse_number_a_word_boundary() {
        // "a" followed by space → matches as 1
        let (rest, n) = parse_number("a creature").unwrap();
        assert_eq!(n, 1);
        assert_eq!(rest, " creature");

        // "another" → must NOT match "a" from "another"
        assert!(parse_number("another").is_err());
        assert!(parse_number("anyone").is_err());
        assert!(parse_number("among").is_err());

        // "an" followed by space → matches as 1
        let (rest2, n2) = parse_number("an artifact").unwrap();
        assert_eq!(n2, 1);
        assert_eq!(rest2, " artifact");

        // "a" at end of input → matches as 1
        let (rest3, n3) = parse_number("a").unwrap();
        assert_eq!(n3, 1);
        assert_eq!(rest3, "");
    }

    #[test]
    fn test_parse_number_or_x() {
        // Digits and English words still work
        let (rest, n) = parse_number_or_x("3 damage").unwrap();
        assert_eq!(n, 3);
        assert_eq!(rest, " damage");

        let (rest2, n2) = parse_number_or_x("five counters").unwrap();
        assert_eq!(n2, 5);
        assert_eq!(rest2, " counters");

        // "x" → 0
        let (rest3, n3) = parse_number_or_x("x +1/+1 counters").unwrap();
        assert_eq!(n3, 0);
        assert_eq!(rest3, " +1/+1 counters");

        // plain parse_number does NOT match "x"
        assert!(parse_number("x damage").is_err());
    }

    #[test]
    fn test_parse_number_failure() {
        assert!(parse_number("xyz").is_err());
    }

    #[test]
    fn test_parse_mana_symbol_basic() {
        let (rest, shard) = parse_mana_symbol("{W}").unwrap();
        assert_eq!(shard, ManaCostShard::White);
        assert_eq!(rest, "");

        let (rest2, shard2) = parse_mana_symbol("{U}").unwrap();
        assert_eq!(shard2, ManaCostShard::Blue);
        assert_eq!(rest2, "");
    }

    #[test]
    fn test_parse_mana_symbol_hybrid() {
        let (rest, shard) = parse_mana_symbol("{W/U}").unwrap();
        assert_eq!(shard, ManaCostShard::WhiteBlue);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_mana_symbol_phyrexian() {
        let (rest, shard) = parse_mana_symbol("{R/P}").unwrap();
        assert_eq!(shard, ManaCostShard::PhyrexianRed);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_mana_cost_mixed() {
        let (rest, cost) = parse_mana_cost("{2}{W}{U}").unwrap();
        assert_eq!(rest, "");
        match cost {
            ManaCost::Cost { shards, generic } => {
                assert_eq!(generic, 2);
                assert_eq!(shards, vec![ManaCostShard::White, ManaCostShard::Blue]);
            }
            _ => panic!("expected Cost variant"),
        }
    }

    #[test]
    fn test_parse_mana_cost_no_braces() {
        assert!(parse_mana_cost("WUB").is_err());
    }

    #[test]
    fn test_parse_color() {
        let (rest, c) = parse_color("white mana").unwrap();
        assert_eq!(c, ManaColor::White);
        assert_eq!(rest, " mana");

        let (rest2, c2) = parse_color("blue").unwrap();
        assert_eq!(c2, ManaColor::Blue);
        assert_eq!(rest2, "");
    }

    #[test]
    fn test_parse_color_failure() {
        assert!(parse_color("purple").is_err());
    }

    #[test]
    fn test_parse_counter_type_plus() {
        let (rest, ct) = parse_counter_type("+1/+1 counter").unwrap();
        assert_eq!(ct, "+1/+1");
        assert_eq!(rest, " counter");
    }

    #[test]
    fn test_parse_counter_type_named() {
        let (rest, ct) = parse_counter_type("loyalty counters").unwrap();
        assert_eq!(ct, "loyalty");
        assert_eq!(rest, " counters");
    }

    #[test]
    fn test_parse_counter_type_failure() {
        assert!(parse_counter_type("unknown_counter").is_err());
    }

    #[test]
    fn test_parse_pt_modifier_positive() {
        let (rest, (p, t)) = parse_pt_modifier("+2/+3 until").unwrap();
        assert_eq!(p, 2);
        assert_eq!(t, 3);
        assert_eq!(rest, " until");
    }

    #[test]
    fn test_parse_pt_modifier_negative() {
        let (rest, (p, t)) = parse_pt_modifier("-1/-1").unwrap();
        assert_eq!(p, -1);
        assert_eq!(t, -1);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_pt_modifier_mixed() {
        let (rest, (p, t)) = parse_pt_modifier("+3/-2").unwrap();
        assert_eq!(p, 3);
        assert_eq!(t, -2);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_pt_modifier_failure() {
        assert!(parse_pt_modifier("3/2").is_err());
    }

    #[test]
    fn test_parse_roman_numeral_range() {
        assert_eq!(parse_roman_numeral("I — ").unwrap(), (" — ", 1));
        assert_eq!(parse_roman_numeral("ii,").unwrap(), (",", 2));
        assert_eq!(parse_roman_numeral("III — ").unwrap(), (" — ", 3));
        assert_eq!(parse_roman_numeral("IV,").unwrap(), (",", 4));
        assert_eq!(parse_roman_numeral("V — ").unwrap(), (" — ", 5));
        assert_eq!(parse_roman_numeral("X — ").unwrap(), (" — ", 10));
        assert_eq!(parse_roman_numeral("XIV,").unwrap(), (",", 14));
        assert_eq!(parse_roman_numeral("XX").unwrap(), ("", 20));
    }

    #[test]
    fn test_parse_roman_numeral_failure() {
        assert!(parse_roman_numeral("ABC").is_err());
        assert!(parse_roman_numeral("").is_err());
    }

    #[test]
    fn test_parse_keyword_name_basic() {
        let (rest, kw) = parse_keyword_name("flying creature").unwrap();
        assert_eq!(kw, "flying");
        assert_eq!(rest, " creature");

        let (rest2, kw2) = parse_keyword_name("first strike, deathtouch").unwrap();
        assert_eq!(kw2, "first strike");
        assert_eq!(rest2, ", deathtouch");

        let (rest3, kw3) = parse_keyword_name("trample over planeswalkers").unwrap();
        assert_eq!(kw3, "trample over planeswalkers");
        assert_eq!(rest3, "");
    }

    #[test]
    fn test_parse_keyword_name_word_boundary() {
        // "flashback" should NOT match as "flash"
        assert!(parse_keyword_name("flashback").is_err());
        // "defender" at end of input → ok
        let (rest, kw) = parse_keyword_name("defender").unwrap();
        assert_eq!(kw, "defender");
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_verb_basic() {
        let (rest, v) = parse_verb("destroy target").unwrap();
        assert_eq!(v, "destroy");
        assert_eq!(rest, " target");

        let (rest2, v2) = parse_verb("draws a card").unwrap();
        assert_eq!(v2, "draws");
        assert_eq!(rest2, " a card");

        let (rest3, v3) = parse_verb("exile it").unwrap();
        assert_eq!(v3, "exile");
        assert_eq!(rest3, " it");
    }

    #[test]
    fn test_parse_verb_word_boundary() {
        // "created" should NOT match "create" (word boundary)
        assert!(parse_verb("created").is_err());
        // "sacrifice" at end of input → ok
        let (rest, v) = parse_verb("sacrifice.").unwrap();
        assert_eq!(v, "sacrifice");
        assert_eq!(rest, ".");
    }

    #[test]
    fn test_parse_phrase_fragment() {
        let (rest, f) = parse_phrase_fragment("you may draw").unwrap();
        assert_eq!(f, "you may");
        assert_eq!(rest, " draw");

        let (rest2, f2) = parse_phrase_fragment("each opponent loses").unwrap();
        assert_eq!(f2, "each opponent");
        assert_eq!(rest2, " loses");
    }
}
