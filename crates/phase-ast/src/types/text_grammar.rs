//! Leaf Oracle text grammar that `Keyword`'s `FromStr` and `Deserialize`
//! impls need. This module is the single AST-layer authority for these
//! combinators: the orphan rule binds both impls to `Keyword`'s crate, so the
//! grammar they call lives beside it rather than in the parser.
//!
//! Each item that parser code still calls is re-exported at its original
//! `parser::…` path, and parser callers keep using those paths; an item no
//! parser code calls has no parser path. `scripts/check-parser-combinators.sh` scans
//! this file, so the nom mandate binds it. Nothing here may name
//! `crate::parser` (`tests/integration/layer_boundary_census.rs`, clause 3).

use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::{tag, take_till, take_while_m_n};
use nom::character::complete::{digit1, space1};
use nom::combinator::{map, map_opt, not, opt, peek, recognize, value};
use nom::multi::many0;
use nom::sequence::{preceded, terminated};
use nom::{IResult, Parser};

use super::ability::{
    AttachmentKind, AttackerBlockStatus, Comparator, ControllerRef, FilterProp, TargetFilter,
    TypeFilter, TypedFilter,
};
use super::card_type::{
    fixed_noncreature_subtypes, noncreature_subtype_set, SubtypeSet, Supertype,
};
use super::keywords::{Keyword, KeywordKind};
use super::mana::ManaColor;
use super::zones::Zone;

// --- from parser/oracle_nom/error.rs ---

pub type OracleError<'a> = nom::error::Error<&'a str>;

pub type OracleResult<'a, O> = IResult<&'a str, O, OracleError<'a>>;

// --- from parser/oracle_nom/primitives.rs ---

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
pub fn parse_digit_number(input: &str) -> OracleResult<'_, u32> {
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

/// CR 903.4: Parse the possessive phrase "your commander's color identity".
///
/// The `alt()` covers the singular possessive and both plural apostrophe
/// placements ("commanders'" and the apostrophe-less "commanders") that
/// Oracle text and normalized text use for a partnered commander pair
/// (CR 702.124c). "commanders'" precedes "commanders" so the longer spelling
/// is tried first. Operates on already-lowercased text and consumes nothing
/// beyond the phrase itself.
pub fn parse_your_commanders_color_identity(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            tag("your "),
            alt((tag("commander's"), tag("commanders'"), tag("commanders"))),
            tag(" color identity"),
        ),
    )
    .parse(input)
}

// --- from parser/oracle_util.rs ---

/// Irregular plural → singular mappings for MTG creature subtypes.
/// Only entries that cannot be resolved by stripping "-s" or "-es".
const SUBTYPE_PLURALS: &[(&str, &str)] = &[
    ("elves", "Elf"),
    ("dwarves", "Dwarf"),
    ("wolves", "Wolf"),
    ("werewolves", "Werewolf"),
    ("halves", "Half"),
    ("fungi", "Fungus"),
    ("loci", "Locus"),
    ("djinn", "Djinn"),
    ("sphinxes", "Sphinx"),
    ("foxes", "Fox"),
    ("octopi", "Octopus"),
    ("octopuses", "Octopus"),
    ("mice", "Mouse"),
    ("oxen", "Ox"),
    ("pegasi", "Pegasus"),
    ("pegasuses", "Pegasus"),
    ("allies", "Ally"),
    ("armies", "Army"),
    ("faeries", "Faerie"),
    ("zombies", "Zombie"),
    ("sorceries", "Sorcery"),
    ("ponies", "Pony"),
    ("harpies", "Harpy"),
    ("berserkers", "Berserker"),
];

/// CR 700.12: An outlaw is an object with the Assassin, Mercenary, Pirate,
/// Rogue, and/or Warlock creature type. Shared by every parser path that
/// recognizes the "outlaw[s]" head noun.
pub const OUTLAW_SUBTYPES: [&str; 5] = ["Assassin", "Mercenary", "Pirate", "Rogue", "Warlock"];

/// MTGJSON CardTypes-derived **creature** subtype vocabulary (`oracle-subtypes.json`),
/// merged at load with canonical noncreature tables from `card_type.rs`.
static ORACLE_SUBTYPES: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| {
    let creature: Vec<String> =
        serde_json::from_str(include_str!("../../data/oracle-subtypes.json"))
            .expect("oracle-subtypes.json well-formed");
    crate::types::card_type::build_parser_subtype_vocabulary(&creature)
});

pub fn oracle_subtypes() -> &'static [String] {
    &ORACLE_SUBTYPES
}

/// Check if `text` starts with `prefix` using ASCII case-insensitive comparison,
/// followed by a word boundary (non-alphanumeric or end of string).
fn starts_with_word_ci(text: &str, prefix: &str) -> bool {
    if text.len() < prefix.len() {
        return false;
    }
    // prefix is always ASCII (subtypes/planeswalker names), but text may contain
    // multi-byte UTF-8 (e.g. em dashes). Guard against slicing inside a character.
    if !text.is_char_boundary(prefix.len()) {
        return false;
    }
    if !text[..prefix.len()].eq_ignore_ascii_case(prefix) {
        return false;
    }
    let after = &text[prefix.len()..];
    after.is_empty() || after.starts_with(|c: char| !c.is_alphanumeric())
}

/// Try to match a subtype at the start of text (case-insensitive).
/// Returns `(canonical_name, bytes_consumed)` or `None`.
/// Handles plural forms (regular and irregular).
pub fn parse_subtype(text: &str) -> Option<(String, usize)> {
    // Check irregular plurals first (they take priority over regular matching)
    for &(plural, singular) in SUBTYPE_PLURALS {
        if starts_with_word_ci(text, plural) {
            return Some((singular.to_string(), plural.len()));
        }
    }

    for subtype in fixed_noncreature_subtypes() {
        if let Some(parsed) = parse_subtype_entry(text, subtype) {
            return Some(parsed);
        }
    }

    // Check each subtype (singular and regular plural)
    for subtype in oracle_subtypes() {
        if let Some(parsed) = parse_subtype_entry(text, subtype.as_str()) {
            return Some(parsed);
        }
    }

    None
}

pub fn parse_subtype_entry(text: &str, subtype: &str) -> Option<(String, usize)> {
    if starts_with_word_ci(text, subtype) {
        return Some((subtype.to_string(), subtype.len()));
    }

    // Try regular plural: subtype + "s" — check subtype prefix + 's' at boundary
    let plural_len = subtype.len() + 1;
    if text.len() >= plural_len
        && text.is_char_boundary(subtype.len())
        && text[..subtype.len()].eq_ignore_ascii_case(subtype)
        && text.as_bytes()[subtype.len()] == b's'
    {
        let after = &text[plural_len..];
        if after.is_empty() || after.starts_with(|c: char| !c.is_alphanumeric()) {
            return Some((subtype.to_string(), plural_len));
        }
    }

    // Try regular "-es" plural: subtypes ending in a sibilant (s, x, z, ch, sh)
    // or in consonant+o pluralize with "-es" rather than "-s" (e.g. "Hero" →
    // "Heroes", "Sphinx" → "Sphinxes"). Without this, such plurals fall through
    // to a naive trailing-'s' strip at call sites, corrupting the subtype name
    // (e.g. "Heroes" → "Heroe"). Irregular forms still take priority via
    // SUBTYPE_PLURALS above.
    if takes_es_plural(subtype) {
        let es_plural_len = subtype.len() + 2;
        if text.len() >= es_plural_len
            && text.is_char_boundary(subtype.len())
            && text[..subtype.len()].eq_ignore_ascii_case(subtype)
            && text[subtype.len()..es_plural_len].eq_ignore_ascii_case("es")
        {
            let after = &text[es_plural_len..];
            if after.is_empty() || after.starts_with(|c: char| !c.is_alphanumeric()) {
                return Some((subtype.to_string(), es_plural_len));
            }
        }
    }

    // Try regular "-ies" plural: subtypes ending in consonant + "y" pluralize by
    // replacing "y" with "ies" (e.g. "Mercenary" → "Mercenaries", "Berserker"
    // is unaffected). Words ending in vowel + "y" take a plain "-s" ("Monkey" →
    // "Monkeys") and are covered by the "-s" rule above. Matching the plural
    // surface form requires stripping the trailing "y" from the subtype stem and
    // matching "ies" at the boundary; only the canonical singular is returned.
    if takes_ies_plural(subtype) {
        let stem_len = subtype.len() - 1; // drop trailing "y"
        let ies_plural_len = stem_len + 3; // stem + "ies"
        if text.len() >= ies_plural_len
            && text.is_char_boundary(stem_len)
            && text[..stem_len].eq_ignore_ascii_case(&subtype[..stem_len])
            && text[stem_len..ies_plural_len].eq_ignore_ascii_case("ies")
        {
            let after = &text[ies_plural_len..];
            if after.is_empty() || after.starts_with(|c: char| !c.is_alphanumeric()) {
                return Some((subtype.to_string(), ies_plural_len));
            }
        }
    }

    None
}

/// Whether an English noun pluralizes by replacing a trailing "-y" with "-ies":
/// nouns ending in consonant + "y" (e.g. "Mercenary" → "Mercenaries"). Nouns
/// ending in vowel + "y" take a plain "-s" ("Monkey" → "Monkeys") and are
/// excluded so the regular "-s" rule handles them.
fn takes_ies_plural(word: &str) -> bool {
    let bytes = word.as_bytes();
    // A one-letter "y" has no preceding consonant, so the "-ies" rule cannot
    // apply; the `len() < 2` guard also makes the penultimate index safe without
    // `saturating_sub`/`get`.
    if bytes.len() < 2 || !matches!(bytes.last(), Some(b'y' | b'Y')) {
        return false;
    }
    !matches!(
        bytes[bytes.len() - 2].to_ascii_lowercase(),
        b'a' | b'e' | b'i' | b'o' | b'u'
    )
}

/// Whether an English noun forms its plural by appending "-es" rather than
/// "-s": nouns ending in a sibilant (s, x, z, ch, sh) or in consonant + "o"
/// (e.g. "Hero" → "Heroes"). Words ending in vowel + "o" take a plain "-s"
/// ("Radio" → "Radios") and are excluded.
fn takes_es_plural(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    if lower.ends_with('s') || lower.ends_with('x') || lower.ends_with('z') {
        return true;
    }
    let bytes = lower.as_bytes();
    if matches!(
        bytes.get(bytes.len().saturating_sub(2)..),
        Some(b"ch" | b"sh")
    ) {
        return true;
    }
    if matches!(bytes.last(), Some(b'o')) {
        return !matches!(
            bytes.get(bytes.len().saturating_sub(2)),
            Some(b'a' | b'e' | b'i' | b'o' | b'u')
        );
    }
    false
}

// --- from parser/oracle_nom/target.rs ---

/// Parse a "non" prefix: "non" or "non-" followed by implicit word boundary.
pub fn parse_non_prefix(input: &str) -> OracleResult<'_, &str> {
    alt((tag("non-"), tag("non"))).parse(input)
}

/// CR 205.4a: Parse a bare supertype word ("legendary", "basic", "snow",
/// "world", "ongoing") without consuming any trailing boundary. Shared building
/// block for both the adjective-prefix form (`parse_supertype_prefix`, word +
/// space) and trailing relative-clause forms ("that aren't legendary", where
/// the word is at end-of-string). Callers that need a boundary apply their own
/// check. Covers the full CR 205.4a set the engine `Supertype` enum models
/// (Host is set-supplemental / not CR 205.4a and is excluded here). None of the
/// five words is a prefix of another, so `alt` ordering is boundary-safe.
pub fn parse_supertype_word(input: &str) -> OracleResult<'_, Supertype> {
    alt((
        value(Supertype::Legendary, tag("legendary")),
        value(Supertype::Basic, tag("basic")),
        value(Supertype::Snow, tag("snow")),
        value(Supertype::World, tag("world")),
        value(Supertype::Ongoing, tag("ongoing")),
    ))
    .parse(input)
}

/// Parse a supertype prefix ("legendary ", "basic ", "snow ") consuming trailing space.
pub fn parse_supertype_prefix(input: &str) -> OracleResult<'_, Supertype> {
    let (rest, st) = parse_supertype_word(input)?;
    let (rest, _) = space1.parse(rest)?;
    Ok((rest, st))
}

/// Parse a single type filter word (singular or plural).
///
/// Uses a manual lookup for core/card types to avoid deep nom `alt` nesting which causes
/// stack overflow in debug builds, then falls back to the shared subtype table.
pub fn parse_type_filter_word(input: &str) -> OracleResult<'_, TypeFilter> {
    // Table of (prefix, TypeFilter) — longest-match-first within shared prefixes.
    static TYPE_WORDS: &[(&str, TypeFilter)] = &[
        ("creatures", TypeFilter::Creature),
        ("creature", TypeFilter::Creature),
        ("artifacts", TypeFilter::Artifact),
        ("artifact", TypeFilter::Artifact),
        ("enchantments", TypeFilter::Enchantment),
        ("enchantment", TypeFilter::Enchantment),
        ("instants", TypeFilter::Instant),
        ("instant", TypeFilter::Instant),
        ("sorceries", TypeFilter::Sorcery),
        ("sorcery", TypeFilter::Sorcery),
        ("planeswalkers", TypeFilter::Planeswalker),
        ("planeswalker", TypeFilter::Planeswalker),
        ("lands", TypeFilter::Land),
        ("land", TypeFilter::Land),
        // Plural before singular (longest-match-first): the word-boundary guard
        // rejects "battle" + trailing 's', and BATTLE_SUBTYPES has no "Battle"
        // entry, so the plural must be an explicit head-noun word here.
        ("battles", TypeFilter::Battle),
        ("battle", TypeFilter::Battle),
        // CR 308: Kindred card type (and legacy Tribal terminology).
        ("kindreds", TypeFilter::Kindred),
        ("kindred", TypeFilter::Kindred),
        ("tribals", TypeFilter::Kindred),
        ("tribal", TypeFilter::Kindred),
        ("permanents", TypeFilter::Permanent),
        ("permanent", TypeFilter::Permanent),
        ("cards", TypeFilter::Card),
        ("card", TypeFilter::Card),
        // CR 112.1: a spell is a card on the stack — "spell"/"spells" → Card.
        ("spells", TypeFilter::Card),
        ("spell", TypeFilter::Card),
    ];

    // CR 700.12: "outlaw"/"outlaws" is a head noun for the Assassin, Mercenary,
    // Pirate, Rogue, and/or Warlock creature types. Tried before the bare-prefix
    // TYPE_WORDS scan and the subtype table because it expands to a disjunction
    // rather than a single subtype. The word-boundary guard prevents "outlawry"
    // (and similar prefixed words) from matching.
    if let Ok((rest, tf)) = parse_outlaw_type(input) {
        return Ok((rest, tf));
    }

    for &(word, ref tf) in TYPE_WORDS {
        if let Some(rest) = input.strip_prefix(word) {
            // Word-boundary guard (mirrors parse_outlaw_type below and
            // parse_subtype_entry above): a head-noun type word must
            // be followed by end-of-input or a non-alphanumeric char. Without
            // this, a TYPE_WORD that prefixes a longer subtype shadows it — e.g.
            // "land" eating "lander" or "spell" eating "spellshaper" instead of
            // falling through to the boundary-guarded subtype table.
            if rest.is_empty() || rest.starts_with(|c: char| !c.is_alphanumeric()) {
                return Ok((rest, tf.clone()));
            }
        }
    }

    if let Some((subtype, consumed)) = parse_subtype(input) {
        return Ok((&input[consumed..], TypeFilter::Subtype(subtype)));
    }

    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

/// CR 700.12: Parse the "outlaw"/"outlaws" head noun into a disjunction of the
/// Assassin, Mercenary, Pirate, Rogue, and Warlock creature types. Matches the
/// plural form first (longest-match), then requires a non-alphanumeric word
/// boundary so words like "outlawry" never match.
fn parse_outlaw_type(input: &str) -> OracleResult<'_, TypeFilter> {
    let (rest, _) = alt((tag("outlaws"), tag("outlaw"))).parse(input)?;
    match rest.chars().next() {
        // Word boundary: end of input or non-alphanumeric follower.
        None => {}
        Some(c) if !c.is_alphanumeric() => {}
        _ => {
            return Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Fail,
            )))
        }
    }
    let any_of = OUTLAW_SUBTYPES
        .iter()
        .map(|s| TypeFilter::Subtype((*s).to_string()))
        .collect();
    Ok((rest, TypeFilter::AnyOf(any_of)))
}

// --- from parser/oracle_nom/filter.rs ---

/// Parse a zone filter phrase from Oracle text.
///
/// Matches "on the battlefield", "in your graveyard", "in your hand",
/// "in exile", "in your library", and opponent-scoped variants.
pub fn parse_zone_filter(input: &str) -> OracleResult<'_, Zone> {
    alt((
        value(Zone::Battlefield, tag("on the battlefield")),
        value(Zone::Graveyard, tag("in your graveyard")),
        value(Zone::Graveyard, tag("in a graveyard")),
        value(Zone::Graveyard, tag("in their graveyard")),
        value(Zone::Hand, tag("in your hand")),
        value(Zone::Hand, tag("in a player's hand")),
        value(Zone::Hand, tag("from your hand")),
        value(Zone::Exile, tag("in exile")),
        value(Zone::Exile, tag("from exile")),
        value(Zone::Library, tag("in your library")),
        value(Zone::Library, tag("from your library")),
        value(Zone::Stack, tag("on the stack")),
        value(Zone::Graveyard, tag("from your graveyard")),
        value(Zone::Graveyard, tag("from a graveyard")),
        value(Zone::Library, tag("of your library")),
    ))
    .parse(input)
}

/// Parse a property filter from Oracle text.
///
/// Matches object property keywords: "tapped", "untapped", "attacking",
/// "blocking", "token", "face down", "nontoken", "enchanted", "equipped".
pub fn parse_property_filter(input: &str) -> OracleResult<'_, FilterProp> {
    alt((
        value(FilterProp::Tapped, tag("tapped")),
        value(FilterProp::Untapped, tag("untapped")),
        // CR 702.171b: "saddled Mount/creature" selector.
        value(FilterProp::IsSaddled, tag("saddled")),
        value(FilterProp::Attacking { defender: None }, tag("attacking")),
        value(FilterProp::Blocking, tag("blocking")),
        value(FilterProp::Token, tag("token")),
        value(FilterProp::NonToken, tag("nontoken")),
        value(FilterProp::FaceDown, tag("face down")),
        // CR 701.27g: "transformed permanent"/"transformed creature" selector.
        value(FilterProp::Transformed, tag("transformed")),
        value(
            FilterProp::BlockStatus {
                status: AttackerBlockStatus::Unblocked,
            },
            tag("unblocked"),
        ),
        // CR 509.1h: "blocked" as a prefix adjective; postfix "blocked by"/"blocked this
        // turn" are handled elsewhere and must not be consumed here.
        value(
            FilterProp::BlockStatus {
                status: AttackerBlockStatus::Blocked,
            },
            terminated(
                tag("blocked"),
                peek(preceded(tag(" "), not(alt((tag("by"), tag("this turn")))))),
            ),
        ),
        value(FilterProp::Suspected, tag("suspected")),
        value(FilterProp::Renowned, tag("renowned")),
        // CR 701.15b/c: standalone "goaded" designation property token.
        value(FilterProp::Goaded, tag("goaded")),
        value(FilterProp::EnchantedBy, tag("enchanted")),
        value(FilterProp::EquippedBy, tag("equipped")),
        parse_color_property,
        value(
            FilterProp::EnteredThisTurn,
            tag("entered the battlefield this turn"),
        ),
    ))
    .parse(input)
}

/// Parse a color-as-property from Oracle text: "white", "blue", "black", "red", "green",
/// "colorless", "monocolored", "multicolored".
/// Returns a `FilterProp` for the color match.
pub fn parse_color_property(input: &str) -> OracleResult<'_, FilterProp> {
    alt((
        map(tag("white"), |_| FilterProp::HasColor {
            color: ManaColor::White,
        }),
        map(tag("blue"), |_| FilterProp::HasColor {
            color: ManaColor::Blue,
        }),
        map(tag("black"), |_| FilterProp::HasColor {
            color: ManaColor::Black,
        }),
        map(tag("red"), |_| FilterProp::HasColor {
            color: ManaColor::Red,
        }),
        map(tag("green"), |_| FilterProp::HasColor {
            color: ManaColor::Green,
        }),
        value(
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 0,
            },
            tag("colorless"),
        ),
        value(
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 1,
            },
            tag("monocolored"),
        ),
        value(
            FilterProp::ColorCount {
                comparator: Comparator::GE,
                count: 2,
            },
            tag("multicolored"),
        ),
    ))
    .parse(input)
}

// --- from parser/oracle_target.rs ---

/// Guard: does text start with something `parse_type_phrase_folding` would recognize?
/// Used to prevent comma/and/or recursion on non-type text.
pub fn starts_with_type_word(text: &str) -> bool {
    // Core type: "creature", "artifact", "permanent", etc.
    if parse_core_type(text).0.is_some() {
        return true;
    }
    // Subtype: "zombie", "vampires", "elves", etc.
    if parse_subtype(text).is_some() {
        return true;
    }
    // Standalone "token"/"tokens" (property word, not a core type or subtype).
    // Reuses parse_token_suffix which handles singular/plural with word boundary.
    if parse_token_suffix(text).is_some() {
        return true;
    }
    // CR 105.1: Color adjective prefix: "blue creature", "red permanent", etc.
    // parse_type_phrase_folding handles color prefixes internally, but the article guard
    // must recognize them to strip "a "/"an " correctly.
    if let Ok((rest, _)) = parse_color(text) {
        if let Ok((after_space, _)) = tag::<_, _, OracleError<'_>>(" ").parse(rest) {
            if starts_with_type_word(after_space) {
                return true;
            }
        }
    }
    // CR 105.2b/c: Color-quality adjective prefix: "multicolored card",
    // "colorless creature", etc.
    if let Some((_prop, consumed)) = parse_color_quality_prefix(text) {
        if starts_with_type_word(&text[consumed..]) {
            return true;
        }
    }
    // CR 205.2a + CR 205.3: Negated type prefix: "noncreature spell", "nonland permanent",
    // "non-Saga token" (Good King Mog XII chapter II — issue #3294), and
    // negated-adjective compounds like "nontoken modified creature" (Akki
    // Ember-Keeper / issue #3677 class) or "nontoken legendary permanent"
    // (Cadric, Soul Kindler). Recurses into `starts_with_type_phrase_lead`
    // (rather than only `parse_core_type`/`parse_token_suffix`) so the article
    // guard recognizes every adjective that can lead a type phrase after a
    // `non-` prefix — color, supertype, "modified", "renowned", "historic" —
    // not just bare core types and tokens.
    if let Ok((after_non, _)) = alt((tag::<_, _, OracleError<'_>>("non-"), tag("non"))).parse(text)
    {
        // Consume the negated word up to whitespace, then check what follows.
        if let Ok((after_space, _)) = (
            take_till::<_, _, OracleError<'_>>(|c: char| c.is_whitespace()),
            tag::<_, _, OracleError<'_>>(" "),
        )
            .parse(after_non)
        {
            if starts_with_type_phrase_lead(after_space) {
                return true;
            }
        }
    }
    // CR 700.9: "modified <type>" adjective phrase leads a type
    // phrase (e.g., "modified creatures you control"). Consume the adjective
    // and verify a type word follows so the comma/and-list recursion can
    // continue across the "modified" leg.
    if let Ok((after_modified, _)) = tag::<_, _, OracleError<'_>>("modified ").parse(text) {
        if starts_with_type_phrase_lead(after_modified) {
            return true;
        }
    }
    // CR 702.112b: "renowned <type>" adjective phrase leads a type phrase.
    if let Ok((after_renowned, _)) = tag::<_, _, OracleError<'_>>("renowned ").parse(text) {
        if starts_with_type_phrase_lead(after_renowned) {
            return true;
        }
    }
    // CR 700.6: "historic <type>" adjective phrase leads a type phrase
    // (e.g., "historic permanents you control"). Consume the adjective and
    // verify a type word follows so the comma/and-list recursion can continue
    // across the "historic" leg.
    if let Ok((after_historic, _)) = tag::<_, _, OracleError<'_>>("historic ").parse(text) {
        if starts_with_type_phrase_lead(after_historic) {
            return true;
        }
    }
    false
}

pub fn starts_with_type_phrase_lead(text: &str) -> bool {
    let text = text.trim_start();
    starts_with_type_word(text)
        || parse_supertype_prefix(text).is_ok()
        || parse_color_prefix(text).is_some()
        || parse_color_quality_prefix(text).is_some()
        || parse_combat_status_prefix(text).is_some()
        // CR 208.1 (#2912): "1/1 creature" leads a type phrase (the P/T
        // designation is followed by a type word).
        || parse_leading_pt_designation(text).is_some()
}

pub fn parse_core_type(text: &str) -> (Option<TypeFilter>, Option<String>, usize) {
    // Delegate to the shared nom combinator table which handles both singular
    // and plural forms in longest-match-first order.
    if let Ok((rest, tf)) = parse_type_filter_word(text) {
        let consumed = text.len() - rest.len();
        return (Some(tf), None, consumed);
    }

    (None, None, 0)
}

pub fn parse_token_suffix(text: &str) -> Option<usize> {
    let trimmed = text.trim_start();

    // Try "tokens" before "token" (longest match first), with word boundary.
    for word in &["tokens", "token"] {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(*word).parse(trimmed) {
            match rest.chars().next() {
                None | Some(' ' | ',' | '.') => return Some(text.len() - rest.len()),
                _ => {}
            }
        }
    }

    None
}

/// Parse a color adjective prefix: "white ", "blue ", "black ", "red ", "green ".
/// Returns (FilterProp::HasColor, bytes consumed including trailing space).
///
/// Delegates to `parse_color` for color word recognition,
/// then verifies a trailing space exists (color as adjective, not standalone).
pub fn parse_color_prefix(text: &str) -> Option<(FilterProp, usize)> {
    let (rest, color) = parse_color(text).ok()?;
    // CR 105.1: A color word is an adjective prefix only when a separator
    // follows, so a bare color word ("whiteness") never matches. Two separators
    // are accepted:
    //   * a trailing space — the ordinary "white creature" prefix (consumed);
    //   * a comma — the color-list continuation "white, blue, or black
    //     creature", where the comma is left in place for the `TYPE_SEPARATORS`
    //     recursion to consume as a ", " / ", or " disjunction separator. That
    //     recursion + `distribute_core_type_to_or` then assemble the ≥3-color
    //     prenominal chain into the same Or-of-legs shape the 2-color "green or
    //     white creature" form already produces, with the core type backfilled
    //     onto every color-only leg.
    let consumed = if let Ok((after_space, _)) = tag::<_, _, OracleError<'_>>(" ").parse(rest) {
        text.len() - after_space.len()
    } else if peek(tag::<_, _, OracleError<'_>>(",")).parse(rest).is_ok() {
        // Comma left in place for the `TYPE_SEPARATORS` recursion to consume.
        text.len() - rest.len()
    } else {
        return None;
    };
    Some((FilterProp::HasColor { color }, consumed))
}

/// Parse color-quality adjective prefixes: "colorless creature",
/// "monocolored permanent", "multicolored card", etc.
/// Returns the filter property and bytes consumed including trailing space.
pub fn parse_color_quality_prefix(text: &str) -> Option<(FilterProp, usize)> {
    let (rest, prop) = alt((
        value(
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 0,
            },
            tag::<_, _, OracleError<'_>>("colorless "),
        ),
        value(
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 1,
            },
            tag("monocolored "),
        ),
        value(
            FilterProp::ColorCount {
                comparator: Comparator::GE,
                count: 2,
            },
            tag("multicolored "),
        ),
    ))
    .parse(text)
    .ok()?;
    Some((prop, text.len() - rest.len()))
}

/// CR 208.1 (#2912): Parse a leading "N/M " power/toughness designation
/// ("1/1 creature", "2/2 creatures") into fixed `(power, toughness)` plus the
/// bytes consumed (including the trailing space). Only matches when a type word
/// follows, so a bare "1/1" elsewhere is not hijacked. Fixed integers only;
/// dynamic "*/*" / "X/X" designations are left to the existing P/T paths.
pub fn parse_leading_pt_designation(input: &str) -> Option<(i32, i32, usize)> {
    let (after_power, power) = parse_number(input).ok()?;
    let (after_slash, _) = tag::<_, _, OracleError<'_>>("/").parse(after_power).ok()?;
    let (after_toughness, toughness) = parse_number(after_slash).ok()?;
    let (after_space, _) = tag::<_, _, OracleError<'_>>(" ")
        .parse(after_toughness)
        .ok()?;
    if !starts_with_type_phrase_lead(after_space) {
        return None;
    }
    Some((
        power as i32,
        toughness as i32,
        input.len() - after_space.len(),
    ))
}

/// CR 509.1h / CR 302.6 / CR 701.60b: Parse status prefixes from type phrases.
/// Called in a loop to consume multiple prefixes (e.g. "unblocked attacking ").
/// Handles combat status (attacking, unblocked), tap status (tapped, untapped),
/// and designation status (suspected — CR 701.60b).
///
/// Delegates to `parse_property_filter` for the common property keywords,
/// then handles "face-down " (hyphenated variant not in the nom combinator).
pub fn parse_combat_status_prefix(text: &str) -> Option<(FilterProp, usize)> {
    // Try the shared nom property filter combinator for combat/tap status keywords.
    // Filter to only the status properties relevant as type phrase prefixes.
    if let Ok((rest, prop)) = parse_property_filter(text) {
        if matches!(
            prop,
            FilterProp::BlockStatus { .. }
                | FilterProp::Attacking { defender: None }
                | FilterProp::Blocking
                | FilterProp::Tapped
                | FilterProp::Untapped
                // CR 702.171b: "saddled" designation as a type-phrase prefix
                // ("saddled Mount", "saddled creature").
                | FilterProp::IsSaddled
                | FilterProp::ProtectorMatches { .. }
                | FilterProp::FaceDown
                // CR 701.27g: "transformed" is a battlefield designation that appears
                // as an adjective prefix in type phrases ("transformed permanent",
                // Mutagen Connoisseur).
                | FilterProp::Transformed
                // CR 701.60b: "suspected" is a battlefield designation that appears
                // as an adjective prefix in type phrases ("suspected creatures").
                | FilterProp::Suspected
        ) {
            // Must be followed by space (prefix, not standalone)
            if let Ok((after_space, _)) = tag::<_, _, OracleError<'_>>(" ").parse(rest) {
                return Some((prop, text.len() - after_space.len()));
            }
        }
    }

    // Handle "face-down " (hyphenated variant not in the nom combinator).
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("face-down ").parse(text) {
        return Some((FilterProp::FaceDown, text.len() - rest.len()));
    }

    None
}

/// A keyword-list separator followed by something other than a keyword, where
/// that something opens another leg of the enclosing list — a type word ("creature
/// with disturb, or enchantment") or a determiner-led leg ("each creature without
/// flying and each planeswalker", "target creature with flying and all Equipment
/// attached to that creature") — belongs to the enclosing list and stays
/// unconsumed.
pub fn separator_continues_enclosing_list(after_separator: &str) -> bool {
    parse_leading_keyword_match(after_separator).is_none()
        && (starts_with_type_word(after_separator)
            || alt((
                tag::<_, _, OracleError<'_>>("each "),
                tag("all "),
                tag("target "),
                tag("up to "),
            ))
            .parse(after_separator)
            .is_ok())
}

/// Parse "without [keyword]" suffix — negated keyword filter.
/// Handles "without flying", "without first strike", etc.
/// Parallels `parse_keyword_suffix` but emits `WithoutKeyword`.
pub fn parse_without_keyword_suffix(text: &str) -> Option<(Vec<FilterProp>, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();
    let (after_without, _) = tag::<_, _, OracleError<'_>>("without ")
        .parse(trimmed)
        .ok()?;
    let mut remaining = after_without;
    let mut consumed = leading_ws + "without ".len();
    let mut properties = Vec::new();

    while let Some((keyword_match, keyword_len)) = parse_leading_keyword_match(remaining) {
        match keyword_match {
            KeywordMatch::Concrete(keyword) => {
                properties.push(FilterProp::WithoutKeyword { value: keyword });
            }
            KeywordMatch::Kind(kind) => {
                properties.push(FilterProp::WithoutKeywordKind { value: kind });
            }
        }
        consumed += keyword_len;
        remaining = &remaining[keyword_len..];

        // Try keyword list separators in longest-match-first order.
        let mut found_sep = false;
        for sep in &[", and ", ", or ", " and ", " or ", ", "] {
            if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(*sep).parse(remaining) {
                if separator_continues_enclosing_list(rest) {
                    break;
                }
                consumed += sep.len();
                remaining = rest;
                found_sep = true;
                break;
            }
        }
        if !found_sep {
            break;
        }
    }

    if properties.is_empty() {
        None
    } else {
        Some((properties, consumed))
    }
}

pub enum KeywordMatch {
    Concrete(Keyword),
    Kind(KeywordKind),
}

pub fn parse_leading_keyword_match(text: &str) -> Option<(KeywordMatch, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();
    let mut candidate_ends = vec![trimmed.len()];

    for (idx, ch) in trimmed.char_indices() {
        if matches!(ch, ' ' | ',' | '.') {
            candidate_ends.push(idx);
        }
    }

    candidate_ends.sort_unstable();
    candidate_ends.dedup();

    for end in candidate_ends.into_iter().rev() {
        let candidate = trimmed[..end].trim();
        if let Some(keyword) = parse_keyword_match(candidate) {
            return Some((keyword, leading_ws + end));
        }
    }

    None
}

fn parse_keyword_match(text: &str) -> Option<KeywordMatch> {
    if let Ok((rest, kind)) = value(
        KeywordKind::Disturb,
        tag::<_, _, OracleError<'_>>("disturb"),
    )
    .parse(text)
    {
        if rest.is_empty() {
            return Some(KeywordMatch::Kind(kind));
        }
    }

    if let Ok((rest, kind)) = value(
        KeywordKind::Augment,
        tag::<_, _, OracleError<'_>>("augment"),
    )
    .parse(text)
    {
        if rest.is_empty() {
            return Some(KeywordMatch::Kind(kind));
        }
    }

    // CR 702.140: Mutate is a parameterized keyword (`Mutate(ManaCost)`), so the
    // `Keyword::from_str` fallback below would yield `Concrete(Keyword::Mutate(cost))`
    // and force an exact-cost match. Text like "creature card with mutate" refers to the
    // keyword class regardless of cost, so map it to the discriminant-level `Kind`.
    if let Ok((rest, kind)) =
        value(KeywordKind::Mutate, tag::<_, _, OracleError<'_>>("mutate")).parse(text)
    {
        if rest.is_empty() {
            return Some(KeywordMatch::Kind(kind));
        }
    }

    // CR 702.168: Disguise is a parameterized keyword (`Disguise(ManaCost)`), so
    // the `Keyword::from_str` fallback would yield a concrete `Keyword::Disguise(cost)`
    // and force an exact-cost match. "creatures you control with disguise" names
    // the keyword class regardless of cost, so map it to the discriminant `Kind`.
    if let Ok((rest, kind)) = value(
        KeywordKind::Disguise,
        tag::<_, _, OracleError<'_>>("disguise"),
    )
    .parse(text)
    {
        if rest.is_empty() {
            return Some(KeywordMatch::Kind(kind));
        }
    }

    // CR 702.113: "card with awaken" (and the other parameterized graveyard/cast
    // keywords) is a keyword-presence meta-reference that must match by
    // discriminant, not exact payload — a `WithKeyword(Awaken { count, cost })`
    // would never match a real instance. Route to `KeywordMatch::Kind`.
    if matches!(
        text,
        "flashback"
            | "cycling"
            | "escape"
            | "embalm"
            | "eternalize"
            | "harmonize"
            | "unearth"
            | "awaken"
            | "foretell"
            | "miracle"
    ) {
        let kind = match text {
            "flashback" => KeywordKind::Flashback,
            "cycling" => KeywordKind::Cycling,
            "escape" => KeywordKind::Escape,
            "embalm" => KeywordKind::Embalm,
            "eternalize" => KeywordKind::Eternalize,
            "harmonize" => KeywordKind::Harmonize,
            "unearth" => KeywordKind::Unearth,
            "awaken" => KeywordKind::Awaken, // allow-noncombinator: normalized keyword-token -> KeywordKind lookup (finite set, gated by matches! above; mirrors flashback/cycling arms), not Oracle-text dispatch
            // CR 702.143 / CR 702.94: "card in your hand without foretell" and the
            // miracle analogue are keyword-presence meta-references — match by
            // discriminant so a granted (cost-bearing) instance still matches.
            "foretell" => KeywordKind::Foretell, // allow-noncombinator: normalized keyword-token -> KeywordKind lookup (finite set, gated by matches! above), not Oracle-text dispatch
            "miracle" => KeywordKind::Miracle, // allow-noncombinator: normalized keyword-token -> KeywordKind lookup (finite set, gated by matches! above; mirrors flashback/cycling arms), not Oracle-text dispatch
            _ => unreachable!(),
        };
        return Some(KeywordMatch::Kind(kind));
    }

    let keyword = Keyword::from_str(text).ok()?;
    if matches!(keyword, Keyword::Unknown(_))
        && !matches!(
            text,
            "plainswalk" | "islandwalk" | "swampwalk" | "mountainwalk" | "forestwalk"
        )
    {
        return None;
    }

    Some(KeywordMatch::Concrete(keyword))
}

// --- from parser/oracle_nom/enchant.rs ---

/// CR 702.5a: One enchantable core-type or supported subtype token. Core
/// types and established basic-land subtype legs stay as literal nom arms;
/// artifact subtypes delegate to the canonical subtype classifier below.
///
/// Basic land subtypes (Forest, Plains, Island, Swamp, Mountain) are included
/// per CR 205.3i — basic land types are the canonical Aura targets for
/// "enchant Forest" / "enchant Plains" patterns used by Old-Growth Troll
/// (KHM) and Harold and Bob, First Numens (FIN-precon). The longest-first
/// ordering inside each cluster keeps "creature" from short-matching against
/// future hypothetical subtype legs.
pub fn parse_enchant_type_leg(input: &str) -> OracleResult<'_, TypeFilter> {
    alt((
        value(TypeFilter::Creature, tag("creature")),
        value(TypeFilter::Artifact, tag("artifact")),
        // CR 205.3g + CR 702.5a: Artifact subtype legs use the canonical
        // subtype registry. This must precede `land` so `Lander` is not
        // short-matched as the core Land type.
        parse_artifact_subtype_enchant_leg,
        value(TypeFilter::Land, tag("land")),
        value(TypeFilter::Enchantment, tag("enchantment")),
        value(TypeFilter::Planeswalker, tag("planeswalker")),
        value(TypeFilter::Permanent, tag("permanent")),
        // CR 702.5a: Instant / Sorcery enable hand- and graveyard-zoned Auras
        // like Spellweaver Volute ("Enchant instant card in a graveyard").
        value(TypeFilter::Instant, tag("instant")),
        value(TypeFilter::Sorcery, tag("sorcery")),
        // CR 205.3i + CR 702.5a: Basic land subtypes. Used by
        // "enchant Forest you control" (Old-Growth Troll, Harold and Bob).
        value(TypeFilter::Subtype("Forest".to_string()), tag("forest")),
        value(TypeFilter::Subtype("Plains".to_string()), tag("plains")),
        value(TypeFilter::Subtype("Island".to_string()), tag("island")),
        value(TypeFilter::Subtype("Swamp".to_string()), tag("swamp")),
        value(TypeFilter::Subtype("Mountain".to_string()), tag("mountain")),
    ))
    .parse(input)
}

fn parse_artifact_subtype_enchant_leg(input: &str) -> OracleResult<'_, TypeFilter> {
    let Some((subtype, consumed)) = parse_subtype(input) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };

    if !matches!(
        noncreature_subtype_set(&subtype),
        Some(SubtypeSet::Artifact)
    ) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }

    Ok((&input[consumed..], TypeFilter::Subtype(subtype)))
}

/// CR 205.4a + CR 702.5a: An Enchant type leg may carry a supertype adjective
/// such as "snow land", "basic land", or "legendary creature", or a
/// tapped-status adjective ("Enchant tapped creature", Entangling Vines). Reuse the
/// shared property and supertype recognizers so Aura legality gets the same
/// properties as ordinary target phrases.
pub fn parse_enchant_qualified_type_leg(input: &str) -> OracleResult<'_, TypedFilter> {
    let (input, status) = opt(map_opt(
        terminated(parse_property_filter, space1),
        |prop| match prop {
            FilterProp::Tapped | FilterProp::Untapped => Some(prop),
            _ => None,
        },
    ))
    .parse(input)?;
    let (input, supertype) = opt(parse_supertype_prefix).parse(input)?;
    let (input, excluded_subtype) = opt(parse_enchant_negated_subtype_prefix).parse(input)?;
    let (input, type_filter) = parse_enchant_type_leg(input)?;
    let mut typed = TypedFilter::new(type_filter);
    if let Some(excluded) = excluded_subtype {
        typed = typed.with_type(excluded);
    }
    let properties = status
        .into_iter()
        .chain(supertype.map(|value| FilterProp::HasSupertype { value }))
        .collect();
    Ok((input, typed.properties(properties)))
}

/// CR 702.5a + CR 205.3a: A negated canonical subtype adjective restricts
/// its owning Enchant leg. Preserve the positive head and conjoin the
/// exclusion, rather than excluding that subtype from every union leg.
fn parse_enchant_negated_subtype_prefix(input: &str) -> OracleResult<'_, TypeFilter> {
    map_opt(
        terminated(preceded(parse_non_prefix, parse_type_filter_word), space1),
        |type_filter| match type_filter {
            subtype @ TypeFilter::Subtype(_) => Some(TypeFilter::Non(Box::new(subtype))),
            _ => None,
        },
    )
    .parse(input)
}

/// Optional trailing controller clause. Ordered longest-first so
/// "an opponent controls" isn't shadowed by "opponent controls".
pub fn parse_enchant_controller_suffix(input: &str) -> OracleResult<'_, ControllerRef> {
    alt((
        value(ControllerRef::You, tag(" you control")),
        value(ControllerRef::Opponent, tag(" an opponent controls")),
        value(ControllerRef::Opponent, tag(" opponent controls")),
    ))
    .parse(input)
}

/// CR 303.4 + CR 702.5a + CR 301.5: Optional trailing attachment qualifier on an
/// "Enchant <type>" line — "with another Aura attached to it" (Daybreak Coronet)
/// further restricts the legal target set to objects that already carry an
/// attachment of the named kind. "Another" is material once SBA attachment
/// legality rechecks the Aura already attached to its host, so preserve it as a
/// source-exclusion axis on the `HasAttachment` filter prop. The leading space
/// ensures the qualifier only matches after a preceding type leg (never as a
/// standalone clause).
pub fn parse_enchant_attachment_qualifier(input: &str) -> OracleResult<'_, FilterProp> {
    let (input, _) = tag(" with ").parse(input)?;
    let (input, exclude_source) = alt((
        value(true, tag("another ")),
        value(false, tag("an ")),
        value(false, tag("a ")),
    ))
    .parse(input)?;
    let (input, kind) = alt((
        value(AttachmentKind::Aura, tag("aura")),
        value(AttachmentKind::Equipment, tag("equipment")),
    ))
    .parse(input)?;
    let (input, _) = tag(" attached to it").parse(input)?;
    Ok((
        input,
        FilterProp::HasAttachment {
            kind,
            controller: None,
            exclude_source: if exclude_source {
                crate::types::ability::SourceExclusion::Exclude
            } else {
                crate::types::ability::SourceExclusion::Include
            },
        },
    ))
}

/// CR 702.5d: "Enchant player" / "Enchant opponent" — the player-axis Aura.
/// The two legs yield the typed `TargetFilter` the rest of the cast pipeline
/// expects. "Enchant player" → `TargetFilter::Player` (any player at the
/// table); "Enchant opponent" → typed filter scoped to opposing players.
pub(crate) fn parse_enchant_player_base(input: &str) -> OracleResult<'_, TargetFilter> {
    alt((
        value(TargetFilter::Player, tag("player")),
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            tag("opponent"),
        ),
    ))
    .parse(input)
}

// --- from parser/oracle_keyword.rs ---

/// CR 702.167b: Build the typed materials filter for a Craft ability. A bare
/// type/subtype in the materials clause matches *either* a permanent on the
/// battlefield you control *or* a card in your graveyard you own (an exception
/// to CR 109.2). The result is a `TargetFilter::Or` of those two zone-scoped
/// legs so the dual-zone eligibility helper and the runtime filter evaluator
/// agree on what may be exiled. This is the single authority for the materials
/// filter shape — `FromStr`, `keyword_from_tagged`, and the Oracle-line parser
/// all route through it.
pub fn craft_materials_filter(types: &[TypeFilter]) -> TargetFilter {
    craft_materials_from_typed_filter(TypedFilter {
        type_filters: types.to_vec(),
        ..TypedFilter::default()
    })
}

pub fn craft_materials_from_typed_filter(filter: TypedFilter) -> TargetFilter {
    let with_types = |base: TypedFilter| -> TypedFilter {
        filter
            .type_filters
            .iter()
            .cloned()
            .fold(base, |acc, tf| acc.with_type(tf))
    };
    TargetFilter::Or {
        filters: vec![
            // Battlefield leg: a permanent you control matching the printed materials class.
            TargetFilter::Typed(
                with_types(TypedFilter::permanent())
                    .controller(ControllerRef::You)
                    .properties({
                        let mut props = filter.properties.clone();
                        props.push(FilterProp::InZone {
                            zone: Zone::Battlefield,
                        });
                        props
                    }),
            ),
            // Graveyard leg: a card you own matching the printed materials class.
            TargetFilter::Typed(with_types(TypedFilter::card()).properties({
                let mut props = filter.properties.clone();
                props.push(FilterProp::InZone {
                    zone: Zone::Graveyard,
                });
                props.push(FilterProp::Owned {
                    controller: ControllerRef::You,
                });
                props
            })),
        ],
    }
}

/// CR 702.167b: Default materials class (creature) used when only the bare
/// "Craft" keyword is available (no Oracle line to specify the materials).
pub fn craft_materials_default() -> TargetFilter {
    craft_materials_filter(&[TypeFilter::Creature])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CR 702.113: "card with awaken" is a parameterized-keyword presence
    /// meta-reference and must map to `KeywordMatch::Kind(Awaken)` (matched by
    /// discriminant), not an exact-payload `WithKeyword` that never matches a
    /// real `Awaken { count, cost }`. Mirrors the flashback/cycling/escape arms.
    #[test]
    fn parse_keyword_match_awaken_is_kind() {
        assert!(matches!(
            parse_keyword_match("awaken"),
            Some(KeywordMatch::Kind(KeywordKind::Awaken))
        ));
    }
}
