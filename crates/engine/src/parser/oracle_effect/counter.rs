use crate::parser::oracle_nom::error::{OracleError, OracleResult};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_till, take_until};
use nom::character::complete::space1;
use nom::combinator::{all_consuming, eof, opt, peek, value};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use crate::types::ability::{
    ChosenCounterCountCondition, Comparator, CounterMoveSelection, CounterTransferMode,
    DoublePTMode, DoubleTarget, Effect, EventCounterReproductionCount, FilterProp, MultiTargetSpec,
    ObjectScope, QuantityExpr, QuantityRef, TargetFilter,
};
use crate::types::counter::{parse_counter_type, CounterType};
use crate::types::mana::ManaColor;

use super::super::oracle_nom::bridge::nom_on_lower;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_target::{
    parse_target, parse_target_with_ctx, parse_type_phrase_folding,
    parse_type_phrase_folding_with_ctx,
};
use super::super::oracle_util::{parse_count_expr, parse_number};
use super::lower::parse_for_each_multiplier_prefix;
use super::{resolve_it_pronoun, ParseContext};
#[cfg(debug_assertions)]
use crate::parser::oracle_ir::ast::assert_no_compound_remainder;
use crate::parser::oracle_ir::ast::replace_fixed_quantity;

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
    nom_on_lower(text, text, |i| {
        value(
            (),
            alt((
                // Bare object pronoun as the whole token or immediately followed
                // by a space, routed through the shared recipient-pronoun
                // combinator (single authority for the it/them/him/her set).
                terminated(
                    nom_primitives::parse_object_recipient_pronoun,
                    alt((eof, peek(tag(" ")))),
                ),
                // Reflexive "*self" forms — a distinct set, matched here directly.
                tag("itself"),
                tag("himself"),
                tag("herself"),
                tag("themselves"),
            )),
        )
        .parse(i)
    })
    .is_some()
}

/// CR 608.2c + CR 111.10 + CR 122.1: a same-chain counter anaphor placed AFTER a
/// token-creating clause binds to that just-created token
/// (`TargetFilter::LastCreated`) — the anaphor's most-recent object referent —
/// rather than the ability source (`SelfRef`) or an unbound parent target.
/// Recognizes three anaphor forms: the object-pronoun class (`it`/`them`/…, via
/// [`is_it_pronoun`]), the demonstratives `that creature`/`that token`/`that
/// permanent`, and the definite back-references `the token`/`the permanent`.
/// `the creature` is deliberately EXCLUDED: it legitimately binds a chosen
/// target slot (Longstalk Brawl's "the creature you control" →
/// `ParentTargetSlot`) and is genuinely ambiguous.
///
/// Returns `Some(LastCreated)` only when the chain's most-recent referent is a
/// created token (`ctx.token_created_in_chain`) AND, for the demonstrative /
/// definite forms, no non-self trigger subject re-anchors the reference — Pip-Boy
/// 3000's "Whenever equipped creature attacks … put a +1/+1 counter on that
/// creature" keeps its `resolve_that_creature_in_trigger` triggering-source
/// binding. That subject gate is intentionally SKIPPED for the object-pronoun
/// form so this helper reproduces the legacy ungated bare-"it" binding exactly,
/// making the it-branch refactor provably behavior-preserving. `None` ⇒ the
/// caller applies its own default (source / parent / typed target).
/// The demonstrative/definite half of the chain-created anaphor, WITHOUT the
/// bare object pronoun.
///
/// "That creature" / "that token" / "the permanent" name the thing the previous
/// instruction produced and nothing else. Bare "it" does not: it is the general
/// anaphor, and every consumer already resolves it through its own subject-aware
/// authority (`resolve_it_pronoun`, `attach_neuter_recipient_resolves_via_subject`).
/// A consumer that wants the chain-created referent for a demonstrative must not
/// get bare "it" smuggled in with it, which is why this is a separate entry point
/// rather than a flag on [`counter_anaphor_created_token_binding`].
pub(super) fn chain_created_demonstrative_binding(
    anaphor_lower: &str,
    ctx: &ParseContext,
) -> Option<TargetFilter> {
    if !anaphor_is_chain_created_demonstrative(anaphor_lower) {
        return None;
    }
    // Same gate the composed entry point applies to the demonstrative form: a
    // non-self trigger subject re-anchors the reference to the triggering object
    // (Pip-Boy 3000), and only a chain that actually produced something can be
    // named at all.
    let subject_allows_token = matches!(
        ctx.subject,
        None | Some(TargetFilter::SelfRef) | Some(TargetFilter::Any)
    );
    (ctx.token_created_in_chain && subject_allows_token).then_some(TargetFilter::LastCreated)
}

/// CR 608.2c: the demonstrative and definite back-reference forms.
/// `the creature` is deliberately EXCLUDED — it legitimately binds a chosen
/// target slot (Longstalk Brawl) and is genuinely ambiguous.
fn anaphor_is_chain_created_demonstrative(anaphor_lower: &str) -> bool {
    nom_on_lower(anaphor_lower, anaphor_lower, |i| {
        value(
            (),
            alt((
                tag("that creature"),
                tag("that token"),
                tag("that permanent"),
                tag("the token"),
                tag("the permanent"),
            )),
        )
        .parse(i)
    })
    .is_some()
}

pub(super) fn counter_anaphor_created_token_binding(
    anaphor_lower: &str,
    ctx: &ParseContext,
) -> Option<TargetFilter> {
    let it_pronoun = is_it_pronoun(anaphor_lower);
    let demonstrative = anaphor_is_chain_created_demonstrative(anaphor_lower);
    if !it_pronoun && !demonstrative {
        return None;
    }
    // A demonstrative/definite anaphor under a non-self, non-`Any` trigger subject
    // refers to that triggering object, not the created token — the subject gate
    // preserves that legacy binding. The bare object pronoun has no such
    // re-anchor, so its gate is only the token flag.
    let subject_allows_token = matches!(
        ctx.subject,
        None | Some(TargetFilter::SelfRef) | Some(TargetFilter::Any)
    );
    if ctx.token_created_in_chain && (it_pronoun || subject_allows_token) {
        Some(TargetFilter::LastCreated)
    } else {
        None
    }
}

fn starts_with_deferred_dynamic_counter_placement(input: &str) -> bool {
    let Ok((after_type, _)) = nom_primitives::parse_counter_type_typed(input) else {
        return false;
    };
    let after_type = after_type.trim_start();
    let Ok((after_counter_word, _)) =
        alt((tag::<_, _, OracleError<'_>>("counters"), tag("counter"))).parse(after_type)
    else {
        return false;
    };
    let after_counter_word = after_counter_word.trim_start();
    let Ok((after_on, _)) = tag::<_, _, OracleError<'_>>("on ").parse(after_counter_word) else {
        return false;
    };

    nom_primitives::scan_contains(after_on, "equal to")
}

/// Output of [`try_parse_put_counter_chain`]: the ordered list of
/// `(counter_type, count)` entries, the shared target, the remaining original-
/// case text after the clause, and any multi-target spec.
pub(super) type PutCounterChain<'a> = (
    Vec<(CounterType, QuantityExpr)>,
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
    ctx: &mut ParseContext,
) -> Option<PutCounterChain<'a>> {
    let ((), after_put) = nom_on_lower(lower, lower, |i| value((), tag("put ")).parse(i))?;
    let mut remaining = after_put.trim_start();
    let mut entries: Vec<(CounterType, QuantityExpr)> = Vec::new();

    loop {
        let (count_expr, rest) = parse_count_expr(remaining)?;
        // Counter types can be multi-word (e.g., "first strike", "double strike"),
        // so use `take_until(" counter")` to consume the full type phrase rather
        // than splitting on the first whitespace.
        let (at_counter, raw_type) = take_until::<_, _, OracleError<'_>>(" counter")
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
    ctx: &mut ParseContext,
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
    let on_offset = lower.len() - on_rest.len();
    let on_text = &text[on_offset..];
    // CR 107.3i: Strip a trailing ", where X is …" binding clause from the
    // target text before calling the target parser. The binding modifies the
    // count expression (resolved upstream by the where-X handlers, e.g.
    // `parse_where_x_quantity_expression`), not the target — leaving it in
    // place feeds the comma + binding into `parse_target` and forces the
    // fallback path on cards like Astarion's Thirst. ASCII-equal-length
    // (asserted above) means the cut byte index found on the lower slice
    // applies identically to the original.
    let on_text = strip_where_x_tail_ascii(on_text, &lower[on_offset..]);
    // CR 608.2c + CR 102.1: Use the ctx-aware variant so the
    // `relative_player_scope` (set to `ScopedPlayer` by an earlier "Choose a
    // player" clause in the same chain) reaches the "they control" suffix
    // parser, routing the controller filter to `ControllerRef::ScopedPlayer`
    // instead of the legacy `You` fallback.
    let (parsed_target, parsed_remainder) = parse_target_with_ctx(on_text, ctx);
    if matches!(parsed_target, TargetFilter::SelfRef) {
        return (TargetFilter::SelfRef, parsed_remainder, None);
    }
    if is_it_pronoun(on_rest) {
        // CR 608.2c: a bare "it" after a token-creating clause earlier in the
        // same effect chain binds to that just-created token — the anaphor's
        // most-recent object referent — not the ability source. Esper Terra:
        // "Create a token ... It gains haste. If it's a Saga, put up to three
        // lore counters on it." `ctx.token_created_in_chain` is seeded by the
        // chunk loop ONLY when the chain's most-recent prior referent is a
        // Token/CopyTokenOf/Populate creator; explicit "~"/name sources return
        // above at the SelfRef guard and never reach here, and non-token
        // self-triggers leave the flag false, so both keep `SelfRef`.
        let it_target = counter_anaphor_created_token_binding(on_rest, ctx)
            .unwrap_or_else(|| resolve_it_pronoun(ctx));
        return (it_target, parsed_remainder, None);
    }
    // CR 608.2k + CR 301.5a: "that creature" in a trigger whose subject is a
    // non-self filter (e.g. Pip-Boy 3000's "Whenever equipped creature
    // attacks ... put a +1/+1 counter on that creature") refers to the
    // triggering source object — not to the parent target (the modal parent
    // here is a `GenericEffect` with no target, leaving `ParentTarget`
    // unbound). Mirrors `resolve_it_pronoun` for the explicit "that creature"
    // anaphor.
    if let Some(rem) = resolve_that_creature_in_trigger(on_rest, ctx) {
        // Map `rem` (sliced from `on_rest`) back into `text` so the returned
        // remainder lifetime matches `text`. `on_rest` is the lowercase view;
        // ASCII-equal-length guard above keeps byte offsets aligned.
        let offset = text.len() - rem.len();
        return (TargetFilter::TriggeringSource, &text[offset..], None);
    }
    // CR 608.2c + CR 111.10: a same-chain demonstrative/definite anaphor
    // ("that creature"/"that token"/"that permanent"/"the token"/"the permanent")
    // placed after a token-creating clause binds to the just-created token, not
    // the unbound parent target. Otterball Antics ("... put a +1/+1 counter on
    // that creature"), Retrieve the Esper ("... put two +1/+1 counters on that
    // token"), Grist ("... put a deathtouch counter on the token"). Reached only
    // after the non-self "that creature" trigger re-anchor above returns None, so
    // Pip-Boy 3000's triggering-source binding is preserved.
    if let Some(bound) = counter_anaphor_created_token_binding(on_rest, ctx) {
        return (bound, parsed_remainder, None);
    }
    // CR 115.1d: "up to N" (and "each of up to N") modifies the target count,
    // not the counter count. Strip it and emit a MultiTargetSpec.
    let (target_text, multi) = if let Some(((), after_up_to)) =
        nom_on_lower(on_rest, on_rest, |i| {
            value((), alt((tag("each of up to "), tag("up to ")))).parse(i)
        }) {
        if let Ok((after_qty, max)) = super::parse_multi_target_count_expr(after_up_to) {
            let on_offset = lower.len() - after_qty.len();
            (&text[on_offset..], Some(MultiTargetSpec::up_to(max)))
        } else {
            let on_offset = lower.len() - on_rest.len();
            (&text[on_offset..], None)
        }
    } else if let Some((count, after_target)) = nom_on_lower(on_rest, on_rest, |i| {
        // CR 601.2c: "each of <N|X> target <type>" — an EXACT-count multi-target
        // distribution (not "up to"). The count binds the number of targets; X
        // resolves from the activation cost's {X}. `peek("target")` does not
        // consume, so the downstream `parse_target_with_ctx` still sees
        // "target <type>". The `space1` + `peek("target")` requirement excludes
        // "each of those creatures" / "each creature" from this arm.
        let (i, ()) = value((), tag("each of ")).parse(i)?;
        let (i, count) = super::parse_multi_target_count_expr(i)?;
        let (i, ()) = value((), space1).parse(i)?;
        let (i, _) = peek(tag("target")).parse(i)?;
        Ok((i, count))
    }) {
        let on_offset = lower.len() - after_target.len();
        (&text[on_offset..], Some(MultiTargetSpec::exact(count)))
    } else {
        let on_offset = lower.len() - on_rest.len();
        (&text[on_offset..], None)
    };
    // CR 107.3i: Mirror the where-X strip above for the "up to N" branch.
    let target_text =
        strip_where_x_tail_ascii(target_text, &lower[lower.len() - target_text.len()..]);
    let (target, remainder) = parse_target_with_ctx(target_text, ctx);
    let (target, remainder) = apply_other_than_that_recipient_suffix(target, remainder, ctx);
    (target, remainder, multi)
}

/// Consume an object-anaphoric distinctness rider on a counter recipient.
///
/// The shared target parser correctly stops after the recipient phrase, leaving
/// `other than that creature` for the counter grammar. Keep the restriction on
/// the typed recipient so ordinary follow-up suffixes (such as `equal to …` or
/// `for each …`) still receive the remaining text.
fn apply_other_than_that_recipient_suffix<'a>(
    target: TargetFilter,
    remainder: &'a str,
    ctx: &mut ParseContext,
) -> (TargetFilter, &'a str) {
    let TargetFilter::Typed(mut typed) = target else {
        return (target, remainder);
    };
    let lower = remainder.to_lowercase();
    let Ok((remaining, ())) = parse_other_than_that_recipient_suffix(&lower) else {
        return (TargetFilter::Typed(typed), remainder);
    };
    let consumed = lower.len() - remaining.len();
    typed.properties.push(FilterProp::DistinctFrom {
        reference: Box::new(resolve_it_pronoun(ctx)),
    });
    (TargetFilter::Typed(typed), &remainder[consumed..])
}

fn parse_other_than_that_recipient_suffix(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        terminated(
            preceded(
                space1,
                preceded(
                    tag("other than that "),
                    alt((tag("creature"), tag("permanent"), tag("token"), tag("card"))),
                ),
            ),
            peek(alt((eof, tag(" "), tag(","), tag(".")))),
        ),
    )
    .parse(input)
}

/// CR 107.3i: Trim a trailing `, where X is …` or ` where X is …` binding
/// from `text` using `text_lower` to locate the cut point. `text` and
/// `text_lower` must be byte-for-byte length-equal (ASCII-only Oracle text,
/// asserted by the caller). Returns `text` unchanged when no binding is
/// present. Local helper so the where-X strip preserves the `'a` lifetime
/// of the caller's `text`; `TextPair`-based variants unify lifetimes and
/// would shorten the returned slice's lifetime to that of `text_lower`.
///
/// Located via a `take_until` combinator on each needle and the minimum
/// consumed prefix wins — this is structural slicing on already-tokenized
/// Oracle text, not a parser dispatch decision.
fn strip_where_x_tail_ascii<'a>(text: &'a str, text_lower: &str) -> &'a str {
    let mut best_pos: Option<usize> = None;
    for needle in [", where x is ", " where x is "] {
        if let Ok((_after, before)) = take_until::<_, _, OracleError<'_>>(needle).parse(text_lower)
        {
            let pos = before.len();
            best_pos = Some(best_pos.map_or(pos, |p| p.min(pos)));
        }
    }
    match best_pos {
        Some(pos) => {
            // allow-noncombinator: structural punctuation/whitespace cleanup
            // on the already-cut prefix, not a parser dispatch decision.
            text[..pos].trim_end_matches(',').trim_end()
        }
        None => text,
    }
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
    let (at_counter, raw_type) = take_until::<_, _, OracleError<'_>>(" counter")
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

/// CR 608.2c + CR 122.1 + CR 122.6: "[a[n]] [additional] counter of that kind
/// on <recipient> [if it doesn't have a counter of that kind on it]" →
/// `Effect::PutChosenCounter`. Anaphoric recipients bind to `ParentTarget` (The
/// Caves of Androzani); declared typed recipients use the shared target parser
/// (Aven Courier). A source-self recipient IS supported: its producer — the
/// printed-population random choice — now binds a concrete kind into
/// resolution state before this clause runs (Crystalline Giant), which is the
/// binding the earlier strict gap was waiting for. The optional suffix becomes an
/// explicit EQ-zero predicate over each resolved target's count of the chosen
/// kind. Combinator-based and fully consuming; `input` has already had the
/// leading "put " stripped.
pub(super) fn try_parse_put_chosen_counter(input: &str) -> Option<Effect> {
    let (recipient_text, _) = (
        opt(alt((tag::<_, _, OracleError<'_>>("an "), tag("a ")))),
        opt(tag("additional ")),
        tag("counter of that kind on "),
    )
        .parse(input.trim())
        .ok()?;

    let (target, remainder) = if let Ok((remainder, _)) = alt((
        tag::<_, _, OracleError<'_>>("that permanent"),
        tag("that creature"),
        tag("it"),
    ))
    .parse(recipient_text)
    {
        (TargetFilter::ParentTarget, remainder)
    } else {
        let (target, remainder) = parse_target(recipient_text);
        // `Any` is this parser's "no recipient recognized", not a legal
        // recipient. `SelfRef` is a real one — "on ~" — and its kind is bound
        // by the preceding choice like every other producer's.
        if matches!(target, TargetFilter::Any) {
            return None;
        }
        (target, remainder)
    };

    let target_condition = |i| -> OracleResult<'_, ChosenCounterCountCondition> {
        let (i, _) = tag("if ").parse(i)?;
        let (i, _) = alt((tag("it doesn't have "), tag("it does not have "))).parse(i)?;
        let (i, _) = alt((tag("a counter"), tag("one or more counters"))).parse(i)?;
        let (i, _) = tag(" of that kind on it").parse(i)?;
        Ok((
            i,
            ChosenCounterCountCondition {
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            },
        ))
    };
    let mut suffix = all_consuming((
        opt(target_condition),
        opt(tag::<_, _, OracleError<'_>>(".")),
    ));
    let (_, (target_condition, _)) = suffix.parse(remainder.trim_start()).ok()?;

    Some(Effect::PutChosenCounter {
        target,
        count: QuantityExpr::Fixed { value: 1 },
        target_condition,
    })
}

pub(super) fn try_parse_put_counter<'a>(
    lower: &str,
    text: &'a str,
    ctx: &mut ParseContext,
) -> Option<(Effect, &'a str, Option<MultiTargetSpec>)> {
    // "put N {type} counter(s) on {target}"
    // Use parse_count_expr to handle Variable("X") for kicker-X patterns.
    let ((), after_put) = nom_on_lower(lower, lower, |i| value((), tag("put ")).parse(i))?;
    let after_put = after_put.trim();

    // CR 608.2d + CR 122.1: "put up to N <type> counters" — the controller may
    // place fewer than N (down to zero). Strip the leading "up to " marker here
    // and wrap the parsed count in `QuantityExpr::up_to` at build time so the AST
    // records the "may pick fewer" grammar. Mirrors the Draw/Discard/PutSticker
    // up-to convention. Only the COUNT-side "up to" (leading, before the counter
    // type) is caught here; the TARGET-side "on up to N target(s)" is a distinct
    // MultiTargetSpec path in `resolve_counter_placement_target`, so the two never
    // shadow each other.
    let (after_put, count_is_up_to) =
        match nom_on_lower(after_put, after_put, |i| value((), tag("up to ")).parse(i)) {
            Some(((), rest)) => (rest.trim_start(), true),
            None => (after_put, false),
        };

    // CR 122.1 + CR 208.3: Detect the dynamic-quantity phrasing
    // "a number of {type} counters equal to {qty}" (Gruff Triplets:
    // "a number of +1/+1 counters equal to its power"). Must run before the
    // generic fixed-count path below — otherwise `parse_count_expr` would read
    // "a" as count=1 and "number" as the counter type. `dynamic_pending`
    // signals that the "equal to {qty}" clause is expected to appear after the
    // counter noun and must be consumed below.
    let (count_expr, rest, dynamic_pending, rebind_deferred_to_placement_object) =
        if let Some(after_phrase) = try_strip_a_number_of(after_put) {
            // Two positions for "equal to {qty}":
            //   eager: "a number of counters equal to X ..." (counter type absent
            //          or implicit — rare in practice)
            //   trailing: "a number of {type} counters equal to X ..." (Gruff class)
            match parse_counter_equal_to(after_phrase) {
                Ok((rest, qty)) => {
                    let rest = rest.strip_prefix(' ').unwrap_or(rest);
                    (qty, rest, false, false)
                }
                Err(_) => (QuantityExpr::Fixed { value: 0 }, after_phrase, true, false),
            }
        } else if starts_with_deferred_dynamic_counter_placement(after_put) {
            // CR 122.1 + CR 208.3: a counter type follows "put" directly with no
            // leading count and no "a number of", and an "equal to {qty}" clause
            // supplies the count after the target ("put +1/+1 counters on it
            // equal to its power" — The Roaring Toeclaws, Experiment Twelve).
            // The "equal to" guard distinguishes this from implicit-count puts
            // ("put a +1/+1 counter on ~"), which have no dynamic clause. Enter
            // the dynamic path so the deferred post-target resolution fills the
            // count.
            (QuantityExpr::Fixed { value: 0 }, after_put, true, true)
        } else {
            let (qty, rest) = parse_count_expr(after_put)?;
            (qty, rest, false, false)
        };

    // CR 122.1: "an additional <type> counter" — the count article was already
    // read by `parse_count_expr` ("an"/"a" → 1; "two additional ..." keeps its
    // numeral). "additional" is a flavor qualifier that does not change the
    // placement count (the counter is added on top of any already present), so
    // strip it before the counter-type parse. Toph, Hardheaded Teacher:
    // "put an additional +1/+1 counter on that land".
    let rest = nom_on_lower(rest, rest, |i| value((), tag("additional ")).parse(i))
        .map(|((), r)| r)
        .unwrap_or(rest);

    // Counter type (e.g. "+1/+1", "loyalty", "charge", "double strike").
    // CR 122.1 + CR 122.1b: route through the shared `parse_counter_type_typed`
    // combinator so multi-word keyword counter names ("first strike", "double
    // strike") canonicalize to `CounterType::Keyword(...)` instead of being
    // truncated at the first whitespace.
    let (after_type, counter_type) = nom_primitives::parse_counter_type_typed(rest).ok()?;

    // Skip "counter" or "counters" keyword, then parse target after "on"
    let after_type = after_type.trim_start();
    let after_counter_word = nom_on_lower(after_type, after_type, |i| {
        value((), alt((tag("counters"), tag("counter")))).parse(i)
    })
    .map(|((), r)| r.trim_start())
    .unwrap_or(after_type);

    // If we entered via "a number of ..." without finding the "equal to" clause
    // eagerly, it may appear here after the counter noun OR after the target.
    // Two Oracle orderings exist:
    //   pre-target:  "a number of +1/+1 counters equal to its power on ..." (Gruff Triplets)
    //   post-target: "a number of +1/+1 counters on ~ equal to that creature's power" (Vincent Valentine)
    // Try consuming "equal to" here; if absent, defer to post-target resolution.
    let (mut count_expr, after_counter_word, dynamic_deferred) = if dynamic_pending {
        match parse_counter_equal_to(after_counter_word) {
            Ok((after_clause, qty)) => {
                // allow-noncombinator: whitespace trim after nom combinator result, not dispatch
                let after_clause = after_clause.strip_prefix(' ').unwrap_or(after_clause);
                (qty, after_clause, false)
            }
            Err(_) => (QuantityExpr::Fixed { value: 0 }, after_counter_word, true),
        }
    } else {
        (count_expr, after_counter_word, false)
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
    let (target, mut remainder, multi_target) =
        resolve_counter_placement_target(on_rest, lower, text, ctx);
    // CR 122.1: Post-target "equal to" clause — when dynamic_deferred is set,
    // the clause wasn't found before "on {target}" and must appear here.
    if dynamic_deferred {
        let trimmed = remainder.trim_start();
        match parse_counter_equal_to(trimmed) {
            Ok((after_clause, qty)) => {
                count_expr = if rebind_deferred_to_placement_object {
                    rebind_post_target_counter_quantity(qty, trimmed, &target)
                } else {
                    qty
                };
                remainder = after_clause.trim_start();
            }
            Err(_) => {
                // CR 122.1 + CR 608.2c: Emit the survivable `Variable { "count" }`
                // placeholder ONLY when there is no in-line "equal to {qty}" clause
                // — i.e. the count is carried externally (a vote-tally head whose
                // "equal to ... votes" suffix was stripped before parsing). Then
                // the PutCounter/PutCounterAll effect still builds and is available
                // for the external bind (`Effect::count_expr_mut`); an unbound
                // `Variable { "count" }` resolves to 0 (quantity.rs `resolve_ref`),
                // so it is inert until bound. Symmetric with the token path's
                // "a number of" placeholder, which is likewise only reached when
                // no inline count was found.
                //
                // If an "equal to" clause IS present but its quantity failed to
                // parse, fall through (return None) to preserve prior dispatch.
                // Emitting a placeholder here would bind a wrong 0-count AND orphan
                // the unparsed quantity / trailing conditional as a swallowed
                // clause — the regression this guard prevents: Drizzt Do'Urden
                // ("...equal to the difference") and Jared Carthalion ("...equal to
                // the number of colors it is") left their following intervening-if
                // / conditional clauses swallowed (Condition_If) when the count
                // silently became a placeholder.
                if nom_on_lower(trimmed, trimmed, |i| value((), tag("equal to ")).parse(i))
                    .is_some()
                {
                    return None;
                }
                count_expr = QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "count".to_string(),
                    },
                };
            }
        }
    }

    if let Some((for_each_count, after_suffix)) = parse_counter_for_each_suffix(remainder) {
        count_expr = replace_fixed_quantity(count_expr, for_each_count);
        remainder = after_suffix;
    }

    Some((
        Effect::PutCounter {
            counter_type,
            // CR 608.2d: wrap the final count in `UpTo` when the clause said
            // "up to N" (stripped above). `count_expr` is never itself an `UpTo`
            // here, so the non-nesting `up_to` debug_assert holds.
            count: if count_is_up_to {
                QuantityExpr::up_to(count_expr)
            } else {
                count_expr
            },
            target,
        },
        remainder,
        multi_target,
    ))
}

/// CR 122.1 + CR 603.2c: Parse the counter-reproduction effect body — "put the
/// same number and kind of counters on <target>" (Captain Marvel, Apex Avenger;
/// Bold Plagiarist) or "put one of each of those kinds of counters on <target>"
/// (Aragorn, Company Leader). Each axis (per-kind magnitude, target) is its own
/// combinator; no verbatim full-line match. Reuses `resolve_counter_placement_target`
/// so `~`→`SelfRef` and "up to one other target creature" are handled exactly as
/// for `PutCounter`.
pub(super) fn try_parse_reproduce_event_counters<'a>(
    lower: &str,
    text: &'a str,
    ctx: &mut ParseContext,
) -> Option<(Effect, &'a str, Option<MultiTargetSpec>)> {
    fn reproduce_counter_phrase(input: &str) -> OracleResult<'_, EventCounterReproductionCount> {
        let (rest, _) = tag("put ").parse(input)?;
        let (rest, per_kind) = alt((
            value(
                EventCounterReproductionCount::SameNumber,
                tag("the same number and kind of counters"),
            ),
            value(
                EventCounterReproductionCount::PerKind(1),
                tag("one of each of those kinds of counters"),
            ),
        ))
        .parse(rest)?;
        let (rest, _) = tag(" on ").parse(rest)?;
        Ok((rest, per_kind))
    }

    let (on_rest, per_kind_count) = reproduce_counter_phrase(lower).ok()?;
    let (target, remainder, multi_target) =
        resolve_counter_placement_target(on_rest, lower, text, ctx);
    Some((
        Effect::ReproduceEventCounters {
            target,
            per_kind_count,
        },
        remainder,
        multi_target,
    ))
}

fn parse_counter_for_each_suffix(remainder: &str) -> Option<(QuantityExpr, &str)> {
    // Delegate to the shared anchored "attach trailing for-each multiplier"
    // authority (CR 107.1 integer count templating) in oracle_effect::lower.
    // The multiplier consumes the entire tail; preserve the original contract
    // of returning an empty post-suffix remainder.
    let count = parse_for_each_multiplier_prefix(remainder)?;
    Some((count, ""))
}

/// CR 122.1 + CR 706.2: Parse a dynamic counter-count "equal to {qty}" clause,
/// covering both the typed-quantity grammar and the event-context back-references
/// that grammar does not reach on its own.
///
/// The nom `parse_equal_to` combinator handles every typed `QuantityRef`
/// (object counts, power/toughness, life refs, …). It does NOT recognize the
/// die-roll / coin-flip back-reference "the result" (CR 706.2) — that phrase is
/// owned by `parse_event_context_quantity`, the single authority for anaphoric
/// event-context amounts ("that much", "that many", "the result"). The
/// equivalent token-creation and life-gain paths already fall back to
/// `parse_event_context_quantity` after their primary quantity parse
/// (`token.rs` count-expression resolution, `imperative.rs::parse_life_equal_quantity`);
/// the counter path is the remaining gap, so it gets the same fallback here
/// rather than leaking "the result" into the shared `parse_quantity_ref` leaf
/// (which would break the `parse_cda_quantity` "returns None for the bare
/// die-result phrase" invariant the where-X binding relies on).
///
/// Inputs reaching here always begin with the literal "equal to " (the three
/// call sites guard on it). The event-context fallback isolates the post-"equal
/// to " phrase up to the first clause boundary so a trailing clause (", then …")
/// stays in the returned remainder, mirroring nom's consume-and-remainder
/// contract.
fn parse_counter_equal_to(
    input: &str,
) -> crate::parser::oracle_nom::error::OracleResult<'_, QuantityExpr> {
    // Typed-quantity grammar first; it owns proper remainder handling.
    if let Ok((rest, qty)) = nom_quantity::parse_equal_to(input) {
        return Ok((rest, qty));
    }

    // CR 107.1b: Composed dynamic quantities ("equal to twice …", "equal to
    // the greatest … among …") live in the CDA grammar, not the nom leaf ref
    // table. Isolate the phrase up to the first clause boundary so trailing
    // ", then …" clauses stay in the remainder.
    let (after_equal, _) = tag("equal to ").parse(input)?;
    let (rest, phrase_raw) =
        take_till::<_, _, OracleError<'_>>(|c| c == ',' || c == '.').parse(after_equal)?;

    // CR 122.1 + CR 603.4 + CR 603.10a: bare anaphoric "the difference" — the
    // count is the difference established by the enclosing trigger's
    // intervening-if comparison ("if it had power greater than ~'s power, put a
    // number of +1/+1 counters on ~ equal to the difference" — Drizzt
    // Do'Urden). The two operands live on the hoisted trigger condition, not in
    // this clause, so emit the deferred placeholder that the trigger-level
    // difference binding in `lower_trigger_ir` (oracle_trigger.rs) resolves
    // against the `QuantityComparison` operands. Distinct from the
    // `parse_cda_quantity` "the difference between A and B" form below, which
    // carries its own operands. Match on the fully-trimmed phrase (both ends) so
    // stray leading whitespace ("equal to  the difference") still binds, and
    // return `take_till`'s own remainder to preserve any trailing ", …" clause.
    if all_consuming(tag::<_, _, OracleError<'_>>("the difference"))
        .parse(phrase_raw.trim())
        .is_ok()
    {
        return Ok((
            rest,
            crate::parser::oracle_effect::difference_anaphor_placeholder(),
        ));
    }

    let phrase = phrase_raw.trim_end();
    if let Some(expr) = crate::parser::oracle_quantity::parse_cda_quantity(phrase) {
        return Ok((&after_equal[phrase.len()..], expr));
    }

    // CR 706.2: event-context back-reference fallback ("equal to the result").
    match crate::parser::oracle_quantity::parse_event_context_quantity(phrase) {
        // Remainder starts immediately after the consumed phrase (before any
        // trailing whitespace), preserving the original clause boundary.
        Some(qty) => Ok((&after_equal[phrase.len()..], qty)),
        None => Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        ))),
    }
}

/// CR 122.1: Consume the "a number of " prefix used in dynamic counter-count
/// phrases, returning the remainder. Returns None when the prefix is absent.
fn try_strip_a_number_of(input: &str) -> Option<&str> {
    tag::<_, _, OracleError<'_>>("a number of ")
        .parse(input)
        .map(|(rest, _)| rest)
        .ok()
}

fn rebind_post_target_counter_quantity(
    count: QuantityExpr,
    clause: &str,
    target: &TargetFilter,
) -> QuantityExpr {
    if !post_target_clause_uses_placement_object(clause) {
        return count;
    }
    rebind_counter_quantity_scope(count, post_target_counter_quantity_scope(target))
}

fn post_target_clause_uses_placement_object(clause: &str) -> bool {
    let Ok((after_equal, _)) = tag::<_, _, OracleError<'_>>("equal to ").parse(clause) else {
        return false;
    };
    alt((
        tag::<_, _, OracleError<'_>>("its power"),
        tag("its toughness"),
        tag("that creature's power"),
        tag("that creature's toughness"),
        tag("that permanent's power"),
        tag("that permanent's toughness"),
    ))
    .parse(after_equal)
    .is_ok()
}

fn post_target_counter_quantity_scope(target: &TargetFilter) -> ObjectScope {
    match target {
        TargetFilter::SelfRef => ObjectScope::Source,
        TargetFilter::TriggeringSource => ObjectScope::EventSource,
        _ => ObjectScope::Target,
    }
}

fn rebind_counter_quantity_scope(count: QuantityExpr, scope: ObjectScope) -> QuantityExpr {
    match count {
        QuantityExpr::Ref { qty } => QuantityExpr::Ref {
            qty: match qty {
                QuantityRef::Power {
                    scope: ObjectScope::Source | ObjectScope::CostPaidObject,
                } => QuantityRef::Power { scope },
                QuantityRef::Toughness {
                    scope: ObjectScope::Source | ObjectScope::CostPaidObject,
                } => QuantityRef::Toughness { scope },
                other => other,
            },
        },
        QuantityExpr::DivideRounded {
            inner,
            divisor,
            rounding,
        } => QuantityExpr::DivideRounded {
            inner: Box::new(rebind_counter_quantity_scope(*inner, scope)),
            divisor,
            rounding,
        },
        QuantityExpr::Offset { inner, offset } => QuantityExpr::Offset {
            inner: Box::new(rebind_counter_quantity_scope(*inner, scope)),
            offset,
        },
        QuantityExpr::ClampMin { inner, minimum } => QuantityExpr::ClampMin {
            inner: Box::new(rebind_counter_quantity_scope(*inner, scope)),
            minimum,
        },
        QuantityExpr::Multiply { factor, inner } => QuantityExpr::Multiply {
            factor,
            inner: Box::new(rebind_counter_quantity_scope(*inner, scope)),
        },
        QuantityExpr::Sum { exprs } => QuantityExpr::Sum {
            exprs: exprs
                .into_iter()
                .map(|expr| rebind_counter_quantity_scope(expr, scope))
                .collect(),
        },
        QuantityExpr::Max { exprs } => QuantityExpr::Max {
            exprs: exprs
                .into_iter()
                .map(|expr| rebind_counter_quantity_scope(expr, scope))
                .collect(),
        },
        QuantityExpr::UpTo { max } => QuantityExpr::UpTo {
            max: Box::new(rebind_counter_quantity_scope(*max, scope)),
        },
        QuantityExpr::Difference { left, right } => QuantityExpr::Difference {
            left: Box::new(rebind_counter_quantity_scope(*left, scope)),
            right: Box::new(rebind_counter_quantity_scope(*right, scope)),
        },
        QuantityExpr::Power { base, exponent } => QuantityExpr::Power {
            base,
            exponent: Box::new(rebind_counter_quantity_scope(*exponent, scope)),
        },
        QuantityExpr::Fixed { .. } => count,
    }
}

/// CR 608.2k + CR 122.1: Anaphoric reference to "the just-referenced counters".
///
/// Recognizes the bare-pronoun / deictic phrases that refer back to a set of
/// counters established earlier in the same ability (a cost, or a trigger's
/// intervening-if condition like "if it has six or more level counters on it"):
///   - deictic: "those counters" / "its counters" / "this {type}'s counters"
///   - bare object pronoun: "them" / "all of them" (CR 608.2k pronoun anaphor)
///
/// This is the single authority for the remove-counter anaphor surface — both
/// `try_parse_remove_counter` (to build the effect) and the imperative dispatch
/// gate (to route the clause here despite the absence of the literal word
/// "counter") delegate to it, so the phrase list never drifts between the two.
pub(super) fn parse_counter_anaphor(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag("all of them"),
            tag("those counters"),
            tag("its counters"),
            tag("this creature's counters"),
            tag("this artifact's counters"),
            tag("this enchantment's counters"),
            tag("this permanent's counters"),
            tag("them"),
        )),
    )
    .parse(input)
}

pub(super) fn try_parse_remove_counter(lower: &str, ctx: &mut ParseContext) -> Option<Effect> {
    // "remove N {type} counter(s) from {target}" or "remove all counters from {target}"
    // CR 122.1: Counter type is optional — "remove all counters" removes every type.
    let ((), after_remove) = nom_on_lower(lower, lower, |i| value((), tag("remove ")).parse(i))?;
    let after_remove = after_remove.trim();

    // CR 608.2k + CR 122.1: Anaphoric counter reference with no "from {target}"
    // clause refers to counters on the ability source (the antecedent
    // established earlier in the same ability's cost or trigger condition,
    // e.g., "if there are four or more charge counters on it, remove those
    // counters and transform it"). Sentinel count -1 with empty counter_type
    // tells the runtime resolver to strip every counter on the source. Mirrors
    // `try_parse_move_counters`' anaphor handling. The anaphor surface is owned
    // by `parse_counter_anaphor` so the dispatch gate and this builder agree.
    if nom_on_lower(after_remove, after_remove, parse_counter_anaphor).is_some() {
        return Some(Effect::RemoveCounter {
            counter_type: None,
            count: QuantityExpr::Fixed { value: -1 },
            target: TargetFilter::SelfRef,
        });
    }

    // CR 122.1: "remove all" uses sentinel count -1, resolved to actual count at runtime.
    // Also handle "up to N" prefix (player may remove fewer).
    // CR 615.5 + CR 608.2h: "that many" / "that much" binds the prevented-damage
    // amount of an enclosing prevention replacement (Protean Hydra class:
    // "prevent that damage and remove that many +1/+1 counters from it"). The
    // amount is the info the rider reads from the prevented event at resolution.
    // Resolves to `EventContextAmount`, matching the `PutCounter` "that many"
    // path used by the Vigor / Phyrexian Hydra cohort.
    let (count, rest) = if let Ok((rest, qty)) = nom_quantity::parse_that_much_or_many(after_remove)
    {
        (QuantityExpr::Ref { qty }, rest.trim_start())
    } else if let Some(((), rest)) = nom_on_lower(after_remove, after_remove, |i| {
        value((), tag("all ")).parse(i)
    }) {
        (QuantityExpr::Fixed { value: -1 }, rest.trim_start())
    } else if let Some(((), rest)) = nom_on_lower(after_remove, after_remove, |i| {
        value((), tag("any number of ")).parse(i)
    }) {
        // CR 107.1c + CR 608.2d: "remove any number of counters" is a
        // resolution-time player choice (any per-type subset, 0..=available,
        // incl. zero). Encode it as `UpTo` over the "remove all" sentinel so the
        // runtime resolver discriminates on the peel FLAG (`count.is_up_to()`),
        // not the scalar: the interactive path derives the legal domain from the
        // board rather than resolving the inner `Fixed{-1}` numerically. If the
        // resolver ever fails to peel, the safe-degrade is the existing
        // "remove all" branch (`Fixed{-1}` clamps to the board — legal, just
        // non-interactive). Rhys, the Evermore / Tetravus.
        (
            QuantityExpr::up_to(QuantityExpr::Fixed { value: -1 }),
            rest.trim_start(),
        )
    } else if let Some(((), rest)) = nom_on_lower(after_remove, after_remove, |i| {
        value((), tag("up to ")).parse(i)
    }) {
        let (n, r) = parse_number(rest.trim())?;
        (QuantityExpr::Fixed { value: n as i32 }, r)
    } else {
        let (n, r) = parse_number(after_remove)?;
        (QuantityExpr::Fixed { value: n as i32 }, r)
    };

    // Try matching "counter(s)" directly (untyped: "remove all counters from ...").
    // If that fails, parse a type word first, then "counter(s)".
    let (counter_type, after_counter_word) = if let Some(((), after_cw)) =
        nom_on_lower(rest, rest, |i| {
            value((), alt((tag("counters"), tag("counter")))).parse(i)
        }) {
        (None, after_cw)
    } else {
        // CR 122.1 + CR 122.1b: shared counter-type combinator handles
        // multi-word keyword counter names (e.g. "first strike").
        let (after_type, counter_type) = nom_primitives::parse_counter_type_typed(rest).ok()?;
        let after_type = after_type.trim_start();
        let ((), after_cw) = nom_on_lower(after_type, after_type, |i| {
            value((), alt((tag("counters"), tag("counter")))).parse(i)
        })?;
        (Some(counter_type), after_cw)
    };
    let after_counter_word = after_counter_word.trim_start();

    let ((), target_text) = nom_on_lower(after_counter_word, after_counter_word, |i| {
        value((), tag("from ")).parse(i)
    })?;
    let target_text = target_text.trim();

    // CR 608.2d: "remove any number of counters from among <objects>" distributes
    // the removal, at resolution, among any number of UNTARGETED permanents — a
    // MULTI-SOURCE choice. The single-source interactive path (Rhys, Tetravus)
    // cannot model "from among", so leave such cards Unimplemented (out of scope)
    // rather than silently collapsing them to a single-source removal (Galloping
    // Lizrog, Eventide's Shadow). Scoped to the `UpTo` ("any number") branch so
    // non-interactive removals are untouched.
    if count.is_up_to()
        && nom_on_lower(target_text, target_text, |i| {
            value((), tag("among ")).parse(i)
        })
        .is_some()
    {
        return None;
    }

    let target = resolve_remove_counter_from_target(target_text, ctx);

    Some(Effect::RemoveCounter {
        counter_type,
        count,
        target,
    })
}

/// CR 115.1d + CR 122.1: Extract the fixed resolution-time selection in
/// "remove … from each of two creatures you control". The surrounding remove
/// parser remains the authority for the counter phrase and object filter; this
/// nom-shaped probe preserves only the untargeted cardinality that `Effect`
/// cannot represent by itself.
pub(super) fn remove_counter_exact_selection_count(lower: &str) -> Option<u32> {
    let descriptor = remove_counter_each_of_descriptor(lower)?;
    parse_fixed_count_descriptor(descriptor).map(|(count, _)| count)
}

/// CR 601.2c: Recover the announced target cardinality in the targeted
/// counterpart, "remove … from each of two target creatures". This stays
/// distinct from the untargeted exact-selection descriptor above.
pub(super) fn remove_counter_exact_target_multi_target(lower: &str) -> Option<MultiTargetSpec> {
    let descriptor = remove_counter_each_of_descriptor(lower)?;
    let (remainder, count) = super::parse_multi_target_count_expr(descriptor).ok()?;
    nom_on_lower(remainder, remainder, |input| {
        value((), preceded(space1, tag("target "))).parse(input)
    })
    .map(|_| MultiTargetSpec::exact(count))
}

/// Extract the descriptor following a remove-counter "from each of" phrase.
/// Keeping the structural prefix parser shared prevents target and untargeted
/// exact-count recognition from drifting.
fn remove_counter_each_of_descriptor(lower: &str) -> Option<&str> {
    let ((), descriptor) = nom_on_lower(lower, lower, |input| {
        preceded(
            take_until(" from each of "),
            value((), tag(" from each of ")),
        )
        .parse(input)
    })?;
    Some(descriptor)
}

/// Parse the non-targeted descriptor after "each of". Targeted fixed-count
/// wording remains in the ordinary target-selection pipeline (CR 601.2c).
fn parse_fixed_count_descriptor(input: &str) -> Option<(u32, &str)> {
    let (count, remainder) = nom_on_lower(input, input, nom_primitives::parse_number)?;
    let is_targeted = nom_on_lower(remainder, remainder, |i| {
        value((), preceded(space1, tag("target "))).parse(i)
    })
    .is_some();
    (!is_targeted && count > 0).then_some((count, remainder.trim_start()))
}

/// Normalize oracle-text counter type strings to canonical engine names.
///
/// - `+1/+1` / `-1/-1` map to the canonical `P1P1` / `M1M1` keys.
/// - All other names are lowercased so that producers emitting different
///   cases (e.g. the replacement parser previously emitted `MINING`, while
///   the cost parser emits `mining`) collapse onto the same `Generic`
///   string at parse time. This makes the AST canonical without relying
///   on the runtime `parse_counter_type` lowercase fallback.
pub(crate) fn normalize_counter_type(raw: &str) -> CounterType {
    parse_counter_type(raw)
}

/// CR 115.1d + CR 122.1: Strip the distribution prefix "each of any number of "
/// from a remove-counter "from <objects>" clause. Returns the remainder for
/// `parse_type_phrase_folding` when the prefix was present (Garnet class — player
/// chooses any number of matching permanents, not a "target" phrase).
fn strip_remove_counter_each_of_any_number(input: &str) -> Option<&str> {
    let after_each_of = nom_on_lower(input, input, |i| value((), opt(tag("each of "))).parse(i))
        .map(|((), rest)| rest)
        .unwrap_or(input);
    nom_on_lower(after_each_of, after_each_of, |i| {
        value((), tag("any number of ")).parse(i)
    })
    .map(|((), rest)| rest.trim_start())
}

/// CR 115.1d + CR 122.1: Strip the fixed-count counterpart of the variable
/// distribution prefix. The number is carried by the imperative AST; this
/// helper leaves the object phrase for the normal typed-filter parser.
fn strip_remove_counter_each_of_fixed_number(input: &str) -> Option<&str> {
    let ((), after_each_of) = nom_on_lower(input, input, |i| value((), tag("each of ")).parse(i))?;
    parse_fixed_count_descriptor(after_each_of).map(|(_, remainder)| remainder)
}

/// CR 122.1 + CR 115.1d: Resolve the "from <objects>" clause of a remove-counter
/// effect. The optional "each of any number of" prefix denotes a variable-count
/// non-target distribution — strip it and parse the remainder as a type phrase
/// instead of routing through `parse_target` (whose bare "each " arm would leave
/// "of any number of …" for `parse_type_phrase_folding` and collapse to `Any`).
fn resolve_remove_counter_from_target(text: &str, ctx: &mut ParseContext) -> TargetFilter {
    if is_self_ref(text) {
        return TargetFilter::SelfRef;
    }
    if is_it_pronoun(text) {
        // CR 608.2k: Bare pronoun — context-dependent
        return resolve_it_pronoun(ctx);
    }
    if let Some(filter_text) = strip_remove_counter_each_of_any_number(text)
        .or_else(|| strip_remove_counter_each_of_fixed_number(text))
    {
        let (t, _rem) = parse_type_phrase_folding_with_ctx(filter_text, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, filter_text);
        return t;
    }
    resolve_counter_target(text, ctx)
}

/// CR 608.2c + CR 608.2k + CR 115.10a: resolve a counter recipient
/// from text — self-ref, bare pronoun, trigger-context "that creature", else a
/// parsed phrase. The anaphor guards run FIRST and in this exact order (order is
/// load-bearing: `it` / `that creature` resolve against the parse context before
/// any descriptor fallback). Shared by put/remove/multiply/double counter
/// parsers, which resolve identically for a targeted and a described recipient —
/// the "target" keyword only changes WHEN the recipient is chosen
/// (CR 115.1 / CR 608.2d), which `lower::target_choice_timing_for_clause` owns.
fn resolve_counter_target(text: &str, ctx: &mut ParseContext) -> TargetFilter {
    if is_self_ref(text) {
        return TargetFilter::SelfRef;
    }
    if is_it_pronoun(text) {
        // CR 608.2k: Bare pronoun — context-dependent.
        return resolve_it_pronoun(ctx);
    }
    if resolve_that_creature_in_trigger(text, ctx).is_some() {
        // CR 608.2k + CR 301.5a: Trigger-context "that creature" → triggering source.
        return TargetFilter::TriggeringSource;
    }
    // The fallback parses with a fresh `ParseContext` so it stays byte-for-byte
    // equivalent to a bare `parse_target(text)` call — the shared `ctx` is read
    // only by the anaphor guards above.
    let (t, _rem) = parse_target(text);
    #[cfg(debug_assertions)]
    assert_no_compound_remainder(_rem, text);
    t
}

/// CR 608.2k + CR 301.5a: Returns `Some(remainder)` when `text` begins with
/// "that creature" AND we are inside a trigger whose subject is a non-self,
/// non-Any filter (e.g. an `AttachedTo` trigger's "Whenever equipped creature
/// attacks"). The remainder is the post-phrase tail so callers can continue
/// parsing trailing punctuation/clauses. Mirrors `resolve_it_pronoun`'s
/// gating: a trigger whose subject is `SelfRef`/`Any` (or no subject) keeps
/// the legacy `ParentTarget` semantics — used for spells/abilities like
/// Twinflame Strive where "that creature" refers back to the parent target.
fn resolve_that_creature_in_trigger<'a>(text: &'a str, ctx: &mut ParseContext) -> Option<&'a str> {
    let (rest, _): (&'a str, &'a str) = tag::<_, _, OracleError<'a>>("that creature")
        .parse(text)
        .ok()?;
    match &ctx.subject {
        Some(subject) if !matches!(subject, TargetFilter::SelfRef | TargetFilter::Any) => {
            Some(rest)
        }
        _ => None,
    }
}

/// CR 122.8: Parse "put its counters on [target]" / "put those counters on
/// [target]" → MoveCounters effect. CR 122.8 covers the trigger-on-leaving
/// case where the source's counters are copied (not strictly moved) to a
/// second object.
///
/// `"its"` / `"this creature's"` / `"those"` all refer anaphorically to the
/// object whose counters the trigger condition (`if it had counters on it`)
/// established. Which object that is depends on the trigger's subject:
///
/// - A **self** dies/leaves trigger ("When ~ dies, put its counters on …" —
///   Scolding Administrator) → the source object itself (`SelfRef`).
/// - An **other-object** leaves trigger ("Whenever a creature you control
///   leaves the battlefield, … put those counters on ~" — The Ozolith) → the
///   triggering creature (`TriggeringSource`), NOT the ability source.
///
/// `resolve_it_pronoun` makes exactly this distinction from `ctx.subject`, so
/// the counter source binds to the right object. The runtime resolver in
/// `effects::counters::resolve_move` performs LKI fallback (CR 400.7), so the
/// counters are read from the leaving object's last-known state either way.
pub(super) fn try_parse_move_counters<'a>(
    lower: &str,
    text: &'a str,
    ctx: &mut ParseContext,
) -> Option<(Effect, &'a str)> {
    let ((), after_put) = nom_on_lower(lower, lower, |i| value((), tag("put ")).parse(i))?;
    let after_put = after_put.trim();
    // Detect the possessive source: "~'s " / "its " / "this creature's " /
    // "those ". The possessive may be followed by a typed counter name
    // ("its +1/+1 counters", Selfless Police Captain) or the bare noun
    // ("its counters", "those counters"). Strip the possessive marker, then
    // parse an optional counter type before the "counter(s)" noun.
    let (source, after_possessive_marker) = if let Some(((), rest)) =
        nom_on_lower(after_put, after_put, |i| value((), tag("~'s ")).parse(i))
    {
        (TargetFilter::SelfRef, rest)
    } else {
        let ((), rest) = nom_on_lower(after_put, after_put, |i| {
            value(
                (),
                alt((tag("its "), tag("this creature's "), tag("those "))),
            )
            .parse(i)
        })?;
        (resolve_it_pronoun(ctx), rest)
    };

    // CR 122.5 + CR 122.8: optional typed counter. "its +1/+1 counters" moves
    // only that kind; "its counters" / "those counters" moves all kinds. When the
    // bare "counter(s)" noun follows the possessive directly, there is no typed
    // qualifier — must check that first, because `parse_counter_type_typed`'s
    // open-ended Generic arm would otherwise consume the noun "counters" itself
    // and leave nothing for the noun match below.
    let bare_counter_noun = nom_on_lower(after_possessive_marker, after_possessive_marker, |i| {
        value((), alt((tag("counters"), tag("counter")))).parse(i)
    })
    .is_some();
    let (counter_type, after_type) = if bare_counter_noun {
        (None, after_possessive_marker)
    } else {
        match nom_primitives::parse_counter_type_typed(after_possessive_marker) {
            Ok((rest, ct)) => (Some(ct), rest.trim_start()),
            Err(_) => (None, after_possessive_marker),
        }
    };

    // Expect the "counter(s)" noun.
    let ((), after_counter_word) = nom_on_lower(after_type, after_type, |i| {
        value((), alt((tag("counters"), tag("counter")))).parse(i)
    })?;
    let ((), after_on) = nom_on_lower(after_counter_word, after_counter_word, |i| {
        value((), tag(" on ")).parse(i)
    })?;

    // Compute byte offset into original `text` for parse_target.
    let offset_in_text = text.len() - after_on.len();
    let (target, remainder) = parse_target(&text[offset_in_text..]);

    Some((
        Effect::MoveCounters {
            // CR 122.8: when a leaves-the-battlefield trigger puts a departed
            // object's counters onto another object, the same number/kinds the
            // object had are placed on the destination — read from the leaving
            // object's last-known information (CR 400.7), so the source must be
            // the triggering object, not the ability source.
            source,
            counter_type,
            count: None,
            mode: CounterTransferMode::Put,
            selection: CounterMoveSelection::StackTarget,
            target,
        },
        remainder,
    ))
}

/// CR 122.5: Parse "move [all/N] [type] counter(s) from [source] onto/to [target]".
/// Handles Bioshift, Fate Transfer, Nesting Grounds, Simic Fluxmage, etc.
pub(super) fn try_parse_move_counters_from(lower: &str, ctx: &mut ParseContext) -> Option<Effect> {
    let ((), after_move) = nom_on_lower(lower, lower, |i| value((), tag("move ")).parse(i))?;
    let after_move = after_move.trim();

    // Parse quantity: "all", "any number of", or a count expression.
    let (count, any_number, rest) = if let Some(((), rest)) =
        nom_on_lower(after_move, after_move, |i| value((), tag("all ")).parse(i))
    {
        (None, false, rest.trim_start())
    } else if let Some(((), rest)) = nom_on_lower(after_move, after_move, |i| {
        value((), tag("any number of ")).parse(i)
    }) {
        (None, true, rest.trim_start())
    } else if let Some((qty, rest)) = parse_count_expr(after_move) {
        (Some(qty), false, rest.trim_start())
    } else {
        // "move a +1/+1 counter" — article consumed by parse_count_expr("a" → 1)
        return None;
    };

    // Try matching "counter(s)" directly (untyped), or parse a type first.
    let (counter_type, after_counter_word) = if let Some(((), after_cw)) =
        nom_on_lower(rest, rest, |i| {
            value((), alt((tag("counters"), tag("counter")))).parse(i)
        }) {
        (None, after_cw)
    } else {
        // CR 122.1 + CR 122.1b: shared counter-type combinator handles
        // multi-word keyword counter names (e.g. "double strike").
        let (after_type, ct) = nom_primitives::parse_counter_type_typed(rest).ok()?;
        let after_type = after_type.trim_start();
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
    let (source_text, target_text) = split_move_counter_from_clause(after_from)?;

    let source = resolve_counter_target(source_text, ctx);
    let target = resolve_counter_target(target_text, ctx);
    let selection = if any_number {
        if contains_target_word(target_text) {
            CounterMoveSelection::StackTargetAnyNumber
        } else {
            CounterMoveSelection::ResolutionDistributionAnyNumber
        }
    } else {
        CounterMoveSelection::StackTarget
    };

    Some(Effect::MoveCounters {
        source,
        counter_type,
        count,
        mode: CounterTransferMode::Move,
        selection,
        target,
    })
}

fn split_move_counter_from_clause(input: &str) -> Option<(&str, &str)> {
    if let Ok((after_delimiter, source_text)) =
        terminated::<_, _, OracleError<'_>, _, _>(take_until(" onto "), tag(" onto ")).parse(input)
    {
        return Some((source_text.trim(), after_delimiter.trim()));
    }
    let (after_delimiter, source_text) =
        terminated::<_, _, OracleError<'_>, _, _>(take_until(" to "), tag(" to "))
            .parse(input)
            .ok()?;
    Some((source_text.trim(), after_delimiter.trim()))
}

fn contains_target_word(input: &str) -> bool {
    input
        .split(|ch: char| !ch.is_ascii_alphabetic())
        .any(|word| word == "target")
}

/// CR 115.10a + CR 608.2d + CR 701.10e: may a counter-multiplication effect be
/// emitted with this recipient? Two admissible shapes: a deterministic anaphor
/// (`SelfRef` / `TriggeringSource` / `ParentTarget` / ... — resolved by the
/// resolver's own tiers) or a DESCRIBED population the resolver can enumerate.
/// Anything else is a phrase the parser failed to classify at all; emitting an
/// effect for it would hand the mass tier a filter that matches every permanent
/// both players control, so it stays an honest `Effect::unimplemented` instead.
fn counter_recipient_is_resolvable(target: &TargetFilter) -> bool {
    target.is_context_ref()
        || (target.names_enumerable_population() && recipient_zone_is_battlefield(target))
}

/// CR 400.1 + CR 701.10e: the non-targeted population tier
/// (`counters::nontargeted_counter_population_ids`) enumerates the BATTLEFIELD
/// only, so a zone-qualified recipient ("each creature in your graveyard")
/// resolves to nothing there. Refuse it at the parser so the clause stays an
/// honest `Effect::unimplemented` instead of lowering to an effect that silently
/// does nothing. `None` means the filter imposes no zone constraint — the
/// battlefield default — and is admissible. The resolver's gate carries the same
/// conjunct as an independent second guard; widening this to a zone-aware
/// enumeration must move BOTH.
fn recipient_zone_is_battlefield(target: &TargetFilter) -> bool {
    matches!(
        target.extract_in_zone(),
        None | Some(crate::types::zones::Zone::Battlefield)
    )
}

/// CR 115.10a + CR 608.2d + CR 701.10e: the single admissibility test both
/// counter-multiplication parsers apply to a recipient phrase — the typed form
/// ("+1/+1 counters", `MultiplyCounter`) and the untyped one ("each kind of
/// counter", `Double { DoubleTarget::Counters }`). One rule, one shape.
///
/// A recipient is admissible when the parser could classify it at all
/// (`counter_recipient_is_resolvable`) AND it is not a singular indefinite
/// descriptor the resolver would wrongly enumerate. The second conjunct is
/// scoped as narrowly as the rules allow: an anaphor is bound by the resolver's
/// own tiers, and a chosen TARGET (CR 115.10a — the text says "target") is bound
/// at announcement, so neither can reach the mass tier and neither is tested for
/// singularity. Only a bare descriptor is, and only a singular indefinite
/// determiner refuses it — see `recipient_is_singular_indefinite` for the
/// determiner set and for why the test is "definitely singular?" rather than
/// "definitely distributive?".
///
/// Wiring counter multiplication into `effects::needs_resolution_object_choice`
/// so the singular form resolves as the choice of one it actually is, rather
/// than staying an honest red, is the real fix and is deliberately out of this
/// change's scope.
fn counter_recipient_is_admissible(target: &TargetFilter, recipient_text: &str) -> bool {
    counter_recipient_is_resolvable(target)
        && (target.is_context_ref()
            || contains_target_word(recipient_text)
            || !recipient_is_singular_indefinite(recipient_text))
}

/// CR 608.2d: does this untargeted recipient phrase name a SINGLE indefinite
/// object ("a creature you control", "an artifact you control") rather than a
/// population?
///
/// The distinction is rules-load-bearing. A plural or distributive phrase
/// ("each creature you control", "creatures you control") names every matching
/// permanent, which the mass tier enumerates correctly. A singular indefinite
/// phrase is a choice of ONE made while the effect is applied, which the engine
/// models with a resolution-time object choice
/// (`effects::needs_resolution_object_choice`) that counter multiplication is
/// not wired into — enumerating it would double counters across the whole
/// population where the card says one.
///
/// The test is deliberately phrased as "is this definitely singular?" rather
/// than "is this definitely distributive?". Refusing everything that does not
/// lead with `each `/`all ` would also refuse a bare plural descriptor
/// ("on creatures you control"), which IS distributive under CR 608.2d and
/// which the mass tier already resolves correctly — turning a working shape
/// into a silent no-op. Only a leading singular determiner is unambiguous
/// evidence of singularity, so only that is refused.
///
/// The determiner set mirrors the one `oracle_target.rs`'s type-phrase parser
/// already strips for this exact purpose (`alt((tag("a "), tag("an "),
/// tag("any ")))`, composed through `"other "`/`"another "`), so the two agree on
/// what counts as a singular indefinite noun phrase. "any creature you control"
/// is documented there as an untargeted SINGULAR controller choice (CR 115.10a,
/// the Kathril class), not a population, and "another creature you control" is
/// the same shape carrying `FilterProp::Another`. Each of the four leading forms
/// is matched with its trailing space, so none of them can match a longer word.
fn recipient_is_singular_indefinite(text: &str) -> bool {
    nom_on_lower(text, text, |i| {
        value(
            (),
            alt((tag("a "), tag("an "), tag("any "), tag("another "))),
        )
        .parse(i)
    })
    .is_some()
}

/// CR 701.10e: Parse "double the number of {type} counters on {target}".
pub(super) fn try_parse_multiply_counter(lower: &str, ctx: &mut ParseContext) -> Option<Effect> {
    let ((), rest) = nom_on_lower(lower, lower, |i| {
        value((), tag("double the number of ")).parse(i)
    })?;
    // Parse counter type — shared combinator handles multi-word keyword
    // counter names (e.g. "first strike") per CR 122.1 + CR 122.1b.
    let (after_type, counter_type) = nom_primitives::parse_counter_type_typed(rest).ok()?;

    // Skip counter type + "counter(s) on "
    let after_type = after_type.trim_start();
    let ((), after_counter_word) = nom_on_lower(after_type, after_type, |i| {
        value((), alt((tag("counters"), tag("counter")))).parse(i)
    })?;
    let after_counter_word = after_counter_word.trim_start();
    let ((), target_text) = nom_on_lower(after_counter_word, after_counter_word, |i| {
        value((), tag("on ")).parse(i)
    })?;

    let target = resolve_counter_target(target_text, ctx);
    if !counter_recipient_is_admissible(&target, target_text) {
        return Some(Effect::unimplemented("double_counters_nontargeted", lower));
    }

    Some(Effect::MultiplyCounter {
        counter_type,
        multiplier: 2,
        target,
    })
}

/// CR 701.10: Dispatch "double the ..." to counter-doubling, life-doubling,
/// mana-doubling, or P/T-doubling.
pub(super) fn try_parse_double_effect(lower: &str, ctx: &mut ParseContext) -> Option<Effect> {
    // CR 701.10e: "double the number of each kind of counter on ..." → all counter
    // kinds. The doubled amount is intrinsic to the resolver ("give as many of
    // those counters as already present"), never a `QuantityExpr` carrier.
    //
    // CR 115.1 + CR 115.10a: the recipient is a TARGET only when the text uses
    // the word "target". A bare descriptor ("each Spider and legendary creature
    // you control") is a POPULATION resolved while the effect is applied
    // (CR 608.2d): `lower::target_choice_timing_for_clause` stamps
    // `TargetChoiceTiming::Resolution` so no stack target slot is built, and
    // `counters::nontargeted_counter_population_ids` enumerates it at
    // resolution — identical treatment to the typed sibling
    // `try_parse_multiply_counter` below. One rule, one shape.
    //
    // The honest-red gate is now keyed on the FILTER VALUE, not on the
    // targeted/descriptor syntax split: a descriptor that names a real
    // population is supported, one the parser could not classify is not.
    if let Some(((), rest)) = nom_on_lower(lower, lower, |i| {
        value((), tag("double the number of each kind of counter on ")).parse(i)
    }) {
        let target = resolve_counter_target(rest, ctx);
        return Some(if counter_recipient_is_admissible(&target, rest) {
            Effect::Double {
                target_kind: DoubleTarget::Counters { counter_type: None },
                target,
            }
        } else {
            // Fragment is the full lowercased clause (diagnostics-only); the
            // parser receives no original-case copy at this dispatch depth.
            Effect::unimplemented("double_counters_nontargeted", lower)
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
        if let Ok((_, target)) = terminated(
            alt((
                value(
                    TargetFilter::ParentTargetController,
                    tag::<_, _, OracleError<'_>>("its controller's life total"),
                ),
                value(
                    TargetFilter::ParentTargetController,
                    tag("that creature's controller's life total"),
                ),
                value(
                    TargetFilter::ParentTargetController,
                    tag("that permanent's controller's life total"),
                ),
            )),
            eof,
        )
        .parse(rest)
        {
            return Some(Effect::Double {
                target_kind: DoubleTarget::LifeTotal,
                target,
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

    // CR 701.10a + CR 613.4c: P/T multiply forms ("double"/"triple" a creature's
    // power/toughness). Delegated so the same three structural arms serve every
    // multiplier word — the `factor` axis (not a Double/Triple effect sibling)
    // distinguishes them.
    try_parse_multiply_pt_effect(lower, ctx)
}

/// CR 701.10a + CR 613.4c: P/T multiplier word → factor. "double" multiplies P/T
/// by 2 (CR 701.10a), "triple" by 3 (Tifa's Limit Break — Final Heaven). The word
/// is the only axis that differs between the forms, so it is parsed once and
/// threaded into the shared P/T arms below as `factor`.
fn parse_pt_multiplier_word(input: &str) -> OracleResult<'_, u32> {
    alt((value(2u32, tag("double ")), value(3u32, tag("triple ")))).parse(input)
}

/// CR 701.10a + CR 613.4c: Parse the three "{multiplier} … power/toughness …"
/// shapes into `Effect::DoublePT`/`DoublePTAll` carrying the parsed `factor`,
/// where `{mult}` is `double` (factor 2) or `triple` (factor 3). The multiplier
/// word is the only varying axis, so the same arms cover both words:
///
/// 1. "{mult} <target>'s power [and toughness]" (possessive, Bulk Up / Tifa).
/// 2. "{mult} its power [and toughness]" (anaphoric "it").
/// 3. "{mult} the power/toughness of <target|each filter>".
pub(super) fn try_parse_multiply_pt_effect(lower: &str, ctx: &mut ParseContext) -> Option<Effect> {
    let (factor, after_word) = nom_on_lower(lower, lower, parse_pt_multiplier_word)?;

    // CR 701.10b: "{mult} <target>'s power [and toughness]" — possessive form covering
    // "double target creature's power" (Bulk Up class) and "double ~'s power" (Devilish
    // Valet / Okaun class, where ~ is the self-reference normalization). Composes with
    // existing `parse_target` building block to cover any target phrase, then matches the
    // possessive P/T tail. Sibling of the "its " and "the " arms below; placed first so
    // the `parse_target`-driven possessive form takes precedence and the more specific
    // "its" / "the X of Y" patterns fall through.
    {
        // Skip patterns owned by sibling arms below.
        let owned_by_sibling = nom_on_lower(after_word, after_word, |i| {
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
            let (target, rem) = parse_target(after_word);
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
                    return Some(Effect::DoublePT {
                        mode,
                        target,
                        factor,
                    });
                }
            }
        }
    }

    // CR 608.2k: "{mult} its power [and toughness]" — possessive "its" is context-dependent
    if let Some(((), rest)) =
        nom_on_lower(after_word, after_word, |i| value((), tag("its ")).parse(i))
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
                factor,
            });
        }
        return None;
    }

    // P/T multiply: "{mult} the power/toughness [and toughness/power] of ..."
    let ((), rest) = nom_on_lower(after_word, after_word, |i| value((), tag("the ")).parse(i))?;

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
        return Some(Effect::DoublePT {
            mode,
            target,
            factor,
        });
    }

    // "each creature you control" / "each other creature" / "each Dragon" → DoublePTAll
    let ((), filter_text) = nom_on_lower(after_mode, after_mode, |i| {
        value((), alt((tag("each "), tag("all ")))).parse(i)
    })?;
    let (target, _) = parse_type_phrase_folding(filter_text);
    Some(Effect::DoublePTAll {
        mode,
        target,
        factor,
    })
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
    use crate::types::ability::TargetChoiceTiming;
    use crate::types::{ControllerRef, FilterProp, TypeFilter, TypedFilter};

    fn default_ctx() -> ParseContext {
        ParseContext::default()
    }

    /// CR 608.2c + CR 122.1: Aven Courier's destination and chosen-counter
    /// absence predicate lower together into the existing placement effect.
    #[test]
    fn put_chosen_counter_parses_typed_target_and_absence_predicate() {
        let effect = try_parse_put_chosen_counter(
            "a counter of that kind on target permanent you control if it doesn't have a counter of that kind on it.",
        )
        .expect("Aven Courier put clause must parse");
        let Effect::PutChosenCounter {
            target: TargetFilter::Typed(filter),
            count,
            target_condition: Some(condition),
        } = effect
        else {
            panic!("expected conditional PutChosenCounter, got {effect:?}");
        };
        assert_eq!(
            filter.type_filters,
            vec![crate::types::ability::TypeFilter::Permanent]
        );
        assert_eq!(filter.controller, Some(ControllerRef::You));
        assert_eq!(count, QuantityExpr::Fixed { value: 1 });
        assert_eq!(condition.comparator, Comparator::EQ);
        assert_eq!(condition.rhs, QuantityExpr::Fixed { value: 0 });
    }

    /// CR 608.2c + CR 122.6: The Caves of Androzani's anaphoric,
    /// condition-free sibling keeps its ParentTarget binding and legacy shape.
    #[test]
    fn put_chosen_counter_preserves_anaphoric_condition_free_form() {
        let effect =
            try_parse_put_chosen_counter("an additional counter of that kind on that permanent.")
                .expect("Caves put clause must parse");
        assert!(matches!(
            effect,
            Effect::PutChosenCounter {
                target: TargetFilter::ParentTarget,
                count: QuantityExpr::Fixed { value: 1 },
                target_condition: None,
            }
        ));
    }

    /// CR 608.2c: Crystalline Giant's source-self consumer. This was a
    /// deliberate strict gap for as long as its producer — the printed-list
    /// random choice — could not bind a kind into resolution state. That
    /// binding now exists (`CounterKindDomain::Printed` +
    /// `CounterKindChooser::Random`), so "on ~" is an ordinary recipient and
    /// the gap is closed rather than merely widened.
    #[test]
    fn put_chosen_counter_accepts_source_self_consumer() {
        let effect = try_parse_put_chosen_counter("a counter of that kind on ~.")
            .expect("source-self recipient is supported once the producer binds a kind");
        assert!(matches!(
            effect,
            Effect::PutChosenCounter {
                target: TargetFilter::SelfRef,
                ..
            }
        ));
    }

    /// CR 115.1d + CR 608.2c + CR 608.2d: Full-card reach guard for Aven
    /// Courier. The source-domain choice is made at resolution, while the
    /// destination is the sole stack-timed target in the following clause.
    #[test]
    fn aven_courier_full_card_separates_stack_target_from_resolution_choice() {
        use crate::parser::oracle::parse_oracle_text;
        use crate::types::triggers::TriggerMode;

        const ORACLE: &str = "Flying\nWhenever this creature attacks, choose a counter on a permanent you control. Put a counter of that kind on target permanent you control if it doesn't have a counter of that kind on it.";
        let parsed = parse_oracle_text(
            ORACLE,
            "Aven Courier",
            &["Flying".to_string()],
            &["Creature".to_string()],
            &["Bird".to_string(), "Advisor".to_string()],
        );
        assert!(
            parsed.parse_warnings.iter().all(|warning| !matches!(
                warning,
                crate::parser::oracle_ir::diagnostic::OracleDiagnostic::SwallowedClause {
                    detector,
                    ..
                } if detector == "Condition_If"
            )),
            "the typed target_condition must discharge the Condition_If audit: {:?}",
            parsed.parse_warnings
        );
        let trigger = parsed
            .triggers
            .iter()
            .find(|trigger| trigger.mode == TriggerMode::Attacks)
            .expect("Aven must have an attack trigger");
        let choose = trigger
            .execute
            .as_ref()
            .expect("attack trigger has an effect");
        assert_eq!(
            choose.target_choice_timing,
            TargetChoiceTiming::Resolution,
            "the untargeted counter-kind choice is made during resolution"
        );
        assert!(matches!(
            choose.effect.as_ref(),
            Effect::ChooseCounterKind {
                target: TargetFilter::Typed(filter),
                ..
            } if filter.type_filters == vec![crate::types::ability::TypeFilter::Permanent]
                && filter.controller == Some(ControllerRef::You)
        ));

        let put = choose
            .sub_ability
            .as_ref()
            .expect("the destination placement follows the choice");
        assert_eq!(
            put.target_choice_timing,
            TargetChoiceTiming::Stack,
            "the printed target is declared when the trigger goes on the stack"
        );
        assert!(matches!(
            put.effect.as_ref(),
            Effect::PutChosenCounter {
                target: TargetFilter::Typed(filter),
                target_condition: Some(ChosenCounterCountCondition {
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Fixed { value: 0 },
                }),
                ..
            } if filter.type_filters == vec![crate::types::ability::TypeFilter::Permanent]
                && filter.controller == Some(ControllerRef::You)
        ));
    }

    /// CR 207.2c + CR 608.2c + CR 608.2d + CR 122.1: Contractual Safeguard's
    /// Addendum effect remains conditional, while its following counter-kind
    /// choice is an untargeted resolution-time choice over creatures the
    /// controller controls. The final instruction applies that chosen kind to
    /// each other creature in the same domain.
    #[test]
    fn contractual_safeguard_full_card_preserves_addendum_and_counter_choice_chain() {
        use crate::parser::oracle::parse_oracle_text;
        use crate::types::ability::AbilityCondition;
        use crate::types::phase::Phase;

        const ORACLE: &str = "Addendum — If you cast this spell during your main phase, put a shield counter on a creature you control. (If it would be dealt damage or destroyed, remove a shield counter from it instead.)\nChoose a kind of counter on a creature you control. Put a counter of that kind on each other creature you control.";
        let parsed = parse_oracle_text(
            ORACLE,
            "Contractual Safeguard",
            &[],
            &["Instant".to_string()],
            &[],
        );
        assert_eq!(
            parsed.abilities.len(),
            1,
            "the three instructions form one sequential spell ability"
        );

        let shield = &parsed.abilities[0];
        assert_eq!(
            shield.condition,
            Some(AbilityCondition::CastDuringPhase {
                phases: vec![Phase::PreCombatMain, Phase::PostCombatMain],
            }),
            "Addendum still gates the shield-counter instruction"
        );
        assert!(matches!(
            shield.effect.as_ref(),
            Effect::PutCounter {
                counter_type: CounterType::Shield,
                target: TargetFilter::Typed(filter),
                ..
            } if filter.type_filters == vec![crate::types::ability::TypeFilter::Creature]
                && filter.controller == Some(ControllerRef::You)
        ));

        let choose = shield
            .sub_ability
            .as_ref()
            .expect("the counter-kind choice follows the Addendum instruction");
        assert_eq!(
            choose.target_choice_timing,
            TargetChoiceTiming::Resolution,
            "the choice uses no printed target and is made during resolution"
        );
        assert!(matches!(
            choose.effect.as_ref(),
            Effect::ChooseCounterKind {
                target: TargetFilter::Typed(filter),
                ..
            } if filter.type_filters == vec![crate::types::ability::TypeFilter::Creature]
                && filter.controller == Some(ControllerRef::You)
        ));

        let put = choose
            .sub_ability
            .as_ref()
            .expect("placing the chosen counter follows the choice");
        assert!(matches!(
            put.effect.as_ref(),
            Effect::PutChosenCounter {
                target: TargetFilter::Typed(filter),
                target_condition: None,
                ..
            } if filter.type_filters == vec![crate::types::ability::TypeFilter::Creature]
                && filter.controller == Some(ControllerRef::You)
                && filter.properties.contains(&FilterProp::Another)
        ));
        assert!(
            put.sub_ability.is_none(),
            "the card has no further instruction after the mass placement"
        );
    }

    #[test]
    fn double_its_controllers_life_total_targets_parent_controller() {
        let result =
            try_parse_double_effect("double its controller's life total", &mut default_ctx());

        assert!(matches!(
            result,
            Some(Effect::Double {
                target_kind: DoubleTarget::LifeTotal,
                target: TargetFilter::ParentTargetController,
            })
        ));
    }

    /// CR 115.1 + CR 608.2d + CR 701.10e: the non-targeted "each kind of counter
    /// on <descriptor>" form names a POPULATION, not a target. It must lower to a
    /// real `Effect::Double { DoubleTarget::Counters }` (never an honest-red
    /// `Unimplemented`) and be stamped `TargetChoiceTiming::Resolution` so no
    /// stack target slot is built; the targeted form keeps `Stack`.
    ///
    /// Revert-failing: before the mass runtime tier existed, all three descriptor
    /// forms below were gated to `Effect::Unimplemented`.
    #[test]
    fn double_each_kind_counter_nontargeted_mass_form_is_resolution_timed() {
        use crate::parser::oracle::parse_oracle_text;

        let mass = try_parse_double_effect(
            "double the number of each kind of counter on each creature you control",
            &mut default_ctx(),
        );
        assert!(
            matches!(
                mass,
                Some(Effect::Double {
                    target_kind: DoubleTarget::Counters { counter_type: None },
                    ..
                })
            ),
            "the non-targeted mass form must lower to Effect::Double, got {mass:?}"
        );

        // CR 608.2d: a SINGULAR indefinite recipient is a choice of ONE made while
        // the effect is applied, NOT a population. The engine models that with a
        // resolution-time object choice (`effects::needs_resolution_object_choice`),
        // which counter multiplication is not wired into — so enumerating it would
        // double counters on the whole population where the card says one. It stays
        // an honest red rather than shipping a silently rules-wrong effect.
        //
        // This is the boundary of what this change claims: the DISTRIBUTIVE form is
        // now supported, the singular form is deliberately not. Reverting the
        // distributive conjunct in `try_parse_double_effect` turns this row green
        // while making the sweep rules-wrong, which is exactly what it guards.
        let singular = try_parse_double_effect(
            "double the number of each kind of counter on a creature you control",
            &mut default_ctx(),
        );
        assert!(
            matches!(
                singular,
                // allow-noncombinator: test assertion on emitted Unimplemented category name
                Some(Effect::Unimplemented { ref name, .. }) if name == "double_counters_nontargeted"
            ),
            "a singular indefinite recipient must stay an honest Unimplemented, got {singular:?}"
        );

        // Positive reach-guard for the negative above: the SAME phrase with a
        // distributive quantifier IS supported, proving the refusal is the
        // plurality conjunct and not an upstream parse failure of "creature you
        // control".
        let distributive = try_parse_double_effect(
            "double the number of each kind of counter on each creature you control",
            &mut default_ctx(),
        );
        assert!(
            matches!(
                distributive,
                Some(Effect::Double {
                    target_kind: DoubleTarget::Counters { counter_type: None },
                    ..
                })
            ),
            "the distributive twin of the singular phrase must be supported, got {distributive:?}"
        );

        let targeted = try_parse_double_effect(
            "double the number of each kind of counter on target creature",
            &mut default_ctx(),
        );
        assert!(
            matches!(
                targeted,
                Some(Effect::Double {
                    target_kind: DoubleTarget::Counters { counter_type: None },
                    ..
                })
            ),
            "targeted form must still resolve to Effect::Double, got {targeted:?}"
        );

        // CR 115.10a + CR 608.2d, at the LOWERING seam: no literal "target" word
        // ⇒ Resolution timing ⇒ `collect_target_slots_inner` builds no slot.
        let descriptor = parse_oracle_text(
            "Double the number of each kind of counter on each creature you control.",
            "Descriptor Counter Doubler",
            &[],
            &["Instant".to_string()],
            &[],
        );
        assert_eq!(
            descriptor.abilities[0].target_choice_timing,
            TargetChoiceTiming::Resolution,
            "a described population is chosen while the effect is applied"
        );
        assert!(
            descriptor.abilities[0].multi_target.is_none(),
            "a described population declares no variable-count target bound"
        );

        let target_word = parse_oracle_text(
            "Double the number of each kind of counter on target creature.",
            "Targeted Counter Doubler",
            &[],
            &["Instant".to_string()],
            &[],
        );
        assert_eq!(
            target_word.abilities[0].target_choice_timing,
            TargetChoiceTiming::Stack,
            "the literal word \"target\" keeps the CR 601.2c stack-time choice"
        );
        assert!(
            target_word.abilities[0].multi_target.is_none(),
            "a single required target declares no MultiTargetSpec"
        );
    }

    /// CR 608.2d — SIBLING PARITY. `counter_recipient_is_admissible` is the single
    /// authority for BOTH counter-multiplication parsers, so the typed form
    /// (`MultiplyCounter`) draws the singular/distributive line in exactly the same
    /// place as the untyped one. A singular indefinite recipient is a choice of ONE
    /// made while the effect is applied, which counter multiplication is not wired
    /// into (`effects::needs_resolution_object_choice` lists only `PutCounter` /
    /// `ChooseCounterKind`), so enumerating it would double counters across the
    /// whole population where the card says one.
    ///
    /// Each negative below is paired with its own positive reach-guard, so a refusal
    /// caused by an upstream parse failure rather than by the plurality conjunct
    /// would fail the test rather than pass it silently.
    #[test]
    fn typed_counter_multiplication_draws_the_same_singular_distributive_line() {
        for (singular, distributive) in [
            (
                "double the number of +1/+1 counters on a creature you control",
                "double the number of +1/+1 counters on each creature you control",
            ),
            (
                "double the number of charge counters on an artifact you control",
                "double the number of charge counters on each artifact you control",
            ),
        ] {
            let refused = try_parse_multiply_counter(singular, &mut default_ctx());
            assert!(
                matches!(
                    refused,
                    Some(Effect::Unimplemented { ref name, .. })
                        // allow-noncombinator: test assertion on emitted Unimplemented category name
                        if name == "double_counters_nontargeted"
                ),
                "singular indefinite recipient must stay an honest Unimplemented \
                 for the TYPED form too, got {refused:?} from {singular:?}"
            );

            let accepted = try_parse_multiply_counter(distributive, &mut default_ctx());
            assert!(
                matches!(accepted, Some(Effect::MultiplyCounter { .. })),
                "reach-guard: the distributive twin must parse, so the refusal above \
                 is the plurality conjunct and not an upstream parse failure; \
                 got {accepted:?} from {distributive:?}"
            );
        }

        // REGRESSION GUARD (CR 608.2d): a bare PLURAL descriptor carries no
        // quantifier and no article, and is distributive — the mass tier already
        // resolves it correctly. Phrasing the conjunct as "definitely singular?"
        // rather than "definitely distributive?" is what keeps this shape working;
        // an `each`/`all`-only test would silently turn it into a no-op.
        let plural = try_parse_multiply_counter(
            "double the number of +1/+1 counters on creatures you control",
            &mut default_ctx(),
        );
        assert!(
            matches!(plural, Some(Effect::MultiplyCounter { .. })),
            "a bare plural descriptor is distributive and must stay admissible, got {plural:?}"
        );

        // CR 115.10a: "any ..." and "another ..." are singular indefinite noun
        // phrases exactly like "a ...", so they are refused for the same reason —
        // the determiner set here mirrors the one `oracle_target.rs` strips.
        // Each is paired with a distributive twin, so a refusal caused by an
        // upstream parse failure would fail the test rather than pass it.
        for (singular, distributive) in [
            (
                "double the number of +1/+1 counters on any creature you control",
                "double the number of +1/+1 counters on each creature you control",
            ),
            (
                "double the number of +1/+1 counters on another creature you control",
                "double the number of +1/+1 counters on each other creature you control",
            ),
        ] {
            let refused = try_parse_multiply_counter(singular, &mut default_ctx());
            assert!(
                matches!(
                    refused,
                    Some(Effect::Unimplemented { ref name, .. })
                        // allow-noncombinator: test assertion on emitted Unimplemented category name
                        if name == "double_counters_nontargeted"
                ),
                "a singular indefinite determiner must be refused, got {refused:?} \
                 from {singular:?}"
            );

            let accepted = try_parse_multiply_counter(distributive, &mut default_ctx());
            assert!(
                matches!(accepted, Some(Effect::MultiplyCounter { .. })),
                "reach-guard: the distributive twin must parse, got {accepted:?} \
                 from {distributive:?}"
            );
        }

        // The other two admissible classes are unaffected by the conjunct.
        let targeted = try_parse_multiply_counter(
            "double the number of +1/+1 counters on target creature",
            &mut default_ctx(),
        );
        assert!(
            matches!(targeted, Some(Effect::MultiplyCounter { .. })),
            "CR 115.1: a chosen target stays admissible, got {targeted:?}"
        );

        let mut ctx = default_ctx();
        ctx.subject = Some(TargetFilter::Typed(TypedFilter::creature()));
        let anaphor =
            try_parse_multiply_counter("double the number of +1/+1 counters on it", &mut ctx);
        assert!(
            matches!(
                anaphor,
                Some(Effect::MultiplyCounter {
                    target: TargetFilter::TriggeringSource,
                    ..
                })
            ),
            "CR 608.2k: a trigger-context anaphor stays admissible, got {anaphor:?}"
        );
    }

    /// V4b (CR 115.1 + CR 608.2d): the parser stays honest-red on BOTH
    /// counter-multiplication forms when the recipient phrase names no
    /// enumerable population. `TargetFilter::names_enumerable_population` is the
    /// single authority; without it the typed form emitted a sweeping
    /// `MultiplyCounter { Typed{} }` that doubles every counter on every
    /// permanent BOTH players control.
    ///
    /// Revert-failing on the typed half: `…+1/+1 counters on each frobnicator you
    /// control` currently yields `Effect::MultiplyCounter`, not `Unimplemented`.
    #[test]
    fn counter_multiplication_with_unresolvable_recipient_stays_unimplemented() {
        let unresolvable = [
            // A phrase the parser cannot classify at all → contentless `Typed`.
            "each frobnicator you control",
            // CR 115.1: `TargetFilter::Any` matches every object.
            "everything",
        ];
        for phrase in unresolvable {
            let untyped = try_parse_double_effect(
                &format!("double the number of each kind of counter on {phrase}"),
                &mut default_ctx(),
            );
            assert!(
                matches!(
                    untyped,
                    // allow-noncombinator: test assertion on emitted Unimplemented category name
                    Some(Effect::Unimplemented { ref name, .. }) if name == "double_counters_nontargeted"
                ),
                "untyped form must stay honest-red for {phrase:?}, got {untyped:?}"
            );
            let typed = try_parse_double_effect(
                &format!("double the number of +1/+1 counters on {phrase}"),
                &mut default_ctx(),
            );
            assert!(
                matches!(
                    typed,
                    // allow-noncombinator: test assertion on emitted Unimplemented category name
                    Some(Effect::Unimplemented { ref name, .. }) if name == "double_counters_nontargeted"
                ),
                "typed form must stay honest-red for {phrase:?}, got {typed:?}"
            );
        }

        // PAIRED POSITIVE REACH-GUARD: a describable population is NOT refused on
        // either form, so the negatives above cannot pass because the whole
        // dispatch short-circuited.
        let untyped_ok = try_parse_double_effect(
            "double the number of each kind of counter on each creature you control",
            &mut default_ctx(),
        );
        assert!(
            matches!(
                untyped_ok,
                Some(Effect::Double {
                    target_kind: DoubleTarget::Counters { counter_type: None },
                    ..
                })
            ),
            "describable population must still lower on the untyped form, got {untyped_ok:?}"
        );
        let typed_ok = try_parse_double_effect(
            "double the number of +1/+1 counters on each creature you control",
            &mut default_ctx(),
        );
        let Some(Effect::MultiplyCounter {
            target: TargetFilter::Typed(ref tf),
            ..
        }) = typed_ok
        else {
            panic!("describable population must still lower on the typed form, got {typed_ok:?}");
        };
        assert!(tf
            .type_filters
            .contains(&crate::types::ability::TypeFilter::Creature));
        assert_eq!(tf.controller, Some(ControllerRef::You));

        // HOSTILE FIXTURE: a deterministic anaphor is admitted by the OTHER half of
        // `counter_recipient_is_resolvable` (`is_context_ref`), not by the
        // population predicate — it must not be swept up by the refusal.
        let mut ctx = default_ctx();
        ctx.subject = Some(TargetFilter::Typed(TypedFilter::creature()));
        let anaphor =
            try_parse_double_effect("double the number of each kind of counter on it", &mut ctx);
        assert!(
            matches!(
                anaphor,
                Some(Effect::Double {
                    target: TargetFilter::TriggeringSource,
                    ..
                })
            ),
            "a trigger-context anaphor stays supported, got {anaphor:?}"
        );

        // DISCLOSED NON-CLAIM (measured): a PARTIALLY classified recipient is
        // ACCEPTED. The predicate's contract is "this filter names SOME
        // population", never "the RIGHT population" — asserting this red would
        // over-claim the gate. `each permanent that isn't a creature you control`
        // parses to `Typed{[Permanent], controller: None}` and still resolves
        // against both players' permanents; that is a pre-existing recipient
        // misparse (backlog root cause 6), out of scope here.
        let partial = try_parse_double_effect(
            "double the number of each kind of counter on each permanent that isn't a creature you control",
            &mut default_ctx(),
        );
        // The MEASURED shape is pinned, not merely the acceptance. The gate's
        // contract is "this filter names SOME population"; what bounds the sweep
        // is which population `parse_target` actually produced. Asserting only
        // `Effect::Double { .. }` would let a future `parse_target` change widen
        // that population silently — e.g. dropping the `Permanent` type filter
        // would turn this into a both-players, every-object sweep while the test
        // stayed green. Pin the two axes that bound it.
        let Some(Effect::Double {
            target: TargetFilter::Typed(ref recipient),
            ..
        }) = partial
        else {
            panic!("a partially classified recipient is accepted, not refused, got {partial:?}");
        };
        assert_eq!(
            recipient.type_filters,
            vec![TypeFilter::Permanent],
            "measured recipient type filter changed — re-audit the sweep scope: {partial:?}"
        );
        assert_eq!(
            recipient.controller, None,
            "measured recipient controller scope changed — re-audit the sweep scope: {partial:?}"
        );
    }

    /// CR 400.1 + CR 701.10e: a ZONE-QUALIFIED recipient stays honest red. The
    /// non-targeted population tier enumerates the BATTLEFIELD only, so "each
    /// creature in your graveyard" would lower to an effect that silently does
    /// nothing. `recipient_zone_is_battlefield` refuses it at the parser instead.
    ///
    /// The reach-guard is what makes this non-vacuous: it asserts the recipient
    /// really does parse to a graveyard-qualified filter, so the refusal is the
    /// zone conjunct and not an upstream parse failure.
    #[test]
    fn counter_multiplication_with_zone_qualified_recipient_stays_unimplemented() {
        use crate::types::zones::Zone;

        let recipient = "each creature in your graveyard";
        let parsed_recipient = resolve_counter_target(recipient, &mut default_ctx());
        assert_eq!(
            parsed_recipient.extract_in_zone(),
            Some(Zone::Graveyard),
            "reach-guard: the recipient must really carry a graveyard zone \
             constraint, got {parsed_recipient:?}"
        );
        assert!(
            parsed_recipient.names_enumerable_population(),
            "reach-guard: it must otherwise look like a real population, so the \
             refusal below is the ZONE conjunct: {parsed_recipient:?}"
        );

        for clause in [
            format!("double the number of each kind of counter on {recipient}"),
            format!("double the number of +1/+1 counters on {recipient}"),
        ] {
            let effect = try_parse_double_effect(&clause, &mut default_ctx());
            assert!(
                matches!(
                    effect,
                    // allow-noncombinator: test assertion on emitted Unimplemented category name
                    Some(Effect::Unimplemented { ref name, .. }) if name == "double_counters_nontargeted"
                ),
                "a zone-qualified recipient must stay honest red, got {effect:?} from {clause:?}"
            );
        }

        // PAIRED POSITIVE: the same phrase WITHOUT the zone qualifier is admitted,
        // so the refusal is the zone conjunct alone.
        let battlefield = try_parse_double_effect(
            "double the number of each kind of counter on each creature you control",
            &mut default_ctx(),
        );
        assert!(
            matches!(
                battlefield,
                Some(Effect::Double {
                    target_kind: DoubleTarget::Counters { counter_type: None },
                    ..
                })
            ),
            "the unqualified twin must still lower, got {battlefield:?}"
        );
    }

    /// §5 F2 ordering (CR 608.2k): the anaphor guards run BEFORE the descriptor
    /// fallback, so "... on it" inside a trigger whose subject is a non-self
    /// creature filter resolves to the triggering object (`TriggeringSource`) as
    /// a Supported single-object target — NOT an honest-red Unimplemented.
    #[test]
    fn double_each_kind_counter_on_it_binds_triggering_source_in_trigger_context() {
        let mut ctx = default_ctx();
        ctx.subject = Some(TargetFilter::Typed(TypedFilter::creature()));
        let text = "double the number of each kind of counter on it";
        let effect =
            try_parse_double_effect(text, &mut ctx).expect("parse double-counter on-it anaphor");
        assert!(
            matches!(
                effect,
                Effect::Double {
                    target_kind: DoubleTarget::Counters { counter_type: None },
                    target: TargetFilter::TriggeringSource,
                }
            ),
            "on-it anaphor in trigger context must bind TriggeringSource, got {effect:?}"
        );
    }

    /// V6 PIN (CR 608.2c + CR 115.10a) — the anaphor classes keep their current
    /// lowering. The new `Double { DoubleTarget::Counters }` arm in
    /// `lower::target_choice_timing_for_clause` carries `!target.is_context_ref()`,
    /// which the sibling `MultiplyCounter` arm deliberately does NOT: retro-fitting
    /// it there would flip THIRTY-ONE shipping cards from `Resolution` to `Stack`
    /// across FOUR anaphor classes — the census measured over the full corpus and
    /// listed card-by-card on that arm in `lower.rs`. The rows below are one
    /// exemplar per class, not the population; do not read their count as the
    /// blast radius. This test is what makes the "separate arms" decision
    /// auditable — unify them and it flips.
    ///
    /// Committed runtime anchors for the `ParentTarget` class:
    /// `crates/engine/tests/integration/scythecat_cub_second_resolution.rs`
    /// (`scythecat_cub_doubles_counters_on_second_landfall_resolution`) and
    /// `crates/engine/src/game/effects/mod.rs` (`turtle_van_doubles_counters_on_matching_crewer`).
    #[test]
    fn counter_multiplication_anaphor_recipients_keep_their_lowered_timing() {
        use crate::parser::oracle::parse_oracle_text;

        fn trigger_execute_timing(oracle: &str, name: &str) -> TargetChoiceTiming {
            let parsed = parse_oracle_text(oracle, name, &[], &["Creature".to_string()], &[]);
            parsed
                .triggers
                .iter()
                .find_map(|t| t.execute.as_ref())
                .map(|a| a.target_choice_timing)
                .unwrap_or_else(|| panic!("{name}: trigger must lower an execute ability"))
        }

        // (i) `Double { Counters, TriggeringSource }` — the NEW arm's context-ref
        // conjunct holds it at Stack, exactly where it already works today.
        assert_eq!(
            trigger_execute_timing(
                "Whenever another creature enters, double the number of each kind of counter on it.",
                "Untyped Anaphor Doubler",
            ),
            TargetChoiceTiming::Stack,
            "a context-ref recipient on the untyped form keeps Stack timing"
        );

        // (ii) `MultiplyCounter { SelfRef }` — Primordial Hydra / Level Up / Lily Bowen.
        assert_eq!(
            trigger_execute_timing(
                "At the beginning of your upkeep, double the number of +1/+1 counters on this creature.",
                "SelfRef Anaphor Doubler",
            ),
            TargetChoiceTiming::Resolution,
            "the MultiplyCounter arm has no context-ref conjunct — SelfRef stays Resolution"
        );

        // (iii) `MultiplyCounter { TriggeringSource }` — Aragorn / Fractal Harness.
        assert_eq!(
            trigger_execute_timing(
                "Whenever another creature enters, double the number of +1/+1 counters on it.",
                "TriggeringSource Anaphor Doubler",
            ),
            TargetChoiceTiming::Resolution,
            "TriggeringSource stays Resolution on the MultiplyCounter arm"
        );

        // (iv) `MultiplyCounter { ParentTarget }` — Scythecat Cub / Turtle Van. The
        // chained shape is what the two real cards produce: a typed targeted parent
        // clause followed by a bare-anaphor counter clause, rebound to
        // `TargetFilter::ParentTarget` by the counter-anaphor fixup in
        // `oracle_effect/mod.rs`.
        let chained = parse_oracle_text(
            "Whenever a land you control enters, put a +1/+1 counter on target creature you control. \
             If you control a Cat, double the number of +1/+1 counters on that creature instead.",
            "ParentTarget Anaphor Doubler",
            &[],
            &["Creature".to_string()],
            &[],
        );
        let mut multiply_timings = Vec::new();
        fn walk(
            def: &crate::types::ability::AbilityDefinition,
            out: &mut Vec<(TargetFilter, TargetChoiceTiming)>,
        ) {
            if let Effect::MultiplyCounter { target, .. } = &*def.effect {
                out.push((target.clone(), def.target_choice_timing));
            }
            if let Some(sub) = &def.sub_ability {
                walk(sub, out);
            }
            if let Some(other) = &def.else_ability {
                walk(other, out);
            }
        }
        for trigger in &chained.triggers {
            if let Some(execute) = &trigger.execute {
                walk(execute, &mut multiply_timings);
            }
        }
        let parent_target_timing = multiply_timings
            .iter()
            .find(|(target, _)| matches!(target, TargetFilter::ParentTarget))
            .map(|(_, timing)| *timing)
            .unwrap_or_else(|| {
                panic!("chained clause must lower a MultiplyCounter{{ParentTarget}}: {multiply_timings:?}")
            });
        assert_eq!(
            parent_target_timing,
            TargetChoiceTiming::Resolution,
            "ParentTarget stays Resolution — a Stack-timed anaphor with no chooser would \
             ask collect_target_slots_inner to build a player-chosen slot"
        );

        // (v) `MultiplyCounter { TrackedSet }` — Biogenic Upgrade / Omnivorous
        // Flytrap. The FOURTH anaphor class, and the one a variant-name census
        // misses: `is_context_ref()` covers `TrackedSet`, so "distribute three
        // +1/+1 counters among one, two, or three target creatures, then double
        // the number of +1/+1 counters on each of those creatures" binds its
        // recipient to the distributed-among set, not to a fresh target slot.
        // The card's SENTENCE contains the word "target", but the effect's own
        // clause does not: `", then "` is a `ClauseBoundary::Then` split, so the
        // `MultiplyCounter` node gets its own fragment — "double the number of
        // +1/+1 counters on each of those creatures" — which carries no
        // "target ". The `MultiplyCounter` arm's `scan_contains` therefore yields
        // `Resolution` on its own, exactly as it does for Omnivorous Flytrap's
        // separate sentence. Do not read this row as evidence that the arm's scan
        // is insufficient for the class.
        let biogenic = parse_oracle_text(
            "Distribute three +1/+1 counters among one, two, or three target creatures, \
             then double the number of +1/+1 counters on each of those creatures.",
            "Biogenic Upgrade",
            &[],
            &["Sorcery".to_string()],
            &[],
        );
        let mut tracked = Vec::new();
        for ability in &biogenic.abilities {
            walk(ability, &mut tracked);
        }
        let tracked_timing = tracked
            .iter()
            .find(|(target, _)| matches!(target, TargetFilter::TrackedSet { .. }))
            .map(|(_, timing)| *timing)
            .unwrap_or_else(|| {
                panic!("Biogenic Upgrade must lower a MultiplyCounter{{TrackedSet}}: {tracked:?}")
            });
        assert_eq!(
            tracked_timing,
            TargetChoiceTiming::Resolution,
            "TrackedSet stays Resolution — it is bound from the distribution, not chosen"
        );
    }

    /// V2 real-card (CR 701.10e + CR 205.4a + CR 115.10a): Ultimate Spider-Man's
    /// back-face "Whenever you attack, double the number of each kind of counter
    /// on each Spider and legendary creature you control" is a NON-targeted
    /// DESCRIBED population. It must lower to a real
    /// `Effect::Double { DoubleTarget::Counters }`, stamped
    /// `TargetChoiceTiming::Resolution` (no stack target slot), over the two-leg
    /// union `Or[ Typed{Spider, You}, Typed{Creature + Legendary, You} ]`.
    ///
    /// Revert-failing on three independent seams: the parser gate (was
    /// `Unimplemented`), the lowering arm (no `Double{Counters}` arm existed) and
    /// the grammar (the bare-"and" branch dropped the supertype-led right
    /// conjunct AND the shared trailing controller suffix).
    #[test]
    fn ultimate_spider_man_mass_double_counter_trigger_lowers_to_resolution_timed_double() {
        use crate::parser::oracle::parse_oracle_text;
        use crate::types::triggers::TriggerMode;

        let parsed = parse_oracle_text(
            "First strike, haste\n\
             Camouflage — {2}: Put a +1/+1 counter on Ultimate Spider-Man. He gains hexproof and becomes colorless until end of turn.\n\
             Whenever you attack, double the number of each kind of counter on each Spider and legendary creature you control.",
            "Ultimate Spider-Man",
            &[],
            &["Legendary".to_string(), "Creature".to_string()],
            &[],
        );

        let trigger = parsed
            .triggers
            .iter()
            .find(|t| t.mode == TriggerMode::YouAttack)
            .expect("the back face must parse a YouAttack trigger");
        let execute = trigger
            .execute
            .as_ref()
            .expect("the YouAttack trigger must lower an execute ability");

        // CR 115.10a + CR 608.2d: described population ⇒ resolution-time choice.
        assert_eq!(
            execute.target_choice_timing,
            TargetChoiceTiming::Resolution,
            "the descriptor population must not declare a stack target slot"
        );
        assert!(
            execute.multi_target.is_none(),
            "a descriptor population declares no variable-count target bound"
        );

        let Effect::Double {
            target_kind: DoubleTarget::Counters { counter_type: None },
            target,
        } = &*execute.effect
        else {
            panic!(
                "USM's YouAttack trigger must lower to an untyped counter doubling, got {:?}",
                execute.effect
            );
        };

        // CR 205.2a + CR 205.4a + CR 109.4: BOTH legs survive the union and BOTH
        // carry the shared trailing "you control" suffix; only the right leg
        // carries the `Legendary` supertype.
        let TargetFilter::Or { filters } = target else {
            panic!("the population must be a two-leg Or union, got {target:?}");
        };
        assert_eq!(filters.len(), 2, "exactly two legs: {filters:?}");
        let legs: Vec<&TypedFilter> = filters
            .iter()
            .map(|f| match f {
                TargetFilter::Typed(tf) => tf,
                other => panic!("each leg must be a Typed filter, got {other:?}"),
            })
            .collect();
        for leg in &legs {
            assert_eq!(
                leg.controller,
                Some(ControllerRef::You),
                "the trailing \"you control\" scopes both legs: {leg:?}"
            );
        }
        let legendary = FilterProp::HasSupertype {
            value: crate::types::card_type::Supertype::Legendary,
        };
        assert!(
            legs[0]
                .type_filters
                .contains(&crate::types::ability::TypeFilter::Subtype(
                    "Spider".to_string()
                )),
            "the left leg is the Spider subtype: {:?}",
            legs[0].type_filters
        );
        assert!(
            !legs[0].properties.contains(&legendary),
            "the Legendary supertype must NOT spread onto the Spider leg: {:?}",
            legs[0].properties
        );
        assert!(
            legs[1]
                .type_filters
                .contains(&crate::types::ability::TypeFilter::Creature),
            "the right leg is a creature: {:?}",
            legs[1].type_filters
        );
        assert!(
            legs[1].properties.contains(&legendary),
            "the right leg carries the Legendary supertype: {:?}",
            legs[1].properties
        );

        // REACH-GUARD, scoped to the trigger's OWN execute sub-tree. A card-wide
        // "zero Unimplemented" assertion would be false both before AND after this
        // change: with no MTGJSON keyword names supplied, the "First strike, haste"
        // line is itself an `Unimplemented` node (a loader-input artefact).
        fn count_unimplemented(def: &crate::types::ability::AbilityDefinition) -> usize {
            let mut total = usize::from(matches!(&*def.effect, Effect::Unimplemented { .. }));
            if let Some(sub) = &def.sub_ability {
                total += count_unimplemented(sub);
            }
            if let Some(other) = &def.else_ability {
                total += count_unimplemented(other);
            }
            total
        }
        assert_eq!(
            count_unimplemented(execute),
            0,
            "the trigger's execute sub-tree must be fully supported"
        );

        // POSITIVE REACH-GUARD: the rest of the card still parses, so the trigger
        // assertions above cannot pass because the card failed to parse at all.
        assert!(
            parsed.abilities.iter().any(|a| matches!(
                &*a.effect,
                Effect::PutCounter {
                    counter_type: crate::types::counter::CounterType::Plus1Plus1,
                    target: TargetFilter::SelfRef,
                    ..
                }
            )),
            "the Camouflage activated ability must still lower a PutCounter on the source: {:?}",
            parsed
                .abilities
                .iter()
                .map(|a| &a.effect)
                .collect::<Vec<_>>()
        );

        // DISCLOSED, PINNED GAP (backlog root cause 14) — NOT fixed by this change.
        // The Camouflage line's "He gains hexproof and becomes colorless until end
        // of turn" conjoins a keyword grant with a COLOUR change, and only the
        // keyword survives: `oracle_effect/subject.rs`'s
        // `try_parse_become_color_modification` has no "colorless" arm (it handles
        // "all colors" / "every color" / a chosen colour), so the colour conjunct is
        // dropped silently. That is a different root cause from this change's
        // counter doubling, at a different seam, and is left for its own fix — the
        // card is listed under root cause 14 in `docs/parser-misparse-backlog.md`
        // precisely so it is not lost.
        //
        // The measured shape is pinned rather than merely described: when a
        // colour-setting arm lands, the second assertion FAILS and whoever adds it
        // is told to drop the backlog entry in the same change. Without this the
        // card reads as fully modelled (`cargo coverage` reports it supported once
        // the trigger is fixed) while still being wrong about colour.
        fn collect_mods(
            def: &crate::types::ability::AbilityDefinition,
            out: &mut Vec<crate::types::ability::ContinuousModification>,
        ) {
            if let Effect::GenericEffect {
                static_abilities, ..
            } = &*def.effect
            {
                for st in static_abilities {
                    out.extend(st.modifications.iter().cloned());
                }
            }
            if let Some(sub) = &def.sub_ability {
                collect_mods(sub, out);
            }
            if let Some(other) = &def.else_ability {
                collect_mods(other, out);
            }
        }
        let mut grants = Vec::new();
        for ability in &parsed.abilities {
            collect_mods(ability, &mut grants);
        }
        assert!(
            grants.iter().any(|m| matches!(
                m,
                crate::types::ability::ContinuousModification::AddKeyword { keyword }
                    if *keyword == crate::types::keywords::Keyword::Hexproof
            )),
            "reach-guard: the hexproof half of the Camouflage grant must parse, or the              colour assertion below is vacuous: {grants:?}"
        );
        assert!(
            !grants.iter().any(|m| matches!(
                m,
                crate::types::ability::ContinuousModification::SetColor { .. }
            )),
            "KNOWN GAP (parser-misparse-backlog root cause 14): the `becomes colorless` conjunct \
             is still dropped. If this assertion now FAILS, a colour-setting arm landed — \
             remove Ultimate Spider-Man from root cause 14 in \
             docs/parser-misparse-backlog.md as part of that change: {grants:?}"
        );
    }

    /// CR 701.10a: "double target creature's power and toughness" → factor 2.
    /// Building-block test for the multiplier-word axis (Tifa's Limit Break —
    /// Meteor Strikes; the historical doubling forms must keep factor 2).
    #[test]
    fn multiply_pt_double_target_pt_factor_two() {
        let result = try_parse_multiply_pt_effect(
            "double target creature's power and toughness",
            &mut default_ctx(),
        );
        assert!(
            matches!(
                result,
                Some(Effect::DoublePT {
                    mode: DoublePTMode::PowerAndToughness,
                    factor: 2,
                    ..
                })
            ),
            "got {result:?}"
        );
    }

    /// CR 613.4c: "triple target creature's power and toughness" → factor 3
    /// (Tifa's Limit Break — Final Heaven). The multiplier word is the only
    /// varying axis, so the same arm emits a factor-3 `DoublePT`.
    #[test]
    fn multiply_pt_triple_target_pt_factor_three() {
        let result = try_parse_multiply_pt_effect(
            "triple target creature's power and toughness",
            &mut default_ctx(),
        );
        let Some(Effect::DoublePT { mode, factor, .. }) = result else {
            panic!("expected DoublePT, got {result:?}");
        };
        assert_eq!(mode, DoublePTMode::PowerAndToughness);
        assert_eq!(factor, 3, "triple must parse factor 3");
    }

    /// CR 613.4c: "triple target creature's power" — power-only triple keeps the
    /// `Power` mode (The Skullspore Nexus is the power-only double sibling).
    #[test]
    fn multiply_pt_triple_target_power_only() {
        let result =
            try_parse_multiply_pt_effect("triple target creature's power", &mut default_ctx());
        let Some(Effect::DoublePT { mode, factor, .. }) = result else {
            panic!("expected DoublePT, got {result:?}");
        };
        assert_eq!(mode, DoublePTMode::Power);
        assert_eq!(factor, 3);
    }

    #[test]
    fn remove_counter_untyped_all() {
        // Vampire Hexmage: "remove all counters from target permanent"
        let result = try_parse_remove_counter(
            "remove all counters from target permanent",
            &mut default_ctx(),
        );
        let Some(Effect::RemoveCounter {
            counter_type,
            count,
            target,
        }) = result
        else {
            panic!("expected RemoveCounter, got {result:?}");
        };
        assert_eq!(counter_type, None, "untyped should be None");
        assert_eq!(
            count,
            QuantityExpr::Fixed { value: -1 },
            "all = sentinel -1"
        );
        assert!(matches!(target, TargetFilter::Typed { .. }));
    }

    #[test]
    fn remove_counter_untyped_single() {
        // Thrull Parasite: "remove a counter from target nonland permanent"
        let result = try_parse_remove_counter(
            "remove a counter from target nonland permanent",
            &mut default_ctx(),
        );
        let Some(Effect::RemoveCounter {
            counter_type,
            count,
            ..
        }) = result
        else {
            panic!("expected RemoveCounter, got {result:?}");
        };
        assert_eq!(counter_type, None);
        assert_eq!(count, QuantityExpr::Fixed { value: 1 });
    }

    #[test]
    fn remove_counter_up_to_n() {
        // Heartless Act mode 2: "remove up to three counters from target creature"
        let result = try_parse_remove_counter(
            "remove up to three counters from target creature",
            &mut default_ctx(),
        );
        let Some(Effect::RemoveCounter {
            counter_type,
            count,
            ..
        }) = result
        else {
            panic!("expected RemoveCounter, got {result:?}");
        };
        assert_eq!(counter_type, None);
        assert_eq!(count, QuantityExpr::Fixed { value: 3 });
    }

    /// CR 608.2k anaphor: "remove those counters" with no "from {target}"
    /// clause refers to counters on the ability source. Building-block coverage
    /// for the full anaphor surface — covers the Primal Amulet class plus
    /// every related anaphor form so the next card with the same shape
    /// (hatchling line, Brass's Tunnel-Grinder, etc.) parses cleanly.
    #[test]
    fn remove_counter_anaphor_no_from_target() {
        let cases = [
            "remove those counters",
            "remove its counters",
            "remove this creature's counters",
            "remove this artifact's counters",
            "remove this enchantment's counters",
            "remove this permanent's counters",
            "remove them",
            "remove all of them",
        ];
        for input in cases {
            let result = try_parse_remove_counter(input, &mut default_ctx());
            let Some(Effect::RemoveCounter {
                counter_type,
                count,
                target,
            }) = result
            else {
                panic!("{input}: expected RemoveCounter, got {result:?}");
            };
            assert_eq!(counter_type, None, "{input}: counter_type None");
            assert_eq!(
                count,
                QuantityExpr::Fixed { value: -1 },
                "{input}: sentinel -1 = all"
            );
            assert!(
                matches!(target, TargetFilter::SelfRef),
                "{input}: target SelfRef, got {target:?}"
            );
        }
    }

    /// CR 115.1d + CR 122.1: Garnet, Princess of Alexandria — "remove a lore
    /// counter from each of any number of Sagas you control" must parse to a
    /// Saga + you-control filter, not `TargetFilter::Any` (which incorrectly
    /// prompts for player targets).
    #[test]
    fn remove_counter_each_of_any_number_sagas_you_control() {
        use crate::types::ability::{ControllerRef, TypeFilter};
        let result = try_parse_remove_counter(
            "remove a lore counter from each of any number of sagas you control",
            &mut default_ctx(),
        );
        let Some(Effect::RemoveCounter {
            counter_type,
            count,
            target,
        }) = result
        else {
            panic!("expected RemoveCounter, got {result:?}");
        };
        assert_eq!(counter_type, Some(CounterType::Lore));
        assert_eq!(count, QuantityExpr::Fixed { value: 1 });
        let TargetFilter::Typed(tf) = target else {
            panic!("expected Typed Saga filter, got {target:?}");
        };
        assert!(
            tf.type_filters
                .iter()
                .any(|f| matches!(f, TypeFilter::Subtype(s) if s == "Saga")),
            "expected Saga subtype, got {:?}",
            tf.type_filters
        );
        assert_eq!(tf.controller, Some(ControllerRef::You));
    }

    /// CR 115.1d: Post-parse fixup must attach `MultiTargetSpec::unlimited(0)` for
    /// the Garnet distribution prefix.
    #[test]
    fn remove_counter_each_of_any_number_carries_multi_target() {
        let clause = super::super::parse_effect_clause(
            "remove a lore counter from each of any number of sagas you control",
            &mut default_ctx(),
        );
        assert_eq!(clause.multi_target, Some(MultiTargetSpec::unlimited(0)));
        let Effect::RemoveCounter { target, .. } = clause.effect else {
            panic!("expected RemoveCounter, got {:?}", clause.effect);
        };
        assert!(matches!(target, TargetFilter::Typed(_)));
    }

    /// CR 601.2c: Fixed-count wording with `target` still announces its
    /// targets. It must not be reclassified as the untargeted tracked-set
    /// selection used by "each of two creatures".
    #[test]
    fn remove_counter_each_of_two_target_creatures_keeps_multi_targeting() {
        let text = "remove a +1/+1 counter from each of two target creatures";
        assert_eq!(remove_counter_exact_selection_count(text), None);
        let clause = super::super::parse_effect_clause(text, &mut default_ctx());
        assert_eq!(
            clause.multi_target,
            Some(MultiTargetSpec::exact(QuantityExpr::Fixed { value: 2 }))
        );
        assert!(matches!(clause.effect, Effect::RemoveCounter { .. }));
    }

    #[test]
    fn remove_counter_typed_still_works() {
        // Existing pattern: "remove a +1/+1 counter from ~"
        let result = try_parse_remove_counter("remove a +1/+1 counter from ~", &mut default_ctx());
        let Some(Effect::RemoveCounter {
            counter_type,
            count,
            ..
        }) = result
        else {
            panic!("expected RemoveCounter, got {result:?}");
        };
        assert_eq!(counter_type, Some(CounterType::Plus1Plus1));
        assert_eq!(count, QuantityExpr::Fixed { value: 1 });
    }

    #[test]
    fn move_counters_from_self_onto_target() {
        // Simic Fluxmage: "move a +1/+1 counter from this creature onto target creature"
        let result = try_parse_move_counters_from(
            "move a +1/+1 counter from this creature onto target creature",
            &mut default_ctx(),
        );
        let Some(Effect::MoveCounters {
            source,
            counter_type,
            count,
            mode,
            selection,
            target,
        }) = result
        else {
            panic!("expected MoveCounters, got {result:?}");
        };
        assert!(matches!(source, TargetFilter::SelfRef));
        assert_eq!(counter_type, Some(CounterType::Plus1Plus1));
        assert_eq!(count, Some(QuantityExpr::Fixed { value: 1 }));
        assert_eq!(mode, CounterTransferMode::Move);
        assert_eq!(selection, CounterMoveSelection::StackTarget);
        assert!(matches!(target, TargetFilter::Typed { .. }));
    }

    #[test]
    fn move_counters_all_types() {
        // Fate Transfer: "move all counters from target creature onto another target creature"
        let result = try_parse_move_counters_from(
            "move all counters from target creature onto another target creature",
            &mut default_ctx(),
        );
        let Some(Effect::MoveCounters {
            counter_type,
            count,
            mode,
            selection,
            ..
        }) = result
        else {
            panic!("expected MoveCounters, got {result:?}");
        };
        assert_eq!(counter_type, None, "untyped = None");
        assert_eq!(count, None, "all counters = None");
        assert_eq!(mode, CounterTransferMode::Move);
        assert_eq!(selection, CounterMoveSelection::StackTarget);
    }

    #[test]
    fn move_counters_typed_from_target_to_self() {
        // Cytoplast Root-Kin: "move a +1/+1 counter from target creature you control onto this creature"
        let result = try_parse_move_counters_from(
            "move a +1/+1 counter from target creature you control onto this creature",
            &mut default_ctx(),
        );
        let Some(Effect::MoveCounters {
            source,
            counter_type,
            count,
            mode,
            selection,
            target,
        }) = result
        else {
            panic!("expected MoveCounters, got {result:?}");
        };
        assert!(matches!(source, TargetFilter::Typed { .. }));
        assert_eq!(counter_type, Some(CounterType::Plus1Plus1));
        assert_eq!(count, Some(QuantityExpr::Fixed { value: 1 }));
        assert_eq!(mode, CounterTransferMode::Move);
        assert_eq!(selection, CounterMoveSelection::StackTarget);
        assert!(matches!(target, TargetFilter::SelfRef));
    }

    #[test]
    fn move_any_number_to_nontarget_destinations_is_resolution_distribution() {
        let result = try_parse_move_counters_from(
            "move any number of +1/+1 counters from this creature onto other creatures you control",
            &mut default_ctx(),
        );
        let Some(Effect::MoveCounters {
            source,
            counter_type,
            count,
            mode,
            selection,
            target,
        }) = result
        else {
            panic!("expected MoveCounters, got {result:?}");
        };
        assert!(matches!(source, TargetFilter::SelfRef));
        assert_eq!(counter_type, Some(CounterType::Plus1Plus1));
        assert_eq!(count, None);
        assert_eq!(mode, CounterTransferMode::Move);
        assert_eq!(
            selection,
            CounterMoveSelection::ResolutionDistributionAnyNumber
        );
        assert!(matches!(target, TargetFilter::Typed { .. }));
    }

    #[test]
    fn move_any_number_to_target_destination_keeps_stack_targets() {
        let result = try_parse_move_counters_from(
            "move any number of counters from target creature onto another target creature",
            &mut default_ctx(),
        );
        let Some(Effect::MoveCounters {
            source,
            counter_type,
            count,
            mode,
            selection,
            target,
        }) = result
        else {
            panic!("expected MoveCounters, got {result:?}");
        };
        assert!(matches!(source, TargetFilter::Typed { .. }));
        assert_eq!(counter_type, None);
        assert_eq!(count, None);
        assert_eq!(mode, CounterTransferMode::Move);
        assert_eq!(selection, CounterMoveSelection::StackTargetAnyNumber);
        assert!(matches!(target, TargetFilter::Typed { .. }));
    }

    /// CR 122.1 + CR 208.3: "put a number of +1/+1 counters equal to its power
    /// on each creature you control named ~" (Gruff Triplets). The dynamic
    /// count binds to the source's power via `QuantityRef::Power { scope: crate::types::ability::ObjectScope::Source }`; the
    /// counter type is "+1/+1", not the word "number"; the target is a mass
    /// filter for creatures with the same name (resolved via `normalize_self_refs`
    /// restoring the card name after the `named ~` re-expansion).
    #[test]
    fn put_counter_a_number_of_equal_to_self_power() {
        use crate::types::ability::{FilterProp, QuantityRef, TypeFilter};
        let (effect, _, _) = try_parse_put_counter(
            "put a number of +1/+1 counters equal to its power on each creature you control named Gruff Triplets",
            "put a number of +1/+1 counters equal to its power on each creature you control named Gruff Triplets",
            &mut default_ctx(),
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
        assert_eq!(counter_type, CounterType::Plus1Plus1);
        assert!(
            matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::Source
                    }
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

    /// CR 122.1 + CR 608.2c: a "put a number of {type} counters on {target}"
    /// body with NO in-line "equal to {qty}" clause carries its count
    /// externally (a vote-tally clause binds it later). The dynamic-deferred
    /// branch must still build the `PutCounter` effect with the survivable
    /// `Variable { "count" }` placeholder instead of returning `None` (which
    /// would lose the effect). This is the load-bearing parser change for the
    /// Emissary Green counter clause. An unbound `Variable { "count" }`
    /// resolves to 0, so the placeholder is inert until the external bind.
    #[test]
    fn put_counter_a_number_of_no_equal_to_emits_count_placeholder() {
        use crate::types::ability::{QuantityExpr, QuantityRef};
        let text = "put a number of +1/+1 counters on each creature you control";
        let (effect, _, _) = try_parse_put_counter(text, text, &mut default_ctx()).expect("parse");
        let Effect::PutCounter {
            counter_type,
            count,
            ..
        } = effect
        else {
            panic!("expected PutCounter, got {effect:?}");
        };
        assert_eq!(counter_type, CounterType::Plus1Plus1);
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "count".to_string(),
                },
            },
            "deferred 'a number of' with no 'equal to' must emit the survivable count placeholder"
        );
    }

    /// CR 122.1 + CR 208.3: target-then-count ordering with NO "a number of"
    /// and no leading count — "put +1/+1 counters on <target> equal to its
    /// power" (The Roaring Toeclaws, Experiment Twelve). The counter type
    /// follows "put" directly; the count is supplied by the post-target
    /// "equal to {qty}" clause and binds to the target's power.
    #[test]
    fn put_counter_on_target_equal_to_power_no_leading_count() {
        use crate::types::ability::{ObjectScope, QuantityRef};
        let text = "put +1/+1 counters on target creature equal to its power";
        let (effect, _, _) = try_parse_put_counter(text, text, &mut default_ctx()).expect("parse");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = effect
        else {
            panic!("expected PutCounter, got {effect:?}");
        };
        assert_eq!(counter_type, CounterType::Plus1Plus1);
        assert!(
            matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Target
                    }
                }
            ),
            "post-target count should bind to the target creature's power, got {count:?}"
        );
        assert!(matches!(target, TargetFilter::Typed { .. }));
    }

    /// CR 122.1 + CR 608.2k: The Roaring Toeclaws template — "put +1/+1
    /// counters on it equal to its power" inside a typed trigger subject. Both
    /// the placement target and the post-target "its power" count bind to the
    /// triggering object, not to the ability source.
    #[test]
    fn put_counter_on_it_equal_to_power_keeps_dynamic_remainder() {
        use crate::types::ability::{ObjectScope, QuantityRef};
        let mut ctx = default_ctx();
        ctx.subject = Some(TargetFilter::Typed(TypedFilter::creature()));
        let text = "put +1/+1 counters on it equal to its power";
        let (effect, remainder, _) =
            try_parse_put_counter(text, text, &mut ctx).expect("parse real pronoun form");
        assert_eq!(remainder, "");
        let Effect::PutCounter { count, target, .. } = effect else {
            panic!("expected PutCounter, got {effect:?}");
        };
        assert_eq!(target, TargetFilter::TriggeringSource);
        assert!(
            matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::EventSource
                    }
                }
            ),
            "post-target pronoun count should bind to the triggering object, got {count:?}"
        );
    }

    /// CR 122.1 + CR 608.2k: Experiment Twelve template — "put +1/+1 counters
    /// on that creature equal to its power". The explicit anaphor keeps the
    /// dynamic suffix as parseable remainder and scopes the count to the
    /// triggering object.
    #[test]
    fn put_counter_on_that_creature_equal_to_power_binds_triggering_object() {
        use crate::types::ability::{ObjectScope, QuantityRef};
        let mut ctx = default_ctx();
        ctx.subject = Some(TargetFilter::Typed(TypedFilter::creature()));
        let text = "put +1/+1 counters on that creature equal to its power";
        let (effect, remainder, _) =
            try_parse_put_counter(text, text, &mut ctx).expect("parse real that-creature form");
        assert_eq!(remainder, "");
        let Effect::PutCounter { count, target, .. } = effect else {
            panic!("expected PutCounter, got {effect:?}");
        };
        assert_eq!(target, TargetFilter::TriggeringSource);
        assert!(
            matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::EventSource
                    }
                }
            ),
            "that-creature count should bind to the triggering object, got {count:?}"
        );
    }

    /// CR 202.3 + CR 608.2k: Dusty Parlor — "Whenever you cast an enchantment spell,
    /// put a number of +1/+1 counters equal to that spell's mana value on
    /// up to one target creature." The dynamic count binds to the triggering
    /// SpellCast event's source object (the spell itself) via
    /// `QuantityRef::ObjectManaValue { scope: EventSource }`, which resolves
    /// to the spell's printed CMC at trigger resolution time.
    #[test]
    fn put_counter_a_number_of_equal_to_spells_mana_value() {
        use crate::types::ability::{ObjectScope, QuantityRef};
        let (effect, _, multi) = try_parse_put_counter(
            "put a number of +1/+1 counters equal to that spell's mana value on up to one target creature",
            "put a number of +1/+1 counters equal to that spell's mana value on up to one target creature",
            &mut default_ctx(),
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
        assert_eq!(counter_type, CounterType::Plus1Plus1);
        assert!(
            matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::EventSource
                    }
                }
            ),
            "count should be ObjectManaValue {{ scope: EventSource }}, got {count:?}"
        );
        assert!(matches!(target, TargetFilter::Typed { .. }));
        assert_eq!(
            multi,
            Some(MultiTargetSpec::fixed(0, 1)),
            "up to one target creature → MultiTargetSpec {{ 0, 1 }}"
        );
    }

    #[test]
    fn put_counter_each_of_up_to_x_target_creatures_keeps_dynamic_max() {
        let (_effect, _, multi) = try_parse_put_counter(
            "put a +1/+1 counter on each of up to x target creatures",
            "put a +1/+1 counter on each of up to x target creatures",
            &mut default_ctx(),
        )
        .expect("parse");

        assert_eq!(
            multi,
            Some(MultiTargetSpec::up_to(QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::Variable {
                    name: "X".to_string()
                }
            }))
        );
    }

    /// CR 601.2c: "each of X target creatures" (no "up to") is an EXACT-count
    /// multi-target — exactly X chosen targets, X bound from the activation
    /// cost. Must be `MultiTargetSpec::exact(Variable("X"))`, NOT `up_to` and
    /// NOT an unconstrained all-creatures filter (the misparse this fixes).
    #[test]
    fn put_counter_each_of_x_target_creatures_exact_count() {
        let (_effect, _, multi) = try_parse_put_counter(
            "put a -1/-1 counter on each of x target creatures",
            "put a -1/-1 counter on each of x target creatures",
            &mut default_ctx(),
        )
        .expect("parse");

        assert_eq!(
            multi,
            Some(MultiTargetSpec::exact(QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::Variable {
                    name: "X".to_string()
                }
            }))
        );
    }

    /// CR 601.2c: fixed-count form "each of two target creatures" ⇒
    /// `MultiTargetSpec::exact(Fixed(2))`.
    #[test]
    fn put_counter_each_of_two_target_creatures_exact_count() {
        let (_effect, _, multi) = try_parse_put_counter(
            "put a -1/-1 counter on each of two target creatures",
            "put a -1/-1 counter on each of two target creatures",
            &mut default_ctx(),
        )
        .expect("parse");

        assert_eq!(
            multi,
            Some(MultiTargetSpec::exact(QuantityExpr::Fixed { value: 2 }))
        );
    }

    #[test]
    fn put_counter_each_of_up_to_x_target_creatures_applies_where_x_max() {
        let def = super::super::parse_effect_chain(
            "Put a +1/+1 counter on each of up to X target creatures, where X is the number of creatures you control.",
            crate::types::ability::AbilityKind::Spell,
        );
        let mut expected_filter = crate::types::ability::TypedFilter::creature();
        expected_filter.controller = Some(crate::types::ability::ControllerRef::You);

        assert_eq!(
            def.multi_target,
            Some(MultiTargetSpec::up_to(QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(expected_filter)
                }
            }))
        );
    }

    /// CR 122.1 + CR 115.1d: Omo, Queen of Vesuva's trigger effect —
    /// "put an everything counter on each of up to one target land and up to
    /// one target creature." Must produce TWO PutCounter siblings: the primary
    /// (land target, up to one) and a chained sub_ability (creature target, up
    /// to one), each optional. Neither may be `Unimplemented`.
    #[test]
    fn omo_dual_target_put_counter_emits_two_siblings() {
        let def = super::super::parse_effect_chain(
            "Put an everything counter on each of up to one target land and up to one target creature.",
            crate::types::ability::AbilityKind::Spell,
        );

        // "up to one" encodes optional targeting as a MultiTargetSpec with
        // min == 0 (CR 601.2c). The parsed clauses carry it via `multi_target`.
        let is_optional = |spec: &Option<MultiTargetSpec>| {
            spec.as_ref()
                .is_some_and(|s| matches!(s.min, QuantityExpr::Fixed { value: 0 }))
        };

        // Primary clause: land target, up to one, optional.
        let Effect::PutCounter {
            ref counter_type,
            target: ref land_target,
            ..
        } = *def.effect
        else {
            panic!("expected primary PutCounter, got {:?}", def.effect);
        };
        assert_eq!(
            *counter_type,
            CounterType::Generic("everything".to_string())
        );
        assert!(matches!(land_target, TargetFilter::Typed(_)));
        assert!(
            is_optional(&def.multi_target),
            "primary land target must be optional (up to one)"
        );

        // Second sibling: creature target, up to one, optional.
        let sub = def
            .sub_ability
            .as_ref()
            .expect("expected a second PutCounter sub_ability");
        let Effect::PutCounter {
            counter_type: ref sub_counter,
            target: ref creature_target,
            ..
        } = *sub.effect
        else {
            panic!(
                "expected second PutCounter sub_ability, got {:?}",
                sub.effect
            );
        };
        assert_eq!(
            *sub_counter,
            CounterType::Generic("everything".to_string()),
            "second sibling reuses the same counter type"
        );
        assert!(matches!(creature_target, TargetFilter::Typed(_)));
        assert!(
            is_optional(&sub.multi_target),
            "second creature target must be optional (up to one)"
        );

        // Neither clause may be Unimplemented.
        assert!(!matches!(*def.effect, Effect::Unimplemented { .. }));
        assert!(!matches!(*sub.effect, Effect::Unimplemented { .. }));
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
            &mut default_ctx(),
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
        assert_eq!(counter_type, CounterType::Plus1Plus1);
        // The parser resolves "cards in your hand" to the more specific
        // ZoneCardCount; either ZoneCardCount or HandSize is semantically valid.
        assert!(
            matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Hand,
                        ..
                    } | QuantityRef::HandSize {
                        player: crate::types::ability::PlayerScope::Controller
                    }
                }
            ),
            "count should be hand-card-count reference, got {count:?}"
        );
        assert!(matches!(target, TargetFilter::SelfRef));
    }

    #[test]
    fn put_counter_for_each_suffix_preserves_self_target() {
        use crate::types::ability::QuantityRef;
        use crate::types::zones::Zone;

        let (effect, rem, _) = try_parse_put_counter(
            "put a corpse counter on this creature for each creature that died this turn",
            "put a corpse counter on this creature for each creature that died this turn",
            &mut default_ctx(),
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

        assert_eq!(counter_type, CounterType::Generic("corpse".to_string()));
        assert!(matches!(target, TargetFilter::SelfRef));
        assert!(
            rem.is_empty(),
            "for-each suffix should be consumed, got {rem:?}"
        );
        assert!(
            matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneChangeCountThisTurn {
                        from: Some(Zone::Battlefield),
                        to: Some(Zone::Graveyard),
                        filter: TargetFilter::Typed(_),
                    }
                }
            ),
            "count should be died-this-turn quantity, got {count:?}"
        );
    }

    #[test]
    fn put_counter_for_each_suffix_multiplies_fixed_count_on_target() {
        use crate::types::ability::{QuantityRef, ZoneRef};

        let (effect, rem, _) = try_parse_put_counter(
            "put two charge counters on target artifact for each card in your hand",
            "put two charge counters on target artifact for each card in your hand",
            &mut default_ctx(),
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

        assert_eq!(counter_type, CounterType::Generic("charge".to_string()));
        assert!(matches!(target, TargetFilter::Typed(_)));
        assert!(
            rem.is_empty(),
            "for-each suffix should be consumed, got {rem:?}"
        );
        assert!(
            matches!(
                count,
                QuantityExpr::Multiply {
                    factor: 2,
                    ref inner,
                } if matches!(
                    **inner,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ZoneCardCount {
                            zone: ZoneRef::Hand,
                            ..
                        }
                    } | QuantityExpr::Ref {
                        qty: QuantityRef::HandSize { .. }
                    }
                )
            ),
            "two counters per card should multiply the hand-card quantity, got {count:?}"
        );
    }

    // Finding-4 regression: the counter for-each suffix must stay anchored — the
    // remainder must *begin* with `for each ` (whitespace-only head). A remainder
    // where `for each` is preceded by unrelated text must NOT be treated as a
    // multiplier (which would silently drop the leading clause).
    #[test]
    fn counter_for_each_suffix_requires_anchored_marker() {
        // Anchored: remainder begins with `for each ` → matches.
        assert!(
            parse_counter_for_each_suffix("for each creature you control").is_some(),
            "an anchored `for each` remainder must parse as a multiplier",
        );
        // Mid-clause: `for each` preceded by non-whitespace text → rejected, so the
        // leading clause is not dropped.
        assert!(
            parse_counter_for_each_suffix("until end of turn for each creature you control")
                .is_none(),
            "a `for each` preceded by unrelated text must not be treated as a multiplier",
        );
    }

    /// #588 (Summon: Good King Mog XII, chapter IV — "Put two +1/+1 counters
    /// on each other Moogle you control"): a creature subtype absent from the
    /// curated `SUBTYPES` list silently dropped BOTH the `Subtype` type-filter
    /// AND the "you control" controller (the failed subtype parse cascaded),
    /// collapsing the filter to "every other permanent" so the counters landed
    /// on opponents and lands. Regression guard: the subtype must be recognized
    /// so the whole filter stays scoped. Drives the real `parse_subtype` path.
    #[test]
    fn put_counter_each_other_moogle_you_control_scopes_filter() {
        use crate::types::ability::{ControllerRef, FilterProp, TypeFilter};
        let text = "put two +1/+1 counters on each other moogle you control";
        let (effect, _rem, _) =
            try_parse_put_counter(text, text, &mut default_ctx()).expect("parse");
        let Effect::PutCounter { target, .. } = effect else {
            panic!("expected PutCounter, got {effect:?}");
        };
        let TargetFilter::Typed(tf) = target else {
            panic!("expected Typed filter, got {target:?}");
        };
        assert!(
            tf.type_filters
                .iter()
                .any(|f| matches!(f, TypeFilter::Subtype(s) if s == "Moogle")),
            "Moogle subtype must be captured, got {:?}",
            tf.type_filters
        );
        assert_eq!(
            tf.controller,
            Some(ControllerRef::You),
            "\"you control\" must scope the controller (it dropped when the subtype was unknown)"
        );
        assert!(
            tf.properties
                .iter()
                .any(|p| matches!(p, FilterProp::Another)),
            "\"other\" must map to FilterProp::Another, got {:?}",
            tf.properties
        );
    }

    /// CR 122.8 + CR 400.7: "~'s counters" in a self-sacrifice activated
    /// ability (Zack Fair) — source is the ability's ~, not the parent target.
    #[test]
    fn move_counters_self_possessive_counters() {
        let lower = "put ~'s counters on that creature";
        let result = try_parse_move_counters(lower, lower, &mut default_ctx());
        let Some((
            Effect::MoveCounters {
                source,
                counter_type,
                count,
                mode,
                selection: _,
                target,
            },
            rem,
        )) = result
        else {
            panic!("expected MoveCounters, got {result:?}");
        };
        assert!(matches!(source, TargetFilter::SelfRef));
        assert_eq!(counter_type, None);
        assert_eq!(count, None);
        assert_eq!(mode, CounterTransferMode::Put);
        assert!(matches!(target, TargetFilter::ParentTarget));
        assert!(rem.is_empty());
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
        let result = try_parse_move_counters(lower, lower, &mut default_ctx());
        let Some((
            Effect::MoveCounters {
                source,
                counter_type,
                count,
                mode,
                selection: _,
                target,
            },
            _,
        )) = result
        else {
            panic!("expected MoveCounters, got {result:?}");
        };
        assert!(matches!(source, TargetFilter::SelfRef));
        assert_eq!(counter_type, None, "all counters move (no type filter)");
        assert_eq!(count, None, "CR 122.8 copies every matching counter");
        assert_eq!(mode, CounterTransferMode::Put);
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

    /// CR 122.8 + CR 400.7 (The Ozolith): "Whenever a creature you control
    /// leaves the battlefield, ... put those counters on ~." Here the trigger
    /// subject is a non-self creature filter, so "those counters" refers to the
    /// triggering creature — the counter SOURCE must be `TriggeringSource`, not
    /// `SelfRef` (the ability source, which never has the counters and would
    /// make the move a no-op).
    #[test]
    fn move_counters_those_counters_from_other_object_trigger() {
        use crate::types::ability::TypedFilter;
        let mut ctx = default_ctx();
        ctx.subject = Some(TargetFilter::Typed(TypedFilter::creature()));
        let lower = "put those counters on ~";
        let result = try_parse_move_counters(lower, lower, &mut ctx);
        let Some((Effect::MoveCounters { source, target, .. }, _)) = result else {
            panic!("expected MoveCounters, got {result:?}");
        };
        assert_eq!(
            source,
            TargetFilter::TriggeringSource,
            "source must be the triggering (leaving) creature, not the ability source"
        );
        assert_eq!(
            target,
            TargetFilter::SelfRef,
            "counters are put onto ~ (the ability source)"
        );
    }

    /// CR 122.1b: Avenging Huntbonder (NCC) attack trigger places a `double
    /// strike` keyword counter. Pre-fix, `try_parse_put_counter` sliced the
    /// counter type at the first whitespace and produced
    /// `CounterType::Generic("double")`, which then failed to consume the
    /// trailing `strike counter` text — dropping the whole effect.
    #[test]
    fn put_counter_double_strike_keyword_target() {
        use crate::types::ability::FilterProp;
        use crate::types::keywords::KeywordKind;
        let input = "put a double strike counter on another target attacking creature";
        let (effect, _rem, _multi) =
            try_parse_put_counter(input, input, &mut default_ctx()).expect("parse");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = effect
        else {
            panic!("expected PutCounter, got {effect:?}");
        };
        assert_eq!(
            counter_type,
            CounterType::Keyword(KeywordKind::DoubleStrike)
        );
        assert_eq!(count, QuantityExpr::Fixed { value: 1 });
        let TargetFilter::Typed(tf) = target else {
            panic!("expected Typed filter, got {target:?}");
        };
        assert!(tf
            .type_filters
            .iter()
            .any(|t| matches!(t, crate::types::ability::TypeFilter::Creature)));
        assert!(
            tf.properties
                .iter()
                .any(|p| matches!(p, FilterProp::Attacking { defender: None })),
            "target should be Attacking, got {:?}",
            tf.properties
        );
        assert!(
            tf.properties
                .iter()
                .any(|p| matches!(p, FilterProp::Another)),
            "target should carry FilterProp::Another (CR 109.5), got {:?}",
            tf.properties
        );
    }

    /// CR 122.1b: companion case for the other multi-word keyword counter
    /// name. Sibling cards in the corpus place `first strike` counters via the
    /// same single-counter path (e.g. Heightened Reflexes-class effects).
    #[test]
    fn put_counter_first_strike_keyword_target() {
        use crate::types::keywords::KeywordKind;
        let input = "put a first strike counter on target creature";
        let (effect, _rem, _multi) =
            try_parse_put_counter(input, input, &mut default_ctx()).expect("parse");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = effect
        else {
            panic!("expected PutCounter, got {effect:?}");
        };
        assert_eq!(counter_type, CounterType::Keyword(KeywordKind::FirstStrike));
        assert_eq!(count, QuantityExpr::Fixed { value: 1 });
        assert!(matches!(target, TargetFilter::Typed { .. }));
    }

    /// CR 122.1 + CR 706.2: `parse_counter_equal_to` still handles the typed
    /// quantity grammar (here a self-power object ref) with the correct
    /// remainder — the event-context fallback must not regress the primary path.
    #[test]
    fn counter_equal_to_typed_quantity_unchanged() {
        let (rest, qty) = parse_counter_equal_to("equal to its power then draw").unwrap();
        assert!(matches!(qty, QuantityExpr::Ref { .. }));
        // "its power" is consumed; the trailing clause survives in the remainder.
        assert_eq!(rest, " then draw", "trailing clause must survive: {rest:?}");
    }

    /// CR 706.2: the die-roll / coin-flip back-reference "the result" routes
    /// through the `parse_event_context_quantity` fallback to `EventContextAmount`
    /// — the typed `parse_equal_to` grammar does not reach it, and it must NOT be
    /// added to the shared `parse_quantity_ref` leaf (that would break the
    /// `parse_cda_quantity` "returns None for the bare die-result phrase"
    /// invariant the where-X binding depends on).
    #[test]
    fn counter_equal_to_the_result_binds_event_context_amount() {
        let (rest, qty) = parse_counter_equal_to("equal to the result").unwrap();
        assert_eq!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            }
        );
        assert_eq!(rest, "");
    }

    /// The event-context fallback preserves a trailing clause as the remainder,
    /// matching nom's consume-and-remainder contract (so downstream
    /// for-each / conditional clauses are not silently swallowed).
    #[test]
    fn counter_equal_to_the_result_preserves_trailing_clause() {
        let (rest, qty) = parse_counter_equal_to("equal to the result, then draw a card").unwrap();
        assert_eq!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            }
        );
        assert_eq!(rest, ", then draw a card");
    }

    /// End-to-end counter-entry (deferred post-target placement): "put +1/+1
    /// counters on it equal to the result" must build a `PutCounter` bound to
    /// `EventContextAmount`, never `Unimplemented`. This is the class the PR
    /// targets (die-roll cards that pump via counters).
    #[test]
    fn put_counter_on_it_equal_to_the_result_binds_event_context_amount() {
        let input = "put +1/+1 counters on it equal to the result";
        let (effect, _rem, _multi) =
            try_parse_put_counter(input, input, &mut default_ctx()).expect("must parse");
        let Effect::PutCounter { count, .. } = effect else {
            panic!("expected PutCounter, got {effect:?}");
        };
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            }
        );
    }

    /// Sibling shape: the "a number of {type} counters … equal to the result"
    /// phrasing must bind the same ref via the eager / pre-target path.
    #[test]
    fn put_a_number_of_counters_equal_to_the_result_binds_event_context_amount() {
        let input = "put a number of +1/+1 counters on it equal to the result";
        let (effect, _rem, _multi) =
            try_parse_put_counter(input, input, &mut default_ctx()).expect("must parse");
        let Effect::PutCounter { count, .. } = effect else {
            panic!("expected PutCounter, got {effect:?}");
        };
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            }
        );
    }

    // ---- Gap-A ("up to N" counter count) + §B2 (token anaphor bind) ----

    const ESPER_CHAPTER: &str = "Create a token that's a copy of target nonlegendary enchantment you control. It gains haste. If it's a Saga, put up to three lore counters on it. Sacrifice it at the beginning of your next end step.";

    fn collect_defs<'a>(
        def: &'a crate::types::ability::AbilityDefinition,
        out: &mut Vec<&'a crate::types::ability::AbilityDefinition>,
    ) {
        out.push(def);
        if let Some(sub) = def.sub_ability.as_deref() {
            collect_defs(sub, out);
        }
        if let Some(els) = def.else_ability.as_deref() {
            collect_defs(els, out);
        }
        for m in &def.mode_abilities {
            collect_defs(m, out);
        }
    }

    fn find_put_counter(
        def: &crate::types::ability::AbilityDefinition,
    ) -> Option<(Effect, Option<crate::types::ability::AbilityCondition>)> {
        let mut all = Vec::new();
        collect_defs(def, &mut all);
        all.into_iter()
            .find(|d| matches!(*d.effect, Effect::PutCounter { .. }))
            .map(|d| ((*d.effect).clone(), d.condition.clone()))
    }

    fn chain_has_unimplemented(def: &crate::types::ability::AbilityDefinition) -> bool {
        let mut all = Vec::new();
        collect_defs(def, &mut all);
        all.iter()
            .any(|d| matches!(*d.effect, Effect::Unimplemented { .. }))
    }

    /// Gap-A CR 608.2d: "put up to three lore counters on <self>" wraps the parsed
    /// count in `UpTo{Fixed(3)}` (not a bare `Fixed(3)`); explicit self-reference
    /// keeps `SelfRef`. Reverting the up-to strip returns `None` (Unimplemented).
    #[test]
    fn put_up_to_three_lore_counters_wraps_up_to_fixed() {
        let text = "put up to three lore counters on this creature";
        let (effect, _, _) = try_parse_put_counter(text, text, &mut default_ctx()).expect("parse");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = effect
        else {
            panic!("expected PutCounter");
        };
        assert_eq!(counter_type, CounterType::Lore);
        assert_eq!(
            count,
            QuantityExpr::UpTo {
                max: Box::new(QuantityExpr::Fixed { value: 3 })
            },
            "up-to must wrap Fixed(3), not drop the marker"
        );
        assert_eq!(
            target,
            TargetFilter::SelfRef,
            "explicit self-ref stays SelfRef"
        );
    }

    /// Gap-A building-block: "up to X" wraps a `Variable("X")` count (the
    /// Clockwork class), proving the wrap is count-kind-agnostic, not hardcoded.
    #[test]
    fn put_up_to_x_counters_wraps_variable() {
        let text = "put up to X +1/+1 counters on this creature";
        let (effect, _, _) = try_parse_put_counter(text, text, &mut default_ctx()).expect("parse");
        let Effect::PutCounter { count, .. } = effect else {
            panic!("expected PutCounter");
        };
        assert_eq!(
            count,
            QuantityExpr::UpTo {
                max: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string()
                    }
                })
            }
        );
    }

    /// Gap-A NEG (count-side vs target-side "up to"): the leading count strip must
    /// NOT steal the TARGET-side "on up to N target(s)". Count stays `Fixed(1)`; a
    /// `MultiTargetSpec` carries the target-side "up to".
    #[test]
    fn count_side_up_to_strip_does_not_steal_target_side() {
        let text = "put a +1/+1 counter on up to three target creatures";
        let (effect, _, multi) =
            try_parse_put_counter(text, text, &mut default_ctx()).expect("parse");
        let Effect::PutCounter { count, .. } = effect else {
            panic!("expected PutCounter");
        };
        assert_eq!(
            count,
            QuantityExpr::Fixed { value: 1 },
            "count-side up-to must not be stolen by the target-side up-to"
        );
        assert!(
            multi.is_some(),
            "target-side up-to preserved as MultiTargetSpec"
        );
    }

    /// §B2 unit: the bare "it" counter branch binds by `token_created_in_chain`.
    /// Flag false (no token creator — the 1,215-card self-trigger class) → SelfRef;
    /// flag true (a token creator is the chain's most-recent referent) →
    /// LastCreated. Directly exercises the `resolve_counter_placement_target`
    /// it-pronoun branch both ways.
    #[test]
    fn put_on_it_binds_by_token_created_in_chain_flag() {
        let text = "put a +1/+1 counter on it";

        let (no_token, _, _) =
            try_parse_put_counter(text, text, &mut default_ctx()).expect("parse");
        let Effect::PutCounter { target, .. } = no_token else {
            panic!("expected PutCounter");
        };
        assert_eq!(
            target,
            TargetFilter::SelfRef,
            "no token in chain → bare it stays SelfRef"
        );

        let mut ctx = default_ctx();
        ctx.token_created_in_chain = true;
        let (with_token, _, _) = try_parse_put_counter(text, text, &mut ctx).expect("parse");
        let Effect::PutCounter { target, .. } = with_token else {
            panic!("expected PutCounter");
        };
        assert_eq!(
            target,
            TargetFilter::LastCreated,
            "token in chain → bare it binds the created token"
        );
    }

    #[test]
    fn put_counter_other_than_that_creature_excludes_context_object() {
        let mut ctx = default_ctx();
        ctx.object_pronoun_ref = Some(TargetFilter::EventTarget);
        let text = "put a +1/+1 counter on target creature you control other than that creature";
        let (effect, remainder, _) =
            try_parse_put_counter(text, text, &mut ctx).expect("counter clause must parse");
        assert_eq!(remainder, "");
        let Effect::PutCounter {
            target: TargetFilter::Typed(target),
            ..
        } = effect
        else {
            panic!("expected a typed PutCounter target, got {effect:?}");
        };
        assert!(target.properties.iter().any(|property| {
            matches!(
                property,
                FilterProp::DistinctFrom { reference }
                    if **reference == TargetFilter::EventTarget
            )
        }));
    }

    /// Gap-A + §B2 integration: the real Esper Terra chapter chain parses with zero
    /// `Unimplemented`, its PutCounter count is `UpTo{Fixed(3)}`, its target is the
    /// created token (`LastCreated`, NOT `SelfRef`), and it carries the is-a-Saga
    /// gate. Reverting the §B2 chunk-loop bind → `SelfRef`; reverting the Gap-A
    /// strip → the whole "put" clause is `Unimplemented`.
    #[test]
    fn esper_chapter_put_counter_up_to_binds_last_created() {
        let def = super::super::parse_effect_chain(
            ESPER_CHAPTER,
            crate::types::ability::AbilityKind::Spell,
        );
        assert!(
            !chain_has_unimplemented(&def),
            "chapter must parse with zero Unimplemented (reach-guard)"
        );
        let (effect, condition) = find_put_counter(&def).expect("chapter contains a PutCounter");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = effect
        else {
            panic!("expected PutCounter");
        };
        assert_eq!(counter_type, CounterType::Lore);
        assert_eq!(
            count,
            QuantityExpr::UpTo {
                max: Box::new(QuantityExpr::Fixed { value: 3 })
            },
            "count must be UpTo{{Fixed(3)}}, not a bare Fixed(3)"
        );
        assert_eq!(
            target,
            TargetFilter::LastCreated,
            "§B2: 'on it' after CopyTokenOf binds the created token, not SelfRef"
        );
        assert!(
            condition.is_some(),
            "the put node keeps its is-a-Saga condition gate"
        );
    }

    /// §B2 guard companion (POSITIVE — original clobber behavior preserved): a
    /// counter "it" that follows a TYPED target with NO token creator (Turtle Van:
    /// "Put a +1/+1 counter on target creature, then double the number of +1/+1
    /// counters on it") STILL binds the parent target (`ParentTarget`). The
    /// `mod.rs:14753` `LastCreated` guard fires only when the parse bound
    /// `LastCreated` (a token creator was present), so this non-token anaphor is
    /// untouched — it must NOT become `LastCreated` or `SelfRef`. Brackets the
    /// guard's revert-to-red (which proves the NEW token behavior).
    #[test]
    fn multiply_counter_on_it_after_typed_target_no_token_stays_parent_target() {
        let def = super::super::parse_effect_chain(
            "Put a +1/+1 counter on target creature, then double the number of +1/+1 counters on it.",
            crate::types::ability::AbilityKind::Spell,
        );
        let mut all = Vec::new();
        collect_defs(&def, &mut all);
        let mc_target = all
            .iter()
            .find_map(|d| match &*d.effect {
                Effect::MultiplyCounter { target, .. } => Some(target.clone()),
                _ => None,
            })
            .expect("chain contains a MultiplyCounter");
        assert_eq!(
            mc_target,
            TargetFilter::ParentTarget,
            "non-token typed-target counter anaphor must stay ParentTarget (guard untouched)"
        );
    }

    /// §B2 NEG (over-rewrite safety, the 9 genuine-source cards — e.g. construct a
    /// cosmic cube / edgar markov's coffin): an explicit self-reference after a
    /// token creator STAYS `SelfRef` (it takes the explicit-target path, never the
    /// it-pronoun branch). Revert-to-red = a post-token structure-only gate would
    /// flip this to `LastCreated`.
    #[test]
    fn explicit_self_ref_after_token_creator_stays_self_ref() {
        let def = super::super::parse_effect_chain(
            "Create a 1/1 white Soldier creature token. Put a +1/+1 counter on this creature.",
            crate::types::ability::AbilityKind::Spell,
        );
        let (effect, _) = find_put_counter(&def).expect("PutCounter present");
        let Effect::PutCounter { target, .. } = effect else {
            panic!("expected PutCounter");
        };
        assert_eq!(
            target,
            TargetFilter::SelfRef,
            "explicit 'this creature' after a token creator must stay SelfRef"
        );
    }
}
