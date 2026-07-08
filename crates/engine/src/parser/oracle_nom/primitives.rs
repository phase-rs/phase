//! Atomic parsing combinators for numbers, mana symbols, colors, counters, and P/T modifiers.

use std::borrow::Cow;

use nom::branch::alt;
use nom::bytes::complete::{tag, take_till1, take_until, take_while_m_n};
use nom::character::complete::{char, digit1, space0};
use nom::combinator::{all_consuming, map, map_res, not, opt, peek, recognize, value};
use nom::multi::{many0, many1};
use nom::sequence::{delimited, preceded};
use nom::Parser;

use super::error::{OracleError, OracleResult};
use crate::types::ability::PtValue;
use crate::types::counter::{CounterType, KEYWORD_COUNTERS};
use crate::types::keywords::KeywordKind;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};

/// Parse a number from Oracle text: digit string OR English words (one through twenty).
///
/// Mirrors `oracle_util::parse_number` but as a nom combinator.
pub fn parse_number(input: &str) -> OracleResult<'_, u32> {
    alt((parse_digit_number, parse_english_number)).parse(input)
}

/// Parse one or more ASCII digits into a u32, accepting English
/// thousands-separator commas ("1,000", "1,000,000").
///
/// A comma is consumed only when it is followed by exactly three digits
/// and no further digit after them — i.e. `DDD(,DDD)*` with the extra
/// constraint that the group after a comma is exactly three digits.
/// This ensures safe behavior at clause boundaries:
///
/// - "1,000" → 1000
/// - "1,000,000" → 1000000
/// - "10," (e.g. "deal 10, then ...") → 10, remainder ","
/// - "1,50" (2-digit group) → 1, remainder ",50"
/// - "1,0000" (4-digit group) → 1, remainder ",0000"
///
/// CR 107.1: Magic numbers are integers; Oracle text conventionally
/// renders large constants with comma grouping (e.g. A Good Thing,
/// Jumbo Cactuar, The Millennium Calendar).
fn parse_digit_number(input: &str) -> OracleResult<'_, u32> {
    let (rest, matched) = recognize((
        digit1,
        many0((
            tag(","),
            take_while_m_n(3, 3, |c: char| c.is_ascii_digit()),
            // Reject a 4th trailing digit — "1,0000" must leave ",0000".
            peek(not(take_while_m_n(1, 1, |c: char| c.is_ascii_digit()))),
        )),
    ))
    .parse(input)?;
    // Strip commas before parsing.
    let digits: String = matched.chars().filter(|c| *c != ',').collect();
    match digits.parse::<u32>() {
        Ok(n) => Ok((rest, n)),
        Err(_) => Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        ))),
    }
}

/// Parse an English number word (one through one hundred, plus "a"/"an").
///
/// "a"/"an" require a word boundary after the match (whitespace, punctuation, or
/// end-of-input) to prevent false matches on words like "another" or "anyone".
///
/// Supports multiples of ten from thirty through ninety, hyphenated compounds
/// from twenty-one through ninety-nine, plus "one hundred".
fn parse_english_number(input: &str) -> OracleResult<'_, u32> {
    // Longest-match-first ordering within shared prefixes (e.g. "fourteen" before "four").
    // Split into multiple alt groups to stay within nom's 21-element tuple limit.
    let (rest, matched) = alt((
        value(100u32, tag("one hundred")),
        parse_hyphenated_english_number,
        parse_english_tens,
    ))
    .or(alt((
        value(19u32, tag("nineteen")),
        value(18, tag("eighteen")),
        value(17, tag("seventeen")),
        value(16, tag("sixteen")),
        value(15, tag("fifteen")),
        value(14, tag("fourteen")),
        value(13, tag("thirteen")),
        value(12, tag("twelve")),
        value(11, tag("eleven")),
        value(10, tag("ten")),
    )))
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
    .parse(input)?;

    // Require a word boundary after the number word so a cardinal isn't matched
    // inside a longer word ("sixth" → "six", "tenfold" → "ten", "nineteenth" →
    // "nineteen"). This mirrors the boundary guard `parse_article_number` already
    // applies to "a"/"an"; the multi-character number words previously lacked it
    // because the `oracle_util::parse_number` wrapper only guards matches of ≤2
    // characters. A following ASCII alphanumeric means the token continues, so
    // the match was a spurious substring.
    match rest.chars().next() {
        Some(c) if c.is_ascii_alphanumeric() => Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        ))),
        _ => Ok((rest, matched)),
    }
}

fn parse_english_tens(input: &str) -> OracleResult<'_, u32> {
    alt((
        value(90u32, tag("ninety")),
        value(80, tag("eighty")),
        value(70, tag("seventy")),
        value(60, tag("sixty")),
        value(50, tag("fifty")),
        value(40, tag("forty")),
        value(30, tag("thirty")),
        value(20, tag("twenty")),
    ))
    .parse(input)
}

fn parse_english_one_to_nine(input: &str) -> OracleResult<'_, u32> {
    alt((
        value(9u32, tag("nine")),
        value(8, tag("eight")),
        value(7, tag("seven")),
        value(6, tag("six")),
        value(5, tag("five")),
        value(4, tag("four")),
        value(3, tag("three")),
        value(2, tag("two")),
        value(1, tag("one")),
    ))
    .parse(input)
}

fn parse_hyphenated_english_number(input: &str) -> OracleResult<'_, u32> {
    map(
        (parse_english_tens, tag("-"), parse_english_one_to_nine),
        |(tens, _, ones)| tens + ones,
    )
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
        _ => Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        ))),
    }
}

/// Parse article "a " or "an " (including trailing space), returning `()`.
///
/// Word-boundary-safe because the trailing space acts as a boundary check.
/// Longest match first: "an " is tried before "a " to avoid partial matches.
pub fn parse_article(input: &str) -> OracleResult<'_, ()> {
    value((), alt((tag("an "), tag("a ")))).parse(input)
}

/// Parse a number OR "x" (as 0). Use for costs, P/T, counter amounts where
/// X represents a variable that resolves to 0 at parse time.
///
/// For effect quantities where X should remain as `Variable("X")`, use
/// [`parse_number`] instead — it does not match "x".
pub fn parse_number_or_x(input: &str) -> OracleResult<'_, u32> {
    alt((parse_number, value(0u32, tag("x")))).parse(input)
}

/// Parse a single mana symbol: `{W}`, `{U}`, `{B}`, `{R}`, `{G}`, `{C}`, `{S}`, `{X}`, `{Z}`,
/// hybrid symbols `{W/U}`, phyrexian `{W/P}`, two-generic hybrid `{2/W}`,
/// or generic `{N}` (digit inside braces).
pub fn parse_mana_symbol(input: &str) -> OracleResult<'_, ManaCostShard> {
    delimited(char('{'), parse_mana_symbol_inner, char('}')).parse(input)
}

/// Parse the inner content of a mana symbol (between `{` and `}`).
fn parse_mana_symbol_inner(input: &str) -> OracleResult<'_, ManaCostShard> {
    alt((
        // Phyrexian-hybrid symbols (longest match first). These 3-part `{C1/C2/P}`
        // symbols must be tried before the 2-part hybrid arms below — otherwise
        // `alt` matches the `W/U` prefix of `{W/U/P}` and the trailing `/P`
        // breaks the `}` delimiter, silently dropping the pip (issue #1416).
        parse_phyrexian_hybrid_symbol_inner,
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
        parse_two_generic_hybrid_symbol_inner,
        // Basic colored and special
        value(ManaCostShard::White, tag("W")),
    ))
    .or(alt((
        // CR 107.4e: Colorless-hybrid symbols. `{C/X}` may be paid with
        // one colorless mana or one mana of color X. The only printed
        // exemplar to date is Ulalek, Fused Atrocity (Foundations) whose
        // cost `{C/W}{C/U}{C/B}{C/R}{C/G}` parsed as empty without these
        // arms, making it freely castable (issue #493).
        value(ManaCostShard::ColorlessWhite, tag("C/W")),
        value(ManaCostShard::ColorlessBlue, tag("C/U")),
        value(ManaCostShard::ColorlessBlack, tag("C/B")),
        value(ManaCostShard::ColorlessRed, tag("C/R")),
        value(ManaCostShard::ColorlessGreen, tag("C/G")),
        value(ManaCostShard::Blue, tag("U")),
        value(ManaCostShard::Black, tag("B")),
        value(ManaCostShard::Red, tag("R")),
        value(ManaCostShard::Green, tag("G")),
        value(ManaCostShard::Colorless, tag("C")),
        value(ManaCostShard::Snow, tag("S")),
        value(ManaCostShard::X, tag("X")),
        value(ManaCostShard::TwoOrMoreColorSource, tag("Z")),
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
        map(parse_two_generic_hybrid_symbol_inner, ManaElement::Shard),
        map(
            map_res(digit1, |s: &str| s.parse::<u32>()),
            ManaElement::Generic,
        ),
        map(parse_mana_symbol_inner, ManaElement::Shard),
    ))
    .parse(input)
}

fn parse_two_generic_hybrid_symbol_inner(input: &str) -> OracleResult<'_, ManaCostShard> {
    alt((
        value(ManaCostShard::TwoWhite, tag("2/W")),
        value(ManaCostShard::TwoBlue, tag("2/U")),
        value(ManaCostShard::TwoBlack, tag("2/B")),
        value(ManaCostShard::TwoRed, tag("2/R")),
        value(ManaCostShard::TwoGreen, tag("2/G")),
    ))
    .parse(input)
}

/// CR 107.4f: Phyrexian-hybrid symbols `{C1/C2/P}` may be paid with one mana of
/// either color or 2 life. Mirrors the 10 such symbols in
/// `ManaCostShard::from_str`. Kept as a dedicated combinator (like
/// `parse_two_generic_hybrid_symbol_inner`) so the 3-part forms can be matched
/// ahead of the 2-part hybrid arms in `parse_mana_symbol_inner` — `alt` does not
/// backtrack, so the longer form must be tried first.
fn parse_phyrexian_hybrid_symbol_inner(input: &str) -> OracleResult<'_, ManaCostShard> {
    alt((
        value(ManaCostShard::PhyrexianWhiteBlue, tag("W/U/P")),
        value(ManaCostShard::PhyrexianWhiteBlack, tag("W/B/P")),
        value(ManaCostShard::PhyrexianBlueBlack, tag("U/B/P")),
        value(ManaCostShard::PhyrexianBlueRed, tag("U/R/P")),
        value(ManaCostShard::PhyrexianBlackRed, tag("B/R/P")),
        value(ManaCostShard::PhyrexianBlackGreen, tag("B/G/P")),
        value(ManaCostShard::PhyrexianRedWhite, tag("R/W/P")),
        value(ManaCostShard::PhyrexianRedGreen, tag("R/G/P")),
        value(ManaCostShard::PhyrexianGreenWhite, tag("G/W/P")),
        value(ManaCostShard::PhyrexianGreenBlue, tag("G/U/P")),
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

/// Parse a counter type: power/toughness counter notation (`+1/+1`,
/// `-0/-1`, etc.) or one of the named counter types recognized by Oracle text
/// (`loyalty`, `charge`, `lore`, …).
///
/// Returns the canonical `CounterType` enum via the single authoritative
/// mapping in `crate::types::counter::parse_counter_type`. Unrecognized
/// tokens from the named-type branch fall through to `CounterType::Generic`
/// through that mapping — so callers never re-parse the same token.
pub fn parse_counter_type_typed(input: &str) -> OracleResult<'_, CounterType> {
    alt((
        map(parse_pt_modifier, |(power, toughness)| {
            crate::types::counter::parse_counter_type(&format!("{power:+}/{toughness:+}"))
        }),
        map(parse_keyword_counter_name, |raw| {
            crate::types::counter::parse_counter_type(raw)
        }),
        map(parse_named_counter_type, |raw| {
            crate::types::counter::parse_counter_type(raw)
        }),
        // CR 122.1b: custom Oracle-named counter types ("hunger", "oil", "page",
        // "feather", …) are open-ended. Consume one whitespace-delimited token
        // and let `types::counter::parse_counter_type` map it to
        // `CounterType::Generic`. Must be the LAST arm so the enumerated
        // alternatives win when they apply.
        map(take_till1(|c: char| c.is_whitespace()), |raw: &str| {
            crate::types::counter::parse_counter_type(raw)
        }),
    ))
    .parse(input)
}

/// Parse a *recognized* counter type, rejecting unknown tokens.
///
/// This is `parse_counter_type_typed` MINUS the open-ended `take_till1 →
/// Generic` fallback arm. Only the three enumerated arms (P/T modifier,
/// keyword counter, named counter) are tried, so an unrecognized token such as
/// "red" or "blue creature" fails the parse instead of mapping to
/// `CounterType::Generic`. Callers use this to validate that a list item names
/// a real counter type before classifying a disjunctive list as a counter
/// choice.
///
/// CR 122.1b: keyword counters (docs/MagicCompRules.txt:1180).
/// CR 122.1: named counters (docs/MagicCompRules.txt:1176).
pub(crate) fn parse_strict_counter_type(input: &str) -> OracleResult<'_, CounterType> {
    alt((
        map(parse_pt_modifier, |(power, toughness)| {
            crate::types::counter::parse_counter_type(&format!("{power:+}/{toughness:+}"))
        }),
        map(parse_keyword_counter_name, |raw| {
            crate::types::counter::parse_counter_type(raw)
        }),
        map(parse_named_counter_type, |raw| {
            crate::types::counter::parse_counter_type(raw)
        }),
    ))
    .parse(input)
}

/// Parse a keyword counter name: "flying", "first strike", "double strike", etc.
///
/// CR 122.1b: A keyword counter on a permanent causes that object to gain the
/// named keyword (flying, first strike, double strike, deathtouch, decayed,
/// exalted, haste, hexproof, indestructible, lifelink, menace, reach, shadow,
/// trample, vigilance).
///
/// SOURCE OF TRUTH: `crate::types::counter::KEYWORD_COUNTERS`
/// (`crates/engine/src/types/counter.rs:53`). The parser iterates that table
/// and applies nom `tag()` to each entry, so additions to the table
/// automatically become parseable keyword counters.
///
/// Multi-word entries (`first strike`, `double strike`) MUST come before any
/// single-word entry whose name could be a prefix of them — nom `alt` commits
/// to the first arm that matches. No current single-word keyword name is a
/// prefix of `first` or `double`, but the ordering is preserved as a
/// non-negotiable invariant for future additions.
fn parse_keyword_counter_name(input: &str) -> OracleResult<'_, &str> {
    for (name, _) in KEYWORD_COUNTERS {
        let parsed: OracleResult<'_, &str> = tag(*name).parse(input);
        if parsed.is_ok() {
            return parsed;
        }
    }

    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Tag,
    )))
}

/// Parse a named counter type: "loyalty", "charge", "lore", "defense", etc.
///
/// CR 122.1b + CR 122.6: counter names are arbitrary strings; this list enumerates
/// the names that appear in printed Oracle text (as of current MTG releases).
/// Returns the matched slice verbatim so the caller can canonicalize it through
/// `types::counter::parse_counter_type` (single authority). Names without a
/// dedicated `CounterType` variant map to `CounterType::Generic(name)`.
fn parse_named_counter_type(input: &str) -> OracleResult<'_, &str> {
    // Split into two alt groups to stay within nom's 21-arm tuple limit.
    alt((
        tag("loyalty"),
        tag("charge"),
        tag("lore"),
        tag("defense"),
        tag("time"),
        tag("quest"),
        tag("energy"),
        tag("valor"),
        tag("verse"),
        tag("level"),
        tag("vitality"),
        // `vigilance` lives in `parse_keyword_counter_name` per CR 122.1b
        // (keyword counters) — it is NOT a named counter. Listing it in both
        // alt branches would be dead code: the keyword-counter combinator runs
        // first in `parse_counter_type_typed`.
        tag("bounty"),
    ))
    .or(alt((
        tag("oil"),
        tag("divinity"),
        tag("shield"),
        tag("judgment"),
        tag("depletion"),
        tag("feather"),
        tag("flood"),
        tag("slumber"),
        tag("sleep"),
        tag("phyresis"),
        tag("fire"),
        tag("shell"),
        tag("pupa"),
        tag("net"),
        tag("stun"),
    )))
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

/// Parse a P/T value pair from a token like "3/3", "X/X", "0/0", or "*/*".
///
/// Each component is either a fixed integer, `X` (variable, resolves later), or
/// `*` (CDA-defined). Returns the pair as `(PtValue, PtValue)`.
///
/// This is the shared building block for token description and animation spec
/// parsing — both need the same P/T tokenization.
pub fn parse_pt_value(input: &str) -> OracleResult<'_, (PtValue, PtValue)> {
    let input = input.trim_start();
    let word_end = input.find(char::is_whitespace).unwrap_or(input.len());
    let token = &input[..word_end];
    let Some(slash) = token.find('/') else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };
    let power_str = token[..slash].trim();
    let toughness_str = token[slash + 1..].trim();
    let power = parse_pt_component(power_str).ok_or_else(|| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
    })?;
    let toughness = parse_pt_component(toughness_str).ok_or_else(|| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
    })?;
    Ok((&input[word_end..], (power, toughness)))
}

fn parse_pt_component(text: &str) -> Option<PtValue> {
    if text.eq_ignore_ascii_case("x") {
        return Some(PtValue::Variable("X".to_string()));
    }
    if text == "*" {
        return Some(PtValue::Variable("*".to_string()));
    }
    text.parse::<i32>().ok().map(PtValue::Fixed)
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
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
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
                return Err(nom::Err::Error(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Fail,
                )));
            }
        };
        if val < prev {
            total = match total.checked_sub(val) {
                Some(t) => t,
                None => {
                    return Err(nom::Err::Error(nom::error::Error::new(
                        input,
                        nom::error::ErrorKind::Fail,
                    )));
                }
            };
        } else {
            total += val;
        }
        prev = val;
    }
    if total == 0 {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((&input[end..], total))
}

/// Parse optional whitespace, consuming zero or more spaces/tabs.
pub fn ws(input: &str) -> OracleResult<'_, &str> {
    space0.parse(input)
}

/// Parse optional whitespace then a specific tag.
pub fn ws_tag(t: &str) -> impl Parser<&str, Output = &str, Error = OracleError<'_>> {
    preceded(opt(space0), tag(t))
}

/// Canonical registry of evergreen keyword names in longest-match-first order
/// (e.g. "first strike" before "flash"). Used both by `parse_keyword_name` for
/// Oracle-text dispatch and by `is_keyword_word` for vocabulary-guard checks
/// in self-reference normalization.
pub(crate) const KEYWORDS: &[&str] = &[
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

/// Test whether a lowercased candidate word matches a registered evergreen
/// keyword name. Used by `normalize_card_name_refs` strategy-5 guard to reject
/// card-name first-word replacements that would corrupt keyword recognition
/// (e.g. `Changeling Berserker` must not replace `changeling` in its own text).
///
/// Uses `all_consuming(parse_keyword_name)` so only keyword entries that fully
/// match the candidate return true. Multi-word entries like "first strike"
/// cannot fully-consume a single token, so this predicate is semantically
/// restricted to single-word keywords by construction.
pub(crate) fn is_keyword_word(candidate_lower: &str) -> bool {
    all_consuming(parse_keyword_name)
        .parse(candidate_lower)
        .is_ok()
}

/// Parse an evergreen keyword name from Oracle text.
///
/// Uses a table lookup (longest-match-first within shared prefixes) to avoid
/// deep nom `alt` nesting which causes stack overflow in debug builds.
/// Returns the keyword string as matched (lowercase).
pub fn parse_keyword_name(input: &str) -> OracleResult<'_, &str> {
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

    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

/// Parse an alt-cost keyword name (lowercase Oracle text) into its `KeywordKind`
/// discriminant. Used by rider parsers that refer to a named alt-cost keyword
/// (e.g., "using its blitz ability", "using their sneak abilities"). Extend the
/// `alt` below with new keyword names as new alt-cost mechanics are supported.
///
/// CR 118.9: alternative costs are often named via keywords; this combinator
/// bridges Oracle-text names to the engine's `KeywordKind` enum.
pub fn parse_alt_cost_keyword_name_to_kind(input: &str) -> OracleResult<'_, KeywordKind> {
    alt((
        value(KeywordKind::Flashback, tag("flashback")),
        value(KeywordKind::Escape, tag("escape")),
        value(KeywordKind::Sneak, tag("sneak")),
        value(KeywordKind::Blitz, tag("blitz")),
        value(KeywordKind::Warp, tag("warp")),
        value(KeywordKind::Mutate, tag("mutate")),
        value(KeywordKind::Bestow, tag("bestow")),
        value(KeywordKind::Harmonize, tag("harmonize")),
        value(KeywordKind::Madness, tag("madness")),
    ))
    .parse(input)
}

/// Parse an imperative verb from Oracle text.
///
/// Matches common Oracle text action verbs: "destroy", "exile", "draw",
/// "create", "sacrifice", "discard", "return", "put", "counter", "gain",
/// "lose", "deal", "tap", "untap", "search", "shuffle", "reveal", "mill",
/// "scry", "surveil", "fight", "seek", "choose", "double".
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
        "seeks",
        "seek",
        "chooses",
        "choose",
        "doubles",
        "double",
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

    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

/// Test whether a lowercased candidate word is a complete Oracle-text imperative
/// verb (e.g. `search`, `destroy`, `return`). Used by `normalize_card_name_refs`
/// strategy-5 guard to reject single-word card-name first-word replacements that
/// would corrupt a sentence-initial instruction verb (e.g. `Search for Tomorrow`
/// must not rewrite `Search your library...` to `~ your library...`).
///
/// Uses `all_consuming(parse_verb)` so only a candidate that *is* a verb in its
/// entirety returns true; a card-name prefix that merely starts with a verb does
/// not. CR 201.5: self-references are by name, never by an instruction verb, so
/// no genuine self-reference is ever an imperative verb.
pub(crate) fn is_verb_word(candidate_lower: &str) -> bool {
    all_consuming(parse_verb).parse(candidate_lower).is_ok()
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

    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

// ── Word-boundary scanning primitives ─────────────────────────────────
//
// These are the shared building blocks for scanning Oracle text at word
// boundaries.  All per-file `scan_*` functions should delegate to these
// rather than re-implementing the scanning loop.

/// Try a nom combinator at every word boundary in `text`, returning the
/// first successful match.  This is the generic primitive behind all
/// `scan_for_*` helpers.
///
/// The combinator is tried at the start of `text`, then at each position
/// after a space.  Returns `Some(value)` on the first match, `None` if
/// no word boundary produces a match.
///
/// # Example
/// ```ignore
/// use nom::bytes::complete::tag;
/// use nom::combinator::value;
/// let found = scan_at_word_boundaries("the creature dies", |i| {
///     value("dies", tag("dies")).parse(i)
/// });
/// assert_eq!(found, Some("dies"));
/// ```
pub fn scan_at_word_boundaries<'a, O, F>(text: &'a str, mut combinator: F) -> Option<O>
where
    F: FnMut(&'a str) -> nom::IResult<&'a str, O, OracleError<'a>>,
{
    let mut remaining = text;
    while !remaining.is_empty() {
        if let Ok((_, val)) = combinator(remaining) {
            return Some(val);
        }
        remaining = remaining
            .find(' ')
            .map_or("", |i| remaining[i + 1..].trim_start());
    }
    None
}

/// Like [`scan_at_word_boundaries`] but returns the **last** successful match,
/// together with the byte offset where that match began.
pub fn scan_last_at_word_boundaries_with_offset<'a, O, F>(
    text: &'a str,
    mut combinator: F,
) -> Option<(usize, O, &'a str)>
where
    F: FnMut(&'a str) -> nom::IResult<&'a str, O, OracleError<'a>>,
{
    let mut remaining = text;
    let mut offset = 0;
    let mut last = None;
    while !remaining.is_empty() {
        if let Ok((rest, val)) = combinator(remaining) {
            last = Some((offset, val, rest));
        }
        if let Some(rel) = remaining.find(' ') {
            offset += rel + 1;
            remaining = remaining[rel + 1..].trim_start();
        } else {
            break;
        }
    }
    last
}

/// Like [`scan_last_at_word_boundaries_with_offset`] but only retains matches
/// whose offset passes `accept_offset`.
pub fn scan_last_valid_at_word_boundaries_with_offset<'a, O, F, P>(
    text: &'a str,
    mut combinator: F,
    mut accept_offset: P,
) -> Option<(usize, O, &'a str)>
where
    F: FnMut(&'a str) -> nom::IResult<&'a str, O, OracleError<'a>>,
    P: FnMut(usize) -> bool,
{
    let mut remaining = text;
    let mut offset = 0;
    let mut last = None;
    while !remaining.is_empty() {
        if let Ok((rest, val)) = combinator(remaining) {
            if accept_offset(offset) {
                last = Some((offset, val, rest));
            }
        }
        if let Some(rel) = remaining.find(' ') {
            offset += rel + 1;
            remaining = remaining[rel + 1..].trim_start();
        } else {
            break;
        }
    }
    last
}

/// Like [`scan_at_word_boundaries`] but returns the **last** successful match.
///
/// Use for terminal riders ("... if <condition>", "... as long as <condition>")
/// where an earlier `as if` phrase must not steal the gate.
pub fn scan_last_at_word_boundaries<'a, O, F>(
    text: &'a str,
    mut combinator: F,
) -> Option<(O, &'a str)>
where
    F: FnMut(&'a str) -> nom::IResult<&'a str, O, OracleError<'a>>,
{
    let mut remaining = text;
    let mut last = None;
    while !remaining.is_empty() {
        if let Ok((rest, val)) = combinator(remaining) {
            last = Some((val, rest));
        }
        remaining = remaining
            .find(' ')
            .map_or("", |i| remaining[i + 1..].trim_start());
    }
    last
}

/// Check whether `phrase` appears at any word boundary in `text`.
///
/// More precise than `str::contains()` — matches complete phrases at word
/// starts, preventing false positives from substring matches inside other
/// words (e.g. `scan_contains("studies", "dies")` → false).
///
/// Equivalent to `scan_at_word_boundaries(text, |i| tag(phrase).parse(i)).is_some()`
/// but avoids the generic closure overhead for the common boolean-guard case.
pub fn scan_contains(text: &str, phrase: &str) -> bool {
    let mut remaining = text;
    while !remaining.is_empty() {
        if tag::<_, _, OracleError<'_>>(phrase)
            .parse(remaining)
            .is_ok()
        {
            return true;
        }
        remaining = remaining
            .find(' ')
            .map_or("", |i| remaining[i + 1..].trim_start());
    }
    false
}

/// Mask every double-quoted span (the quotes and their contents) with a single
/// ASCII space, so line classifiers see the spell's own grammar and not the text
/// of a granted/printed ability inside quotes (a created token's "with \"…\"" text
/// or a perpetually-gained spell ability). Borrowed when the text has no '"'.
/// An unterminated quote passes the remainder through unchanged (no panic).
pub fn strip_double_quoted_spans(text: &str) -> Cow<'_, str> {
    // Zero-alloc fast path: no double quote means nothing to mask. This scans for
    // a char (a single quote mark), not a string literal, so it is not parsing
    // dispatch and stays off the combinator-mandate ban list.
    if text.find('"').is_none() {
        return Cow::Borrowed(text);
    }

    let mut out = String::with_capacity(text.len());
    let mut remaining = text;
    while !remaining.is_empty() {
        match remaining.find('"') {
            None => {
                // No further quote: copy the tail verbatim and finish.
                out.push_str(remaining);
                break;
            }
            Some(open) => {
                // Copy the verbatim text before the opening quote.
                out.push_str(&remaining[..open]);
                let after_open = &remaining[open..];
                // Recognize the quoted span with combinators: opening quote,
                // contents up to the closing quote, closing quote.
                match delimited(char::<_, OracleError<'_>>('"'), take_until("\""), char('"'))
                    .parse(after_open)
                {
                    Ok((rest, _span)) => {
                        // Whole span (quotes + contents) collapses to one space.
                        out.push(' ');
                        remaining = rest;
                    }
                    Err(_) => {
                        // Unterminated quote: pass the remainder through unchanged.
                        out.push_str(after_open);
                        break;
                    }
                }
            }
        }
    }
    Cow::Owned(out)
}

/// Scan `text` at word boundaries using `combinator`. Returns `(prefix, matched_start)` where
/// `prefix` is the text before the first match and `matched_start` is the slice beginning at
/// the matched position (combinator input pointer). Returns `None` if no match is found.
///
/// Unlike `scan_at_word_boundaries` which discards positional information, this variant
/// preserves it — use when you need to split text at a phrase boundary to extract a subject
/// prefix (e.g. `text[..prefix.len()]`).
///
/// # Example
/// ```ignore
/// let (prefix, rest) = scan_split_at_phrase("the creature dies", |i| tag("dies").parse(i)).unwrap();
/// assert_eq!(prefix, "the creature ");
/// assert_eq!(rest, "dies");
/// ```
pub fn scan_split_at_phrase<'a, O, F>(
    text: &'a str,
    mut combinator: F,
) -> Option<(&'a str, &'a str)>
where
    F: FnMut(&'a str) -> nom::IResult<&'a str, O, OracleError<'a>>,
{
    let mut remaining = text;
    while !remaining.is_empty() {
        if combinator(remaining).is_ok() {
            let offset = text.len() - remaining.len();
            return Some((&text[..offset], remaining));
        }
        remaining = remaining
            .find(' ')
            .map_or("", |i| remaining[i + 1..].trim_start());
    }
    None
}

/// Scan `text` at word boundaries and, on the first successful match, return
/// `(before, value, rest)` — the prefix preceding the match, the combinator's
/// output, and the post-match remainder (a slice of `text`).
///
/// Unlike [`scan_split_at_phrase`], this variant preserves the combinator's
/// output *and* its `IResult` remainder, eliminating the common double-parse
/// pattern where callers locate a phrase with `scan_split_at_phrase` and then
/// re-invoke the same combinator on the returned slice to extract values.
///
/// The matched span's length is `text.len() - before.len() - rest.len()`, which
/// is useful for clause-stripping arithmetic.
///
/// # Example
/// ```ignore
/// use nom::bytes::complete::tag;
/// use nom::sequence::preceded;
/// use nom::Parser;
/// let (before, value, rest) = scan_preceded("then it dies soon", |i| {
///     preceded(tag::<_, _, OracleError<'_>>("it "), tag("dies")).parse(i)
/// }).unwrap();
/// assert_eq!(before, "then ");
/// assert_eq!(value, "dies");
/// assert_eq!(rest, " soon");
/// ```
pub fn scan_preceded<'a, O, F>(text: &'a str, mut combinator: F) -> Option<(&'a str, O, &'a str)>
where
    F: FnMut(&'a str) -> nom::IResult<&'a str, O, OracleError<'a>>,
{
    let mut remaining = text;
    while !remaining.is_empty() {
        if let Ok((rest, val)) = combinator(remaining) {
            let offset = text.len() - remaining.len();
            return Some((&text[..offset], val, rest));
        }
        remaining = remaining
            .find(' ')
            .map_or("", |i| remaining[i + 1..].trim_start());
    }
    None
}

/// Split `input` on the first occurrence of `separator`, returning `(before, after)`.
///
/// Equivalent to `str::split_once(separator)` but as a nom combinator — uses
/// `take_until` + `tag` internally, producing structured `OracleError` traces
/// on failure instead of a bare `None`.
///
/// # Example
/// ```ignore
/// let (rest, (before, after)) = split_once_on("hello, world", ", ")?;
/// assert_eq!(before, "hello");
/// assert_eq!(after, "world");  // rest == ""
/// ```
pub fn split_once_on<'a>(
    input: &'a str,
    separator: &'a str,
) -> nom::IResult<&'a str, (&'a str, &'a str), OracleError<'a>> {
    let (rest, before) = take_until(separator).parse(input)?;
    let (after, _) = tag(separator).parse(rest)?;
    Ok(("", (before, after)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nom::bytes::complete::tag;

    #[test]
    fn strip_double_quoted_spans_no_quote_borrows_unchanged() {
        let out = strip_double_quoted_spans("creatures you control can't block");
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out, "creatures you control can't block");
    }

    #[test]
    fn strip_double_quoted_spans_single_span_collapses_to_one_space() {
        // The span plus its quotes becomes a single space; surrounding text intact.
        let out = strip_double_quoted_spans(r#"a "b c" d"#);
        assert_eq!(out, "a   d");
        assert!(matches!(out, Cow::Owned(_)));
    }

    #[test]
    fn strip_double_quoted_spans_masks_two_spans() {
        let out = strip_double_quoted_spans(r#"x "one" y "two" z"#);
        assert_eq!(out, "x   y   z");
    }

    #[test]
    fn strip_double_quoted_spans_unterminated_passes_through() {
        let out = strip_double_quoted_spans(r#"a "b c"#);
        assert_eq!(out, r#"a "b c"#);
    }

    #[test]
    fn strip_double_quoted_spans_empty_ok() {
        let out = strip_double_quoted_spans("");
        assert_eq!(out, "");
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    /// Extended number words (30, 40, ..., 100) for cards like Lux Artillery
    /// ("thirty or more counters") and Hundred-Handed One.
    #[test]
    fn test_parse_number_high_words() {
        assert_eq!(parse_number("thirty").unwrap().1, 30);
        assert_eq!(parse_number("forty").unwrap().1, 40);
        assert_eq!(parse_number("fifty").unwrap().1, 50);
        assert_eq!(parse_number("sixty").unwrap().1, 60);
        assert_eq!(parse_number("seventy").unwrap().1, 70);
        assert_eq!(parse_number("eighty").unwrap().1, 80);
        assert_eq!(parse_number("ninety").unwrap().1, 90);
        assert_eq!(parse_number("one hundred").unwrap().1, 100);
    }

    #[test]
    fn test_parse_number_hyphenated_words() {
        assert_eq!(parse_number("twenty-one").unwrap().1, 21);
        assert_eq!(parse_number("ninety-nine").unwrap().1, 99);
        assert_eq!(parse_number("ninety").unwrap().1, 90);
        assert_eq!(parse_number("one hundred").unwrap().1, 100);
    }

    /// A cardinal number word must not be matched inside a longer word (e.g.
    /// the ordinal "sixth" or "tenfold"). The multi-character words previously
    /// lacked the word-boundary guard that `parse_article_number` applies to
    /// "a"/"an", and the `oracle_util::parse_number` wrapper only guarded
    /// matches of ≤2 characters — so "sixth" parsed as 6, "tenth" as 10, etc.
    #[test]
    fn test_parse_english_number_requires_word_boundary() {
        // Longer words that merely start with a cardinal must NOT parse.
        for embedded in [
            "sixth",
            "tenth",
            "tenfold",
            "threefold",
            "nineteenth",
            "fourteener",
        ] {
            assert!(
                parse_number(embedded).is_err(),
                "{embedded:?} must not parse as an embedded cardinal"
            );
        }
        // Genuine cardinals with a trailing boundary still parse, remainder intact.
        assert_eq!(parse_number("six cards").unwrap(), (" cards", 6));
        assert_eq!(parse_number("ten").unwrap(), ("", 10));
        assert_eq!(
            parse_number("nineteen creatures").unwrap(),
            (" creatures", 19)
        );
        assert_eq!(
            parse_number("three, then draw").unwrap(),
            (", then draw", 3)
        );
        // Distinct number words that merely share a prefix are unaffected.
        assert_eq!(parse_number("sixteen").unwrap(), ("", 16));
        assert_eq!(parse_number("sixty").unwrap(), ("", 60));
    }

    /// `parse_strict_counter_type` accepts recognized counter tokens (keyword
    /// counters, named counters) and rejects unknown tokens — unlike
    /// `parse_counter_type_typed`, which maps anything to `CounterType::Generic`.
    #[test]
    fn test_parse_strict_counter_type_rejects_unrecognized() {
        assert!(parse_strict_counter_type("menace").is_ok());
        assert!(parse_strict_counter_type("red").is_err());
    }

    /// "one hundred" must be tried BEFORE "one" so "one hundred cards"
    /// parses as 100, not 1 followed by " hundred cards".
    #[test]
    fn test_parse_number_one_hundred_before_one() {
        let (rest, n) = parse_number("one hundred cards").unwrap();
        assert_eq!(n, 100);
        assert_eq!(rest, " cards");
    }

    #[test]
    fn test_parse_number_single_word_still_works() {
        assert_eq!(parse_number("one").unwrap().1, 1);
        assert_eq!(parse_number("twenty").unwrap().1, 20);
    }

    #[test]
    fn test_scan_split_at_phrase_at_start() {
        let result = scan_split_at_phrase("dies to removal", |i| {
            tag::<_, _, OracleError<'_>>("dies").parse(i)
        });
        assert_eq!(result, Some(("", "dies to removal")));
    }

    #[test]
    fn test_scan_split_at_phrase_mid_string() {
        let result = scan_split_at_phrase("the creature dies", |i| {
            tag::<_, _, OracleError<'_>>("dies").parse(i)
        });
        assert_eq!(result, Some(("the creature ", "dies")));
    }

    #[test]
    fn test_scan_split_at_phrase_not_found() {
        let result = scan_split_at_phrase("the creature enters", |i| {
            tag::<_, _, OracleError<'_>>("dies").parse(i)
        });
        assert!(result.is_none());
    }

    #[test]
    fn test_scan_split_at_phrase_word_boundary_safe() {
        // "studies" must NOT match "dies" mid-word
        let result = scan_split_at_phrase("studies hard", |i| {
            tag::<_, _, OracleError<'_>>("dies").parse(i)
        });
        assert!(result.is_none());
    }

    #[test]
    fn test_scan_preceded_threads_value_and_remainder() {
        use nom::combinator::value;
        let (before, val, rest) = scan_preceded("the creature dies to removal", |i| {
            value("dies", tag::<_, _, OracleError<'_>>("dies")).parse(i)
        })
        .unwrap();
        assert_eq!(before, "the creature ");
        assert_eq!(val, "dies");
        assert_eq!(rest, " to removal");
        // Matched span length reconstructs via subtraction — the idiom that
        // motivated this helper.
        let text = "the creature dies to removal";
        assert_eq!(text.len() - before.len() - rest.len(), "dies".len());
    }

    #[test]
    fn test_scan_preceded_word_boundary_safe() {
        // "studies" must NOT match "dies" mid-word even with value capture.
        let result = scan_preceded("studies hard", |i| {
            tag::<_, _, OracleError<'_>>("dies").parse(i)
        });
        assert!(result.is_none());
    }

    #[test]
    fn test_scan_preceded_not_found() {
        let result = scan_preceded("the creature enters", |i| {
            tag::<_, _, OracleError<'_>>("dies").parse(i)
        });
        assert!(result.is_none());
    }

    #[test]
    fn test_scan_last_at_word_boundaries_picks_terminal_if_gate() {
        let (_, tail) = scan_last_at_word_boundaries(
            "you may cast as if they had flash if you control a zombie",
            |i| tag::<_, _, OracleError<'_>>("if ").parse(i),
        )
        .expect("terminal if gate");
        assert_eq!(tail, "you control a zombie");
    }

    #[test]
    fn test_scan_last_at_word_boundaries_with_offset_picks_terminal_if_gate() {
        let (_, _, tail) = scan_last_at_word_boundaries_with_offset(
            "you can't play lands as if there were no rule if ten or more lands are on the battlefield",
            |i| tag::<_, _, OracleError<'_>>("if ").parse(i),
        )
        .expect("terminal if gate");
        assert_eq!(tail, "ten or more lands are on the battlefield");
    }

    #[test]
    fn test_scan_last_at_word_boundaries_with_offset_skips_as_if() {
        let result = scan_last_valid_at_word_boundaries_with_offset(
            "you can't play lands as if there were no rule",
            |i| tag::<_, _, OracleError<'_>>("if ").parse(i),
            |if_offset| {
                let Some(start) = if_offset.checked_sub(3) else {
                    return true;
                };
                !"you can't play lands as if there were no rule".is_char_boundary(start)
                    || tag::<_, _, OracleError<'_>>("as ")
                        .parse(&"you can't play lands as if there were no rule"[start..if_offset])
                        .is_err()
            },
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_number_digits() {
        let (rest, n) = parse_number("42 damage").unwrap();
        assert_eq!(n, 42);
        assert_eq!(rest, " damage");
    }

    #[test]
    fn test_parse_number_comma_thousands() {
        // Basic thousands separator
        let (rest, n) = parse_number("1,000 or more life").unwrap();
        assert_eq!(n, 1000);
        assert_eq!(rest, " or more life");

        // Millions
        let (rest, n) = parse_number("1,000,000 damage").unwrap();
        assert_eq!(n, 1_000_000);
        assert_eq!(rest, " damage");

        // Trailing comma at a clause boundary must not be consumed
        let (rest, n) = parse_number("10, then draw a card").unwrap();
        assert_eq!(n, 10);
        assert_eq!(rest, ", then draw a card");

        // Invalid 2-digit group leaves the comma unconsumed
        let (rest, n) = parse_number("1,50 damage").unwrap();
        assert_eq!(n, 1);
        assert_eq!(rest, ",50 damage");

        // Invalid 4-digit group leaves the comma unconsumed
        let (rest, n) = parse_number("1,0000 damage").unwrap();
        assert_eq!(n, 1);
        assert_eq!(rest, ",0000 damage");
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

    /// CR 107.4e: Eldrazi colorless-hybrid `{C/X}` symbols (BFZ/OGW).
    /// Regression guard for #493 — without these arms, Ulalek Fused
    /// Atrocity's `{C/W}{C/U}{C/B}{C/R}{C/G}` cost parsed as empty.
    #[test]
    fn test_parse_mana_symbol_colorless_hybrid() {
        for (input, expected) in [
            ("{C/W}", ManaCostShard::ColorlessWhite),
            ("{C/U}", ManaCostShard::ColorlessBlue),
            ("{C/B}", ManaCostShard::ColorlessBlack),
            ("{C/R}", ManaCostShard::ColorlessRed),
            ("{C/G}", ManaCostShard::ColorlessGreen),
        ] {
            let (rest, shard) = parse_mana_symbol(input).expect(input);
            assert_eq!(shard, expected, "{input}");
            assert_eq!(rest, "", "{input}");
        }
    }

    /// CR 107.4e: Ulalek Fused Atrocity full cost regression — five
    /// colorless-hybrid shards must accumulate as five distinct shards
    /// with zero generic, not collapse to empty.
    #[test]
    fn test_parse_mana_cost_ulalek_full() {
        let (rest, cost) = parse_mana_cost("{C/W}{C/U}{C/B}{C/R}{C/G}").unwrap();
        assert_eq!(rest, "");
        match cost {
            ManaCost::Cost { shards, generic } => {
                assert_eq!(generic, 0, "Ulalek has no generic mana in cost");
                assert_eq!(
                    shards,
                    vec![
                        ManaCostShard::ColorlessWhite,
                        ManaCostShard::ColorlessBlue,
                        ManaCostShard::ColorlessBlack,
                        ManaCostShard::ColorlessRed,
                        ManaCostShard::ColorlessGreen,
                    ]
                );
            }
            _ => panic!("expected ManaCost::Cost variant, got {cost:?}"),
        }
    }

    #[test]
    fn test_parse_mana_symbol_two_or_more_color_source() {
        let (rest, shard) = parse_mana_symbol("{Z}").unwrap();
        assert_eq!(shard, ManaCostShard::TwoOrMoreColorSource);
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
    fn test_parse_mana_cost_two_or_more_color_source() {
        let (rest, cost) = parse_mana_cost("{1}{Z}").unwrap();
        assert_eq!(rest, "");
        match cost {
            ManaCost::Cost { shards, generic } => {
                assert_eq!(generic, 1);
                assert_eq!(shards, vec![ManaCostShard::TwoOrMoreColorSource]);
            }
            _ => panic!("expected Cost variant"),
        }
    }

    #[test]
    fn test_parse_mana_cost_two_generic_hybrid() {
        let (rest, cost) = parse_mana_cost("{2/W}{2/U}{2/B}{2/R}{2/G}").unwrap();
        assert_eq!(rest, "");
        match cost {
            ManaCost::Cost { shards, generic } => {
                assert_eq!(generic, 0);
                assert_eq!(
                    shards,
                    vec![
                        ManaCostShard::TwoWhite,
                        ManaCostShard::TwoBlue,
                        ManaCostShard::TwoBlack,
                        ManaCostShard::TwoRed,
                        ManaCostShard::TwoGreen,
                    ]
                );
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
        let (rest, ct) = parse_counter_type_typed("+1/+1 counter").unwrap();
        assert_eq!(ct, CounterType::Plus1Plus1);
        assert_eq!(rest, " counter");
    }

    #[test]
    fn test_parse_counter_type_legacy_asymmetric_pt() {
        let (rest, ct) = parse_counter_type_typed("-0/-1 counter").unwrap();
        assert_eq!(
            ct,
            CounterType::PowerToughness {
                power: 0,
                toughness: -1
            }
        );
        assert_eq!(rest, " counter");

        let (rest, ct) = parse_counter_type_typed("-0/-2 counters").unwrap();
        assert_eq!(
            ct,
            CounterType::PowerToughness {
                power: 0,
                toughness: -2
            }
        );
        assert_eq!(rest, " counters");

        let (rest, ct) = parse_counter_type_typed("-1/-0 counters").unwrap();
        assert_eq!(
            ct,
            CounterType::PowerToughness {
                power: -1,
                toughness: 0
            }
        );
        assert_eq!(rest, " counters");
    }

    #[test]
    fn test_parse_counter_type_named() {
        let (rest, ct) = parse_counter_type_typed("loyalty counters").unwrap();
        assert_eq!(ct, CounterType::Loyalty);
        assert_eq!(rest, " counters");
    }

    /// CR 122.1b: an unrecognized single-word counter type ("hunger", "page",
    /// "oil", etc.) is open-ended Oracle text and falls through to
    /// `CounterType::Generic`. This matches the legacy
    /// `find(whitespace)` + `normalize_counter_type` behavior the combinator
    /// replaces. The combinator only fails on empty input or input starting
    /// with whitespace, since no arm can consume zero characters.
    #[test]
    fn test_parse_counter_type_unknown_falls_back_to_generic() {
        let (rest, ct) = parse_counter_type_typed("hunger counters").unwrap();
        assert_eq!(ct, CounterType::Generic("hunger".to_string()));
        assert_eq!(rest, " counters");

        // Empty input has no token to consume — combinator fails.
        assert!(parse_counter_type_typed("").is_err());
    }

    /// CR 122.1b: every entry in `KEYWORD_COUNTERS` must be recognized by
    /// `parse_keyword_counter_name` and round-trip through `parse_counter_type_typed`
    /// to the corresponding `CounterType::Keyword(kind)`.
    #[test]
    fn test_parse_keyword_counter_name_covers_table() {
        for (name, kind) in KEYWORD_COUNTERS {
            // 1. The bare keyword name parses through the keyword combinator.
            let (rest, raw) = parse_keyword_counter_name(name)
                .unwrap_or_else(|_| panic!("parse_keyword_counter_name failed for {name:?}"));
            assert_eq!(raw, *name, "keyword name {name:?}");
            assert_eq!(rest, "", "keyword name {name:?} should fully consume");

            // 2. End-to-end: "{name} counter on it" → CounterType::Keyword(kind),
            //    remainder beginning with " counter on it".
            let input = format!("{name} counter on it");
            let (rest, ct) = parse_counter_type_typed(&input)
                .unwrap_or_else(|_| panic!("parse_counter_type_typed failed for {name:?}"));
            assert_eq!(ct, CounterType::Keyword(*kind), "keyword name {name:?}");
            assert_eq!(rest, " counter on it", "keyword name {name:?}");
        }
    }

    #[test]
    fn test_keyword_counter_table_is_longest_match_first() {
        for pair in KEYWORD_COUNTERS.windows(2) {
            assert!(
                pair[0].0.len() >= pair[1].0.len(),
                "KEYWORD_COUNTERS must stay longest-match-first: {:?} before {:?}",
                pair[0].0,
                pair[1].0
            );
        }
    }

    /// Explicit readability checks for the two multi-word entries — these are
    /// the cases the previous `.find(whitespace)` slicing broke.
    #[test]
    fn test_parse_keyword_counter_name_multi_word() {
        let (rest, raw) = parse_keyword_counter_name("double strike").unwrap();
        assert_eq!(raw, "double strike");
        assert_eq!(rest, "");

        let (rest, raw) = parse_keyword_counter_name("first strike").unwrap();
        assert_eq!(raw, "first strike");
        assert_eq!(rest, "");

        // With trailing text the combinator must consume only the keyword name.
        let (rest, raw) = parse_keyword_counter_name("double strike counter on it").unwrap();
        assert_eq!(raw, "double strike");
        assert_eq!(rest, " counter on it");
    }

    /// CR 122.1b: end-to-end check matching the Avenging Huntbonder trigger
    /// effect's counter clause through `parse_counter_type_typed`.
    #[test]
    fn test_parse_counter_type_typed_double_strike_full_clause() {
        let (rest, ct) =
            parse_counter_type_typed("double strike counter on another target attacking creature")
                .unwrap();
        assert_eq!(ct, CounterType::Keyword(KeywordKind::DoubleStrike));
        assert_eq!(rest, " counter on another target attacking creature");
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

    #[test]
    fn test_parse_alt_cost_keyword_name_to_kind() {
        let cases = [
            ("flashback ability", KeywordKind::Flashback),
            ("escape ability", KeywordKind::Escape),
            ("sneak abilities", KeywordKind::Sneak),
            ("blitz ability", KeywordKind::Blitz),
            ("warp ability", KeywordKind::Warp),
            ("mutate ability", KeywordKind::Mutate),
            ("bestow ability", KeywordKind::Bestow),
            ("harmonize ability", KeywordKind::Harmonize),
            ("madness", KeywordKind::Madness),
        ];
        for (input, expected) in cases {
            let (_, kind) = parse_alt_cost_keyword_name_to_kind(input).unwrap();
            assert_eq!(kind, expected, "input: {input:?}");
        }
        assert!(parse_alt_cost_keyword_name_to_kind("unknown").is_err());
    }
}
