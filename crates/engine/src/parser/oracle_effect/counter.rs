use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::Parser;

use crate::types::ability::{DoublePTMode, DoubleTarget, Effect, MultiTargetSpec, TargetFilter};
use crate::types::mana::ManaColor;

use super::super::oracle_nom::bridge::nom_on_lower;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_target::{parse_target, parse_type_phrase};
use super::super::oracle_util::{parse_count_expr, parse_number};
use super::{resolve_it_pronoun, ParseContext};

/// Check if text starts with a self-reference: "this ", "~"
fn is_self_ref(text: &str) -> bool {
    nom_on_lower(text, text, |i| {
        value((), alt((tag("this "), tag("~")))).parse(i)
    })
    .is_some()
}

/// Check if text is or starts with an "it" pronoun: "it", "it ", "itself"
fn is_it_pronoun(text: &str) -> bool {
    text == "it"
        || nom_on_lower(text, text, |i| {
            value((), alt((tag("itself"), tag("it ")))).parse(i)
        })
        .is_some()
}

pub(super) fn try_parse_put_counter<'a>(
    lower: &str,
    text: &'a str,
    ctx: &ParseContext,
) -> Option<(Effect, &'a str, Option<MultiTargetSpec>)> {
    // "put N {type} counter(s) on {target}"
    // Use parse_count_expr to handle Variable("X") for kicker-X patterns.
    let ((), after_put) = nom_on_lower(lower, lower, |i| value((), tag("put ")).parse(i))?;
    let after_put = after_put.trim();
    let (count_expr, rest) = parse_count_expr(after_put)?;
    // Next word is counter type (e.g. "+1/+1", "loyalty", "charge")
    let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let raw_type = &rest[..type_end];
    let counter_type = normalize_counter_type(raw_type);

    // Skip "counter" or "counters" keyword, then parse target after "on"
    let after_type = rest[type_end..].trim_start();
    let after_counter_word = nom_on_lower(after_type, after_type, |i| {
        value((), alt((tag("counters"), tag("counter")))).parse(i)
    })
    .map(|((), r)| r.trim_start())
    .unwrap_or(after_type);

    let (target, remainder, multi_target) = if let Some(((), on_rest)) =
        nom_on_lower(after_counter_word, after_counter_word, |i| {
            value((), tag("on ")).parse(i)
        }) {
        if is_self_ref(on_rest) {
            // Explicit self-reference — always SelfRef
            (TargetFilter::SelfRef, "", None)
        } else if is_it_pronoun(on_rest) {
            // CR 608.2k: Bare pronoun — context-dependent
            (resolve_it_pronoun(ctx), "", None)
        } else {
            // CR 115.1d: Strip "up to N" quantifier before target parsing.
            // "put a +1/+1 counter on up to one target creature" — the "up to N"
            // modifies the target count, not the counter count.
            let (target_text, multi) = if let Some(((), after_up_to)) =
                nom_on_lower(on_rest, on_rest, |i| {
                    value((), alt((tag("each of up to "), tag("up to ")))).parse(i)
                }) {
                if let Some((n, after_n)) = parse_number(after_up_to) {
                    let on_offset = lower.len() - after_n.len();
                    (
                        &text[on_offset..],
                        Some(MultiTargetSpec {
                            min: 0,
                            max: Some(n as usize),
                        }),
                    )
                } else {
                    let on_offset = lower.len() - on_rest.len();
                    (&text[on_offset..], None)
                }
            } else {
                let on_offset = lower.len() - on_rest.len();
                (&text[on_offset..], None)
            };

            let (target, rem) = parse_target(target_text);
            (target, rem, multi)
        }
    } else {
        (TargetFilter::SelfRef, "", None)
    };

    Some((
        Effect::PutCounter {
            counter_type,
            count: count_expr,
            target,
        },
        remainder,
        multi_target,
    ))
}

pub(super) fn try_parse_remove_counter(lower: &str, ctx: &ParseContext) -> Option<Effect> {
    // "remove N {type} counter(s) from {target}" or "remove all counters from {target}"
    // CR 121.1: Counter type is optional — "remove all counters" removes every type.
    let ((), after_remove) = nom_on_lower(lower, lower, |i| value((), tag("remove ")).parse(i))?;
    let after_remove = after_remove.trim();

    // CR 122.1: "remove all" uses sentinel count -1, resolved to actual count at runtime.
    // Also handle "up to N" prefix (player may remove fewer).
    let (count, rest) = if let Some(((), rest)) = nom_on_lower(after_remove, after_remove, |i| {
        value((), tag("all ")).parse(i)
    }) {
        (-1i32, rest.trim_start())
    } else if let Some(((), rest)) = nom_on_lower(after_remove, after_remove, |i| {
        value((), tag("up to ")).parse(i)
    }) {
        let (n, r) = parse_number(rest.trim())?;
        (n as i32, r)
    } else {
        let (n, r) = parse_number(after_remove)?;
        (n as i32, r)
    };

    // Try matching "counter(s)" directly (untyped: "remove all counters from ...").
    // If that fails, parse a type word first, then "counter(s)".
    let (counter_type, after_counter_word) = if let Some(((), after_cw)) =
        nom_on_lower(rest, rest, |i| {
            value((), alt((tag("counters"), tag("counter")))).parse(i)
        }) {
        // No type specified — empty string signals "all types" to the handler.
        (String::new(), after_cw)
    } else {
        let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        let raw_type = &rest[..type_end];
        let counter_type = normalize_counter_type(raw_type);
        let after_type = rest[type_end..].trim_start();
        let ((), after_cw) = nom_on_lower(after_type, after_type, |i| {
            value((), alt((tag("counters"), tag("counter")))).parse(i)
        })?;
        (counter_type, after_cw)
    };
    let after_counter_word = after_counter_word.trim_start();

    let ((), target_text) = nom_on_lower(after_counter_word, after_counter_word, |i| {
        value((), tag("from ")).parse(i)
    })?;
    let target_text = target_text.trim();
    let target = if is_self_ref(target_text) {
        TargetFilter::SelfRef
    } else if is_it_pronoun(target_text) {
        // CR 608.2k: Bare pronoun — context-dependent
        resolve_it_pronoun(ctx)
    } else {
        let (t, _rem) = parse_target(target_text);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, target_text);
        t
    };

    Some(Effect::RemoveCounter {
        counter_type,
        count,
        target,
    })
}

/// Normalize oracle-text counter type strings to canonical engine names.
pub(crate) fn normalize_counter_type(raw: &str) -> String {
    match raw {
        "+1/+1" => "P1P1".to_string(),
        "-1/-1" => "M1M1".to_string(),
        other => other.to_string(),
    }
}

/// Resolve a counter target from text: self-ref, pronoun, or parse_target.
/// Shared by put/remove/multiply counter parsers.
fn resolve_counter_target(text: &str, ctx: &ParseContext) -> TargetFilter {
    if is_self_ref(text) {
        TargetFilter::SelfRef
    } else if is_it_pronoun(text) {
        // CR 608.2k: Bare pronoun — context-dependent
        resolve_it_pronoun(ctx)
    } else {
        let (t, _rem) = parse_target(text);
        #[cfg(debug_assertions)]
        super::types::assert_no_compound_remainder(_rem, text);
        t
    }
}

/// CR 121.5: Parse "put its counters on [target]" → MoveCounters effect.
/// "its" / "this creature's" are possessive pronouns referring to the ability source.
pub(super) fn try_parse_move_counters<'a>(lower: &str, text: &'a str) -> Option<(Effect, &'a str)> {
    let ((), after_put) = nom_on_lower(lower, lower, |i| value((), tag("put ")).parse(i))?;
    let after_put = after_put.trim();
    // Detect "its counters" / "this creature's counters"
    let ((), after_possessive) = nom_on_lower(after_put, after_put, |i| {
        value(
            (),
            alt((tag("its counter"), tag("this creature's counter"))),
        )
        .parse(i)
    })?;
    // Skip past optional "s" (counter vs counters) then expect " on "
    let after_counters = nom_on_lower(after_possessive, after_possessive, |i| {
        value((), tag("s")).parse(i)
    })
    .map(|((), r)| r)
    .unwrap_or(after_possessive);
    let ((), after_on) = nom_on_lower(after_counters, after_counters, |i| {
        value((), tag(" on ")).parse(i)
    })?;

    // Compute byte offset into original `text` for parse_target.
    let offset_in_text = text.len() - after_on.len();
    let (target, remainder) = parse_target(&text[offset_in_text..]);

    Some((
        Effect::MoveCounters {
            source: TargetFilter::SelfRef,
            counter_type: None,
            target,
        },
        remainder,
    ))
}

/// CR 121.5: Parse "move [all/N] [type] counter(s) from [source] onto/to [target]".
/// Handles Bioshift, Fate Transfer, Nesting Grounds, Simic Fluxmage, etc.
pub(super) fn try_parse_move_counters_from(lower: &str, ctx: &ParseContext) -> Option<Effect> {
    let ((), after_move) = nom_on_lower(lower, lower, |i| value((), tag("move ")).parse(i))?;
    let after_move = after_move.trim();

    // Parse quantity: "all", "any number of", or a number.
    // count is informational (None = all, Some(n) = at most n).
    let rest = if let Some(((), rest)) =
        nom_on_lower(after_move, after_move, |i| value((), tag("all ")).parse(i))
    {
        rest.trim_start()
    } else if let Some(((), rest)) = nom_on_lower(after_move, after_move, |i| {
        value((), tag("any number of ")).parse(i)
    }) {
        rest.trim_start()
    } else if let Some((_, rest)) = parse_number(after_move) {
        rest.trim_start()
    } else {
        // "move a +1/+1 counter" — article consumed by parse_number("a" → 1)
        return None;
    };

    // Try matching "counter(s)" directly (untyped), or parse a type first.
    let (counter_type, after_counter_word) = if let Some(((), after_cw)) =
        nom_on_lower(rest, rest, |i| {
            value((), alt((tag("counters"), tag("counter")))).parse(i)
        }) {
        (None, after_cw)
    } else {
        let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        let raw_type = &rest[..type_end];
        let ct = normalize_counter_type(raw_type);
        let after_type = rest[type_end..].trim_start();
        let ((), after_cw) = nom_on_lower(after_type, after_type, |i| {
            value((), alt((tag("counters"), tag("counter")))).parse(i)
        })?;
        (Some(ct), after_cw)
    };
    let after_counter_word = after_counter_word.trim_start();

    // Expect "from "
    let ((), after_from) = nom_on_lower(after_counter_word, after_counter_word, |i| {
        value((), tag("from ")).parse(i)
    })?;
    let after_from = after_from.trim();

    // Parse source target — delimited by " onto " or " to ".
    let split_pos = after_from
        .find(" onto ")
        .or_else(|| after_from.find(" to "));
    let (source_text, target_text) = match split_pos {
        Some(pos) => {
            let source = &after_from[..pos];
            let rest = &after_from[pos..];
            let target = rest
                .strip_prefix(" onto ")
                .or_else(|| rest.strip_prefix(" to "))
                .unwrap_or(rest)
                .trim();
            (source, target)
        }
        None => return None,
    };

    let source = resolve_counter_target(source_text, ctx);
    let (target, _rem) = parse_target(target_text);

    Some(Effect::MoveCounters {
        source,
        counter_type,
        target,
    })
}

/// CR 701.10e: Parse "double the number of {type} counters on {target}".
pub(super) fn try_parse_multiply_counter(lower: &str, ctx: &ParseContext) -> Option<Effect> {
    let ((), rest) = nom_on_lower(lower, lower, |i| {
        value((), tag("double the number of ")).parse(i)
    })?;
    // Parse counter type — next word(s) before "counter(s)"
    let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let raw_type = &rest[..type_end];
    let counter_type = normalize_counter_type(raw_type);

    // Skip counter type + "counter(s) on "
    let after_type = rest[type_end..].trim_start();
    let ((), after_counter_word) = nom_on_lower(after_type, after_type, |i| {
        value((), alt((tag("counters"), tag("counter")))).parse(i)
    })?;
    let after_counter_word = after_counter_word.trim_start();
    let ((), target_text) = nom_on_lower(after_counter_word, after_counter_word, |i| {
        value((), tag("on ")).parse(i)
    })?;

    let target = resolve_counter_target(target_text, ctx);

    Some(Effect::MultiplyCounter {
        counter_type,
        multiplier: 2,
        target,
    })
}

/// CR 701.10: Dispatch "double the ..." to counter-doubling, life-doubling,
/// mana-doubling, or P/T-doubling.
pub(super) fn try_parse_double_effect(lower: &str, ctx: &ParseContext) -> Option<Effect> {
    // CR 701.10e: "double the number of each kind of counter on ..." → all counter types
    if let Some(((), rest)) = nom_on_lower(lower, lower, |i| {
        value((), tag("double the number of each kind of counter on ")).parse(i)
    }) {
        let target = resolve_counter_target(rest, ctx);
        return Some(Effect::Double {
            target_kind: DoubleTarget::Counters { counter_type: None },
            target,
        });
    }

    // Counter doubling: "double the number of ..."
    if nom_on_lower(lower, lower, |i| {
        value((), tag("double the number of ")).parse(i)
    })
    .is_some()
    {
        return try_parse_multiply_counter(lower, ctx);
    }

    // CR 701.10d: "double your life total" / "double target player's life total"
    if let Some(((), rest)) = nom_on_lower(lower, lower, |i| value((), tag("double ")).parse(i)) {
        if nom_on_lower(rest, rest, |i| value((), tag("your life total")).parse(i)).is_some() {
            return Some(Effect::Double {
                target_kind: DoubleTarget::LifeTotal,
                target: TargetFilter::Controller,
            });
        }
        if nom_on_lower(rest, rest, |i| value((), tag("target ")).parse(i)).is_some()
            && rest.contains("life total")
        {
            let (target, _) = parse_target(rest);
            return Some(Effect::Double {
                target_kind: DoubleTarget::LifeTotal,
                target,
            });
        }
    }

    // CR 701.10f: "double the amount of {color} mana in your mana pool"
    if let Some(((), rest)) = nom_on_lower(lower, lower, |i| {
        value((), tag("double the amount of ")).parse(i)
    }) {
        if rest.contains("mana") {
            let color = parse_mana_color_from_text(rest);
            return Some(Effect::Double {
                target_kind: DoubleTarget::ManaPool { color },
                target: TargetFilter::Controller,
            });
        }
    }

    // CR 608.2k: "double its power [and toughness]" — possessive "its" is context-dependent
    if let Some(((), rest)) = nom_on_lower(lower, lower, |i| value((), tag("double its ")).parse(i))
    {
        let mode: Option<DoublePTMode> = nom_on_lower(rest, rest, |i| {
            alt((
                value(DoublePTMode::PowerAndToughness, tag("power and toughness")),
                value(DoublePTMode::Power, tag("power")),
                value(DoublePTMode::Toughness, tag("toughness")),
            ))
            .parse(i)
        })
        .map(|(m, _)| m);
        if let Some(mode) = mode {
            return Some(Effect::DoublePT {
                mode,
                target: resolve_it_pronoun(ctx),
            });
        }
        return None;
    }

    // P/T doubling: "double the power/toughness [and toughness/power] of ..."
    let ((), rest) = nom_on_lower(lower, lower, |i| value((), tag("double the ")).parse(i))?;

    let (mode, after_mode) = nom_on_lower(rest, rest, |i| {
        alt((
            value(
                DoublePTMode::PowerAndToughness,
                tag("power and toughness of "),
            ),
            value(DoublePTMode::Power, tag("power of ")),
            value(DoublePTMode::Toughness, tag("toughness of ")),
        ))
        .parse(i)
    })?;

    // "target creature you control" → targeted DoublePT
    if nom_on_lower(after_mode, after_mode, |i| {
        value((), tag("target ")).parse(i)
    })
    .is_some()
    {
        let (target, _) = parse_target(after_mode);
        return Some(Effect::DoublePT { mode, target });
    }

    // "each creature you control" / "each other creature" / "each Dragon" → DoublePTAll
    let ((), filter_text) = nom_on_lower(after_mode, after_mode, |i| {
        value((), alt((tag("each "), tag("all ")))).parse(i)
    })?;
    let (target, _) = parse_type_phrase(filter_text);
    Some(Effect::DoublePTAll { mode, target })
}

/// Parse a mana color name from text like "red mana in your mana pool".
///
/// Delegates to `nom_primitives::parse_color` for color word recognition.
fn parse_mana_color_from_text(text: &str) -> Option<ManaColor> {
    let lower = text.split_whitespace().next()?.to_lowercase();
    let (_rest, color) = nom_primitives::parse_color.parse(&lower).ok()?;
    Some(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_ctx() -> ParseContext {
        ParseContext::default()
    }

    #[test]
    fn remove_counter_untyped_all() {
        // Vampire Hexmage: "remove all counters from target permanent"
        let result =
            try_parse_remove_counter("remove all counters from target permanent", &default_ctx());
        let Some(Effect::RemoveCounter {
            counter_type,
            count,
            target,
        }) = result
        else {
            panic!("expected RemoveCounter, got {result:?}");
        };
        assert!(counter_type.is_empty(), "untyped should be empty string");
        assert_eq!(count, -1, "all = sentinel -1");
        assert!(matches!(target, TargetFilter::Typed { .. }));
    }

    #[test]
    fn remove_counter_untyped_single() {
        // Thrull Parasite: "remove a counter from target nonland permanent"
        let result = try_parse_remove_counter(
            "remove a counter from target nonland permanent",
            &default_ctx(),
        );
        let Some(Effect::RemoveCounter {
            counter_type,
            count,
            ..
        }) = result
        else {
            panic!("expected RemoveCounter, got {result:?}");
        };
        assert!(counter_type.is_empty());
        assert_eq!(count, 1);
    }

    #[test]
    fn remove_counter_up_to_n() {
        // Heartless Act mode 2: "remove up to three counters from target creature"
        let result = try_parse_remove_counter(
            "remove up to three counters from target creature",
            &default_ctx(),
        );
        let Some(Effect::RemoveCounter {
            counter_type,
            count,
            ..
        }) = result
        else {
            panic!("expected RemoveCounter, got {result:?}");
        };
        assert!(counter_type.is_empty());
        assert_eq!(count, 3);
    }

    #[test]
    fn remove_counter_typed_still_works() {
        // Existing pattern: "remove a +1/+1 counter from ~"
        let result = try_parse_remove_counter("remove a +1/+1 counter from ~", &default_ctx());
        let Some(Effect::RemoveCounter {
            counter_type,
            count,
            ..
        }) = result
        else {
            panic!("expected RemoveCounter, got {result:?}");
        };
        assert_eq!(counter_type, "P1P1");
        assert_eq!(count, 1);
    }

    #[test]
    fn move_counters_from_self_onto_target() {
        // Simic Fluxmage: "move a +1/+1 counter from this creature onto target creature"
        let result = try_parse_move_counters_from(
            "move a +1/+1 counter from this creature onto target creature",
            &default_ctx(),
        );
        let Some(Effect::MoveCounters {
            source,
            counter_type,
            target,
        }) = result
        else {
            panic!("expected MoveCounters, got {result:?}");
        };
        assert!(matches!(source, TargetFilter::SelfRef));
        assert_eq!(counter_type, Some("P1P1".to_string()));
        assert!(matches!(target, TargetFilter::Typed { .. }));
    }

    #[test]
    fn move_counters_all_types() {
        // Fate Transfer: "move all counters from target creature onto another target creature"
        let result = try_parse_move_counters_from(
            "move all counters from target creature onto another target creature",
            &default_ctx(),
        );
        let Some(Effect::MoveCounters { counter_type, .. }) = result else {
            panic!("expected MoveCounters, got {result:?}");
        };
        assert_eq!(counter_type, None, "untyped = None");
    }

    #[test]
    fn move_counters_typed_from_target_to_self() {
        // Cytoplast Root-Kin: "move a +1/+1 counter from target creature you control onto this creature"
        let result = try_parse_move_counters_from(
            "move a +1/+1 counter from target creature you control onto this creature",
            &default_ctx(),
        );
        let Some(Effect::MoveCounters {
            source,
            counter_type,
            target,
        }) = result
        else {
            panic!("expected MoveCounters, got {result:?}");
        };
        assert!(matches!(source, TargetFilter::Typed { .. }));
        assert_eq!(counter_type, Some("P1P1".to_string()));
        assert!(matches!(target, TargetFilter::SelfRef));
    }
}
