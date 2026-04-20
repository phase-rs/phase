use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::{tag, tag_no_case};
use nom::character::complete::{multispace0, multispace1, satisfy};
use nom::combinator::{opt, peek, recognize, value};
use nom::multi::{many0, separated_list1};
use nom::sequence::{pair, preceded};
use nom::Parser;

use super::super::oracle_nom::error::OracleResult;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_util::split_around;
use super::token::{
    map_token_keyword, push_unique_string, split_token_keyword_list, title_case_word,
};
use super::types::*;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;

pub(super) fn parse_animation_spec(text: &str) -> Option<AnimationSpec> {
    let lower = text.to_lowercase();
    if lower.contains(" copy of ")
        || lower.contains(" of your choice")
        || lower.contains(" all activated abilities ")
        || lower.contains(" loses all other card types ")
        || lower.contains(" all colors")
    {
        return None;
    }

    let mut spec = AnimationSpec::default();
    let mut rest = text.trim().trim_end_matches('.');

    // Check for ability-loss suffixes using pre-lowered text
    let rest_lower = rest.to_lowercase();
    for suffix in [
        " and loses all other abilities",
        " and it loses all other abilities",
        " and loses all abilities",
    ] {
        if rest_lower.ends_with(suffix) {
            let end = rest.len() - suffix.len();
            rest = rest[..end].trim_end_matches(',').trim();
            spec.remove_all_abilities = true;
            break;
        }
    }

    if let Some(stripped) = rest.strip_prefix("a ") {
        rest = stripped;
    } else if let Some(stripped) = rest.strip_prefix("an ") {
        rest = stripped;
    }

    if let Some((power, toughness, after_pt)) = parse_fixed_become_pt_prefix(rest) {
        spec.power = Some(power);
        spec.toughness = Some(toughness);
        rest = after_pt;
    }

    if let Some((descriptor, power, toughness)) = split_animation_base_pt_clause(rest) {
        spec.power = Some(power);
        spec.toughness = Some(toughness);
        rest = descriptor;
    }

    let (descriptor, keywords) = split_animation_keyword_clause(rest);
    spec.keywords = keywords;
    rest = descriptor;

    if let Some((colors, after_colors)) = parse_animation_color_prefix(rest) {
        spec.colors = Some(colors);
        rest = after_colors;
    }

    spec.types = parse_animation_types(rest, spec.power.is_some() || spec.toughness.is_some());

    if spec.power.is_none()
        && spec.toughness.is_none()
        && spec.colors.is_none()
        && spec.keywords.is_empty()
        && spec.types.is_empty()
        && !spec.remove_all_abilities
    {
        None
    } else {
        Some(spec)
    }
}

pub(super) fn animation_modifications(
    spec: &AnimationSpec,
) -> Vec<crate::types::ability::ContinuousModification> {
    use crate::types::ability::ContinuousModification;
    use crate::types::card_type::CoreType;

    let mut modifications = Vec::new();

    if let Some(power) = spec.power {
        modifications.push(ContinuousModification::SetPower { value: power });
    }
    if let Some(toughness) = spec.toughness {
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }
    if let Some(colors) = &spec.colors {
        modifications.push(ContinuousModification::SetColor {
            colors: colors.clone(),
        });
    }
    if spec.remove_all_abilities {
        modifications.push(ContinuousModification::RemoveAllAbilities);
    }
    for keyword in &spec.keywords {
        modifications.push(ContinuousModification::AddKeyword {
            keyword: keyword.clone(),
        });
    }
    for type_name in &spec.types {
        if let Ok(core_type) = CoreType::from_str(type_name) {
            modifications.push(ContinuousModification::AddType { core_type });
        } else {
            modifications.push(ContinuousModification::AddSubtype {
                subtype: type_name.clone(),
            });
        }
    }

    modifications
}

/// Parse a color word prefix from animation text, handling "colorless" and
/// the five MTG colors.
///
/// Delegates color word recognition to `nom_primitives::parse_color` for the
/// five named colors, with manual handling for "colorless" (no `ManaColor`).
fn parse_animation_color_prefix(text: &str) -> Option<(Vec<ManaColor>, &str)> {
    let mut rest = text.trim_start();
    let mut saw_color = false;
    let mut colors = Vec::new();

    loop {
        if let Some(stripped) = strip_prefix_word(rest, "colorless") {
            saw_color = true;
            rest = stripped;
        } else {
            // Delegate the five named colors to nom combinator
            let lower = rest.to_lowercase();
            if let Ok((rest_lower, color)) = nom_primitives::parse_color.parse(&lower) {
                let consumed = lower.len() - rest_lower.len();
                let after = &rest[consumed..];
                // Word boundary: color word must be followed by whitespace or end
                if after.is_empty() || after.starts_with(char::is_whitespace) {
                    saw_color = true;
                    colors.push(color);
                    rest = after.trim_start();
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        if let Some(stripped) = rest.strip_prefix("and ") {
            rest = stripped;
            continue;
        }
        break;
    }

    saw_color.then_some((colors, rest.trim_start()))
}

fn strip_prefix_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(word)?;
    if rest.is_empty() {
        Some(rest)
    } else if rest.starts_with(' ') {
        Some(rest.trim_start())
    } else {
        None
    }
}

pub(super) fn parse_fixed_become_pt_prefix(text: &str) -> Option<(i32, i32, &str)> {
    let (power, toughness, rest) = parse_token_pt_prefix(text)?;
    match (power, toughness) {
        (
            crate::types::ability::PtValue::Fixed(power),
            crate::types::ability::PtValue::Fixed(toughness),
        ) => Some((power, toughness, rest)),
        _ => None,
    }
}

fn parse_token_pt_prefix(
    text: &str,
) -> Option<(
    crate::types::ability::PtValue,
    crate::types::ability::PtValue,
    &str,
)> {
    let text = text.trim_start();
    let word_end = text.find(char::is_whitespace).unwrap_or(text.len());
    let token = &text[..word_end];
    let slash = token.find('/')?;
    let power = token[..slash].trim();
    let toughness = token[slash + 1..].trim();
    let power = parse_token_pt_component(power)?;
    let toughness = parse_token_pt_component(toughness)?;
    Some((power, toughness, text[word_end..].trim_start()))
}

fn parse_token_pt_component(text: &str) -> Option<crate::types::ability::PtValue> {
    if text.eq_ignore_ascii_case("x") {
        return Some(crate::types::ability::PtValue::Variable("X".to_string()));
    }
    text.parse::<i32>()
        .ok()
        .map(crate::types::ability::PtValue::Fixed)
}

fn split_animation_base_pt_clause(text: &str) -> Option<(&str, i32, i32)> {
    const NEEDLE: &str = " with base power and toughness ";
    let lower = text.to_lowercase();
    let (before, _) = split_around(&lower, NEEDLE)?;
    let pos = before.len();
    let descriptor = text[..pos].trim_end_matches(',').trim();
    let pt_text = text[pos + NEEDLE.len()..].trim();
    let (power, toughness, _) = parse_fixed_become_pt_prefix(pt_text)?;
    Some((descriptor, power, toughness))
}

/// Classification of a single token within a "becomes [type expression]" noun
/// phrase. Encodes the full design space so callers can't conflate core types
/// (emitted as `AddType`) with subtypes (emitted as `AddSubtype`) or leak
/// supertypes (recognized-but-discarded: animations never change supertypes).
#[derive(Debug, Clone, PartialEq, Eq)]
enum AnimationTypeToken {
    /// CR 205.2a core type — maps to `ContinuousModification::AddType`.
    CoreType(&'static str),
    /// CR 205.3 subtype — maps to `ContinuousModification::AddSubtype`.
    Subtype(String),
    /// CR 205.4 supertype — recognized to avoid halting the sequence, but
    /// not emitted as a modification (animations don't grant supertypes).
    Supertype,
}

/// Zero-width word-boundary check: next char must be non-alphabetic (whitespace,
/// punctuation, or end-of-input). Mirrors the pattern used by `parse_article_number`
/// and `parse_keyword_name` to prevent "land" from swallowing "landwalk".
fn alpha_word_boundary(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        peek(alt((
            nom::combinator::eof,
            recognize(satisfy(|c: char| !c.is_ascii_alphabetic())),
        ))),
    )
    .parse(input)
}

/// Parse a CR 205.2a core type keyword (case-insensitive, word-boundary terminated).
fn parse_animation_core_type(input: &str) -> OracleResult<'_, AnimationTypeToken> {
    let (rest, core) = alt((
        value("Artifact", tag_no_case("artifact")),
        value("Creature", tag_no_case("creature")),
        value("Enchantment", tag_no_case("enchantment")),
        value("Land", tag_no_case("land")),
        value("Planeswalker", tag_no_case("planeswalker")),
    ))
    .parse(input)?;
    let (rest, _) = alpha_word_boundary(rest)?;
    Ok((rest, AnimationTypeToken::CoreType(core)))
}

/// Parse a CR 205.4 supertype keyword (case-insensitive, word-boundary terminated).
fn parse_animation_supertype(input: &str) -> OracleResult<'_, AnimationTypeToken> {
    let (rest, _) = alt((
        tag_no_case("legendary"),
        tag_no_case("basic"),
        tag_no_case("snow"),
    ))
    .parse(input)?;
    let (rest, _) = alpha_word_boundary(rest)?;
    Ok((rest, AnimationTypeToken::Supertype))
}

/// Parse a CR 205.3 subtype: a capitalized proper-noun word of length ≥ 2,
/// optionally hyphenated (`Power-Plant`, `Lhurgoyf`). Rejects single-letter
/// tokens (`X` in "X/X"), lowercase connectives (`and`, `gets`, `gains`,
/// `until`), and mid-word positions (if followed by `/`, `:`, digits, etc.).
fn parse_animation_subtype(input: &str) -> OracleResult<'_, AnimationTypeToken> {
    let (rest, word) = recognize(pair(
        // First char: capital letter.
        satisfy(|c: char| c.is_ascii_uppercase()),
        // Second char must be alphabetic (no leading-hyphen tokens like "A-B").
        // Subsequent chars may be alphabetic or hyphen (for "Power-Plant").
        pair(
            satisfy(|c: char| c.is_ascii_alphabetic()),
            many0(satisfy(|c: char| c.is_ascii_alphabetic() || c == '-')),
        ),
    ))
    .parse(input)?;
    // Word-boundary: reject follow-ups like `/`, `:`, digits, `{`, `+`, `"` —
    // these indicate we landed mid-P/T-token (`Dragon3/3`) or mid-cost (`B:`).
    let (rest, _) = peek(alt((
        nom::combinator::eof,
        recognize(satisfy(|c: char| {
            c.is_whitespace() || matches!(c, ',' | '.' | ';' | ')' | '!' | '?')
        })),
    )))
    .parse(rest)?;
    Ok((rest, AnimationTypeToken::Subtype(word.to_string())))
}

fn parse_animation_type_token(input: &str) -> OracleResult<'_, AnimationTypeToken> {
    alt((
        parse_animation_core_type,
        parse_animation_supertype,
        parse_animation_subtype,
    ))
    .parse(input)
}

/// Parse a whitespace-separated sequence of type tokens, halting at the first
/// non-type token. Used by [`parse_animation_types`] as the grammar root.
fn parse_animation_type_sequence(input: &str) -> OracleResult<'_, Vec<AnimationTypeToken>> {
    separated_list1(multispace1, parse_animation_type_token).parse(input)
}

/// CR 205.1a + CR 205.2 + CR 205.3 + CR 613.1c: Decompose a "becomes a
/// [subtype]* [core-type]+ [in addition to its other types]?" descriptor into
/// a list of typed `ContinuousModification`s.
///
/// Built on the shared `parse_animation_type_sequence` combinator so callers
/// outside the effect-animation path (e.g., static-ability parsing of
/// "target creature ... becomes a Horror enchantment creature in addition to
/// its other types") get the same type-line decomposition: one `AddType` per
/// CR 205.2 core type, one `AddSubtype` per CR 205.3 subtype, supertypes
/// discarded (CR 205.4 — animations never grant supertypes).
///
/// The descriptor is the noun phrase *after* the "becomes a"/"becomes an"
/// article and *before* any trailing "in addition to its other types" clause.
/// Input must preserve original casing because the CR 205.3 subtype grammar
/// requires capitalized proper nouns.
pub(crate) fn parse_becomes_type_modifications(
    descriptor: &str,
) -> Vec<crate::types::ability::ContinuousModification> {
    use crate::types::ability::ContinuousModification;
    use crate::types::card_type::CoreType;

    let trimmed = descriptor
        .trim()
        .trim_end_matches('.')
        .trim_end_matches(',')
        .trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    // Forward-parse: type-token sequence (halts at first non-classifying word
    // such as "in"), then optionally consume a trailing "in addition to its
    // other [creature] types" clause. The longer alternative is tried first
    // because nom's alt() is short-circuit. See oracle_nom/PATTERNS.md
    // ("Optional trailing clause after a token sequence").
    let mut parser = (
        parse_animation_type_sequence,
        opt(preceded(
            multispace0,
            alt((
                tag("in addition to its other creature types"),
                tag("in addition to its other types"),
            )),
        )),
    );
    let tokens = match parser.parse(trimmed) {
        Ok((_, (tokens, _))) => tokens,
        Err(_) => return Vec::new(),
    };

    let mut modifications = Vec::new();
    for token in tokens {
        match token {
            AnimationTypeToken::CoreType(name) => {
                if let Ok(ct) = CoreType::from_str(name) {
                    let modification = ContinuousModification::AddType { core_type: ct };
                    if !modifications.contains(&modification) {
                        modifications.push(modification);
                    }
                }
            }
            AnimationTypeToken::Subtype(name) => {
                let modification = ContinuousModification::AddSubtype {
                    subtype: title_case_word(&name),
                };
                if !modifications.contains(&modification) {
                    modifications.push(modification);
                }
            }
            AnimationTypeToken::Supertype => {}
        }
    }
    modifications
}

/// Parse the "becomes a [type expression]" noun phrase into core types +
/// subtypes. Built on nom combinators: tokenizes a sequence of type/subtype
/// words separated by whitespace, halting at the first token that doesn't
/// classify — punctuation (`,`, `.`), lowercase connectives (`and`, `gets`,
/// `gains`, `until`), P/T values (`3/3`, `X/X`), or cost tokens (`{B}:`).
/// This prevents misparses like *"this creature becomes a Dragon, gets +5/+3,
/// and gains flying"* from sweeping `Gets`, `And`, `Gains`, `Flying` in as
/// AddSubtype modifications — a common coverage false-positive pattern.
fn parse_animation_types(text: &str, infer_creature: bool) -> Vec<String> {
    let descriptor = text.trim().trim_end_matches(',').trim();
    if descriptor.is_empty() {
        return Vec::new();
    }

    // See parse_becomes_type_modifications for the same forward-parse pattern.
    // oracle_nom/PATTERNS.md ("Optional trailing clause after a token sequence").
    let mut parser = (
        parse_animation_type_sequence,
        opt(preceded(
            multispace0,
            alt((
                tag("in addition to its other creature types"),
                tag("in addition to its other types"),
            )),
        )),
    );
    let tokens = match parser.parse(descriptor) {
        Ok((_, (tokens, _))) => tokens,
        Err(_) => return Vec::new(),
    };

    let mut core_types = Vec::new();
    let mut subtypes = Vec::new();
    for token in tokens {
        match token {
            AnimationTypeToken::CoreType(name) => push_unique_string(&mut core_types, name),
            AnimationTypeToken::Subtype(name) => subtypes.push(title_case_word(&name)),
            AnimationTypeToken::Supertype => {}
        }
    }

    if core_types.is_empty() && subtypes.is_empty() {
        return Vec::new();
    }
    if core_types.is_empty() && infer_creature {
        push_unique_string(&mut core_types, "Creature");
    }

    let mut types = core_types;
    for subtype in subtypes {
        push_unique_string(&mut types, subtype);
    }
    types
}

fn split_animation_keyword_clause(text: &str) -> (&str, Vec<Keyword>) {
    const NEEDLE: &str = " with ";
    let lower = text.to_lowercase();
    let Some((before, _)) = split_around(&lower, NEEDLE) else {
        return (text, Vec::new());
    };

    let pos = before.len();
    let prefix = text[..pos].trim_end_matches(',').trim();
    // allow-noncombinator: structural post-processing of an already-chunked
    // keyword phrase (split at the first `"` above); this is not parsing
    // dispatch. A nom-combinator rewrite would add a word-boundary scan
    // helper without improving correctness.
    let keyword_text = text[pos + NEEDLE.len()..]
        .split('"')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches('.')
        .trim_end_matches(" in addition to its other types");
    let keywords = split_token_keyword_list(keyword_text)
        .into_iter()
        .filter_map(map_token_keyword)
        .collect();
    (prefix, keywords)
}

#[cfg(test)]
mod test_den_bugbear {
    use super::*;

    #[test]
    fn test_animation_with_quoted_trigger() {
        let text = r#"a 3/2 red Goblin creature with "Whenever this creature attacks, create a 1/1 red Goblin creature token that's tapped and attacking." It's still a land"#;
        let spec = parse_animation_spec(text);
        eprintln!("spec = {:?}", spec);
        assert!(spec.is_some(), "animation spec should be Some");
        let spec = spec.unwrap();
        assert_eq!(spec.power, Some(3));
        assert_eq!(spec.toughness, Some(2));
    }

    /// Regression: parse_animation_types must halt at connectives and
    /// punctuation rather than sweeping subsequent words in as subtypes.
    /// Previously a text like "Dragon, gets +5/+3, and gains flying and trample"
    /// produced subtypes ["Dragon", "Gets", "+5/+3", "And", "Gains", "Flying", "Trample"].
    #[test]
    fn animation_types_halts_at_connectives_and_punctuation() {
        assert_eq!(
            parse_animation_types("Dragon", true),
            vec!["Creature", "Dragon"]
        );
        assert_eq!(
            parse_animation_types("artifact creature Golem", false),
            vec!["Artifact", "Creature", "Golem"]
        );

        // Trailing comma on a valid subtype: accept the subtype, stop after.
        assert_eq!(
            parse_animation_types("Dragon, gets +5/+3, and gains flying", true),
            vec!["Creature", "Dragon"]
        );

        // Lowercase word immediately after subtype must terminate parsing.
        assert_eq!(
            parse_animation_types("Golem until end of combat", false),
            vec!["Golem"]
        );

        // P/T tokens and quoted triggers must not become subtypes.
        assert_eq!(
            parse_animation_types("Cat X/X", true),
            vec!["Creature", "Cat"]
        );
        assert_eq!(
            parse_animation_types("Shade and gains \"{B}: This creature gets +1/+1\"", true),
            vec!["Creature", "Shade"],
        );

        // Leading lowercase connective before any subtype → nothing parseable.
        assert_eq!(
            parse_animation_types("in addition to its other types and gains flying", false),
            Vec::<String>::new()
        );
    }

    /// Regression: supertypes (CR 205.4) must be recognized-and-discarded
    /// so they don't halt the sequence. Animations never grant supertypes,
    /// but a leading `legendary` / `basic` / `snow` word in the noun phrase
    /// must not prevent the subtype that follows from being captured.
    #[test]
    fn animation_types_discards_supertypes_without_halting_sequence() {
        assert_eq!(
            parse_animation_types("legendary Angel creature", false),
            vec!["Creature", "Angel"]
        );
        assert_eq!(parse_animation_types("basic Forest", false), vec!["Forest"]);
        // Supertype between core type and subtype must not halt.
        assert_eq!(
            parse_animation_types("snow Creature Elemental", false),
            vec!["Creature", "Elemental"]
        );
    }

    /// Regression: the subtype grammar must reject tokens where a capital
    /// letter is followed directly by a hyphen (`A-B`). Real MTG subtypes
    /// like `Power-Plant` have at least two alphabetic chars before the
    /// hyphen, so tightening the grammar here closes a lexicon-laxness gap.
    #[test]
    fn animation_subtype_rejects_leading_hyphen_tokens() {
        assert!(parse_animation_subtype("A-B").is_err());
        // Valid hyphenated subtype still parses.
        let (_, token) = parse_animation_subtype("Power-Plant").expect("hyphenated subtype");
        assert_eq!(token, AnimationTypeToken::Subtype("Power-Plant".into()));
    }

    /// CR 205.1a + CR 205.2 + CR 205.3 + CR 613.1c: "becomes a ..." descriptor
    /// must decompose into one AddType per core type and one AddSubtype per
    /// canonical subtype, rather than collapsing the whole phrase into a
    /// single AddSubtype string. This guards the Jump Scare pattern and the
    /// general class of compound type grants.
    #[test]
    fn becomes_type_modifications_decomposes_subtype_and_core_types() {
        use crate::types::ability::ContinuousModification;
        use crate::types::card_type::CoreType;

        // Pure core type.
        assert_eq!(
            parse_becomes_type_modifications("creature"),
            vec![ContinuousModification::AddType {
                core_type: CoreType::Creature
            }]
        );

        // Two core types.
        assert_eq!(
            parse_becomes_type_modifications("artifact creature"),
            vec![
                ContinuousModification::AddType {
                    core_type: CoreType::Artifact
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature
                },
            ]
        );

        // Jump Scare: subtype + two core types.
        assert_eq!(
            parse_becomes_type_modifications("Horror enchantment creature"),
            vec![
                ContinuousModification::AddSubtype {
                    subtype: "Horror".into()
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Enchantment
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature
                },
            ]
        );

        // Vehicle is a CR 205.3 artifact subtype, not a core type.
        assert_eq!(
            parse_becomes_type_modifications("Vehicle artifact creature"),
            vec![
                ContinuousModification::AddSubtype {
                    subtype: "Vehicle".into()
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Artifact
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature
                },
            ]
        );

        // Trailing "in addition to its other types" clause is stripped.
        assert_eq!(
            parse_becomes_type_modifications(
                "Horror enchantment creature in addition to its other types"
            ),
            vec![
                ContinuousModification::AddSubtype {
                    subtype: "Horror".into()
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Enchantment
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature
                },
            ]
        );

        // Supertype (CR 205.4) is recognized and discarded.
        assert_eq!(
            parse_becomes_type_modifications("legendary Angel creature"),
            vec![
                ContinuousModification::AddSubtype {
                    subtype: "Angel".into()
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature
                },
            ]
        );

        // Empty / malformed input produces no modifications.
        assert!(parse_becomes_type_modifications("").is_empty());
        assert!(parse_becomes_type_modifications("   ").is_empty());
    }
}
