use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::value;
use nom::Parser;
use nom_language::error::VerboseError;

use crate::types::ability::{
    DoublePTMode, DoubleTarget, Effect, MultiTargetSpec, QuantityExpr, TargetFilter,
};
use crate::types::mana::ManaColor;

use super::super::oracle_nom::bridge::nom_on_lower;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
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

/// Check if text is or starts with a bare object pronoun: "it"/"itself",
/// "him"/"himself", "her"/"herself", "them"/"themselves". CR 608.2k: these
/// anaphoric references resolve against the parse context's subject rather
/// than an outer targeted object. Gendered pronouns ("him", "her") must route
/// through the same resolver so ETB-self triggers like "put X counters on
/// him" bind to `SelfRef` when the trigger subject is the source permanent.
fn is_it_pronoun(text: &str) -> bool {
    matches!(text, "it" | "him" | "her" | "them")
        || nom_on_lower(text, text, |i| {
            value(
                (),
                alt((
                    tag("itself"),
                    tag("himself"),
                    tag("herself"),
                    tag("themselves"),
                    tag("it "),
                    tag("him "),
                    tag("her "),
                    tag("them "),
                )),
            )
            .parse(i)
        })
        .is_some()
}

/// Output of [`try_parse_put_counter_chain`]: the ordered list of
/// `(counter_type, count)` entries, the shared target, the remaining original-
/// case text after the clause, and any multi-target spec.
pub(super) type PutCounterChain<'a> = (
    Vec<(String, QuantityExpr)>,
    TargetFilter,
    &'a str,
    Option<MultiTargetSpec>,
);

/// CR 122.1: Parse "put a X counter, a Y counter[, and a Z counter] on TARGET"
/// — a list of counters of distinct types placed on a shared target. Covers
/// Abigale, Unexpected Fangs, Gift of the Viper, Qarsi Revenant, Nezumi
/// Prowler, Arwen, Champion of Dusan, Quicksilver, and any future card that
/// stacks multiple typed counters on one target in a single clause.
///
/// Returns `None` for single-counter phrases (handled by the usual
/// `try_parse_put_counter` path) or when the list pattern doesn't match.
/// Returned `Vec` always has `len() >= 2`.
pub(super) fn try_parse_put_counter_chain<'a>(
    lower: &str,
    text: &'a str,
    ctx: &ParseContext,
) -> Option<PutCounterChain<'a>> {
    let ((), after_put) = nom_on_lower(lower, lower, |i| value((), tag("put ")).parse(i))?;
    let mut remaining = after_put.trim_start();
    let mut entries: Vec<(String, QuantityExpr)> = Vec::new();

    loop {
        let (count_expr, rest) = parse_count_expr(remaining)?;
        // Counter types can be multi-word (e.g., "first strike", "double strike"),
        // so use `take_until(" counter")` to consume the full type phrase rather
        // than splitting on the first whitespace.
        let (at_counter, raw_type) = take_until::<_, _, VerboseError<&str>>(" counter")
            .parse(rest)
            .ok()?;
        if raw_type.is_empty() {
            return None;
        }
        let counter_type = normalize_counter_type(raw_type);
        let ((), after_space) =
            nom_on_lower(at_counter, at_counter, |i| value((), tag(" ")).parse(i))?;
        let ((), after_counter_word) = nom_on_lower(after_space, after_space, |i| {
            value((), alt((tag("counters"), tag("counter")))).parse(i)
        })?;
        entries.push((counter_type, count_expr));

        // After the counter noun we expect either:
        //   - a list separator (", a ", ", and a ", " and a ", + "an" variants)
        //     followed by another "<count> <type> counter(s)" tuple, or
        //   - " on " beginning the shared-target clause.
        if let Some(next) = try_consume_counter_list_separator(after_counter_word) {
            remaining = next;
            continue;
        }
        remaining = after_counter_word;
        break;
    }

    if entries.len() < 2 {
        return None;
    }

    let ((), on_rest) = nom_on_lower(remaining, remaining, |i| {
        value((), alt((tag(" on "), tag("on ")))).parse(i)
    })?;

    let (target, remainder_text, multi_target) =
        resolve_counter_placement_target(on_rest, lower, text, ctx);

    Some((entries, target, remainder_text, multi_target))
}

/// Resolve the target of a `put counter ... on <target>` clause. `on_rest` is
/// the lowercase remainder after the literal `on `; `lower`/`text` are the
/// full lowercase/original inputs so byte offsets can map back to original-
/// case slices for `parse_target`. Extracted so the single-counter and
/// list-counter paths share one target-resolution building block.
fn resolve_counter_placement_target<'a>(
    on_rest: &str,
    lower: &str,
    text: &'a str,
    ctx: &ParseContext,
) -> (TargetFilter, &'a str, Option<MultiTargetSpec>) {
    // The byte-offset math below (`lower.len() - on_rest.len()` → `&text[offset..]`)
    // requires that `text` and `lower` are byte-for-byte length-equal. That holds
    // when `lower` was produced by `to_lowercase()` on ASCII-only input — true for
    // all Oracle text in current MTG card data. Guard against a future Unicode
    // regression.
    debug_assert_eq!(
        text.len(),
        lower.len(),
        "counter target offset math requires ASCII-equal-length lower/original pair"
    );
    if is_self_ref(on_rest) {
        return (TargetFilter::SelfRef, "", None);
    }
    if is_it_pronoun(on_rest) {
        return (resolve_it_pronoun(ctx), "", None);
    }
    // CR 115.1d: "up to N" (and "each of up to N") modifies the target count,
    // not the counter count. Strip it and emit a MultiTargetSpec.
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

/// Consume a comma / "and" separator between items in a counter list —
/// leaves the leading article ("a"/"an") so the next iteration's
/// `parse_count_expr` consumes it uniformly. Returns `None` unless the
/// separator is immediately followed by `(a|an) <word> counter(s)` to
/// avoid stealing a compound connector from a different clause.
fn try_consume_counter_list_separator(input: &str) -> Option<&str> {
    let ((), after_sep) = nom_on_lower(input, input, |i| {
        value((), alt((tag(", and "), tag(" and "), tag(", ")))).parse(i)
    })?;
    let ((), after_article) = nom_on_lower(after_sep, after_sep, |i| {
        value((), alt((tag("an "), tag("a ")))).parse(i)
    })?;
    // Peek ahead: after the article there must be "<type> counter(s)". The
    // counter-type phrase may be multi-word ("first strike"), so delimit it
    // with `take_until(" counter")` instead of splitting on whitespace.
    let (at_counter, raw_type) = take_until::<_, _, VerboseError<&str>>(" counter")
        .parse(after_article)
        .ok()?;
    if raw_type.is_empty() {
        return None;
    }
    let ((), after_space) = nom_on_lower(at_counter, at_counter, |i| value((), tag(" ")).parse(i))?;
    nom_on_lower(after_space, after_space, |i| {
        value((), alt((tag("counters"), tag("counter")))).parse(i)
    })?;
    Some(after_sep)
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

    // CR 122.1 + CR 208.3: Detect the dynamic-quantity phrasing
    // "a number of {type} counters equal to {qty}" (Gruff Triplets:
    // "a number of +1/+1 counters equal to its power"). Must run before the
    // generic fixed-count path below — otherwise `parse_count_expr` would read
    // "a" as count=1 and "number" as the counter type. `dynamic_pending`
    // signals that the "equal to {qty}" clause is expected to appear after the
    // counter noun and must be consumed below.
    let (count_expr, rest, dynamic_pending) =
        if let Some(after_phrase) = try_strip_a_number_of(after_put) {
            // Two positions for "equal to {qty}":
            //   eager: "a number of counters equal to X ..." (counter type absent
            //          or implicit — rare in practice)
            //   trailing: "a number of {type} counters equal to X ..." (Gruff class)
            match nom_quantity::parse_equal_to(after_phrase) {
                Ok((rest, qty)) => {
                    let rest = rest.strip_prefix(' ').unwrap_or(rest);
                    (qty, rest, false)
                }
                Err(_) => (QuantityExpr::Fixed { value: 0 }, after_phrase, true),
            }
        } else {
            let (qty, rest) = parse_count_expr(after_put)?;
            (qty, rest, false)
        };

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

    // If we entered via "a number of ..." without finding the "equal to" clause
    // eagerly, it MUST appear here after the counter noun. Consume it and
    // overwrite the placeholder count. Abort the dynamic path if the clause
    // is missing — the phrase is malformed as a dynamic-count.
    let (count_expr, after_counter_word) = if dynamic_pending {
        let (after_clause, qty) = nom_quantity::parse_equal_to(after_counter_word).ok()?;
        let after_clause = after_clause.strip_prefix(' ').unwrap_or(after_clause);
        (qty, after_clause)
    } else {
        (count_expr, after_counter_word)
    };

    // CR 122.1: The placement clause MUST begin with "on <target>" — MTG never
    // prints a bare "put a counter" without a zone/target. Falling back to
    // SelfRef on a missed "on " silently swallows unconsumed tails (this was
    // the root cause of the Abigale multi-counter misparse before the list
    // path was added). Propagate parse failure instead so upstream dispatch
    // can try another handler or produce Unimplemented.
    let ((), on_rest) = nom_on_lower(after_counter_word, after_counter_word, |i| {
        value((), tag("on ")).parse(i)
    })?;
    let (target, remainder, multi_target) =
        resolve_counter_placement_target(on_rest, lower, text, ctx);

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

/// CR 122.1: Consume the "a number of " prefix used in dynamic counter-count
/// phrases, returning the remainder. Returns None when the prefix is absent.
fn try_strip_a_number_of(input: &str) -> Option<&str> {
    tag::<_, _, nom_language::error::VerboseError<&str>>("a number of ")
        .parse(input)
        .map(|(rest, _)| rest)
        .ok()
}

pub(super) fn try_parse_remove_counter(lower: &str, ctx: &ParseContext) -> Option<Effect> {
    // "remove N {type} counter(s) from {target}" or "remove all counters from {target}"
    // CR 122.1: Counter type is optional — "remove all counters" removes every type.
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

/// CR 122.8: Parse "put its counters on [target]" / "put those counters on
/// [target]" → MoveCounters effect. CR 122.8 covers the trigger-on-leaving
/// case where the source's counters are copied (not strictly moved) to a
/// second object.
///
/// `"its"` / `"this creature's"` are possessive pronouns referring to the
/// ability source (live state). `"those"` is an anaphoric reference to the
/// counters that were on the source — typically used in dies / leaves-
/// battlefield triggers gated by an `if it had counters on it` condition
/// (Scolding Administrator class). The runtime resolver in
/// `effects::counters::resolve_move` already performs LKI fallback on the
/// source object (CR 400.7), so dies-triggers correctly read counters from
/// the dying creature's last-known state regardless of pronoun form.
pub(super) fn try_parse_move_counters<'a>(lower: &str, text: &'a str) -> Option<(Effect, &'a str)> {
    let ((), after_put) = nom_on_lower(lower, lower, |i| value((), tag("put ")).parse(i))?;
    let after_put = after_put.trim();
    // Detect "its counters" / "this creature's counters" / "those counters"
    let ((), after_possessive) = nom_on_lower(after_put, after_put, |i| {
        value(
            (),
            alt((
                tag("its counter"),
                tag("this creature's counter"),
                tag("those counter"),
            )),
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

/// CR 122.5: Parse "move [all/N] [type] counter(s) from [source] onto/to [target]".
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
    let pos = split_pos?;
    let source_text = &after_from[..pos];
    let rest = &after_from[pos..];
    let target_text = rest
        .strip_prefix(" onto ")
        .or_else(|| rest.strip_prefix(" to "))
        .unwrap_or(rest)
        .trim();

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

    // CR 701.10b: "double <target>'s power [and toughness]" — possessive form covering
    // "double target creature's power" (Bulk Up class) and "double ~'s power" (Devilish
    // Valet / Okaun class, where ~ is the self-reference normalization). Composes with
    // existing `parse_target` building block to cover any target phrase, then matches the
    // possessive P/T tail. Sibling of the `tag("double its ")` and `tag("double the ")`
    // arms below; placed first so the `parse_target`-driven possessive form takes
    // precedence and the more specific "its" / "the X of Y" patterns fall through.
    if let Some(((), after_double)) =
        nom_on_lower(lower, lower, |i| value((), tag("double ")).parse(i))
    {
        // Skip patterns owned by sibling arms below.
        let owned_by_sibling = nom_on_lower(after_double, after_double, |i| {
            value(
                (),
                alt((
                    tag("its "),
                    tag("the "),
                    tag("your "),
                    tag("target player"),
                    tag("the amount"),
                    tag("the number"),
                )),
            )
            .parse(i)
        })
        .is_some();
        if !owned_by_sibling {
            let (target, rem) = parse_target(after_double);
            if !matches!(target, TargetFilter::Any) {
                let rem_lower = rem.to_lowercase();
                let mode: Option<DoublePTMode> = nom_on_lower(&rem_lower, &rem_lower, |i| {
                    alt((
                        value(
                            DoublePTMode::PowerAndToughness,
                            tag("'s power and toughness"),
                        ),
                        value(DoublePTMode::Power, tag("'s power")),
                        value(DoublePTMode::Toughness, tag("'s toughness")),
                    ))
                    .parse(i)
                })
                .map(|(m, _)| m);
                if let Some(mode) = mode {
                    return Some(Effect::DoublePT { mode, target });
                }
            }
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

    /// CR 122.1 + CR 208.3: "put a number of +1/+1 counters equal to its power
    /// on each creature you control named ~" (Gruff Triplets). The dynamic
    /// count binds to the source's power via `QuantityRef::SelfPower`; the
    /// counter type is "+1/+1", not the word "number"; the target is a mass
    /// filter for creatures with the same name (resolved via `normalize_self_refs`
    /// restoring the card name after the `named ~` re-expansion).
    #[test]
    fn put_counter_a_number_of_equal_to_self_power() {
        use crate::types::ability::{FilterProp, QuantityRef, TypeFilter};
        let (effect, _, _) = try_parse_put_counter(
            "put a number of +1/+1 counters equal to its power on each creature you control named Gruff Triplets",
            "put a number of +1/+1 counters equal to its power on each creature you control named Gruff Triplets",
            &default_ctx(),
        )
        .expect("parse");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = effect
        else {
            panic!("expected PutCounter, got {effect:?}");
        };
        assert_eq!(counter_type, "P1P1");
        assert!(
            matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::SelfPower
                }
            ),
            "count should be SelfPower, got {count:?}"
        );
        let TargetFilter::Typed(tf) = target else {
            panic!("expected Typed filter, got target");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::Named { name } if name.eq_ignore_ascii_case("Gruff Triplets"))));
    }

    /// CR 603.7c: Dusty Parlor — "Whenever you cast an enchantment spell,
    /// put a number of +1/+1 counters equal to that spell's mana value on
    /// up to one target creature." The dynamic count binds to the triggering
    /// SpellCast event's source object (the spell itself) via
    /// `QuantityRef::EventContextSourceManaValue`, which resolves to the
    /// spell's printed CMC at trigger resolution time.
    #[test]
    fn put_counter_a_number_of_equal_to_spells_mana_value() {
        use crate::types::ability::QuantityRef;
        let (effect, _, multi) = try_parse_put_counter(
            "put a number of +1/+1 counters equal to that spell's mana value on up to one target creature",
            "put a number of +1/+1 counters equal to that spell's mana value on up to one target creature",
            &default_ctx(),
        )
        .expect("parse");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = effect
        else {
            panic!("expected PutCounter, got {effect:?}");
        };
        assert_eq!(counter_type, "P1P1");
        assert!(
            matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::EventContextSourceManaValue
                }
            ),
            "count should be EventContextSourceManaValue, got {count:?}"
        );
        assert!(matches!(target, TargetFilter::Typed { .. }));
        assert_eq!(
            multi,
            Some(MultiTargetSpec {
                min: 0,
                max: Some(1)
            }),
            "up to one target creature → MultiTargetSpec {{ 0, 1 }}"
        );
    }

    /// Sibling coverage: same dynamic-count phrase shape with a different
    /// quantity reference ("equal to the number of cards in your hand").
    /// Confirms the building block generalizes beyond just SelfPower.
    #[test]
    fn put_counter_a_number_of_equal_to_hand_size() {
        use crate::types::ability::{QuantityRef, ZoneRef};
        let (effect, _, _) = try_parse_put_counter(
            "put a number of +1/+1 counters equal to the number of cards in your hand on ~",
            "put a number of +1/+1 counters equal to the number of cards in your hand on ~",
            &default_ctx(),
        )
        .expect("parse");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = effect
        else {
            panic!("expected PutCounter, got {effect:?}");
        };
        assert_eq!(counter_type, "P1P1");
        // The parser resolves "cards in your hand" to the more specific
        // ZoneCardCount; either ZoneCardCount or HandSize is semantically valid.
        assert!(
            matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Hand,
                        ..
                    } | QuantityRef::HandSize
                }
            ),
            "count should be hand-card-count reference, got {count:?}"
        );
        assert!(matches!(target, TargetFilter::SelfRef));
    }

    /// CR 122.8 + CR 400.7: "put those counters on [target]" — anaphoric
    /// counter-copy from a dies/leaves trigger. Source = SelfRef; the runtime
    /// resolver in `effects::counters::resolve_move` performs LKI fallback so
    /// the counters from the dying creature's last-known state are read.
    /// Used by Scolding Administrator: "When this creature dies, if it had
    /// counters on it, put those counters on up to one target creature."
    #[test]
    fn move_counters_those_counters_anaphora() {
        let lower = "put those counters on up to one target creature";
        let result = try_parse_move_counters(lower, lower);
        let Some((
            Effect::MoveCounters {
                source,
                counter_type,
                target,
            },
            _,
        )) = result
        else {
            panic!("expected MoveCounters, got {result:?}");
        };
        assert!(matches!(source, TargetFilter::SelfRef));
        assert_eq!(counter_type, None, "all counters move (no type filter)");
        match target {
            TargetFilter::Typed(tf) => {
                assert!(tf
                    .type_filters
                    .iter()
                    .any(|t| matches!(t, crate::types::ability::TypeFilter::Creature)));
            }
            other => panic!("expected typed creature target, got {other:?}"),
        }
    }
}
