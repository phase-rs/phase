use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::{opt, rest, value};
use nom::sequence::{preceded, terminated};
use nom::Parser;
use nom_language::error::VerboseError;

use super::counter::{
    try_parse_double_effect, try_parse_move_counters_from, try_parse_put_counter,
    try_parse_remove_counter,
};
use super::mana::{try_parse_activate_only_condition, try_parse_add_mana_effect};
use super::token::try_parse_token;
use super::{
    attach_controller_if_absent, is_bare_object_pronoun, resolve_it_pronoun, ParseContext,
};
use crate::parser::oracle_ir::ast::*;
use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
use crate::parser::oracle_nom::bridge::nom_on_lower;
use crate::parser::oracle_nom::primitives as nom_primitives;
use crate::parser::oracle_static::{
    parse_continuous_modifications, parse_quoted_ability_modifications,
};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, CategoryChooserScope, ChoiceType, Chooser,
    ContinuousModification, ControllerRef, Duration, Effect, GainLifePlayer, LibraryPosition,
    MultiTargetSpec, PaymentCost, PlayerScope, PreventionAmount, PreventionScope, PtValue,
    QuantityExpr, QuantityRef, SearchSelectionConstraint, StaticDefinition, TargetFilter,
    TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::phase::Phase;
use crate::types::player::PlayerCounterKind;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::super::oracle_target::parse_target;
use super::super::oracle_util::{
    contains_object_pronoun, contains_possessive, parse_count_expr, parse_mana_symbols,
    parse_ordinal, split_around, starts_with_possessive, strip_after, TextPair,
};

/// CR 702.26: Phasing direction used by the "phase in"/"phase out" dispatch.
#[derive(Copy, Clone)]
enum PhaseDir {
    In,
    Out,
}

/// Earthbend keyword action default target: "target land you control".
/// Used when the Earthbend verb appears without an explicit target (e.g., after
/// reminder text stripping removes the parenthetical that contains the target).
pub(super) fn default_earthbend_target() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You))
}

/// Shared ControlNextTurn suffix parser (CR 722.1). Called after a prefix
/// combinator ("you control " or "gain control of ") has matched; parses the
/// target, then " during that player's next turn", then the optional extra-turn
/// tail (CR 722.1 doesn't require it; some cards like Emrakul grant it).
/// Returns `None` when the suffix doesn't apply, allowing the caller to treat
/// the match as a different effect (e.g., plain `GainControl`).
fn try_parse_control_next_turn_suffix(_text: &str, rest: &str) -> Option<(TargetFilter, bool)> {
    let (target_text, _) = super::strip_optional_target_prefix(rest);
    let (target, rem) = parse_target(target_text);
    let rem_lower = rem.to_ascii_lowercase();
    tag::<_, _, VerboseError<&str>>(" during that player's next turn")
        .parse(rem_lower.as_str())
        .ok()?;
    let rem_after_during = &rem[" during that player's next turn".len()..];
    let rem_after_during_lower = rem_after_during.to_ascii_lowercase();
    let (_tail, grant_extra_turn_after) = if let Ok((tail, _)) = alt((
        tag::<_, _, VerboseError<&str>>(". after that turn, that player takes an extra turn"),
        tag(" after that turn, that player takes an extra turn"),
        tag("after that turn, that player takes an extra turn"),
    ))
    .parse(rem_after_during_lower.as_str())
    {
        (
            &rem_after_during[rem_after_during.len() - tail.len()..],
            true,
        )
    } else {
        (rem_after_during, false)
    };
    #[cfg(debug_assertions)]
    assert_no_compound_remainder(_tail, _text);
    Some((target, grant_extra_turn_after))
}

/// Parse "earthbend [N] [target <type>]" from the text after "earthbend ".
/// Returns `(target, power, toughness)`. Defaults to "target land you control"
/// when no explicit target remains (reminder text stripped, sequence connectors,
/// or variable amounts like "X, where X is...").
///
/// Shared by both the single-imperative parser (`parse_targeted_action_ast`)
/// and the sequence-level parser (`try_parse_verb_and_target` in `mod.rs`).
pub(super) fn parse_earthbend_params(text: &str, lower_rest: &str) -> (TargetFilter, i32, i32) {
    // Delegate to nom combinator (input already lowercase from lower_rest parameter).
    let parsed_number = nom_primitives::parse_number.parse(lower_rest).ok();
    let (pt, target_text) = parsed_number
        .map(|(rem, n)| (n as i32, rem.trim_start()))
        .unwrap_or((0, lower_rest));
    let target = resolve_earthbend_target(text, target_text, parsed_number.is_some());
    (target, pt, pt)
}

/// Parse "earthbend [N | X[, where X is …]] [target <type>]" returning the
/// counter count as a `QuantityExpr` so dynamic amounts (Toph's "earthbend X,
/// where X is the number of experience counters you have") flow through to
/// `Effect::PutCounter` instead of collapsing to `Fixed { value: 0 }`.
///
/// Dispatch order:
/// 1. Literal N (`parse_number` succeeds) → `QuantityExpr::Fixed`.
/// 2. `"x, where X is the number of <kind> counters <possessor>"` →
///    `QuantityExpr::Ref { qty: QuantityRef::PlayerCounter { … } }`.
/// 3. Bare `"x"` (no tail) → `QuantityExpr::Ref { qty: Variable { "X" } }`,
///    matching the spell-cost X resolution path.
/// 4. None of the above → `Fixed { value: 0 }` with the default target,
///    preserving the prior behaviour for unsupported text shapes.
///
/// Used only by `try_parse_earthbend_clause` (the full-expansion path that
/// emits Animate + PutCounter + delayed return). The literal-N AST path
/// retains `parse_earthbend_params` to avoid perturbing its two callers.
///
/// `text` is the full original-case clause (including the leading
/// "Earthbend "); `lower_rest` is the lowercased remainder after that prefix.
/// The shared `resolve_earthbend_target` helper recovers the original-case
/// target slice as `text[text.len() - target_text.len()..]`, which only lands
/// at the correct byte boundary because ASCII lowercasing preserves byte
/// length and the entire Oracle-text dispatcher operates on ASCII.
pub(super) fn parse_earthbend_count_expr(
    text: &str,
    lower_rest: &str,
) -> (TargetFilter, QuantityExpr) {
    if let Ok((rem, n)) = nom_primitives::parse_number.parse(lower_rest) {
        let target_text = rem.trim_start();
        let target = resolve_earthbend_target(text, target_text, true);
        return (target, QuantityExpr::Fixed { value: n as i32 });
    }
    if let Ok((rem, _)) = tag::<_, _, VerboseError<&str>>("x").parse(lower_rest) {
        // CR 122.1: "X, where X is the number of <kind> counters <possessor>".
        if let Ok((rem2, qty)) = preceded(
            tag::<_, _, VerboseError<&str>>(", where x is "),
            crate::parser::oracle_nom::quantity::parse_the_number_of_player_counters,
        )
        .parse(rem)
        {
            let target_text = rem2.trim_start();
            let target = resolve_earthbend_target(text, target_text, true);
            return (target, QuantityExpr::Ref { qty });
        }
        // CR 107.3a + CR 601.2b: bare X resolves through the spell-cost path.
        let target_text = rem.trim_start();
        let target = resolve_earthbend_target(text, target_text, true);
        return (
            target,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
        );
    }
    (default_earthbend_target(), QuantityExpr::Fixed { value: 0 })
}

/// Shared target-text reduction for earthbend parsing. Distinguishes between
/// "explicit target follows the numeric slot" and "use the default target".
/// Factored out so `parse_earthbend_params` and `parse_earthbend_count_expr`
/// can't drift in target detection.
fn resolve_earthbend_target(
    text: &str,
    target_text: &str,
    parsed_numeric_slot: bool,
) -> TargetFilter {
    let has_explicit_target = if !parsed_numeric_slot {
        false
    } else {
        let trimmed =
            target_text.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
        !trimmed.is_empty()
            && tag::<_, _, VerboseError<&str>>("then ")
                .parse(trimmed)
                .is_err()
            && tag::<_, _, VerboseError<&str>>("and ")
                .parse(trimmed)
                .is_err()
    };
    if has_explicit_target {
        let (t, _) = parse_target(&text[text.len() - target_text.len()..]);
        t
    } else {
        default_earthbend_target()
    }
}

/// Parse a dynamic count phrase that begins with "cards"/"a card" or "that
/// many" against an already-lowercased tail. Returns the resolved
/// `QuantityExpr` or `None` if no recognized shape matches.
///
/// Covers the cross-verb dynamic-count idioms shared by Draw/Mill/Discard:
/// - "cards equal to <ref>"  / "a card equal to <ref>"  → Ref{<ref>}
/// - "that many cards"       / "that many"              → Ref{EventContextAmount}
///
/// CR 121.1 / CR 701.13a / CR 701.8a — chained-effect amounts and target-
/// relative quantity refs route through one combinator instead of being
/// re-implemented per verb.
fn parse_dynamic_count_phrase(lower: &str) -> Option<QuantityExpr> {
    if let Ok((qty_tail, _)) = alt((
        tag::<_, _, VerboseError<&str>>("cards equal to "),
        tag("a card equal to "),
    ))
    .parse(lower)
    {
        let qty_text = qty_tail.trim_end_matches('.').trim();
        if let Some(qty) = crate::parser::oracle_quantity::parse_quantity_ref(qty_text) {
            return Some(QuantityExpr::Ref { qty });
        }
    }
    if let Ok((rest, _)) = alt((
        tag::<_, _, VerboseError<&str>>("that many cards"),
        tag("that many"),
    ))
    .parse(lower)
    {
        // M3 guard: only match when the tail is empty/punctuation. Avoids
        // false positives like "that many cards from the top of their library".
        if rest.trim_start_matches('.').trim().is_empty() {
            return Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            });
        }
    }
    None
}

pub(super) fn parse_numeric_imperative_ast(
    text: &str,
    lower: &str,
) -> Option<NumericImperativeAst> {
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| value((), tag("draw ")).parse(input))
        .or_else(|| {
            nom_on_lower(text, lower, |input| {
                let (rest, _) = (
                    take_until::<_, _, VerboseError<&str>>(" draws "),
                    tag(" draws "),
                )
                    .parse(input)?;
                Ok((rest, ()))
            })
        })
    {
        // CR 608.2d + CR 121.1: "draw up to N cards" — opt-choice draw,
        // mirrors the up_to pattern on Discard / Sacrifice. Strip the prefix
        // before count parsing so "up to two cards" → count=Fixed{2}, up_to=true.
        let (rest, up_to) = match nom_on_lower(rest, &rest.to_ascii_lowercase(), |i| {
            value((), tag("up to ")).parse(i)
        }) {
            Some((_, after)) => (after, true),
            None => (rest, false),
        };
        // CR 121.1 / CR 609.3: dynamic-count tails — "cards equal to <ref>",
        // "a card equal to <ref>", "that many cards", "that many".
        let rest_lower = rest.to_ascii_lowercase();
        if let Some(count) = parse_dynamic_count_phrase(rest_lower.as_str()) {
            return Some(NumericImperativeAst::Draw { count, up_to });
        }
        // CR 119.1 / CR 121.1: When the verb committed but the quantity phrase
        // can't be classified, return None so the line surfaces as
        // `Effect::Unimplemented` upstream. Silently substituting Fixed{1} hides
        // dynamic-quantity gaps from the coverage report.
        let count = parse_count_expr(rest).map(|(q, _)| q)?;
        return Some(NumericImperativeAst::Draw { count, up_to });
    }

    if nom_primitives::scan_contains(lower, "gain") && nom_primitives::scan_contains(lower, "life")
    {
        // CR 119.1: Handle "life equal to {quantity}" — dynamic amount from game state.
        if let Some(qty_text) =
            strip_after(lower, "life equal to ").map(|s| s.trim_end_matches('.'))
        {
            if let Some(qty) =
                crate::parser::oracle_quantity::parse_event_context_quantity(qty_text)
            {
                return Some(NumericImperativeAst::GainLife { amount: qty });
            }
            // CR 119.1: target-relative quantity refs ("target creature's
            // power/toughness/mana value"). Mirrors LoseLife. Soul's Grace,
            // Heron's Grace Champion, Lifeblood Hydra, etc.
            if let Some(qty) = crate::parser::oracle_quantity::parse_quantity_ref(qty_text) {
                return Some(NumericImperativeAst::GainLife {
                    amount: QuantityExpr::Ref { qty },
                });
            }
        }
        let after_gain = nom_on_lower(text, lower, |input| {
            value((), alt((tag("you gain "), tag("gain ")))).parse(input)
        })
        .map(|(_, rest)| rest)
        .or_else(|| {
            nom_on_lower(text, lower, |input| {
                let (rest, _) = (
                    take_until::<_, _, VerboseError<&str>>(" gains "),
                    tag(" gains "),
                )
                    .parse(input)?;
                Ok((rest, ()))
            })
            .map(|(_, rest)| rest)
        })
        .unwrap_or("");
        if !after_gain.is_empty() {
            // CR 603.7c + CR 119.1: "gain that much life" / "gain that many life" —
            // amount is the triggering event's amount (Exquisite Blood). Extract the
            // amount phrase before " life" and route through the event-context
            // quantity parser so "that much" resolves to `EventContextAmount`
            // rather than defaulting to 1.
            let after_lower = after_gain.to_ascii_lowercase();
            let amount_phrase = take_until::<_, _, VerboseError<&str>>(" life")
                .parse(after_lower.as_str())
                .map(|(_, before)| before.trim())
                .unwrap_or_else(|_: nom::Err<VerboseError<&str>>| {
                    after_lower.trim_end_matches('.').trim()
                });
            if let Some(qty) =
                crate::parser::oracle_quantity::parse_event_context_quantity(amount_phrase)
            {
                return Some(NumericImperativeAst::GainLife { amount: qty });
            }
            // CR 119.1: GainLife committed but quantity phrase unclassified —
            // surface as Unimplemented rather than fabricating Fixed{1}.
            let amount = parse_count_expr(after_gain).map(|(q, _)| q)?;
            return Some(NumericImperativeAst::GainLife { amount });
        }
    }

    if nom_primitives::scan_contains(lower, "lose") && nom_primitives::scan_contains(lower, "life")
    {
        if let Some(expr) = try_parse_half_life_amount(lower) {
            return Some(NumericImperativeAst::LoseLife { amount: expr });
        }
        // CR 119.3: Handle "life equal to {quantity}" — dynamic amount from game state.
        if let Some(qty_text) =
            strip_after(lower, "life equal to ").map(|s| s.trim_end_matches('.'))
        {
            if let Some(qty) =
                crate::parser::oracle_quantity::parse_event_context_quantity(qty_text)
            {
                return Some(NumericImperativeAst::LoseLife { amount: qty });
            }
            // CR 119.3: target-relative quantity refs ("target creature's
            // power/toughness/mana value", etc.) — Final Punishment, Tomb
            // Blade-class drain, Genesis of the Daleks. Delegates to the
            // shared `parse_quantity_ref` building block.
            if let Some(qty) = crate::parser::oracle_quantity::parse_quantity_ref(qty_text) {
                return Some(NumericImperativeAst::LoseLife {
                    amount: QuantityExpr::Ref { qty },
                });
            }
        }
        // CR 603.7c + CR 119.3: "lose that much life" / "lose that many life" —
        // amount is the triggering event's amount. Probe for event-context phrases
        // before falling back to the numeric last-word extractor.
        if let Ok((_, before_life)) = take_until::<_, _, VerboseError<&str>>("life").parse(lower) {
            let after_verb = nom_on_lower(text, lower, |input| {
                value((), alt((tag("you lose "), tag("lose ")))).parse(input)
            })
            .map(|(_, rest)| rest)
            .unwrap_or("");
            if !after_verb.is_empty() {
                let after_lower = after_verb.to_ascii_lowercase();
                let amount_phrase = take_until::<_, _, VerboseError<&str>>(" life")
                    .parse(after_lower.as_str())
                    .map(|(_, before)| before.trim())
                    .unwrap_or_else(|_: nom::Err<VerboseError<&str>>| {
                        after_lower.trim_end_matches('.').trim()
                    });
                if let Some(qty) =
                    crate::parser::oracle_quantity::parse_event_context_quantity(amount_phrase)
                {
                    return Some(NumericImperativeAst::LoseLife { amount: qty });
                }
            }
            // CR 119.3: LoseLife committed but neither the event-context phrase
            // nor a numeric tail parsed — return None so the line lands in
            // `Effect::Unimplemented` upstream instead of fabricating Fixed{1}.
            // (This is the Keen Duelist class: "lose life equal to <unparsed>".)
            let before_life = before_life.trim();
            let last_word = before_life.split_whitespace().next_back().unwrap_or("");
            let amount = parse_count_expr(last_word).map(|(q, _)| q)?;
            return Some(NumericImperativeAst::LoseLife { amount });
        }
        return None;
    }

    if nom_primitives::scan_contains(lower, "gets +")
        || nom_primitives::scan_contains(lower, "gets -")
        || nom_primitives::scan_contains(lower, "get +")
        || nom_primitives::scan_contains(lower, "get -")
    {
        // Accept any pump — discard the target. Callers that need subject threading
        // (e.g., try_parse_for_each_effect) extract the subject separately via
        // thread_for_each_subject after lowering the AST.
        if let Some(Effect::Pump {
            power, toughness, ..
        }) = super::try_parse_pump(lower, text)
        {
            return Some(NumericImperativeAst::Pump { power, toughness });
        }
    }

    // Keyword action verbs with numeric count: scry N, surveil N, mill N.
    // CR 701.22a + CR 701.25a: Oracle uses third-person conjugations
    // ("Target player scries 2", "Target opponent surveils 1") — match both
    // the bare-form imperative and the conjugated form. `mills` is included
    // for symmetry with the "Target player mills N" pattern.
    if let Some((verb, rest)) = nom_on_lower(text, lower, |input| {
        alt((
            value("scry", alt((tag("scry "), tag("scries ")))),
            value("surveil", alt((tag("surveil "), tag("surveils ")))),
            value("mill", alt((tag("mill "), tag("mills ")))),
        ))
        .parse(input)
    }) {
        // CR 121.1 / CR 609.3 / CR 701.13a: dynamic-count tails for the
        // shared mill/scry/surveil family.
        let rest_lower = rest.to_ascii_lowercase();
        if let Some(count) = parse_dynamic_count_phrase(rest_lower.as_str()) {
            // allow-noncombinator: dispatch on already-parsed verb tag (combinator output, not Oracle text)
            return match verb {
                "scry" => Some(NumericImperativeAst::Scry { count }), // allow-noncombinator: combinator-output dispatch
                "surveil" => Some(NumericImperativeAst::Surveil { count }), // allow-noncombinator: combinator-output dispatch
                "mill" => Some(NumericImperativeAst::Mill { count }), // allow-noncombinator: combinator-output dispatch
                _ => unreachable!(),
            };
        }
        // CR 701.22a / CR 701.25a / CR 701.13a: Scry/Surveil/Mill verbs always
        // require a count. If the count phrase doesn't parse, return None so the
        // line surfaces as Unimplemented rather than silently scrying/milling 1.
        let count = parse_count_expr(rest).map(|(q, _)| q)?;
        return match verb {
            "scry" => Some(NumericImperativeAst::Scry { count }),
            "surveil" => Some(NumericImperativeAst::Surveil { count }),
            "mill" => Some(NumericImperativeAst::Mill { count }),
            _ => unreachable!(),
        };
    }

    None
}

/// CR 107.1a: Parse "lose(s) half [possessive] life, rounded up/down" →
/// `HalfRounded` expression by delegating to the shared quantity combinator.
///
/// Strips the `lose(s) ` verb prefix, then runs
/// [`super::super::oracle_nom::quantity::parse_half_rounded`] over the
/// remainder so every possessive quantity the combinator recognizes
/// (`"half their life"`, `"half your life total"`, `"half his or her life"`,
/// …) unlocks a typed amount. Previously this helper hand-rolled a small
/// `their life` / `your life` dispatch that (a) dropped "their life total"
/// and (b) silently mis-bound the nom remainder. Both bugs disappear by
/// routing through the shared combinator.
fn try_parse_half_life_amount(lower: &str) -> Option<QuantityExpr> {
    // Strip "lose " / "loses " and any intervening whitespace.
    let (after_verb, _) = alt((tag::<_, _, VerboseError<&str>>("lose "), tag("loses ")))
        .parse(lower)
        .ok()?;
    let after_verb = after_verb.trim_start();
    // Delegate to the shared "half ..." combinator. This picks up the
    // possessive inner ref AND the rounding suffix in one call.
    let (_, expr) = super::super::oracle_nom::quantity::parse_half_rounded(after_verb).ok()?;
    Some(expr)
}

pub(super) fn lower_numeric_imperative_ast(ast: NumericImperativeAst) -> Effect {
    match ast {
        // CR 121.1: Default `target: TargetFilter::Controller` — the imperative
        // path doesn't see the subject, which is later threaded via
        // `inject_subject_target` for "target player draws ..." patterns
        // (CR 601.2c per-mode targeting).
        // CR 121.1 + CR 608.2d: Lower the AST `up_to: bool` into the typed
        // `count: QuantityExpr::UpTo { max }` wrapper. Plain `up_to: false`
        // leaves the count expression unchanged; `up_to: true` wraps it so
        // the resolver peels it back at runtime.
        NumericImperativeAst::Draw { count, up_to } => Effect::Draw {
            count: if up_to {
                QuantityExpr::up_to(count)
            } else {
                count
            },
            target: TargetFilter::Controller,
        },
        NumericImperativeAst::GainLife { amount } => Effect::GainLife {
            amount,
            player: GainLifePlayer::Controller,
        },
        NumericImperativeAst::LoseLife { amount } => Effect::LoseLife {
            amount,
            target: None,
        },
        // CR 608.2c: Pump uses TargetFilter::Any as a sentinel — callers
        // (inject_subject_target, thread_for_each_subject) replace it with the
        // parsed subject's target. No warning here; Any is an expected intermediate.
        NumericImperativeAst::Pump { power, toughness } => Effect::Pump {
            power,
            toughness,
            target: TargetFilter::Any,
        },
        // CR 701.22a + CR 601.2c: Default Controller target — `inject_subject_target`
        // upgrades to `TargetFilter::Player` for "target player scrys ..." subjects.
        NumericImperativeAst::Scry { count } => Effect::Scry {
            count,
            target: TargetFilter::Controller,
        },
        // CR 701.25a + CR 601.2c: Same Controller default; subject promotion
        // wires "target opponent surveils ..." through inject_subject_target.
        NumericImperativeAst::Surveil { count } => Effect::Surveil {
            count,
            target: TargetFilter::Controller,
        },
        NumericImperativeAst::Mill { count } => Effect::Mill {
            count,
            // CR 701.17a: "Mill" with no subject defaults to the controller.
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        },
    }
}

/// Strip leading "a " / "an " article from target text before passing to `parse_target`.
/// Follows the same pattern used by `oracle_cost.rs` for sacrifice cost parsing.
fn strip_article(text: &str) -> &str {
    let lower = text.to_lowercase();
    nom_on_lower(text, &lower, nom_primitives::parse_article)
        .map(|(_, rest)| rest)
        .unwrap_or(text)
}

/// CR 107.1a + CR 701.16a: Extract the typed filter embedded in an
/// `ObjectCount` quantity expression. Used by the sacrifice AST builder to
/// lift "half the permanents they control" → ObjectCount's filter into the
/// effect's target, so eligibility matches the same set the count was
/// computed against. Recurses through `HalfRounded` / `Multiply` / `Offset`
/// wrappers since the filter belongs to the innermost ObjectCount; returns
/// `None` for expressions that carry no filter (Fixed, Variable(X), etc.).
fn extract_object_count_filter(expr: &QuantityExpr) -> Option<TargetFilter> {
    match expr {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } => Some(filter.clone()),
        QuantityExpr::HalfRounded { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::Offset { inner, .. } => extract_object_count_filter(inner),
        _ => None,
    }
}

/// CR 608.2c: Extract "unless you discard a [type] card" suffix from discard text.
/// Returns the text with the suffix stripped and the parsed filter, or the original text
/// with None if no "unless you discard" clause is present.
///
/// Handles: creature, artifact, instant or sorcery, basic land, enchantment, subtype cards.
fn parse_discard_unless_filter<'a>(
    lower: &'a str,
    _original: &'a str,
) -> (&'a str, Option<TargetFilter>) {
    let Some((before, after_unless)) = split_around(lower, " unless you discard ") else {
        return (lower, None);
    };

    // Strip leading article "a " / "an "
    let type_text = strip_article(after_unless);
    // Strip trailing " card" / " card." — parse_target expects type phrase without "card"
    let type_text = type_text
        .strip_suffix(" card.")
        .or_else(|| type_text.strip_suffix(" card"))
        .unwrap_or(type_text)
        .trim_end_matches('.');

    let (filter, _) = parse_target(type_text);
    if matches!(filter, TargetFilter::Any) {
        // parse_target couldn't parse the type — don't strip
        return (lower, None);
    }
    (before, Some(filter))
}

/// CR 701.9a + CR 608.2c: Parse the card-type filter portion of a discard phrase.
///
/// Recognizes "a <type> card" / "an <type> card" / "<N> <type> cards" where the
/// type portion is anything `parse_target` understands (subtypes, core types,
/// "instant or sorcery", etc.). Returns `None` when no type qualifier appears
/// (plain "a card" / "N cards" means any card is legal).
///
/// Mirrors `AbilityCost::Discard.filter` so the trigger-effect discard on
/// Dokuchi Silencer ("you may discard a creature card") preserves the same
/// filter data as cost-form discards like "Discard a creature card:".
/// Extract the type qualifier from the post-count tail of a discard noun phrase.
///
/// Caller contract: `tail` is the text **after** the count token has already
/// been consumed by `parse_count_expr`. So for "discard two creature cards"
/// the count parser eats "two " and hands "creature cards" here. For "a card"
/// (count = 1, no type qualifier) the count parser eats "a " and hands
/// "card" here, which has no leading type word and returns `None`.
///
/// Mirrors `AbilityCost::Discard.filter` so the trigger-effect discard on
/// Dokuchi Silencer ("you may discard a creature card") preserves the same
/// filter data as cost-form discards like "Discard a creature card:".
fn parse_discard_card_filter(tail: &str) -> Option<TargetFilter> {
    // Find the " card" / " cards" suffix — the type phrase lies before it.
    // No suffix or empty before-suffix → no type qualifier.
    let type_phrase = tail
        .strip_suffix(" cards") // allow-noncombinator: structural suffix cleanup on pre-chunked sub-phrase (PATTERNS.md §9)
        .or_else(|| tail.strip_suffix(" card"))? // allow-noncombinator: see line above
        .trim();
    if type_phrase.is_empty() {
        return None;
    }
    let (filter, remainder) = parse_target(type_phrase);
    if !remainder.trim().is_empty() || matches!(filter, TargetFilter::Any) {
        return None;
    }
    Some(filter)
}

/// CR 701.21a + CR 608.2k: When a targeted-action body has been stripped of an
/// actor prefix ("you (may) ", "an opponent (may) ", "each opponent ", "each
/// player "), `ctx.actor` carries the resolving player's controller-ref. This
/// helper defaults `TargetFilter::Typed.controller` to that actor whenever the
/// parsed target phrase didn't supply one. Without the default, the resolver
/// treats `controller: None` as Any — letting the actor sacrifice / discard /
/// return any object on the battlefield, violating CR 701.21a (sacrifice) and
/// the analogous owner / controller restrictions on other actor-bound verbs.
fn apply_actor_default(filter: &mut TargetFilter, ctx: &mut ParseContext) {
    if let Some(actor) = ctx.actor.as_ref() {
        attach_controller_if_absent(filter, actor.clone());
    }
}

/// CR 701.21a/b: "of their choice" / "of your choice" confirms who chooses
/// what to sacrifice; it is not part of the sacrificed-object filter. Preserve
/// trailing relative clauses so `parse_target` can consume predicates like
/// "that shares a card type with it".
fn strip_sacrifice_choice_marker(target_text: &str) -> String {
    let lower = target_text.to_lowercase();

    if let Some((_, rest)) = nom_on_lower(target_text, &lower, |input| {
        value((), alt((tag("of their choice"), tag("of your choice")))).parse(input)
    }) {
        return rest.trim_start().to_string();
    }

    if let Some((before_len, rest)) = nom_on_lower(target_text, &lower, |input| {
        let (rest, before) = alt((
            terminated(take_until(" of their choice"), tag(" of their choice")),
            terminated(take_until(" of your choice"), tag(" of your choice")),
        ))
        .parse(input)?;
        Ok((rest, before.len()))
    }) {
        let before = &target_text[..before_len];
        return format!("{before}{rest}");
    }

    target_text.to_string()
}

/// NOTE: Shares verb prefixes with `try_parse_verb_and_target` in `mod.rs`.
/// When adding a new targeted verb here, check if it also needs to be added there
/// (for compound action splitting like "tap target creature and put a counter on it").
pub(super) fn parse_targeted_action_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<TargetedImperativeAst> {
    // CR 701.26a/b: Tap/untap all — mass variants must be checked before single-target
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("tap all "), tag("tap each ")))).parse(input)
    }) {
        let (target, _rem) = parse_target(rest);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::TapAll { target });
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("untap all "), tag("untap each ")))).parse(input)
    }) {
        let (target, _rem) = parse_target(rest);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::UntapAll { target });
    }
    // CR 701.16a: "sacrifice [count] <filter> [of their choice]" —
    // delegates to `parse_count_expr` so "a"/"an"/"X"/"half the permanents
    // they control" all flow through one authority. "Of their choice" is
    // the default per CR 701.16b (the sacrificing player chooses); strip
    // it as a confirmation suffix rather than bleeding into the filter.
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("sacrifice ")).parse(input)
    }) {
        let (count, after_count) = super::super::oracle_util::parse_count_expr(rest).unwrap_or((
            crate::types::ability::QuantityExpr::Fixed { value: 1 },
            rest,
        ));
        let (target_text, _) = super::strip_optional_target_prefix(after_count.trim_start());
        // Strip the "of their choice" / "of your choice" confirmation suffix —
        // CR 701.16b makes player choice the default, so the phrase is a no-op
        // that must be consumed so it doesn't bleed into the filter. Two
        // shapes exist: (1) the filter precedes the phrase ("permanents
        // they control of their choice" — split at the leading space), and
        // (2) the count subsumes the filter and only the phrase is left
        // ("of their choice" — treat the entire remainder as the phrase).
        let target_text = strip_sacrifice_choice_marker(target_text);
        // CR 107.2: Skip `parse_target` on an empty remainder — the count
        // subsumed the filter ("sacrifice half the permanents they control
        // of their choice"), so there is nothing left to classify. Avoids
        // emitting a `target-fallback` parse warning for a well-formed parse.
        //
        // CR 608.2k: When the remainder is a bare object pronoun ("it",
        // "itself", "them", "him", "her") AND the parse context carries an
        // explicit trigger subject, resolve the pronoun against that subject
        // instead of defaulting to `ParentTarget`. On a self-ETB trigger
        // ("When Phlage enters, sacrifice it unless it escaped") the subject
        // is `SelfRef` and there is no outer targeted object, so "it" binds
        // to the source permanent. For context-free parses (e.g., the
        // populate anaphor chain "populate. … sacrifice it at the beginning
        // of the next end step") the antecedent is set later by
        // `rewrite_parent_target_to_last_created`, so we must preserve
        // `ParentTarget` when no subject is provided.
        let target = if target_text.trim().is_empty() {
            TargetFilter::Any
        } else if ctx.subject.is_some() && is_bare_object_pronoun(target_text.trim()) {
            resolve_it_pronoun(ctx)
        } else {
            let (target, _rem) = parse_target(&target_text);
            #[cfg(debug_assertions)]
            assert_no_compound_remainder(_rem, text);
            target
        };
        // CR 701.16a: When the count expression already carries a typed filter
        // ("half the permanents they control" → ObjectCount{Typed[Permanent,
        // controller:You]}) and the target text didn't yield a filter, lift the
        // count's filter into `target` so eligibility matches the same set the
        // count was computed against. Without this lift, Sacrifice would fall
        // back to `Any` and the parser-warned filter would be silently dropped.
        let mut target = if matches!(target, TargetFilter::Any) {
            extract_object_count_filter(&count).unwrap_or(target)
        } else {
            target
        };
        // CR 701.21a: Default the sacrificed permanent's controller to the
        // resolving player when the target phrase didn't specify one. "You may
        // sacrifice a non-Demon creature" must restrict the prompt to the
        // actor's permanents — sacrificing requires controlling the permanent.
        apply_actor_default(&mut target, ctx);
        return Some(TargetedImperativeAst::Sacrifice { target, count });
    }
    // Simple targeted verbs: tap, untap — parse target after verb prefix
    if let Some((verb, rest)) = nom_on_lower(text, lower, |input| {
        alt((value("tap", tag("tap ")), value("untap", tag("untap ")))).parse(input)
    }) {
        let (target_text, _) = super::strip_optional_target_prefix(strip_article(rest));
        let (target, _rem) = parse_target(target_text);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return match verb {
            "tap" => Some(TargetedImperativeAst::Tap { target }),
            "untap" => Some(TargetedImperativeAst::Untap { target }),
            _ => unreachable!(),
        };
    }
    if let Some((_, after_discard_orig)) =
        nom_on_lower(text, lower, |input| value((), tag("discard ")).parse(input))
    {
        let after_discard = &lower[lower.len() - after_discard_orig.len()..];
        // CR 701.9a: Back-reference discard — "discard that card" / "discard
        // those cards" target a specific card identified by the parent effect
        // (Seek/Conjure/Reveal-Choose populate ParentTarget at runtime). Must
        // be checked before the player-choice count-based discard path, since
        // "that card" is not a count phrase.
        if alt((
            tag::<_, _, VerboseError<&str>>("that card"),
            tag("those cards"),
        ))
        .parse(after_discard)
        .is_ok()
        {
            return Some(TargetedImperativeAst::DiscardCard {
                target: TargetFilter::ParentTarget,
            });
        }
        // CR 701.9a: Detect "at random" suffix for random discard effects.
        let random = nom_primitives::scan_contains(after_discard, "at random");
        // CR 701.9b: Detect "up to" prefix for optional partial discard.
        let (after_discard, up_to) =
            match tag::<_, _, VerboseError<&str>>("up to ").parse(after_discard) {
                Ok((rest, _)) => (rest, true),
                Err(_) => (after_discard, false),
            };
        // Strip "all the cards in " / "all cards in " prefix compositionally for
        // patterns like "discard all the cards in your hand" / "discards all cards in their hand".
        let after_discard = alt((
            tag::<_, _, VerboseError<&str>>("all the cards in "),
            tag("all cards in "),
        ))
        .parse(after_discard)
        .map(|(rest, _)| rest)
        .unwrap_or(after_discard);
        // Detect whole-hand discard patterns before falling through to count parsing.
        // Uses tag prefix (not contains) to avoid matching "discard a card from your hand".
        if alt((
            tag::<_, _, VerboseError<&str>>("your hand"),
            tag("their hand"),
            tag("his or her hand"),
        ))
        .parse(after_discard)
        .is_ok()
        {
            return Some(TargetedImperativeAst::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                random,
                up_to,
                unless_filter: None,
                filter: None,
            });
        }
        // CR 701.8a: "discard any number of [filter] cards" — opt-choice
        // discard where the player picks any 0..hand-size. Encoded as
        // count = HandSize with up_to = true so the controller chooses how
        // many to actually discard. Mind Maggots ("discard any number of
        // creature cards"), Fervent Mastery, Sirocco-class chains.
        if let Ok((rest, _)) =
            tag::<_, _, VerboseError<&str>>("any number of ").parse(after_discard)
        {
            let filter = parse_discard_card_filter(rest);
            return Some(TargetedImperativeAst::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                random,
                up_to: true,
                unless_filter: None,
                filter,
            });
        }
        // CR 608.2c: Strip "unless you discard a [type] card" suffix before count parsing.
        // Compute original-case offset before the unless strip narrows the slice.
        let original_after = &text[text.len() - after_discard.len()..];
        let (after_discard, unless_filter) =
            parse_discard_unless_filter(after_discard, original_after);
        // Re-derive original_after for the narrowed (unless-stripped) text.
        let original_after = &original_after[..after_discard.len()];
        // CR 121.1 / CR 609.3 / CR 701.8a: dynamic-count tails for Discard
        // — Fervent Mastery, Hordewing Skaab discard sub-ability, Sirocco
        // chains, etc.
        let after_discard_lower = after_discard.to_ascii_lowercase();
        if let Some(count) = parse_dynamic_count_phrase(after_discard_lower.as_str()) {
            let filter = parse_discard_card_filter(after_discard);
            return Some(TargetedImperativeAst::Discard {
                count,
                random,
                up_to,
                unless_filter,
                filter,
            });
        }
        // CR 701.8a: Discard count must be explicit (or the implicit 1 from
        // "a/an" inside `parse_count_expr`). If the count phrase doesn't parse,
        // return None so the line surfaces as Unimplemented.
        // Forward the post-count remainder to the filter probe so it never
        // re-sees the count token — the type qualifier (if any) is whatever
        // is left after the count was eaten.
        let (count, after_count) = parse_count_expr(original_after)?;
        // CR 701.9a + CR 608.2c: Extract card-type filter from phrases like
        // "a creature card" / "an artifact card". Mirrors the filter slot on
        // `AbilityCost::Discard` so trigger-effect discards carry the same
        // restriction data as cost discards (Dokuchi Silencer's "you may
        // discard a creature card").
        let filter = parse_discard_card_filter(after_count.trim_start());
        return Some(TargetedImperativeAst::Discard {
            count,
            random,
            up_to,
            unless_filter,
            filter,
        });
    }
    // CR 400.7 + CR 611.2c: Unified `return [all|each]?` dispatcher. Consumes
    // the verb plus an optional `all`/`each` plural quantifier, then routes
    // by destination + origin. Mass-bounce ("return all creatures to their
    // owners' hands") promotes to `ReturnAll` ⇒ `Effect::BounceAll`.
    // Battlefield-targeted return-all ("return all artifact and enchantment
    // cards from all graveyards to the battlefield") falls through to the
    // single-target `ReturnToBattlefield` AST — promoting it to a mass
    // variant requires an `Effect::ChangeZoneAll` field-shape extension that
    // is out of scope here. Mirrors the `tap all`/`tap each` precheck arms
    // above plus the bare `tag("return ")` arm's destination routing.
    // The combinator captures the plurality directly: `true` = mass quantifier
    // (`all`/`each`), `false` = single-target. Avoids post-hoc string
    // equality on the consumed head (parser-combinator gate).
    if let Some((is_mass, rest)) = nom_on_lower(text, lower, |input| {
        alt((
            value(true, alt((tag("return all "), tag("return each ")))),
            value(false, tag("return ")),
        ))
        .parse(input)
    }) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        let (target_text, dest) = super::strip_return_destination_ext(rest);
        let (target, _rem) = parse_target(target_text);
        let origin = super::infer_origin_zone(rest_lower);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);

        // CR 400.7: Battlefield destination ⇒ ChangeZone (single-target until
        // a separate ChangeZoneAll-shape extension lands). Non-hand non-
        // battlefield destinations route through ReturnToZone unchanged. Only
        // pure mass-bounce (battlefield⇒hand, no graveyard/library origin)
        // promotes to `ReturnAll` ⇒ `Effect::BounceAll`.
        return match dest {
            Some(d) if d.zone == Zone::Battlefield => {
                Some(TargetedImperativeAst::ReturnToBattlefield {
                    target,
                    origin,
                    enter_transformed: d.transformed,
                    under_your_control: d.under_your_control,
                    enter_tapped: d.enter_tapped,
                    enter_with_counters: d.enter_with_counters,
                })
            }
            Some(d) if d.zone == Zone::Hand => {
                // Mass return only when the source zone is implicit (battlefield):
                // "return all <filter> from your graveyard to your hand" must
                // remain `ChangeZone { origin: Graveyard, destination: Hand }`,
                // not `BounceAll` (whose resolver only scans the battlefield).
                if is_mass && origin.is_none() {
                    Some(TargetedImperativeAst::ReturnAll { target })
                } else if is_mass {
                    Some(TargetedImperativeAst::ReturnToZone {
                        target,
                        origin,
                        destination: Zone::Hand,
                    })
                } else {
                    Some(TargetedImperativeAst::Return { target })
                }
            }
            Some(d) => Some(TargetedImperativeAst::ReturnToZone {
                target,
                origin,
                destination: d.zone,
            }),
            // No explicit destination phrase. "Return all <filter>" with no
            // "to ..." tail still defaults to owner's hand for the
            // mass-bounce class. Single-target "return <X>" likewise defaults
            // to hand — preserves the pre-existing behavior.
            None => {
                if is_mass {
                    Some(TargetedImperativeAst::ReturnAll { target })
                } else {
                    Some(TargetedImperativeAst::Return { target })
                }
            }
        };
    }
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("fight ")).parse(input))
    {
        let (target_text, _) = super::strip_optional_target_prefix(rest);
        let (target, _rem) = parse_target(target_text);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::Fight { target });
    }
    // CR 722.1: "You control target player during that player's next turn"
    // (Mindslaver). Declarative form — "you" is not stripped as an imperative
    // subject because this isn't a verb-on-controller pattern. Must match
    // before the "gain control of" branch below since the prefixes differ.
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("you control ")).parse(input)
    }) {
        if let Some((target, grant_extra_turn_after)) =
            try_parse_control_next_turn_suffix(text, rest)
        {
            return Some(TargetedImperativeAst::ControlNextTurn {
                target,
                grant_extra_turn_after,
            });
        }
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("gain control of ")).parse(input)
    }) {
        // Check for ControlNextTurn suffix first (rare phrasing combining both
        // forms) before falling back to the standard GainControl effect.
        if let Some((target, grant_extra_turn_after)) =
            try_parse_control_next_turn_suffix(text, rest)
        {
            return Some(TargetedImperativeAst::ControlNextTurn {
                target,
                grant_extra_turn_after,
            });
        }
        let (target_text, _) = super::strip_optional_target_prefix(rest);
        let (target, _rem) = parse_target(target_text);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::GainControl { target });
    }
    // Earthbend: "earthbend [N] [target <type>]"
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("earthbend ")).parse(input)
    }) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        let (target, power, toughness) = parse_earthbend_params(text, rest_lower);
        return Some(TargetedImperativeAst::Earthbend {
            target,
            power,
            toughness,
        });
    }
    // Airbend: "airbend target <type> <mana_cost>" → GrantCastingPermission(ExileWithAltCost)
    if let Some((_, original_rest)) =
        nom_on_lower(text, lower, |input| value((), tag("airbend ")).parse(input))
    {
        let (target_text, _) = super::strip_optional_target_prefix(original_rest);
        let (target, after_target) = parse_target(target_text);
        let cost = parse_mana_symbols(after_target.trim_start())
            .map(|(c, _)| c)
            .unwrap_or(crate::types::mana::ManaCost::Cost {
                generic: 2,
                shards: vec![],
            });
        return Some(TargetedImperativeAst::Airbend { target, cost });
    }
    None
}

pub(super) fn lower_targeted_action_ast(ast: TargetedImperativeAst) -> Effect {
    match ast {
        TargetedImperativeAst::Tap { target } => Effect::Tap { target },
        TargetedImperativeAst::Untap { target } => Effect::Untap { target },
        TargetedImperativeAst::TapAll { target } => Effect::TapAll { target },
        TargetedImperativeAst::UntapAll { target } => Effect::UntapAll { target },
        TargetedImperativeAst::Sacrifice { target, count } => Effect::Sacrifice { target, count },
        // CR 701.9b + CR 608.2d: Lower the AST `up_to: bool` into the typed
        // `count: QuantityExpr::UpTo { max }` wrapper.
        TargetedImperativeAst::Discard {
            count,
            random,
            up_to,
            unless_filter,
            filter,
        } => Effect::Discard {
            count: if up_to {
                QuantityExpr::up_to(count)
            } else {
                count
            },
            // CR 701.9a: "Discard" with no subject defaults to the controller.
            // Subject injection overrides this for "target player discards" patterns.
            target: TargetFilter::Controller,
            random,
            unless_filter,
            filter,
        },
        // CR 701.9a: Back-reference discard — "discard that card" / "discard those
        // cards" — discards specific cards via ParentTarget binding. Count is
        // implicit (1 per parent-affected ID; the runtime expands ParentTarget
        // into the full set at rebind time).
        TargetedImperativeAst::DiscardCard { target } => Effect::DiscardCard { count: 1, target },
        TargetedImperativeAst::Return { target } => Effect::Bounce {
            target,
            destination: None,
        },
        // CR 400.7 + CR 611.2c: "Return all/each [filter]" mass-bounce — the
        // resolver iterates every matching permanent. Class filter is preserved
        // as-is; single-object refs (SelfRef / TriggeringSource / AttachedTo /
        // ParentTarget) cannot reach this AST variant because the bare
        // `tag("return ")` arm above handles those.
        TargetedImperativeAst::ReturnAll { target } => Effect::BounceAll {
            target,
            destination: None,
        },
        // CR 400.7: Return to battlefield is a zone change, not a bounce.
        TargetedImperativeAst::ReturnToBattlefield {
            target,
            origin,
            enter_transformed,
            under_your_control,
            enter_tapped,
            enter_with_counters,
        } => Effect::ChangeZone {
            origin,
            destination: Zone::Battlefield,
            target,
            owner_library: false,
            enter_transformed,
            under_your_control,
            enter_tapped,
            enters_attacking: false,
            up_to: false,
            enter_with_counters,
        },
        // CR 400.6: Return to a non-hand, non-battlefield zone (graveyard, library).
        TargetedImperativeAst::ReturnToZone {
            target,
            origin,
            destination,
        } => Effect::ChangeZone {
            origin,
            destination,
            target,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },
        TargetedImperativeAst::Fight { target } => Effect::Fight {
            target,
            subject: TargetFilter::SelfRef,
        },
        TargetedImperativeAst::GainControl { target } => Effect::GainControl { target },
        TargetedImperativeAst::ControlNextTurn {
            target,
            grant_extra_turn_after,
        } => Effect::ControlNextTurn {
            target,
            grant_extra_turn_after,
        },
        TargetedImperativeAst::Earthbend {
            target,
            power,
            toughness,
        } => Effect::Animate {
            power: Some(power),
            toughness: Some(toughness),
            types: vec!["Creature".to_string()],
            remove_types: vec![],
            target,
            keywords: vec![crate::types::keywords::Keyword::Haste],
        },
        TargetedImperativeAst::Airbend { target, cost } => Effect::GrantCastingPermission {
            permission: crate::types::ability::CastingPermission::ExileWithAltCost {
                cost,
                cast_transformed: false,
                constraint: None,
            },
            target,
            grantee: Default::default(),
        },
        TargetedImperativeAst::ZoneCounterProxy(ast) => lower_zone_counter_ast(*ast),
    }
}

/// CR 400.7 + CR 701.23 + CR 701.24: Recognize the multi-zone same-name exile
/// pattern used by Deadly Cover-Up and the Lost Legacy class.
///
/// The matched grammar is, in BNF-like form:
///
/// ```text
/// "search " <possessive>
///     ("graveyard, hand, and library" | <permutation>)
///     " for " ("any number of cards" | "all cards" | "a card")
///     " with that name and exile them"
/// ```
///
/// Returns `Some(())` on match — the lowering step constructs the
/// `Effect::ChangeZoneAll` directly (multi-zone origin + filter + destination
/// are fixed by the matched pattern). Returns `None` for any other shape so
/// the regular library-search branch can run.
fn try_parse_multi_zone_same_name_exile(lower: &str) -> Option<()> {
    fn run(input: &str) -> Result<(&str, ()), nom::Err<VerboseError<&str>>> {
        // search <possessive> graveyard, hand, and library
        let (input, _) = tag::<_, _, VerboseError<&str>>("search ").parse(input)?;
        let (input, _) = alt((
            tag::<_, _, VerboseError<&str>>("its owner's "),
            tag("their "),
            tag("that player's "),
            tag("target player's "),
            tag("target opponent's "),
            tag("an opponent's "),
            tag("your "),
        ))
        .parse(input)?;
        let (input, _) = alt((
            tag::<_, _, VerboseError<&str>>("graveyard, hand, and library"),
            tag("graveyard, hand and library"),
            tag("graveyard, library, and hand"),
            tag("hand, graveyard, and library"),
            tag("library, graveyard, and hand"),
        ))
        .parse(input)?;
        // for [any number of] cards with that name and exile them
        let (input, _) = tag::<_, _, VerboseError<&str>>(" for ").parse(input)?;
        let (input, _) = alt((
            value((), tag::<_, _, VerboseError<&str>>("any number of cards")),
            value((), tag("all cards")),
            value((), tag("a card")),
        ))
        .parse(input)?;
        // Match the trailing same-name suffix in either of two synonymous forms:
        //   " with that name and exile them"            (Deadly Cover-Up, Lost Legacy)
        //   " with the same name as that card and exile them"  (Surgical Extraction)
        let (input, _) = alt((
            value(
                (),
                tag::<_, _, VerboseError<&str>>(" with that name and exile them"),
            ),
            value((), tag(" with the same name as that card and exile them")),
        ))
        .parse(input)?;
        Ok((input, ()))
    }
    run(lower).ok().map(|_| ())
}

pub(super) fn parse_search_and_creation_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<SearchCreationImperativeAst> {
    if let Some((_, _)) = nom_on_lower(text, lower, |input| value((), tag("seek ")).parse(input)) {
        let details = super::parse_seek_details(lower, ctx);
        return Some(SearchCreationImperativeAst::Seek {
            filter: details.filter,
            count: details.count,
            destination: details.destination,
            enter_tapped: details.enter_tapped,
        });
    }
    // CR 400.7 + CR 701.23 + CR 701.24: "search [possessive] graveyard, hand,
    // and library for [filter] and exile them" — multi-zone exile of every card
    // matching the filter. Recognized before the single-zone library search
    // because both patterns share the "search " prefix; multi-zone wins on match.
    if try_parse_multi_zone_same_name_exile(lower).is_some() {
        return Some(SearchCreationImperativeAst::MultiZoneSameNameExile);
    }
    if starts_with_possessive(lower, "search", "library")
        || nom_on_lower(lower, lower, |i| {
            alt((
                value((), tag("search target opponent's library")),
                value((), tag("search target player's library")),
                value((), tag("search an opponent's library")),
            ))
            .parse(i)
        })
        .is_some()
    {
        let details = super::parse_search_library_details(lower, ctx);
        return Some(SearchCreationImperativeAst::SearchLibrary {
            filter: details.filter,
            count: details.count,
            reveal: details.reveal,
            target_player: details.target_player,
            up_to: details.up_to,
            selection_constraint: details.selection_constraint,
            reference_target: details.reference_target,
            extra_filters: details.extra_filters,
            multi_destination: details.multi_destination,
            multi_enter_tapped: details.multi_enter_tapped,
        });
    }
    // CR 701.16a + CR 701.20a: "look at the top N" (private) and "reveal the top N" (public)
    // both produce Dig — the reveal flag distinguishes visibility semantics.
    if let Some((reveal, rest)) = nom_on_lower(text, lower, |input| {
        alt((
            value(false, tag("look at the top ")),
            value(true, tag("reveal the top ")),
            value(true, tag("reveals the top ")),
        ))
        .parse(input)
    }) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        // Try numeric count first ("three cards"), then "x" as a variable
        // resolved later by apply_where_x_effect_expression.
        let count = if let Ok((_, n)) = nom_primitives::parse_number.parse(rest_lower) {
            QuantityExpr::Fixed { value: n as i32 }
        } else if tag::<_, _, VerboseError<&str>>("x")
            .parse(rest_lower)
            .is_ok()
        {
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            }
        } else {
            QuantityExpr::Fixed { value: 1 }
        };
        return Some(SearchCreationImperativeAst::Dig { count, reveal });
    }
    // CR 701.16a: "look at that many cards from the top of your library" — variable-count dig
    // where "that many" references the result of a previous effect (e.g., damage dealt).
    if let Some((reveal, _)) = nom_on_lower(text, lower, |input| {
        alt((
            value(
                false,
                tag("look at that many cards from the top of your library"),
            ),
            value(
                true,
                tag("reveal that many cards from the top of your library"),
            ),
        ))
        .parse(input)
    }) {
        let count = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        return Some(SearchCreationImperativeAst::Dig { count, reveal });
    }
    if let Some((_, _)) = nom_on_lower(text, lower, |input| value((), tag("create ")).parse(input))
    {
        return match try_parse_token(lower, text, ctx) {
            Some(Effect::CopyTokenOf {
                target,
                enters_attacking,
                tapped,
                extra_keywords,
                additional_modifications,
                ..
            }) => Some(SearchCreationImperativeAst::CopyTokenOf {
                target,
                enters_attacking,
                tapped,
                extra_keywords,
                additional_modifications,
            }),
            Some(Effect::Token {
                name,
                power,
                toughness,
                types,
                colors,
                keywords,
                tapped,
                count,
                attach_to,
                static_abilities,
                enters_attacking,
                ..
            }) => Some(SearchCreationImperativeAst::Token {
                token: Box::new(TokenDescription {
                    name,
                    power: Some(power),
                    toughness: Some(toughness),
                    types,
                    colors,
                    keywords,
                    tapped,
                    count,
                    attach_to,
                    static_abilities,
                    enters_attacking,
                }),
            }),
            _ => None,
        };
    }
    None
}

pub(super) fn lower_search_and_creation_ast(ast: SearchCreationImperativeAst) -> Effect {
    match ast {
        SearchCreationImperativeAst::SearchLibrary {
            filter,
            count,
            reveal,
            target_player,
            up_to,
            selection_constraint,
            reference_target: _,
            // Extras are consumed in `lower_imperative_family_ast` via
            // `lower_multi_filter_search_library`, which builds a chained
            // `ParsedEffectClause`. At this bare-Effect lowering site, multiple
            // filters collapse to the primary — but that path is unreachable
            // for multi-filter searches because the family-level lowering
            // intercepts them first.
            extra_filters: _,
            multi_destination: _,
            multi_enter_tapped: _,
        } => Effect::SearchLibrary {
            filter,
            // CR 107.1c + CR 701.23d: Lower the AST `up_to: bool` into the
            // typed `count: QuantityExpr::UpTo { max }` wrapper.
            count: if up_to {
                QuantityExpr::up_to(count)
            } else {
                count
            },
            reveal,
            target_player,
            selection_constraint,
        },
        SearchCreationImperativeAst::Dig { count, reveal } => Effect::Dig {
            count,
            destination: None,
            keep_count: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: None,
            reveal,
        },
        SearchCreationImperativeAst::CopyTokenOf {
            target,
            enters_attacking,
            tapped,
            extra_keywords,
            additional_modifications,
        } => Effect::CopyTokenOf {
            target,
            enters_attacking,
            tapped,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords,
            additional_modifications,
        },
        SearchCreationImperativeAst::Token { token } => Effect::Token {
            name: token.name,
            power: token.power.unwrap_or(PtValue::Fixed(0)),
            toughness: token.toughness.unwrap_or(PtValue::Fixed(0)),
            types: token.types,
            colors: token.colors,
            keywords: token.keywords,
            tapped: token.tapped,
            count: token.count,
            owner: TargetFilter::Controller,
            attach_to: token.attach_to,
            enters_attacking: token.enters_attacking,
            supertypes: vec![],
            static_abilities: token.static_abilities,
            enter_with_counters: vec![],
        },
        SearchCreationImperativeAst::Seek {
            filter,
            count,
            destination,
            enter_tapped,
        } => Effect::Seek {
            filter,
            count,
            destination,
            enter_tapped,
        },
        // CR 400.7 + CR 701.23 + CR 701.24: Multi-zone same-name exile.
        // The target filter encodes both the zone union (graveyard, hand,
        // library) via `InAnyZone` and the name match against the parent
        // target via `SameNameAsParentTarget`. The ChangeZoneAll resolver
        // reads multi-zone origins from the filter and per-object zone-of-
        // origin to track hand-origin exiles for the downstream draw count.
        SearchCreationImperativeAst::MultiZoneSameNameExile => Effect::ChangeZoneAll {
            origin: None,
            destination: Zone::Exile,
            target: TargetFilter::Typed(TypedFilter::default().properties(vec![
                crate::types::ability::FilterProp::InAnyZone {
                    zones: vec![Zone::Graveyard, Zone::Hand, Zone::Library],
                },
                crate::types::ability::FilterProp::SameNameAsParentTarget,
            ])),
        },
    }
}

pub(super) fn parse_hand_reveal_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<HandRevealImperativeAst> {
    if nom_on_lower(text, lower, |input| value((), tag("look at ")).parse(input)).is_some()
        && nom_primitives::scan_contains(lower, "hand")
    {
        if contains_possessive(lower, "look at", "hand") {
            // CR 603.7c: "that player's hand" / "their hand" resolves to the player
            // from the triggering event or prior instruction context.
            let target = if nom_primitives::scan_contains(lower, "that player's hand")
                || nom_primitives::scan_contains(lower, "their hand")
            {
                TargetFilter::TriggeringPlayer
            } else {
                ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
                    context: "unrecognized look-at target".into(),
                    text: lower.trim().into(),
                    line_index: 0,
                });
                TargetFilter::Any
            };
            return Some(HandRevealImperativeAst::LookAt { target });
        }

        let (_, after_look_at) =
            nom_on_lower(text, lower, |input| value((), tag("look at ")).parse(input))?;
        let (target, _) = parse_target(after_look_at);
        return Some(HandRevealImperativeAst::LookAt { target });
    }

    let (_, after_reveal) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("reveal "), tag("reveals ")))).parse(input)
    })?;

    // CR 701.20a: Back-reference reveal — "reveal it" / "reveal that card" /
    // "reveal those cards" — reveals a specific card identified by the parent
    // effect's affected IDs. Common in "look at top → reveal it" sequences
    // (Frost Augur, Archghoul of Thraben, Leaf-Crowned Elder).
    let after_reveal_lower = &lower[lower.len() - after_reveal.len()..];
    if alt((
        tag::<_, _, VerboseError<&str>>("it"),
        tag("that card"),
        tag("those cards"),
    ))
    .parse(after_reveal_lower)
    .is_ok()
    {
        return Some(HandRevealImperativeAst::RevealBackRef);
    }

    // CR 701.20a: "reveals a number of cards from their hand equal to X"
    if nom_primitives::scan_contains(lower, "hand")
        && nom_primitives::scan_contains(lower, "equal to ")
    {
        if let Some((_, qty_text)) = lower.split_once("equal to ") {
            let qty_text = qty_text.trim_end_matches('.');
            if let Some(qty) = super::super::oracle_quantity::parse_quantity_ref(qty_text) {
                return Some(HandRevealImperativeAst::RevealPartial {
                    count: crate::types::ability::QuantityExpr::Ref { qty },
                });
            }
        }
    }

    // "reveal the top N" is now handled by parse_search_and_creation_ast → Dig path.
    // This function only handles hand-related reveals.

    if nom_primitives::scan_contains(lower, "hand") {
        return Some(HandRevealImperativeAst::RevealAll);
    }

    None
}

pub(super) fn lower_hand_reveal_ast(ast: HandRevealImperativeAst) -> Effect {
    match ast {
        HandRevealImperativeAst::LookAt { target } => Effect::RevealHand {
            target,
            card_filter: TargetFilter::None,
            count: None,
            choice_optional: false,
        },
        HandRevealImperativeAst::RevealAll => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter: TargetFilter::None,
            count: None,
            choice_optional: false,
        },
        HandRevealImperativeAst::RevealPartial { count } => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter: TargetFilter::None,
            count: Some(count),
            choice_optional: false,
        },
        // CR 701.20a: Back-reference reveal — distinct from RevealHand (zone-wide).
        // ParentTarget binds at runtime to the parent ability's affected IDs.
        HandRevealImperativeAst::RevealBackRef => Effect::Reveal {
            target: TargetFilter::ParentTarget,
        },
    }
}

pub(super) fn parse_choose_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ChooseImperativeAst> {
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("choose ")).parse(input))
    {
        let rest_lower = &lower[lower.len() - rest.len()..];

        // CR 101.4 + CR 701.21a: "choose from among ... an artifact, a creature, ..."
        // or "choose an artifact, a creature, ... from among ..."
        // Must be checked before is_choose_as_targeting since these are NOT targeting.
        if let Some(ast) = parse_category_and_sacrifice_rest(rest_lower) {
            return Some(ast);
        }

        if super::is_choose_as_targeting(rest_lower) {
            // CR 115.1c + CR 601.2c: "Choose target X and target Y" declares
            // two independent target slots on the same activated/triggered
            // ability. Detect the compound "target ... and target ..." shape
            // BEFORE falling through to `parse_effect` (which collapses
            // target slot B into the surrounding effect text and yields a
            // single-target Reparse) so the second slot (e.g., Goblin
            // Welder's "artifact card in that player's graveyard") is
            // preserved instead of being silently dropped.
            if let Some(ast) = try_parse_two_targets(rest) {
                return Some(ast);
            }
            let inner = super::parse_effect(rest);
            if !matches!(inner, Effect::Unimplemented { .. }) {
                return Some(ChooseImperativeAst::Reparse {
                    text: rest.to_string(),
                });
            }
            let (target, _) = parse_target(rest);
            return Some(ChooseImperativeAst::TargetOnly { target });
        }
    }

    if let Some(choice_type) = super::try_parse_named_choice(lower) {
        return Some(ChooseImperativeAst::NamedChoice { choice_type });
    }

    if nom_on_lower(text, lower, |input| value((), tag("choose ")).parse(input)).is_some()
        && nom_primitives::scan_contains(lower, "card from it")
    {
        let choice_optional = nom_on_lower(text, lower, |input| {
            value((), tag("you may choose ")).parse(input)
        })
        .is_some();
        return Some(ChooseImperativeAst::RevealHandFilter {
            card_filter: super::parse_choose_filter(lower, ctx),
            choice_optional,
        });
    }

    // "choose N of them/those [cards]" / "you choose N of those cards" /
    // "an opponent chooses N of them" — anaphoric reference to a previously
    // revealed/exiled set, producing ChooseFromZone.
    if let Some((count, chooser)) = parse_choose_anaphoric(lower) {
        return Some(ChooseImperativeAst::FromTrackedSet { count, chooser });
    }

    None
}

/// CR 115.1c + CR 601.2c + CR 608.2c: Detect "target X and target Y" wording
/// after a "Choose " prefix and split it into two independent target slots.
///
/// CR 115.1c: "An activated ability is targeted if it identifies something it
/// will affect by using the phrase 'target [something]' …" — both halves are
/// part of the same activated ability.
///
/// CR 601.2c: "If the spell uses the word 'target' in multiple places, the
/// same object or player can be chosen once for each instance of the word
/// 'target' (as long as it fits the targeting criteria)."
///
/// Strategy: scan the lowercased text for an "and target " (or "and another
/// target ") connector at a word boundary using nom combinators. If present,
/// split the text there and run `parse_target` independently on each side —
/// the prefix becomes slot A's filter, the suffix becomes slot B's. This
/// keeps the second slot intact even when `parse_target` on the prefix
/// stops short of "a player controls" (an unrecognized controller suffix
/// today): the connector is anchored on "and target", not on the precise
/// length of slot A's filter.
///
/// The combinator-based "and target " split also rejects non-target
/// continuations ("target creature and put a counter on it") that would
/// otherwise look like compound targeting.
///
/// Returns `None` when the connector is absent, when either target parses
/// as `TargetFilter::Any` (failed extraction), or when the prefix isn't a
/// targeting phrase (`is_choose_as_targeting`-style check) — caller handles
/// the single-target fallback.
fn try_parse_two_targets(rest: &str) -> Option<ChooseImperativeAst> {
    type E<'a> = VerboseError<&'a str>;

    // CR 601.2c connector parser: "and target " or "and another target ".
    // `scan_split_at_phrase` advances at word boundaries (jumping past each
    // space), so the connector body itself is matched without a leading
    // space — the word boundary is enforced by the scan loop. Trailing
    // space is required so the next character is the start of the second
    // target's type/quantity phrase.
    fn parse_connector(input: &str) -> nom::IResult<&str, (), E<'_>> {
        value((), alt((tag("and target "), tag("and another target ")))).parse(input)
    }

    let lower = rest.to_ascii_lowercase();
    let (lower_prefix, lower_match_start) =
        nom_primitives::scan_split_at_phrase(lower.as_str(), parse_connector)?;

    // Map both the prefix and the match-start back to original-case slices
    // so `parse_target` operates on unmodified text. The prefix ends at the
    // word boundary just before "and target …"; the trailing space (if any)
    // is part of the prefix.
    let prefix_orig = &rest[..lower_prefix.len()];
    let match_start_orig = &rest[rest.len() - lower_match_start.len()..];

    // CR 115.1c slot A: the prefix must be a targeting phrase. `parse_target`
    // returning `Any` means "no recognized target" — we refuse to split.
    let (target_a, _rem_a) = parse_target(prefix_orig.trim_end());
    if matches!(target_a, TargetFilter::Any) {
        return None;
    }

    // CR 115.1c slot B: skip the leading "and " on the matched connector
    // and parse the second target. `tag("and ").parse(input)` returns
    // `(remainder, matched)` so we bind the first element.
    let (after_and_orig, _) = tag::<_, _, E>("and ").parse(match_start_orig).ok()?;
    let (target_b, _rem_b) = parse_target(after_and_orig);
    if matches!(target_b, TargetFilter::Any) {
        return None;
    }

    Some(ChooseImperativeAst::TwoTargets { target_a, target_b })
}

/// Parse anaphoric "choose N of them/those [cards]" patterns using nom combinators.
/// Returns (count, chooser) if the pattern matches.
fn parse_choose_anaphoric(lower: &str) -> Option<(u32, Chooser)> {
    type E<'a> = VerboseError<&'a str>;

    // Determine chooser from prefix: "an opponent chooses" / "target opponent chooses" → Opponent,
    // "you choose" / bare "choose" → Controller.
    let (rest, chooser) = alt((
        value(
            Chooser::Opponent,
            alt((
                tag::<_, _, E>("an opponent chooses "),
                tag("target opponent chooses "),
            )),
        ),
        value(
            Chooser::Controller,
            alt((tag::<_, _, E>("you choose "), tag("choose "))),
        ),
    ))
    .parse(lower)
    .ok()?;

    // Optional "up to " prefix.
    let rest = tag::<_, _, E>("up to ")
        .parse(rest)
        .map(|(r, _)| r)
        .unwrap_or(rest);

    // Parse count (one/two/three/N).
    let (rest, count) = nom_primitives::parse_number.parse(rest).ok()?;

    // Must be followed by " of them" or " of those" (optionally with trailing type noun).
    let _ = alt((tag::<_, _, E>(" of them"), tag(" of those")))
        .parse(rest)
        .ok()?;

    Some((count, chooser))
}

/// Public entry for Tragic Arrogance-style patterns where the chooser_scope is ControllerForAll.
/// Called from `parse_effect_clause` when "for each player, you choose " prefix is detected.
pub(super) fn parse_category_and_sacrifice_rest_pub(
    rest_lower: &str,
) -> Option<ChooseImperativeAst> {
    parse_category_and_sacrifice_rest(rest_lower).map(|ast| match ast {
        ChooseImperativeAst::CategoryAndSacrificeRest { categories, .. } => {
            ChooseImperativeAst::CategoryAndSacrificeRest {
                categories,
                chooser_scope: CategoryChooserScope::ControllerForAll,
            }
        }
        other => other,
    })
}

/// CR 101.4 + CR 701.21a: Parse the "from among ... an artifact, a creature, ..."
/// or "an artifact, a creature, ... from among ..." pattern after "choose " has been stripped.
///
/// Handles two word orders:
/// 1. "from among the permanents they control an artifact, a creature, ..." (Cataclysm)
/// 2. "an artifact, a creature, ... from among ..." (Cataclysmic Gearhulk)
///
/// Parser structure (nom combinators):
/// - `tag("from among")` detects pattern 1
/// - `take_until("from among")` + category parsing for pattern 2
/// - Category list: `parse_category_item` composed with comma + "and" separator
fn parse_category_and_sacrifice_rest(rest_lower: &str) -> Option<ChooseImperativeAst> {
    type E<'a> = VerboseError<&'a str>;

    // Pattern 1: "from among the permanents [they/that player] control[s] an artifact, ..."
    if let Ok((after_from_among, _)) = tag::<_, _, E>("from among ").parse(rest_lower) {
        // Skip past "the permanents they control" / "the permanents that player controls"
        // to find the category list.
        let categories_text = skip_permanent_clause(after_from_among)?;
        let categories = parse_category_list(categories_text)?;
        return Some(ChooseImperativeAst::CategoryAndSacrificeRest {
            categories,
            chooser_scope: CategoryChooserScope::EachPlayerSelf,
        });
    }

    // Pattern 2: "an artifact, a creature, ... from among [the nonland] permanents they control"
    if let Ok((_, before_from)) = take_until::<_, _, E>("from among").parse(rest_lower) {
        let categories = parse_category_list(before_from.trim())?;
        return Some(ChooseImperativeAst::CategoryAndSacrificeRest {
            categories,
            chooser_scope: CategoryChooserScope::EachPlayerSelf,
        });
    }

    None
}

/// Skip past "the permanents they control" / "the [nonland] permanents that player controls"
/// clauses to find the category list that follows.
fn skip_permanent_clause(input: &str) -> Option<&str> {
    type E<'a> = VerboseError<&'a str>;

    // "the permanents they control " / "the permanents that player controls "
    // / "the nonland permanents they control "
    let (rest, _) = tag::<_, _, E>("the ").parse(input).ok()?;

    // Optional "nonland " modifier
    let rest = tag::<_, _, E>("nonland ")
        .parse(rest)
        .map(|(r, _)| r)
        .unwrap_or(rest);

    let (rest, _) = tag::<_, _, E>("permanents ").parse(rest).ok()?;

    // "they control" / "that player controls"
    let rest = if let Ok((r, _)) = tag::<_, _, E>("they control").parse(rest) {
        r
    } else if let Ok((r, _)) = tag::<_, _, E>("that player controls").parse(rest) {
        r
    } else {
        return None;
    };

    // Strip optional trailing space/comma
    let rest = rest.trim_start_matches(' ');
    if rest.is_empty() {
        return None;
    }
    Some(rest)
}

/// Parse a comma-separated category list: "an artifact, a creature, an enchantment, and a land"
/// Uses nom combinators for each category item.
fn parse_category_list(input: &str) -> Option<Vec<CoreType>> {
    type E<'a> = VerboseError<&'a str>;

    let mut categories = Vec::new();
    let mut remaining = input.trim();

    loop {
        // Strip optional leading ", " or ", and " or "and "
        if let Ok((r, _)) = tag::<_, _, E>(", and ").parse(remaining) {
            remaining = r;
        } else if let Ok((r, _)) = tag::<_, _, E>(", ").parse(remaining) {
            remaining = r;
        } else if let Ok((r, _)) = tag::<_, _, E>("and ").parse(remaining) {
            remaining = r;
        }

        // Parse article + type: "an artifact" / "a creature" / "a land" / "a planeswalker" / "an enchantment"
        let (after_article, _) = alt((tag::<_, _, E>("an "), tag("a ")))
            .parse(remaining)
            .ok()?;

        let (rest, core_type) = parse_core_type_name(after_article)?;
        categories.push(core_type);
        remaining = rest.trim();

        if remaining.is_empty()
            || tag::<_, _, E>(", then ").parse(remaining).is_ok()
            || tag::<_, _, E>(". then ").parse(remaining).is_ok()
            || tag::<_, _, E>("from among").parse(remaining).is_ok()
        {
            break;
        }
    }

    if categories.is_empty() {
        return None;
    }
    Some(categories)
}

/// Parse a core type name from lowercase text using nom combinators.
fn parse_core_type_name(input: &str) -> Option<(&str, CoreType)> {
    type E<'a> = VerboseError<&'a str>;

    // Ordered longest-first to prevent prefix collisions.
    alt((
        value(CoreType::Planeswalker, tag::<_, _, E>("planeswalker")),
        value(CoreType::Enchantment, tag("enchantment")),
        value(CoreType::Artifact, tag("artifact")),
        value(CoreType::Creature, tag("creature")),
        value(CoreType::Land, tag("land")),
    ))
    .parse(input)
    .ok()
}

pub(super) fn lower_choose_ast(ast: ChooseImperativeAst) -> Effect {
    match ast {
        ChooseImperativeAst::TargetOnly { target } => Effect::TargetOnly { target },
        ChooseImperativeAst::Reparse { text } => super::parse_effect(&text),
        ChooseImperativeAst::NamedChoice { choice_type } => Effect::Choose {
            // CR 201.3 / CR 113.6: "With the chosen name" static/trigger filters
            // (Petrified Hamlet, Cheering Fanatic) resolve against the source
            // object's `chosen_attributes`. CardName choices must persist so
            // those later references find the bound name. CreatureType choices
            // also persist for chained filters such as "that aren't of the chosen type."
            persist: matches!(choice_type, ChoiceType::CardName | ChoiceType::CreatureType),
            choice_type,
        },
        ChooseImperativeAst::RevealHandFilter {
            card_filter,
            choice_optional,
        } => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter,
            count: None,
            choice_optional,
        },
        // CR 700.2: Anaphoric "choose N of them/those" → select from the tracked set
        // populated by the preceding effect (RevealTop, RevealHand, ExileTop, etc.).
        ChooseImperativeAst::FromTrackedSet { count, chooser } => Effect::ChooseFromZone {
            count,
            zone: Zone::Exile,
            chooser,
            up_to: false,
            constraint: None,
        },
        // CR 101.4 + CR 701.21a: Multi-category permanent selection + sacrifice rest.
        ChooseImperativeAst::CategoryAndSacrificeRest {
            categories,
            chooser_scope,
        } => Effect::ChooseAndSacrificeRest {
            categories,
            chooser_scope,
        },
        // CR 115.1c + CR 601.2c: Two independent target slots. The bare-Effect
        // lowering surfaces only the first slot — the chained `TargetOnly`
        // sub_ability for the second slot is attached by
        // `lower_imperative_family_ast`, which can express a `sub_ability`
        // chain (a single `Effect` cannot). Direct callers of
        // `lower_choose_ast` are restricted to single-effect contexts and do
        // not exercise this variant.
        ChooseImperativeAst::TwoTargets { target_a, .. } => Effect::TargetOnly { target: target_a },
    }
}

pub(super) fn parse_utility_imperative_ast(
    text: &str,
    lower: &str,
) -> Option<UtilityImperativeAst> {
    // Simple verb dispatch: prevent, regenerate, copy
    if let Some((verb, rest)) = nom_on_lower(text, lower, |input| {
        alt((
            value("prevent", tag("prevent ")),
            value("regenerate", tag("regenerate ")),
            value("copy", tag("copy ")),
        ))
        .parse(input)
    }) {
        return match verb {
            "prevent" => Some(UtilityImperativeAst::Prevent {
                text: text.to_string(),
            }),
            "regenerate" => Some(UtilityImperativeAst::Regenerate {
                text: text.to_string(),
            }),
            "copy" => {
                let (target, _rem) = parse_target(rest);
                #[cfg(debug_assertions)]
                assert_no_compound_remainder(_rem, text);
                Some(UtilityImperativeAst::Copy { target })
            }
            _ => unreachable!(),
        };
    }
    // CR 701.27 + CR 701.28: "transform" and "convert" are equivalent game actions.
    // CR 608.2k: bare object pronoun "it" / "itself" inside a self-trigger sub-ability
    // refers to the trigger source — Primal Amulet's "remove those counters and transform
    // it" is the canonical case. The literal-match list covers every form that resolves
    // to `SelfRef`; the dynamic `parse_target` fallback below picks up explicit targets.
    if matches!(
        lower,
        "transform"
            | "transform ~"
            | "transform it"
            | "transform itself"
            | "transform this"
            | "transform this creature"
            | "transform this permanent"
            | "transform this artifact"
            | "transform this land"
            | "convert"
            | "convert ~"
            | "convert it"
            | "convert itself"
            | "convert this"
            | "convert this creature"
            | "convert this permanent"
            | "convert this artifact"
            | "convert this land"
    ) {
        return Some(UtilityImperativeAst::Transform {
            target: TargetFilter::SelfRef,
        });
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("transform "), tag("convert ")))).parse(input)
    }) {
        let (target, _) = parse_target(rest);
        if !matches!(target, TargetFilter::Any) {
            return Some(UtilityImperativeAst::Transform { target });
        }
    }
    // CR 613.4d: "switch [target]'s power and toughness"
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("switch ")).parse(input))
    {
        let (target, rem) = parse_target(rest);
        // Consume "'s power and toughness" or " power and toughness" suffix
        let rem_lower = rem.to_lowercase();
        if tag::<_, _, VerboseError<&str>>("'s power and toughness")
            .parse(rem_lower.as_str())
            .is_ok()
            || tag::<_, _, VerboseError<&str>>(" power and toughness")
                .parse(rem_lower.as_str())
                .is_ok()
        {
            return Some(UtilityImperativeAst::SwitchPT { target });
        }
    }
    if let Some(((attachment, target), rem)) = nom_on_lower(text, lower, |input| {
        preceded(tag("attach "), parse_attach_anaphor_to_token).parse(input)
    }) {
        if rem.trim().is_empty() {
            return Some(UtilityImperativeAst::Attach { attachment, target });
        }
    }
    if let Some(((attachment_text, target_text), rem)) =
        nom_on_lower(text, lower, parse_explicit_targeted_attach)
    {
        if rem.trim().is_empty() {
            let (attachment, _attachment_rem) = parse_target(&attachment_text);
            let (target, _target_rem) = parse_attach_recipient(&target_text);
            #[cfg(debug_assertions)]
            assert_no_compound_remainder(_attachment_rem, text);
            #[cfg(debug_assertions)]
            assert_no_compound_remainder(_target_rem, text);
            return Some(UtilityImperativeAst::Attach { attachment, target });
        }
    }
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("attach ")).parse(input))
    {
        let tp = TextPair::new(text, lower);
        let after_to = tp.strip_after(" to ").map(|tp| tp.original).unwrap_or(rest);
        let (target, _rem) = parse_target(after_to);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(UtilityImperativeAst::Attach {
            attachment: TargetFilter::SelfRef,
            target,
        });
    }
    None
}

fn parse_explicit_targeted_attach(
    input: &str,
) -> nom::IResult<&str, (String, String), VerboseError<&str>> {
    let (input, _) = tag("attach ").parse(input)?;
    let (input, _) = opt(alt((
        tag("up to one "),
        tag("up to two "),
        tag("up to three "),
        tag("any number of "),
    )))
    .parse(input)?;
    let (input, attachment) = take_until(" to ").parse(input)?;
    let (input, _) = tag(" to ").parse(input)?;
    let (input, target) = rest.parse(input)?;
    Ok((input, (attachment.to_string(), target.to_string())))
}

fn parse_attach_recipient(text: &str) -> (TargetFilter, &str) {
    let (target, rest) = parse_target(text);
    if matches!(target, TargetFilter::ParentTarget) {
        let trimmed = text.trim_start();
        let lower = trimmed.to_lowercase();
        if matches!(lower.trim(), "her" | "him") {
            return (TargetFilter::SelfRef, &trimmed[lower.len()..]);
        }
    }
    (target, rest)
}

fn parse_attach_anaphor_to_token(
    input: &str,
) -> nom::IResult<&str, (TargetFilter, TargetFilter), VerboseError<&str>> {
    let (input, attachment) = alt((
        value(
            TargetFilter::TriggeringSource,
            alt((
                tag("it"),
                tag("that aura"),
                tag("that enchantment"),
                tag("that equipment"),
                tag("that permanent"),
            )),
        ),
        value(TargetFilter::SelfRef, tag("~")),
    ))
    .parse(input)?;
    let (input, _) = tag(" to ").parse(input)?;
    let (input, target) = value(
        TargetFilter::LastCreated,
        alt((
            tag("the token created this way"),
            tag("that token created this way"),
            tag("the token"),
            tag("that token"),
        )),
    )
    .parse(input)?;
    Ok((input, (attachment, target)))
}

pub(super) fn lower_utility_imperative_ast(ast: UtilityImperativeAst) -> Effect {
    match ast {
        UtilityImperativeAst::Prevent { text } => parse_prevent_effect(&text),
        UtilityImperativeAst::Regenerate { text } => {
            let lower = text.to_lowercase();
            let rest = tag::<_, _, VerboseError<&str>>("regenerate ")
                .parse(&*lower)
                .map(|(r, _)| r)
                .unwrap_or(&lower);
            let (target, _) = parse_target(rest);
            Effect::Regenerate { target }
        }
        UtilityImperativeAst::Copy { target } => Effect::CopySpell { target },
        UtilityImperativeAst::Transform { target } => Effect::Transform { target },
        UtilityImperativeAst::Attach { attachment, target } => {
            Effect::Attach { attachment, target }
        }
        // CR 613.4d: Switch power and toughness.
        UtilityImperativeAst::SwitchPT { target } => Effect::SwitchPT { target },
    }
}

/// CR 615: Parse "prevent" damage effects into `Effect::PreventDamage`.
///
/// Handles patterns like:
/// - "prevent the next N damage that would be dealt to any target this turn"
/// - "prevent all damage that would be dealt this turn"
/// - "prevent all combat damage that would be dealt this turn"
/// - "prevent the next N damage that would be dealt to target creature"
fn parse_prevent_effect(text: &str) -> Effect {
    let lower = text.to_lowercase();
    let rest = tag::<_, _, VerboseError<&str>>("prevent ")
        .parse(&*lower)
        .map(|(r, _)| r)
        .unwrap_or(&lower);

    // Determine scope: combat damage only vs all damage
    let scope = if nom_primitives::scan_contains(rest, "combat damage") {
        PreventionScope::CombatDamage
    } else {
        PreventionScope::AllDamage
    };

    // Determine amount: "all damage" vs "the next N damage"
    let amount = if tag::<_, _, VerboseError<&str>>("all ").parse(rest).is_ok() {
        PreventionAmount::All
    } else if let Ok((after_next, _)) = tag::<_, _, VerboseError<&str>>("the next ").parse(rest) {
        let n = nom_primitives::parse_number
            .parse(after_next)
            .map(|(_, n)| n)
            .unwrap_or(1);
        PreventionAmount::Next(n)
    } else {
        // Fallback: try to extract a number
        let n = nom_primitives::parse_number
            .parse(rest)
            .map(|(_, n)| n)
            .unwrap_or(1);
        PreventionAmount::Next(n)
    };

    // Determine target
    let target = if nom_primitives::scan_contains(rest, "any target") {
        TargetFilter::Any
    } else if nom_primitives::scan_contains(rest, "target creature")
        || nom_primitives::scan_contains(rest, "target permanent")
    {
        // Extract the target from the text
        let tp = TextPair::new(text, &lower);
        if let Ok((_, before)) = take_until::<_, _, VerboseError<&str>>("target ").parse(tp.lower) {
            let (_, from_target) = tp.split_at(before.len());
            let (t, _) = parse_target(from_target.original);
            t
        } else {
            TargetFilter::Any
        }
    } else if nom_primitives::scan_contains(rest, "to you")
        || nom_primitives::scan_contains(rest, "to its controller")
    {
        TargetFilter::Controller
    } else {
        // Default: "that would be dealt" with no specific target → Any
        TargetFilter::Any
    };

    Effect::PreventDamage {
        amount,
        target,
        scope,
        damage_source_filter: None,
    }
}

/// CR 113.3 + CR 604.1: Parse "gain `<quoted ability>`" / `"gain "<...>" until
/// end of turn"` in the imperative path. Handles inline ability grants like
/// `gain "When this creature dies, draw a card."` (Rabid Attack class) by
/// delegating to the existing `parse_quoted_ability_modifications` helper —
/// which already routes trigger-prefix quoted text to `GrantTrigger`,
/// keyword-form text to `AddKeyword`, and other ability text to
/// `GrantAbility`.
///
/// Returns `None` when the gain clause contains no quoted text — bare keyword
/// grants are handled by `try_parse_gain_keyword`. Designed as a fallback
/// after the bare-keyword path fails.
fn try_parse_gain_quoted_ability(text: &str) -> Option<Effect> {
    // Cheap pre-check: must contain at least one matched quote pair to be a
    // candidate. Avoids invoking the heavier modification parser on bare
    // keyword text.
    if !text.contains('"') {
        return None;
    }
    let (text_without_duration, duration) = super::strip_trailing_duration(text);
    let modifications = parse_quoted_ability_modifications(text_without_duration);
    if modifications.is_empty() {
        return None;
    }
    // CR 113.3a: Granted abilities last as long as the granting effect. For
    // sub_ability inline grants in pump-style spells the parent's UntilEndOfTurn
    // is the typical default; preserve any explicitly stripped duration.
    let duration = duration.or(Some(Duration::UntilEndOfTurn));
    Some(Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(modifications)
            .description(text.to_string())],
        duration,
        target: None,
    })
}

/// CR 702: Parse bare "gain [keyword]" / "gain [keyword] until end of turn"
/// in the imperative path. Handles "gain haste", "gain trample and haste",
/// "gain flying until end of turn", etc.
///
/// Reuses `parse_continuous_modifications` which already handles
/// "gain/gains [keyword]" via `extract_keyword_clause`.
fn try_parse_gain_keyword(text: &str) -> Option<Effect> {
    let (text_without_duration, duration) = super::strip_trailing_duration(text);
    let modifications = parse_continuous_modifications(text_without_duration);

    // Only accept if we got at least one AddKeyword or RemoveKeyword modification
    let has_keyword = modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::AddKeyword { .. }
                | ContinuousModification::RemoveKeyword { .. }
        )
    });
    if !has_keyword {
        return None;
    }

    // Default duration: UntilEndOfTurn for keyword granting sub-abilities
    let duration = duration.or(Some(Duration::UntilEndOfTurn));

    Some(Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(modifications)
            .description(text.to_string())],
        duration,
        target: None,
    })
}

pub(super) fn lower_imperative_ast(ast: ImperativeAst) -> Effect {
    match ast {
        ImperativeAst::Numeric(ast) => lower_numeric_imperative_ast(ast),
        ImperativeAst::Targeted(ast) => lower_targeted_action_ast(ast),
        ImperativeAst::SearchCreation(ast) => lower_search_and_creation_ast(ast),
        ImperativeAst::HandReveal(ast) => lower_hand_reveal_ast(ast),
        ImperativeAst::Choose(ast) => lower_choose_ast(ast),
        ImperativeAst::Utility(ast) => lower_utility_imperative_ast(ast),
    }
}

pub(super) fn parse_put_ast(text: &str, lower: &str) -> Option<PutImperativeAst> {
    tag::<_, _, VerboseError<&str>>("put ").parse(lower).ok()?;

    if let Ok((after, _)) = tag::<_, _, VerboseError<&str>>("put the top ").parse(lower) {
        if nom_primitives::scan_contains(lower, "graveyard") {
            let count = nom_primitives::parse_number
                .parse(after)
                .map(|(_, n)| n)
                .unwrap_or(1);
            return Some(PutImperativeAst::Mill { count });
        }
    }

    // CR 701.24g: "put X on top of Y's library" — specific position, no auto-shuffle.
    // Must check before try_parse_put_zone_change which would emit ChangeZone (auto-shuffles).
    // Only matches forms WITHOUT an explicit origin zone ("from your hand") — those
    // specify a real zone transfer and should go through try_parse_put_zone_change.
    if nom_primitives::scan_contains(lower, "on top of")
        && nom_primitives::scan_contains(lower, "library")
    {
        let has_origin = nom_primitives::scan_contains(lower, " from ");
        if !has_origin {
            return Some(PutImperativeAst::TopOfLibrary);
        }
    }

    // CR 701.24g: "put that card on top" / "put it on top" / "put them on top" —
    // abbreviated form used after "shuffle" in search-and-put-on-top tutors (41 cards).
    if lower.ends_with("on top") {
        return Some(PutImperativeAst::TopOfLibrary);
    }

    // CR 701.24g: "put X on the bottom of Y's library" — specific position without
    // explicit origin zone. Forms with "from" (e.g. "from your hand") go through
    // try_parse_put_zone_change for proper ChangeZone handling.
    if nom_primitives::scan_contains(lower, "on the bottom of")
        && nom_primitives::scan_contains(lower, "library")
    {
        let has_origin = nom_primitives::scan_contains(lower, " from ");
        if !has_origin {
            return Some(PutImperativeAst::BottomOfLibrary);
        }
    }

    // CR 701.24g: "put that card on the bottom" / "put it on the bottom" —
    // abbreviated form without "of Y's library".
    if lower.ends_with("on the bottom") {
        return Some(PutImperativeAst::BottomOfLibrary);
    }

    // CR 701.24g: "put X into Y's library Nth from the top" —
    // specific positional placement (God-Eternals, Approach, Bury in Books).
    if let Ok((_, before_from)) =
        take_until::<_, _, VerboseError<&str>>("from the top").parse(lower)
    {
        {
            // Look backwards from "from the top" to find the ordinal
            let before = before_from.trim_end();
            if let Some(last_space) = before.rfind(' ') {
                let ordinal_word = &before[last_space + 1..];
                if let Some((n, _)) = parse_ordinal(ordinal_word) {
                    return Some(PutImperativeAst::NthFromTop { n });
                }
            }
        }
    }

    if let Some(Effect::ChangeZone {
        origin,
        destination,
        target,
        under_your_control,
        enter_tapped,
        enters_attacking,
        enter_with_counters,
        ..
    }) = super::try_parse_put_zone_change(lower, text)
    {
        return Some(PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
            under_your_control,
            enter_tapped,
            enters_attacking,
            enter_with_counters,
        });
    }

    None
}

pub(super) fn lower_put_ast(ast: PutImperativeAst) -> Effect {
    match ast {
        PutImperativeAst::Mill { count } => Effect::Mill {
            count: QuantityExpr::Fixed {
                value: count as i32,
            },
            // CR 701.17a: "Put top N into graveyard" is self-mill.
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        },
        PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
            under_your_control,
            enter_tapped,
            enters_attacking,
            enter_with_counters,
        } => {
            // CR 610.3: Mass filters (ExiledBySource, TrackedSet) act on all matching
            // objects without individual targeting — use ChangeZoneAll.
            // ExiledBySource always originates from Exile regardless of inferred zone.
            // CR 122.1: ChangeZoneAll has no counter-stamping channel — those
            // patterns are single-target only in current Oracle text, so the
            // mass-filter branch deliberately drops `enter_with_counters`.
            if matches!(
                target,
                TargetFilter::ExiledBySource | TargetFilter::TrackedSet { .. }
            ) && enter_with_counters.is_empty()
            {
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Exile),
                    destination,
                    target,
                }
            } else {
                Effect::ChangeZone {
                    origin,
                    destination,
                    target,
                    owner_library: false,
                    enter_transformed: false,
                    under_your_control,
                    enter_tapped,
                    // CR 508.4: Propagated from the inline-tail patcher in
                    // `try_parse_put_zone_change` (Kaalia / Ilharg class).
                    enters_attacking,
                    up_to: false,
                    enter_with_counters,
                }
            }
        }
        // CR 701.24g: Place at a specific position — uses move_to_library_position,
        // not ChangeZone which auto-shuffles per CR 401.3. `count` defaults to
        // `Fixed(1)` here; the cardinality patcher in `oracle_effect/mod.rs`
        // upgrades it (and the target filter) by re-inspecting the imperative
        // text once the clause has been lowered.
        PutImperativeAst::TopOfLibrary => Effect::PutAtLibraryPosition {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::Top,
        },
        PutImperativeAst::BottomOfLibrary => Effect::PutAtLibraryPosition {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::Bottom,
        },
        PutImperativeAst::NthFromTop { n } => Effect::PutAtLibraryPosition {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::NthFromTop { n },
        },
    }
}

/// Parse "put that many {type} counter(s) on {target}" — dynamic counter count from event context.
/// CR 120.1: "that many" references the amount from the triggering event (e.g., damage dealt).
/// Produces PutCounter with count=0 as a sentinel for event-context resolution.
fn try_parse_that_many_counters(lower: &str, ctx: &mut ParseContext) -> Option<Effect> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("put that many ")
        .parse(lower)
        .ok()?;
    // Next word(s) are counter type: "+1/+1", "charge", "loyalty", etc.
    let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let raw_type = &rest[..type_end];
    let counter_type = super::counter::normalize_counter_type(raw_type);

    // Skip "counter" or "counters" keyword
    let after_type = rest[type_end..].trim_start();
    let after_counter = alt((tag::<_, _, VerboseError<&str>>("counters"), tag("counter")))
        .parse(after_type)
        .map(|(r, _)| r)
        .unwrap_or(after_type)
        .trim_start();

    // Parse target after "on"
    let target =
        if let Ok((on_rest, _)) = tag::<_, _, VerboseError<&str>>("on ").parse(after_counter) {
            if alt((tag::<_, _, VerboseError<&str>>("~"), tag("this ")))
                .parse(on_rest)
                .is_ok()
            {
                TargetFilter::SelfRef
            } else if alt((tag::<_, _, VerboseError<&str>>("it"), tag("itself")))
                .parse(on_rest)
                .is_ok()
            {
                // CR 608.2k: Bare pronoun — context-dependent
                resolve_it_pronoun(ctx)
            } else {
                let (t, _) = parse_target(on_rest);
                t
            }
        } else {
            TargetFilter::SelfRef
        };

    // CR 603.7c: "that many" — resolve from trigger event context at runtime.
    Some(Effect::PutCounter {
        counter_type,
        count: QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },
        target,
    })
}

/// CR 506.4: Parse "remove [target] from combat" patterns.
/// Matches: "remove it from combat", "remove ~ from combat",
/// "remove target [creature] from combat", "remove that creature from combat".
fn parse_remove_from_combat_ast(lower: &str, ctx: &mut ParseContext) -> Option<TargetFilter> {
    // Strip the "remove " prefix
    let (rest, _) = tag::<_, _, VerboseError<&str>>("remove ")
        .parse(lower)
        .ok()?;
    // Check that "from combat" appears in the remainder
    let from_combat_pos = rest.find("from combat")?;
    let subject = rest[..from_combat_pos].trim();
    // Resolve the subject to a target filter
    let target = match subject {
        "it" | "that creature" | "that land" | "that permanent" => TargetFilter::ParentTarget,
        "" => TargetFilter::SelfRef,
        _ => {
            // Try parsing as a target phrase (e.g., "target attacking creature you control")
            let (tf, _rest) = parse_target(subject);
            if matches!(tf, TargetFilter::Any) && !subject.starts_with("target") {
                // parse_target returns Any when it doesn't recognize the phrase —
                // bail out to avoid false matches.
                return None;
            }
            // structural: not dispatch — mirrors guard above for warning diagnostic
            if matches!(tf, TargetFilter::Any) && subject.starts_with("target") {
                ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
                    context: "'target' prefix but unrecognized filter".into(),
                    text: subject.into(),
                    line_index: 0,
                });
            }
            tf
        }
    };
    Some(target)
}

/// Parse a possessive determiner from a fixed set of MTG Oracle variants.
///
/// Accepts: "your", "their", "its owner's", "that player's". These are the possessives
/// that can precede a zone reference in a "shuffle X into Y" phrase.
fn parse_possessive_determiner(input: &str) -> nom::IResult<&str, (), VerboseError<&str>> {
    value(
        (),
        alt((
            tag("your"),
            tag("their"),
            tag("its owner's"),
            tag("that player's"),
        )),
    )
    .parse(input)
}

fn parse_shuffle_origin_zones(input: &str) -> nom::IResult<&str, Vec<Zone>, VerboseError<&str>> {
    alt((
        value(vec![Zone::Hand, Zone::Graveyard], tag("hand and graveyard")),
        value(vec![Zone::Graveyard, Zone::Hand], tag("graveyard and hand")),
        value(vec![Zone::Hand], tag("hand")),
        value(vec![Zone::Graveyard], tag("graveyard")),
        value(vec![Zone::Exile], tag("exile")),
    ))
    .parse(input)
}

/// Parse "shuffle [the cards {from|in}] {possessive} {zone-list} into
/// {possessive} library" and return the origin zones.
///
/// CR 400.6 + CR 701.24a: Recognizes whole-zone bulk moves like Whirlpool Drake's
/// "shuffle the cards from your hand into your library" and Midnight Clock's
/// "shuffle your hand and graveyard into your library". The origin-zone phrase
/// names every card in each listed zone — no targeting or filtering — so the
/// resulting AST lowers to `ChangeZoneAll` (not `ChangeZone`).
///
/// Supports zones: hand, graveyard, exile. Returns None for any other structure.
fn parse_mass_zones_to_library(lower: &str) -> Option<Vec<Zone>> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("shuffle ")
        .parse(lower)
        .ok()?;
    let (rest, _) = opt(preceded(
        tag::<_, _, VerboseError<&str>>("the cards "),
        alt((tag("from "), tag("in "))),
    ))
    .parse(rest)
    .ok()?;
    // "{possessive} "
    let (rest, _) = parse_possessive_determiner(rest).ok()?;
    let (rest, _) = tag::<_, _, VerboseError<&str>>(" ").parse(rest).ok()?;
    let (rest, origins) = parse_shuffle_origin_zones(rest).ok()?;
    // " into {possessive} library"
    let (rest, _) = tag::<_, _, VerboseError<&str>>(" into ").parse(rest).ok()?;
    let (rest, _) = parse_possessive_determiner(rest).ok()?;
    let (_rest, _) = tag::<_, _, VerboseError<&str>>(" library")
        .parse(rest)
        .ok()?;
    Some(origins)
}

pub(super) fn parse_shuffle_ast(text: &str, lower: &str) -> Option<ShuffleImperativeAst> {
    if matches!(
        lower,
        "shuffle" | "shuffles" | "then shuffle" | "then shuffles"
    ) {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Controller,
        });
    }
    // "shuffle the rest into your library" — the "rest" are already in the library
    // from a preceding dig/reveal effect; this is just a shuffle.
    if nom_primitives::scan_contains(lower, "shuffle the rest")
        || nom_primitives::scan_contains(lower, "shuffle them")
    {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Controller,
        });
    }
    if matches!(lower, "that player shuffles" | "target player shuffles") {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Player,
        });
    }
    if tag::<_, _, VerboseError<&str>>("shuffle")
        .parse(lower)
        .is_err()
        || !nom_primitives::scan_contains(lower, "library")
    {
        return None;
    }

    // "shuffle {possessive} library" — extract the possessive word to determine the target.
    // Only matches the exact form "shuffle your library" / "shuffle their library" etc.;
    // compound forms like "shuffle your graveyard into your library" fall through.
    if let Some(possessive) = tag::<_, _, VerboseError<&str>>("shuffle ")
        .parse(lower)
        .ok()
        .map(|(rest, _)| rest)
        .and_then(|s| s.strip_suffix(" library"))
    {
        let target = match possessive {
            "your" => Some(TargetFilter::Controller),
            "their" | "its owner's" | "that player's" => Some(TargetFilter::Player),
            _ => None,
        };
        if let Some(target) = target {
            return Some(ShuffleImperativeAst::ShuffleLibrary { target });
        }
    }
    // CR 701.24a + CR 400.3: "shuffle <pronoun> into <possessive> library" —
    // covers "shuffle it into its owner's library" (Cavalier cycle), "shuffle
    // ~ into your library", "shuffle that card into its owner's library"
    // (search-then-shuffle tutors), "shuffle them into their owners' libraries"
    // (compound subject). Both pronoun (it/them/that card/those cards/~) and
    // possessive (its owner's / their owner's / their owners' / your) are
    // classified via nom combinators so the lowered `ChangeZone` carries the
    // correct `target` (SelfRef vs ParentTarget) and `owner_library` flag.
    if contains_object_pronoun(lower, "shuffle", "into")
        || contains_object_pronoun(lower, "shuffles", "into")
    {
        // Pronoun classification. Walk word-boundaries, peel "shuffle"/
        // "shuffles" + " ", then alt() over the four object-pronoun variants.
        // "it" / "~" → SelfRef (singular, anaphoric to the source object);
        // "them" / "that card" / "those cards" → ParentTarget (refers to a
        // previously-bound target). The fall-through "SelfRef" arm only
        // engages when the outer `contains_object_pronoun` guard somehow
        // matched a pronoun the inner combinator didn't recognize — defensive
        // and also matches the existing "shuffle this creature into …" form.
        let target = nom_primitives::scan_at_word_boundaries(lower, |input| {
            let (rest, _) = alt((
                tag::<_, _, VerboseError<&str>>("shuffle "),
                tag("shuffles "),
            ))
            .parse(input)?;
            alt((
                value(TargetFilter::ParentTarget, tag("them")),
                value(TargetFilter::ParentTarget, tag("that card")),
                value(TargetFilter::ParentTarget, tag("those cards")),
                value(TargetFilter::SelfRef, tag("it")),
                value(TargetFilter::SelfRef, tag("~")),
            ))
            .parse(rest)
        })
        .unwrap_or(TargetFilter::SelfRef);
        // Library possessor. CR 400.3 routes the move to the card's *owner*
        // when the Oracle names a possessive that resolves to the owner —
        // "its owner's", "their owner's", "their owners'". Bare "their" /
        // "their library" is intentionally NOT treated as owner-routing:
        // "their" is ambiguous (controller vs owner vs plural antecedent)
        // and would mis-classify "each player shuffles their library".
        // "your library" leaves owner_library: false (the default).
        // TODO(CR 400.3): When `owner_library: true`, the `Shuffle` sub_ability
        // produced by `with_shuffle_sub_ability` still targets `Controller`,
        // so a stolen creature shuffles its current controller's library
        // instead of its owner's. Fixing this requires lifting `Effect::Shuffle`
        // to accept an owner-of-target binding (separate commit).
        let owner_library = nom_primitives::scan_at_word_boundaries(lower, |input| {
            alt((
                value((), tag::<_, _, VerboseError<&str>>("its owner's library")),
                value((), tag("their owner's library")),
                value((), tag("their owners' libraries")),
            ))
            .parse(input)
        })
        .is_some();
        return Some(ShuffleImperativeAst::ChangeZoneToLibrary {
            target,
            owner_library,
        });
    }
    // CR 701.24a + CR 400.6: "shuffle the cards from {possessive} {zone} into
    // {possessive} library" and "shuffle {possessive} hand and graveyard into
    // {possessive} library" — whole-zone mass move(s) + implicit shuffle.
    // Must run before the generic targeted-shuffle path below, which would otherwise
    // consume "the cards" as a `ParentTarget` pronoun.
    if let Some(origins) = parse_mass_zones_to_library(lower) {
        return Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origins });
    }
    if contains_possessive(lower, "shuffle", "graveyard") {
        return Some(ShuffleImperativeAst::ChangeZoneAllToLibrary {
            origins: vec![Zone::Graveyard],
        });
    }
    if contains_possessive(lower, "shuffle", "hand") {
        return Some(ShuffleImperativeAst::ChangeZoneAllToLibrary {
            origins: vec![Zone::Hand],
        });
    }
    // CR 701.24a: "shuffle target card from your graveyard into your library" —
    // targeted zone change (origin → library) + implicit shuffle.
    // Placed after possessive checks to avoid matching "shuffle your graveyard into library".
    if let Some((_, after_shuffle)) =
        nom_on_lower(text, lower, |input| value((), tag("shuffle ")).parse(input))
    {
        if nom_primitives::scan_contains(lower, "into")
            && nom_primitives::scan_contains(lower, "library")
            && nom_primitives::scan_contains(lower, "from")
        {
            let (target, _) = parse_target(after_shuffle);
            let origin = if nom_primitives::scan_contains(lower, "graveyard") {
                Some(Zone::Graveyard)
            } else if nom_primitives::scan_contains(lower, "from your hand") {
                Some(Zone::Hand)
            } else if nom_primitives::scan_contains(lower, "from exile") {
                Some(Zone::Exile)
            } else {
                None
            };
            return Some(ShuffleImperativeAst::TargetedChangeZoneToLibrary { target, origin });
        }
    }

    Some(ShuffleImperativeAst::Unimplemented {
        text: text.to_string(),
    })
}

/// CR 701.24a: Lower a shuffle AST into a `ParsedEffectClause`.
/// Compound forms ("shuffle X into library") produce a `ChangeZone` + `Shuffle` sub_ability
/// chain so the library is actually randomized after the zone move.
pub(super) fn lower_shuffle_ast(ast: ShuffleImperativeAst) -> ParsedEffectClause {
    match ast {
        ShuffleImperativeAst::ShuffleLibrary { target } => {
            parsed_clause(Effect::Shuffle { target })
        }
        ShuffleImperativeAst::ChangeZoneToLibrary {
            target,
            owner_library,
        } => {
            // CR 701.24a + CR 400.3: `target` and `owner_library` are
            // populated by `parse_shuffle_ast`'s combinator-based pronoun /
            // possessive classification. See the construction site for the
            // detection grammar and the TODO on the `Shuffle` sub-target.
            let effect = Effect::ChangeZone {
                origin: None,
                destination: Zone::Library,
                target,
                owner_library,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            };
            with_shuffle_sub_ability(effect)
        }
        ShuffleImperativeAst::ChangeZoneAllToLibrary { origins } => {
            // CR 400.6 + CR 400.3: "shuffle {possessive} {zone} into {possessive}
            // library" moves every card in the origin zone(s) owned by the
            // identified player. The sentinel `TargetFilter::Controller` is
            // later remapped to a concrete player by `inject_subject_target`
            // when a subject like "that player" precedes the shuffle phrase
            // (Jace's ultimate) — otherwise the resolver treats it as "the
            // ability controller's cards".
            lower_change_zone_all_to_library(origins)
        }
        // CR 701.24a: Targeted zone change to library with implicit shuffle sub_ability.
        ShuffleImperativeAst::TargetedChangeZoneToLibrary { target, origin } => {
            let effect = Effect::ChangeZone {
                origin,
                destination: Zone::Library,
                target,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            };
            with_shuffle_sub_ability(effect)
        }
        ShuffleImperativeAst::Unimplemented { text } => parsed_clause(Effect::Unimplemented {
            name: "shuffle".to_string(),
            description: Some(text),
        }),
    }
}

/// CR 122.1 + CR 608.2c: Lower a multi-typed counter list to a ParsedEffectClause
/// whose primary effect carries the resolved target and whose `sub_ability`
/// chain re-applies each remaining counter via `TargetFilter::ParentTarget`.
/// Because `ParentTarget` is a context-ref filter (see
/// `TargetFilter::is_context_ref`), the sub-ability chain does not surface
/// additional target-selection slots — the player chooses the target once
/// on the primary effect and every chained `PutCounter` inherits it.
pub(super) fn lower_put_counter_list(
    entries: Vec<(String, QuantityExpr)>,
    target: TargetFilter,
    multi_target: Option<MultiTargetSpec>,
) -> ParsedEffectClause {
    let mut iter = entries.into_iter();
    let (first_type, first_count) = iter
        .next()
        .expect("PutCounterList must have at least one entry");

    // Build the sub_ability chain right-to-left so each link owns the next.
    let mut sub_ability: Option<Box<AbilityDefinition>> = None;
    for (counter_type, count) in iter.collect::<Vec<_>>().into_iter().rev() {
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type,
                count,
                target: TargetFilter::ParentTarget,
            },
        );
        def.sub_ability = sub_ability;
        sub_ability = Some(Box::new(def));
    }

    let mut clause = parsed_clause(Effect::PutCounter {
        counter_type: first_type,
        count: first_count,
        target,
    });
    clause.sub_ability = sub_ability;
    clause.multi_target = multi_target;
    clause
}

/// CR 701.23a + CR 107.1: Lower a multi-filter library search ("a X card and
/// a Y card [and a Z card ...], put them onto the battlefield [tapped], then
/// shuffle") into a `ParsedEffectClause`. The result shape is an interleaved
/// chain of `SearchLibrary` and `ChangeZone` effects — one search per filter,
/// each followed by a move-to-destination for the found card — terminated by
/// the final `SearchLibrary`. The terminal `ChangeZone` for that last search
/// is added downstream by the sequence parser's intrinsic continuation (the
/// same path that handles single-filter searches), so the total resolved
/// chain is: `Search(f1) → ChangeZone → Search(f2) → ... → Search(fN) →
/// ChangeZone → [Shuffle]`.
///
/// Each search runs independently (CR 701.23 — search is a compound action
/// per filter) and shares the same `reveal` / `target_player` / `up_to`
/// semantics derived from the sentence.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_multi_filter_search_library(
    primary_filter: TargetFilter,
    count: QuantityExpr,
    reveal: bool,
    target_player: Option<TargetFilter>,
    up_to: bool,
    selection_constraint: SearchSelectionConstraint,
    extra_filters: Vec<TargetFilter>,
    destination: Zone,
    enter_tapped: bool,
) -> ParsedEffectClause {
    // Build the chain right-to-left so each link owns its successor. The chain
    // ends at a `SearchLibrary` (the last extra filter) so the outer intrinsic
    // continuation can append the terminal `ChangeZone` for that last search.
    let change_zone_effect = || Effect::ChangeZone {
        origin: Some(Zone::Library),
        destination,
        target: TargetFilter::Any,
        owner_library: false,
        enter_transformed: false,
        under_your_control: false,
        enter_tapped,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
    };

    // CR 107.1c + CR 701.23d: Wrap the count in `UpTo` once at the helper's
    // entrance — every search in the chain shares the same `up_to` semantic,
    // so the per-arm constructions below all read the wrapped form.
    let chain_count = if up_to {
        QuantityExpr::up_to(count)
    } else {
        count
    };

    let mut tail: Option<Box<AbilityDefinition>> = None;
    for extra_filter in extra_filters.into_iter().rev() {
        // Append `Search(extra)` first (it is the successor of the ChangeZone
        // we will prepend in the next step).
        let mut search_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SearchLibrary {
                filter: extra_filter,
                count: chain_count.clone(),
                reveal,
                target_player: target_player.clone(),
                selection_constraint: selection_constraint.clone(),
            },
        );
        search_def.sub_ability = tail;
        // Prepend the `ChangeZone` that moves the PREVIOUS search's found card
        // to the destination. This sits between the preceding SearchLibrary
        // (either the primary or a prior extra) and this extra's search.
        let mut change_zone_def = AbilityDefinition::new(AbilityKind::Spell, change_zone_effect());
        change_zone_def.sub_ability = Some(Box::new(search_def));
        tail = Some(Box::new(change_zone_def));
    }

    let mut clause = parsed_clause(Effect::SearchLibrary {
        filter: primary_filter,
        count: chain_count,
        reveal,
        target_player,
        selection_constraint,
    });
    clause.sub_ability = tail;
    clause
}

#[allow(clippy::too_many_arguments)]
fn lower_target_referenced_search_library(
    reference_target: TargetFilter,
    filter: TargetFilter,
    count: QuantityExpr,
    reveal: bool,
    target_player: Option<TargetFilter>,
    up_to: bool,
    selection_constraint: SearchSelectionConstraint,
    extra_filters: Vec<TargetFilter>,
    destination: Zone,
    enter_tapped: bool,
) -> ParsedEffectClause {
    let search_clause = if extra_filters.is_empty() {
        parsed_clause(Effect::SearchLibrary {
            filter,
            count: if up_to {
                QuantityExpr::up_to(count)
            } else {
                count
            },
            reveal,
            target_player,
            selection_constraint,
        })
    } else {
        lower_multi_filter_search_library(
            filter,
            count,
            reveal,
            target_player,
            up_to,
            selection_constraint,
            extra_filters,
            destination,
            enter_tapped,
        )
    };

    let mut search_def = AbilityDefinition::new(AbilityKind::Spell, search_clause.effect);
    search_def.sub_ability = search_clause.sub_ability;

    let mut clause = parsed_clause(Effect::TargetOnly {
        target: reference_target,
    });
    clause.sub_ability = Some(Box::new(search_def));
    clause
}

/// Wrap an effect with a `Shuffle` sub_ability for compound "X into library" operations.
pub(super) fn with_shuffle_sub_ability(effect: Effect) -> ParsedEffectClause {
    let shuffle = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Shuffle {
            target: TargetFilter::Controller,
        },
    );
    ParsedEffectClause {
        effect,
        duration: None,
        sub_ability: Some(Box::new(shuffle)),
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
    }
}

fn change_zone_all_to_library_effect(origin: Zone) -> Effect {
    Effect::ChangeZoneAll {
        origin: Some(origin),
        destination: Zone::Library,
        target: TargetFilter::Controller,
    }
}

fn lower_change_zone_all_to_library(origins: Vec<Zone>) -> ParsedEffectClause {
    let (first, rest) = origins
        .split_first()
        .expect("ChangeZoneAllToLibrary must have at least one origin");
    let first = *first;

    let mut tail: Option<Box<AbilityDefinition>> = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Shuffle {
            target: TargetFilter::Controller,
        },
    )));
    for origin in rest.iter().rev().copied() {
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            change_zone_all_to_library_effect(origin),
        );
        def.sub_ability = tail;
        tail = Some(Box::new(def));
    }

    let mut clause = parsed_clause(change_zone_all_to_library_effect(first));
    clause.sub_ability = tail;
    clause
}

pub(super) fn parse_destroy_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    if nom_on_lower(text, lower, |input| {
        value((), alt((tag("destroy all "), tag("destroy each ")))).parse(input)
    })
    .is_some()
    {
        let (_, rest) = nom_on_lower(text, lower, |input| value((), tag("destroy ")).parse(input))?;
        let (target, _rem) = parse_target(rest);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(ZoneCounterImperativeAst::Destroy { target, all: true });
    }
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("destroy ")).parse(input))
    {
        let (target, _rem) = parse_target(rest);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(ZoneCounterImperativeAst::Destroy { target, all: false });
    }
    None
}

/// Detect "target {player,opponent}'s {graveyard,library,hand}" prefixes.
///
/// CR 400.12: A zone-targeting effect operates on every card in the named zone.
/// "target player's" and "target opponent's" are not in the shared `POSSESSIVES`
/// list (those reflect possessive pronouns / determiner phrases for objects);
/// they appear only in *zone-as-operand* contexts like Nihil Spellbomb, Bojuka
/// Bog, Tormod's Crypt, Cremate, Faerie Macabre, etc. — so we recognize them
/// here at the dispatch site rather than widening `POSSESSIVES` globally.
fn starts_with_target_possessive_zone(rest_lower: &str) -> bool {
    fn inner(i: &str) -> nom::IResult<&str, &str, VerboseError<&str>> {
        preceded(
            alt((tag("target player's "), tag("target opponent's "))),
            alt((tag("graveyard"), tag("library"), tag("hand"))),
        )
        .parse(i)
    }
    inner(rest_lower).is_ok()
}

pub(super) fn parse_exile_ast(
    text: &str,
    lower: &str,
    ctx: &ParseContext,
) -> Option<ZoneCounterImperativeAst> {
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("exile the top ").parse(lower) {
        let (count, remainder) = if let Ok((rem, n)) = nom_primitives::parse_number.parse(rest) {
            (QuantityExpr::Fixed { value: n as i32 }, rem.trim_start())
        } else if let Ok((rem, _)) = tag::<_, _, VerboseError<&str>>("x").parse(rest) {
            (
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                rem.trim_start(),
            )
        } else {
            (QuantityExpr::Fixed { value: 1 }, rest)
        };
        let that_player = that_player_library_filter(ctx);
        for (pattern, player) in [
            ("card of your library", TargetFilter::Controller),
            ("cards of your library", TargetFilter::Controller),
            ("card of that player's library", that_player.clone()),
            ("cards of that player's library", that_player),
        ] {
            if tag::<_, _, VerboseError<&str>>(pattern)
                .parse(remainder)
                .is_ok()
            {
                return Some(ZoneCounterImperativeAst::ExileTop { player, count });
            }
        }
    }

    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("exile all "), tag("exile each ")))).parse(input)
    }) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        let (parsed_target, _rem) = parse_target(rest);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        // CR 701.5a: "exile all spells" must constrain to the stack.
        let target = if nom_primitives::scan_contains(rest_lower, "spell") {
            super::constrain_filter_to_stack(parsed_target)
        } else {
            parsed_target
        };
        let origin = super::infer_origin_zone(rest_lower);
        return Some(ZoneCounterImperativeAst::Exile {
            origin,
            target,
            all: true,
        });
    }

    let (_, rest_text) = nom_on_lower(text, lower, |input| value((), tag("exile ")).parse(input))?;
    let rest_lower = &lower[lower.len() - rest_text.len()..];

    // CR 400.12: "exile their graveyard" / "exile target player's graveyard"
    // act on all cards in that zone. Bare possessive zone references and
    // "target {player,opponent}'s <zone>" share semantics with "exile all/each".
    // CR 404 (graveyard) / CR 406 (exile) — the zone itself is the operand.
    let mass_zone = starts_with_possessive(rest_lower, "", "graveyard")
        || starts_with_possessive(rest_lower, "", "library")
        || starts_with_possessive(rest_lower, "", "hand")
        || starts_with_target_possessive_zone(rest_lower);
    if mass_zone {
        let (target, _rem) = parse_target(rest_text);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        let origin = super::infer_origin_zone(rest_lower);
        return Some(ZoneCounterImperativeAst::Exile {
            origin,
            target,
            all: true,
        });
    }

    let (parsed_target, _rem) = parse_target(rest_text);
    #[cfg(debug_assertions)]
    assert_no_compound_remainder(_rem, text);
    // CR 701.5a: "exile target spell" must constrain targeting to the stack,
    // mirroring parse_counter_ast at line 1218-1219.
    let target = if nom_primitives::scan_contains(rest_lower, "spell") {
        super::constrain_filter_to_stack(parsed_target)
    } else {
        parsed_target
    };
    let origin = super::infer_origin_zone(rest_lower);
    Some(ZoneCounterImperativeAst::Exile {
        origin,
        target,
        all: false,
    })
}

fn that_player_library_filter(ctx: &ParseContext) -> TargetFilter {
    match &ctx.subject {
        Some(TargetFilter::Typed(tf)) if tf.type_filters.is_empty() && tf.controller.is_some() => {
            TargetFilter::TriggeringPlayer
        }
        Some(TargetFilter::Player) => TargetFilter::TriggeringPlayer,
        _ => TargetFilter::ParentTarget,
    }
}

pub(super) fn parse_counter_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    // CR 701.6 + CR 405.1: "Counter all/each [filter] spells/abilities"
    // mass-counter precheck. Mirrors the `parse_destroy_ast` precheck +
    // `BounceAll` precedent: consume the verb plus an optional `all`/`each`
    // pluralizer, set `all: true`, and let the existing target / spell-stack
    // / activated-or-triggered-ability dispatch run unchanged on the tail.
    let (mass_consumed, rest_orig, rest) = if let Some((_, rest_orig)) =
        nom_on_lower(text, lower, |input| {
            value((), alt((tag("counter all "), tag("counter each ")))).parse(input)
        }) {
        let rest_lower = &lower[lower.len() - rest_orig.len()..];
        (true, rest_orig, rest_lower)
    } else {
        let (_, rest_orig) =
            nom_on_lower(text, lower, |input| value((), tag("counter ")).parse(input))?;
        let rest_lower = &lower[lower.len() - rest_orig.len()..];
        (false, rest_orig, rest_lower)
    };

    // CR 113.3: "abilities" (or "activated or triggered ability") with no
    // intervening type phrase ⇒ stack-ability filter. Mass mode also accepts
    // bare "abilities" (Kadena's Silencer: "counter all abilities your
    // opponents control"). Single-target mode keeps the original strict
    // `activated or triggered ability` requirement to avoid false positives
    // on noun-counter phrases like "page counter on this artifact".
    // Bare "abilities" head: tag-match (skipping leading whitespace) so the
    // dispatch is a real nom combinator, not a string starts_with.
    fn abilities_head(i: &str) -> nom::IResult<&str, &str, VerboseError<&str>> {
        preceded(nom::character::complete::multispace0, tag("abilities")).parse(i)
    }
    let abilities_match = nom_primitives::scan_contains(rest, "activated or triggered ability")
        || (mass_consumed && abilities_head(rest).is_ok());
    if abilities_match {
        // CR 118.12: Parse "unless pays" even for ability counters.
        let unless_payment = super::parse_unless_payment(rest);
        return Some(ZoneCounterImperativeAst::Counter {
            target: TargetFilter::StackAbility,
            source_static: None,
            unless_payment,
            all: mass_consumed,
        });
    }

    let (target, _rem) = parse_target(rest_orig);
    #[cfg(debug_assertions)]
    assert_no_compound_remainder(_rem, text);
    let target = if nom_primitives::scan_contains(rest, "spell") {
        super::constrain_filter_to_stack(target)
    } else {
        target
    };
    // CR 118.12: Parse "unless its controller pays {X}" for conditional counters
    let unless_payment = super::parse_unless_payment(rest);
    Some(ZoneCounterImperativeAst::Counter {
        target,
        source_static: None,
        unless_payment,
        all: mass_consumed,
    })
}

/// CR 118.8 + CR 119.4: Parse the amount portion of a "pay <amount> life" cost.
///
/// `rest` is the lowercase text after the leading `"pay "` token. Returns the
/// resolved `QuantityExpr` on success — literal (`"3 life"`), X variable
/// (`"X life"`), or a dynamic reference (`"life equal to its power"`,
/// `"life equal to <quantity-ref>"`). All dispatch is nom-combinator based,
/// with a shared `life_with_boundary` combinator that guards `"life"` against
/// accidental alpha-suffix matches (e.g., `"x lifelink"`).
fn parse_pay_life_amount(rest: &str) -> Option<QuantityExpr> {
    use crate::parser::oracle_nom::error::OracleResult;
    use nom::character::complete::one_of;
    use nom::combinator::{eof, peek, recognize};
    use nom::sequence::terminated;

    // Shared word-boundary guard: the token just consumed must be followed by
    // end-of-input or punctuation/whitespace — not another alpha char. This
    // blocks false matches like "lifelink" when we only want "life".
    fn word_boundary(i: &str) -> OracleResult<'_, ()> {
        peek(alt((value((), eof), value((), recognize(one_of(" .,")))))).parse(i)
    }

    // CR 118.8: "pay life equal to <quantity-ref>" — delegates to the shared
    // event-context / named-quantity resolvers so every dynamic amount pattern
    // already recognized for gain/lose life composes here too. The quantity
    // helpers are not nom-based, so content cleanup (trailing period + space)
    // happens on the already-dispatched remainder — nom owns the dispatch,
    // not the content normalization.
    if let Ok((tail, _)) = tag::<_, _, VerboseError<&str>>("life equal to ").parse(rest) {
        let qty_text = tail.trim_end().trim_end_matches('.').trim_end();
        if let Some(expr) = crate::parser::oracle_quantity::parse_event_context_quantity(qty_text) {
            return Some(expr);
        }
        if let Some(qty) = crate::parser::oracle_quantity::parse_quantity_ref(qty_text) {
            return Some(QuantityExpr::Ref { qty });
        }
        return None;
    }

    // CR 107.1b: "pay X life" — variable amount resolved from `chosen_x`.
    if terminated(tag::<_, _, VerboseError<&str>>("x life"), word_boundary)
        .parse(rest)
        .is_ok()
    {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        });
    }

    // CR 118.8: "pay N life" — literal amount via `parse_number` (digit words
    // or numerals, never "X" — handled above). Same word-boundary guard so
    // hypothetical phrases like "3 lifelink" cannot false-match.
    if let Ok((_, (n, _))) = (
        nom_primitives::parse_number,
        terminated(tag::<_, _, VerboseError<&str>>(" life"), word_boundary),
    )
        .parse(rest)
    {
        return Some(QuantityExpr::Fixed { value: n as i32 });
    }

    None
}

pub(super) fn parse_cost_resource_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<CostResourceImperativeAst> {
    if let Some(Effect::Unimplemented {
        name,
        description: Some(description),
    }) = try_parse_activate_only_condition(text)
    {
        if name == "activate_only_if_controls_land_subtype_any" {
            return Some(
                CostResourceImperativeAst::ActivateOnlyIfControlsLandSubtypeAny {
                    subtypes: description.split('|').map(ToString::to_string).collect(),
                },
            );
        }
    }
    if let Some((_, rest_orig)) =
        nom_on_lower(text, lower, |input| value((), tag("pay ")).parse(input))
    {
        let rest = &lower[lower.len() - rest_orig.len()..];
        // CR 118.8 + CR 119.4: `pay <amount> life` — literal count, X variable,
        // or dynamic reference (`pay life equal to its power`). Dispatched with
        // nom combinators over the post-"pay " remainder.
        if let Some(amount) = parse_pay_life_amount(rest) {
            return Some(CostResourceImperativeAst::Pay {
                cost: PaymentCost::Life { amount },
            });
        }
        // CR 107.14: "pay any amount of {E}" → variable energy payment
        if let Ok((_, _)) = tag::<_, _, VerboseError<&str>>("any amount of {e}").parse(rest) {
            return Some(CostResourceImperativeAst::Pay {
                cost: PaymentCost::Energy {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                },
            });
        }
        // "pay an amount of {e} equal to ..." → variable energy payment
        if let Ok((rest_after, _)) =
            tag::<_, _, VerboseError<&str>>("an amount of {e} equal to ").parse(rest)
        {
            // Parse the quantity reference after "equal to"
            let rest_trimmed = rest_after.trim().trim_end_matches('.');
            if let Some(qty) = super::super::oracle_quantity::parse_quantity_ref(rest_trimmed) {
                return Some(CostResourceImperativeAst::Pay {
                    cost: PaymentCost::Energy {
                        amount: QuantityExpr::Ref { qty },
                    },
                });
            }
            // Fallback: variable energy payment
            return Some(CostResourceImperativeAst::Pay {
                cost: PaymentCost::Energy {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                },
            });
        }
        // CR 107.14: "pay {E}", "pay {E}{E}", "pay N {E}" → PaymentCost::Energy
        if rest.contains("{e}") {
            let energy_count = rest.matches("{e}").count() as u32;
            let cleaned = rest.replace("{e}", "").replace(' ', "");
            if cleaned.is_empty() {
                // Pure {E} symbols: "pay {e}{e}"
                return Some(CostResourceImperativeAst::Pay {
                    cost: PaymentCost::Energy {
                        amount: QuantityExpr::Fixed {
                            value: energy_count as i32,
                        },
                    },
                });
            }
            // "pay N {e}" / "pay eight {e}" — number prefix + {e} suffix
            if rest.ends_with("{e}") {
                let prefix = rest.trim_end_matches("{e}").trim();
                if let Ok((_, n)) = nom_primitives::parse_number.parse(prefix) {
                    return Some(CostResourceImperativeAst::Pay {
                        cost: PaymentCost::Energy {
                            amount: QuantityExpr::Fixed { value: n as i32 },
                        },
                    });
                }
            }
        }
        // "pay {2}{B}" → PaymentCost::Mana (CR 117.1)
        if let Some((mana_cost, _)) = parse_mana_symbols(rest_orig.trim()) {
            return Some(CostResourceImperativeAst::Pay {
                cost: PaymentCost::Mana { cost: mana_cost },
            });
        }
    }
    if nom_on_lower(text, lower, |input| value((), tag("add ")).parse(input)).is_some() {
        return match try_parse_add_mana_effect(text) {
            Some(Effect::Mana {
                produced,
                restrictions,
                target,
                ..
            }) => Some(CostResourceImperativeAst::Mana {
                produced,
                restrictions,
                target,
            }),
            _ => None,
        };
    }
    if let Some(effect) = super::try_parse_damage(lower, text, ctx) {
        return match effect {
            Effect::DealDamage {
                amount,
                target,
                damage_source: None,
            } => Some(CostResourceImperativeAst::Damage {
                amount,
                target,
                all: false,
            }),
            Effect::DamageAll {
                amount,
                target,
                player_filter: None,
            } => Some(CostResourceImperativeAst::Damage {
                amount,
                target,
                all: true,
            }),
            // DealDamage with damage_source, DamageEachPlayer, etc. — pass through directly
            other => Some(CostResourceImperativeAst::DamageEffect(Box::new(other))),
        };
    }
    None
}

pub(super) fn lower_cost_resource_ast(ast: CostResourceImperativeAst) -> Effect {
    match ast {
        CostResourceImperativeAst::ActivateOnlyIfControlsLandSubtypeAny { subtypes } => {
            Effect::Unimplemented {
                name: "activate_only_if_controls_land_subtype_any".to_string(),
                description: Some(subtypes.join("|")),
            }
        }
        CostResourceImperativeAst::Mana {
            produced,
            restrictions,
            target,
        } => Effect::Mana {
            produced,
            restrictions,
            grants: vec![],
            expiry: None,
            target,
        },
        CostResourceImperativeAst::Damage {
            amount,
            target,
            all,
        } => {
            if all {
                Effect::DamageAll {
                    amount,
                    target,
                    player_filter: None,
                }
            } else {
                Effect::DealDamage {
                    amount,
                    target,
                    damage_source: None,
                }
            }
        }
        CostResourceImperativeAst::Pay { cost } => Effect::PayCost {
            cost,
            payer: TargetFilter::Controller,
        },
        CostResourceImperativeAst::DamageEffect(effect) => *effect,
    }
}

pub(super) fn parse_imperative_family_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ImperativeFamilyAst> {
    let first_word = lower.split_whitespace().next().unwrap_or("");

    // CR 500.8: Additional step/phase effects can appear in various sentence structures
    // ("there is an additional combat phase", "after this phase, there is an additional...").
    // Intercept early regardless of first_word.
    if nom_primitives::scan_contains(lower, "additional combat phase") {
        let with_main =
            nom_primitives::scan_contains(lower, "followed by an additional main phase");
        return Some(ImperativeFamilyAst::GainKeyword(Effect::AdditionalPhase {
            target: TargetFilter::Controller,
            phase: Phase::BeginCombat,
            after: Phase::EndCombat,
            followed_by: if with_main {
                vec![Phase::PostCombatMain]
            } else {
                vec![]
            },
        }));
    }
    if nom_primitives::scan_contains(lower, "additional upkeep step") {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::AdditionalPhase {
            target: TargetFilter::Controller,
            phase: Phase::Upkeep,
            after: Phase::Upkeep,
            followed_by: vec![],
        }));
    }

    // CR 722.1: "You control target player during that player's next turn"
    // (Mindslaver / Word of Command class). "You" is the spell/ability controller
    // in a declarative sentence (not an imperative verb), so this bypasses the
    // first_word dispatch below. Delegates to the ControlNextTurn combinator
    // in `parse_targeted_action_ast`.
    if tag::<_, _, VerboseError<&str>>("you control ")
        .parse(lower)
        .is_ok()
    {
        if let Some(ast) = parse_targeted_action_ast(text, lower, ctx) {
            return Some(ImperativeFamilyAst::Structured(ImperativeAst::Targeted(
                ast,
            )));
        }
    }

    // NOTE: when adding verbs here, also add them to IMPERATIVE_EXTRA_VERBS
    // in game/gap_analysis.rs so the parser gap analyzer can classify them.
    match first_word {
        // ── Unambiguous single-category verbs ──

        // Cost/resource verbs (CR 117-118)
        "pay" | "spend" => {
            parse_cost_resource_ast(text, lower, ctx).map(ImperativeFamilyAst::CostResource)
        }

        // CR 701.10: "double the power/toughness" or "double the number of counters"
        "double" => try_parse_double_effect(lower, ctx).map(ImperativeFamilyAst::GainKeyword),

        // Zone-change/counter verbs (CR 701)
        "destroy" => parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter),
        "exile" => parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter),
        "counter" => parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter),

        // Numeric verbs (CR 121)
        "draw" if nom_primitives::scan_contains(lower, "that many") => {
            // "draw that many cards" → EventContextAmount, bypass numeric AST
            // which can only represent fixed u32 counts.
            Some(ImperativeFamilyAst::GainKeyword(Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            }))
        }
        "draw" => parse_numeric_imperative_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast))),
        "scry" | "surveil" | "mill" => parse_numeric_imperative_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast))),

        // Targeted action verbs (CR 701)
        "tap" | "untap" | "sacrifice" | "discard" | "return" | "fight" => {
            parse_targeted_action_ast(text, lower, ctx)
                .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Targeted(ast)))
        }
        "earthbend" | "airbend" => parse_targeted_action_ast(text, lower, ctx)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Targeted(ast))),

        // Search/creation verbs (CR 701.18, CR 111.2)
        "search" | "seek" => parse_search_and_creation_ast(text, lower, ctx)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(ast))),
        "create" => parse_search_and_creation_ast(text, lower, ctx) // allow-noncombinator: pre-existing match dispatch, only threading ctx through
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(ast))),

        // Utility verbs (CR 615, CR 701.19, CR 701.6, CR 613.4d)
        "prevent" | "regenerate" | "copy" | "attach" | "switch" => {
            parse_utility_imperative_ast(text, lower)
                .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Utility(ast)))
        }
        // CR 701.27 + CR 701.28: "transform" and "convert" are equivalent game actions.
        "transform" | "transforms" | "convert" | "converts" => {
            parse_utility_imperative_ast(text, lower)
                .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Utility(ast)))
        }

        // Shuffle (CR 701.19)
        "shuffle" | "shuffles" => parse_shuffle_ast(text, lower).map(ImperativeFamilyAst::Shuffle),

        // Reveal: "reveal the top N" → Dig (via search path), else hand reveal (CR 701.16, CR 701.20)
        "reveal" | "reveals" => parse_search_and_creation_ast(text, lower, ctx)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(ast)))
            .or_else(|| {
                parse_hand_reveal_ast(text, lower, ctx)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::HandReveal(ast)))
            }),

        // Choose (CR 700.2)
        "choose" | "secretly" => parse_choose_ast(text, lower, ctx)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Choose(ast))),

        // ── Exact-match keyword actions ──
        "explore" if lower == "explore" || lower == "explore again" => {
            Some(ImperativeFamilyAst::Explore)
        }
        // CR 702.162a: "connive" / "connives" — extract target from remainder
        "connive" | "connives" => {
            let rest = lower[first_word.len()..].trim();
            if !rest.is_empty() {
                let (target, _) = parse_target(rest);
                Some(ImperativeFamilyAst::GainKeyword(Effect::Connive {
                    target,
                    count: 1,
                }))
            } else {
                Some(ImperativeFamilyAst::Connive)
            }
        }
        // CR 702.136: "investigate"
        "investigate" => Some(ImperativeFamilyAst::Investigate),
        // CR 701.48a: "learn"
        "learn" => Some(ImperativeFamilyAst::Learn),
        // CR 701.62a: "manifest dread" / CR 701.40a: "manifest the top card of your library"
        "manifest" => {
            if tag::<_, _, VerboseError<&str>>("manifest dread")
                .parse(lower)
                .is_ok()
            {
                Some(ImperativeFamilyAst::ManifestDread)
            } else if let Ok((rest, _)) =
                tag::<_, _, VerboseError<&str>>("manifest the top ").parse(lower)
            {
                // CR 701.40a: "manifest the top card of your library"
                // or "manifest the top N cards of your library"
                let count = if rest.starts_with("card ") {
                    QuantityExpr::Fixed { value: 1 }
                } else if let Ok((_, n)) = nom_primitives::parse_number.parse(rest) {
                    QuantityExpr::Fixed { value: n as i32 }
                } else {
                    QuantityExpr::Fixed { value: 1 }
                };
                Some(ImperativeFamilyAst::Manifest { count })
            } else {
                None
            }
        }
        "proliferate" => Some(ImperativeFamilyAst::Proliferate),
        // CR 701.56a: "time travel" / "time travel N times"
        "time" => {
            if tag::<_, _, VerboseError<&str>>("time travel")
                .parse(lower)
                .is_ok()
            {
                Some(ImperativeFamilyAst::TimeTravel)
            } else {
                None
            }
        }
        // CR 701.36a: "populate"
        "populate" => Some(ImperativeFamilyAst::Populate),
        // CR 701.30: "clash with an opponent"
        "clash" => {
            if tag::<_, _, VerboseError<&str>>("clash with an opponent")
                .parse(lower)
                .is_ok()
            {
                Some(ImperativeFamilyAst::Clash)
            } else {
                None
            }
        }
        // CR 701.60a: "suspect it" / "suspect target creature"
        "suspect" | "suspects" => {
            let rest = lower[first_word.len()..].trim();
            let target = if !rest.is_empty() {
                let (t, _) = parse_target(rest);
                t
            } else {
                crate::types::ability::TargetFilter::ParentTarget
            };
            Some(ImperativeFamilyAst::GainKeyword(Effect::Suspect { target }))
        }
        // CR 701.35a: "detain target creature an opponent controls"
        "detain" | "detains" => {
            let rest = lower[first_word.len()..].trim();
            let target = if !rest.is_empty() {
                let (t, _) = parse_target(rest);
                t
            } else {
                crate::types::ability::TargetFilter::ParentTarget
            };
            Some(ImperativeFamilyAst::GainKeyword(Effect::Detain { target }))
        }
        // Blight N as an effect (e.g. trigger effect "blight 1")
        "blight" => {
            let rest = alt((tag::<_, _, VerboseError<&str>>("blight "), tag("blight")))
                .parse(lower)
                .map(|(r, _)| r)
                .unwrap_or("");
            let count = nom_primitives::parse_number
                .parse(rest.trim())
                .map(|(_, n)| n)
                .unwrap_or(1);
            Some(ImperativeFamilyAst::GainKeyword(Effect::BlightEffect {
                count,
                target: crate::types::ability::TargetFilter::ParentTarget,
            }))
        }
        // Forage keyword action (CR 702.166a)
        "forage" => Some(ImperativeFamilyAst::GainKeyword(Effect::Forage)),
        // Collect evidence N keyword action (CR 702.163a)
        "collect" => {
            if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("collect evidence ").parse(lower)
            {
                let count = nom_primitives::parse_number
                    .parse(rest.trim())
                    .map(|(_, n)| n)
                    .unwrap_or(1);
                Some(ImperativeFamilyAst::GainKeyword(Effect::CollectEvidence {
                    amount: count,
                }))
            } else {
                None
            }
        }
        // Endure N keyword action
        "endure" | "endures" => {
            let rest = alt((tag::<_, _, VerboseError<&str>>("endure "), tag("endures ")))
                .parse(lower)
                .map(|(r, _)| r)
                .unwrap_or("");
            let count = nom_primitives::parse_number
                .parse(rest.trim())
                .map(|(_, n)| n)
                .unwrap_or(1);
            Some(ImperativeFamilyAst::GainKeyword(Effect::Endure {
                amount: count,
            }))
        }
        // CR 701.53a: "incubate N"
        "incubate" => try_parse_incubate(lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.47a: "amass [Type] N"
        "amass" => try_parse_amass(text, lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.37a: "monstrosity N"
        "monstrosity" => try_parse_monstrosity(lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.46a: "adapt N"
        "adapt" => try_parse_adapt(lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.39a: "bolster N"
        "bolster" => try_parse_bolster(lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.41a: "support N"
        "support" => {
            let (rest, _) = tag::<_, _, VerboseError<&str>>("support ")
                .parse(lower)
                .ok()?;
            let rest = rest.trim().trim_end_matches('.');
            let count = nom_primitives::parse_number
                .parse(rest)
                .map(|(_, n)| n)
                .unwrap_or(1);
            // CR 701.41a: On a permanent, Support targets "other" creatures.
            // On an instant/sorcery, it targets any creatures. When parsing within
            // a trigger effect (subject is Some), the card is a permanent.
            let is_other = ctx.subject.is_some();
            Some(ImperativeFamilyAst::Support { count, is_other })
        }
        // CR 508.1d: "attacks/attack this turn/combat if able" — forced attack requirement.
        // CR 508.1d: "attacks/attack this turn/combat if able" — forced attack requirement.
        "attacks" | "attack" => try_parse_attack_if_able(lower),
        // CR 509.1b / CR 508.1d: "can't be blocked [this turn]", "can't attack", etc.
        // These appear as subjectless clauses in compound effects (e.g., "gets +2/+0 and can't be blocked this turn").
        "can't" | "cannot" => try_parse_subjectless_cant(lower),
        // CR 705: "flip a coin" / "flip N coins" / "flip a coin until you lose a flip"
        "flip" | "flips" => {
            // Longest-match first: "flip a coin until you lose a flip" must
            // precede plain "flip a coin". The N-coin form is tried after
            // "until lose" (since "until lose" always has "a coin"), but
            // before the 1-coin fallback.
            if let Ok((_, ast)) = value::<_, _, VerboseError<&str>, _>(
                ImperativeFamilyAst::FlipCoinUntilLose,
                alt((
                    tag::<_, _, VerboseError<&str>>("flip a coin until you lose a flip"),
                    tag("flips a coin until they lose a flip"),
                )),
            )
            .parse(lower)
            {
                return Some(ast);
            }
            // CR 705.1 + CR 107.1: "flip N coins" / "flip X coins" — N-coin form.
            if let Some(ast) = try_parse_flip_n_coins(lower) {
                return Some(ast);
            }
            value::<_, _, VerboseError<&str>, _>(
                ImperativeFamilyAst::FlipCoin,
                alt((
                    tag::<_, _, VerboseError<&str>>("flip a coin"),
                    tag("flips a coin"),
                )),
            )
            .parse(lower)
            .ok()
            .map(|(_, ast)| ast)
        }
        // CR 706: "roll a d20"
        "roll" | "rolls" => {
            try_parse_roll_die_sides(lower).map(|sides| ImperativeFamilyAst::RollDie { sides })
        }
        // CR 725.1: "become the monarch"
        "become" | "becomes" => {
            if lower == "become the monarch" || lower == "becomes the monarch" {
                Some(ImperativeFamilyAst::BecomeMonarch)
            } else {
                None
            }
        }
        // CR 701.49: "venture into the dungeon" / "venture into the Undercity"
        "venture" => alt((
            value(
                ImperativeFamilyAst::VentureIntoUndercity,
                tag::<_, _, VerboseError<&str>>("venture into the undercity"),
            ),
            value(
                ImperativeFamilyAst::VentureIntoDungeon,
                tag("venture into the dungeon"),
            ),
        ))
        .parse(lower)
        .ok()
        .map(|(_, ast)| ast),
        // CR 500.7: "take an extra turn after this one"
        // CR 725: "take the initiative"
        "take" | "takes" => {
            if alt((
                value((), tag::<_, _, VerboseError<&str>>("take the initiative")),
                value((), tag("takes the initiative")),
            ))
            .parse(lower)
            .is_ok()
            {
                Some(ImperativeFamilyAst::TakeTheInitiative)
            } else if nom_primitives::scan_contains(lower, "extra turn") {
                Some(ImperativeFamilyAst::GainKeyword(Effect::ExtraTurn {
                    target: TargetFilter::Controller,
                }))
            } else {
                None
            }
        }
        // CR 702.26a + CR 702.26c: "phase out" / "phases out" / "phase in" /
        // "phases in" — with optional "target ..." clause. Nom-combinator
        // dispatch on the lowercase input; the target extraction delegates
        // to the shared `parse_target` helper so the full typed filter
        // vocabulary (target creature, each creature you control, etc.) is
        // reused. A leading "~" placeholder (post-subject-strip self-ref)
        // is accepted implicitly: the subject-strip pipeline collapses
        // "~ phases out" to "phases out" before this match runs.
        "phase" | "phases" => {
            // Verb head: "phase out" / "phases out" / "phase in" / "phases in"
            let parsed = alt((
                value(PhaseDir::Out, tag::<_, _, VerboseError<&str>>("phase out")),
                value(PhaseDir::Out, tag("phases out")),
                value(PhaseDir::In, tag("phase in")),
                value(PhaseDir::In, tag("phases in")),
            ))
            .parse(lower)
            .ok();

            parsed.map(|(rest, dir)| {
                // Extract optional "target ..." / filter tail. Empty tail =
                // self-reference (the imperative subject handles the
                // attachment); a non-empty tail routes through parse_target
                // for full filter vocabulary.
                let tail = rest.trim_start_matches([' ', ',', '.', ';']).trim();
                let target = if tail.is_empty() {
                    TargetFilter::Any
                } else {
                    let (t, _) = parse_target(tail);
                    t
                };
                match dir {
                    PhaseDir::Out => ImperativeFamilyAst::GainKeyword(Effect::PhaseOut { target }),
                    PhaseDir::In => ImperativeFamilyAst::GainKeyword(Effect::PhaseIn { target }),
                }
            })
        }

        // CR 701.15a: "goad target creature" / "goads target creature" / "goad it"
        "goad" | "goads" => {
            let rest = lower[first_word.len()..].trim();
            if !rest.is_empty() {
                let (target, _) = parse_target(rest);
                Some(ImperativeFamilyAst::GainKeyword(Effect::Goad { target }))
            } else {
                Some(ImperativeFamilyAst::Goad)
            }
        }

        // CR 701.12a: "exchange control of <two-target-spec>"
        // Two grammatical shapes the parser must extract per-slot filters from:
        //   • Quantified: "two target Xs"        (Switcheroo, Role Reversal)
        //   • Compound:   "target X and target Y" / "target X and another target Y"
        //                                          (Phyrexian Infiltrator, Oko, Trade the Helm)
        //                "this <type> and target Y" / "target X and this <type>"
        //                                          (Avarice Totem, Eyes Everywhere — SelfRef one side)
        // Both shapes lower to ExchangeControl { target_a, target_b }; in the
        // quantified case both filters are identical.
        "exchange" => {
            let (rest, _) = tag::<_, _, VerboseError<&str>>("exchange control of ")
                .parse(lower)
                .ok()?;
            // Strip trailing terminator from the candidate target span so per-slot
            // parse_target sees clean input (parse_target is whitespace-tolerant
            // but stops on punctuation only via its own grammar).
            let span = rest.trim_end_matches(['.', ';']);
            try_parse_exchange_control_targets(span).map(|(target_a, target_b)| {
                ImperativeFamilyAst::ExchangeControl { target_a, target_b }
            })
        }

        // ── Combat-related ──

        // CR 509.1g: "block [object] this turn/combat if able"
        // Handles: "block this turn if able", "blocks ~ this turn if able",
        // "blocks it this combat if able", "blocks this creature this turn if able"
        "block" | "blocks" => {
            if nom_primitives::scan_contains(lower, "this turn if able")
                || nom_primitives::scan_contains(lower, "this combat if able")
            {
                Some(ImperativeFamilyAst::ForceBlock)
            } else {
                None
            }
        }
        // CR 509.1c: "must be blocked [this turn] [if able]"
        "must" => {
            if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("must be blocked").parse(lower) {
                let rest = rest.trim();
                if rest.is_empty()
                    || rest == "this turn if able"
                    || rest == "if able"
                    || rest == "this turn"
                {
                    return Some(ImperativeFamilyAst::MustBeBlocked);
                }
            }
            None
        }

        // ── Multi-category verbs (priority sub-dispatch) ──

        // "put that many +1/+1 counters on ~" — dynamic counter count from event context.
        // Intercepted before standard dispatch because parse_number can't handle "that many".
        // Produces a PutCounter with the counter type and target, using EventContextAmount
        // for the count. The engine resolver reads the count from the resolved ability's
        // event_context_amount field.
        "put"
            if nom_primitives::scan_contains(lower, "that many")
                && nom_primitives::scan_contains(lower, "counter") =>
        {
            try_parse_that_many_counters(lower, ctx)
                .map(ImperativeFamilyAst::GainKeyword)
                .or_else(|| {
                    parse_zone_counter_ast(text, lower, ctx)
                        .map(ImperativeFamilyAst::ZoneCounter)
                        .or_else(|| parse_put_ast(text, lower).map(ImperativeFamilyAst::Put))
                })
        }
        // "put" → counter (step 2) first, then zone-change (step 12)
        "put" => parse_zone_counter_ast(text, lower, ctx)
            .map(ImperativeFamilyAst::ZoneCounter)
            .or_else(|| parse_put_ast(text, lower).map(ImperativeFamilyAst::Put)),

        // "remove" → "remove from combat" (CR 506.4) → counter removal (step 2)
        "remove" => parse_remove_from_combat_ast(lower, ctx) // allow-noncombinator: pre-existing match dispatch, only threading ctx through
            .map(ImperativeFamilyAst::RemoveFromCombat)
            .or_else(|| {
                parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter)
            }),

        // "move" → counter movement (step 2): "move N counters from X onto Y"
        "move" => parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter),

        // "add" → mana/cost-resource (step 1)
        // allow-noncombinator: pre-existing match dispatch, only threading ctx through
        "add" => parse_cost_resource_ast(text, lower, ctx).map(ImperativeFamilyAst::CostResource),

        // "gain" → "gain control of" (step 4) → "gain life" (step 3) → keyword (step 8)
        // The current if/else chain checks numeric first (step 3), but numeric guards with
        // `contains("gain") && contains("life")`, so "gain control of" never matches numeric.
        // This reordering makes the disambiguation explicit.
        "gain" | "gains" => {
            if nom_primitives::scan_contains(lower, "control of") {
                parse_targeted_action_ast(text, lower, ctx)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Targeted(ast)))
            } else if nom_primitives::scan_contains(lower, "life") {
                parse_numeric_imperative_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)))
            } else {
                // CR 702: bare keyword grant first; CR 113.3 + CR 604.1: fall
                // back to quoted-ability grant when the gain clause carries an
                // inline ability ('gain "When this creature dies, draw a card."').
                try_parse_gain_keyword(text)
                    .or_else(|| try_parse_gain_quoted_ability(text))
                    .map(ImperativeFamilyAst::GainKeyword)
            }
        }

        // "lose" → "lose the game" (step 6) → "lose all counters" (step 6.5)
        //       → "lose life" (step 3) → keyword (step 7)
        "lose" | "loses" => {
            if nom_primitives::scan_contains(lower, "the game") {
                Some(ImperativeFamilyAst::LoseTheGame)
            } else if let Some(effect) = try_parse_lose_all_player_counters(text, lower) {
                // CR 122.1: Player-scoped "lose all counters" —
                // Suncleanser ("target opponent loses all counters") and
                // Final Act mode 5 ("each opponent loses all counters"). The
                // `each opponent` subject is already stripped upstream via
                // `strip_each_player_subject`, leaving `lose all counters` to
                // dispatch here; `target opponent loses all counters` retains
                // its target for the parse_target call below.
                Some(ImperativeFamilyAst::GainKeyword(effect))
            } else if nom_primitives::scan_contains(lower, "life") {
                parse_numeric_imperative_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)))
            } else if !nom_primitives::scan_contains(lower, "mana") {
                try_parse_gain_keyword(text).map(ImperativeFamilyAst::LoseKeyword)
            } else {
                None
            }
        }

        // CR 104.3a: "win the game"
        "win" | "wins" => {
            if nom_primitives::scan_contains(lower, "the game") {
                Some(ImperativeFamilyAst::WinTheGame)
            } else {
                None
            }
        }

        // "look" → "look at the top" (step 5) → "look at hand" (step 10)
        "look" => parse_search_and_creation_ast(text, lower, ctx) // allow-noncombinator: pre-existing match dispatch, only threading ctx through
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(ast)))
            .or_else(|| {
                parse_hand_reveal_ast(text, lower, ctx)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::HandReveal(ast)))
            }),

        // "gets"/"get" → try player counter first, then pump (step 3)
        "gets" | "get" => try_parse_player_counter(lower).or_else(|| {
            parse_numeric_imperative_ast(text, lower)
                .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)))
        }),

        // "deals"/"deal" → damage (via cost_resource, which contains try_parse_damage)
        "deals" | "deal" => {
            parse_cost_resource_ast(text, lower, ctx).map(ImperativeFamilyAst::CostResource)
        }

        // "you may" → optional wrapper
        "you" => nom_on_lower(text, lower, |input| value((), tag("you may ")).parse(input)).map(
            |(_, stripped)| ImperativeFamilyAst::YouMay {
                text: stripped.to_string(),
            },
        ),

        // "may" → optional wrapper (produced after strip_each_player_subject strips "each player ")
        // e.g. "Each player may discard their hand" → subject stripped → "may discard their hand"
        "may" => nom_on_lower(text, lower, |input| value((), tag("may ")).parse(input)).map(
            |(_, stripped)| ImperativeFamilyAst::YouMay {
                text: stripped.to_string(),
            },
        ),

        // Unknown first word — try position-agnostic parsers that use `contains`/`find`
        // rather than `starts_with`. This handles cases where the verb isn't the first
        // word (e.g., "Lightning Bolt deals 3 damage" after failed subject stripping,
        // "each player gains 2 life" where "each" isn't a verb, or
        // "that player shuffles" where "that" precedes the verb).
        _ => {
            // Damage: try_parse_damage uses lower.find("deals ") — matches anywhere
            if let Some(ast) = parse_cost_resource_ast(text, lower, ctx) {
                return Some(ImperativeFamilyAst::CostResource(ast));
            }
            // Numeric: contains("gain")+contains("life"), contains("gets +"), etc.
            if let Some(ast) = parse_numeric_imperative_ast(text, lower) {
                return Some(ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)));
            }
            // Shuffle: "that player shuffles" / "target player shuffles" have
            // non-verb first words but are exact-match shuffle patterns
            if let Some(ast) = parse_shuffle_ast(text, lower) {
                return Some(ImperativeFamilyAst::Shuffle(ast));
            }
            None
        }
    }
}

/// CR 701.12a: Extract the two per-slot target filters from the "<...>" body of
/// "exchange control of <...>". Returns `None` for unrecognised shapes so the
/// caller can fall through (no Effect is emitted) rather than silently dropping
/// targets into a bare ExchangeControl.
///
/// Two grammatical shapes are recognised:
/// 1. Quantified: "two target Xs" — both slot filters are identical. Driven by
///    `parse_target`'s built-in "two " quantifier handling, which consumes the
///    count word and returns a single filter.
/// 2. Compound: "<slot> and <slot>" — each slot is parsed independently. A slot
///    is either a "target …" phrase (with optional "another"/"other", delegated
///    to `parse_target`) or "this <type>" (Avarice Totem, Eyes Everywhere,
///    Phyrexian Infiltrator — lowered to `SelfRef`).
///
/// Each per-slot parse must consume its entire substring (no trailing remainder)
/// so we don't accept malformed inputs like "target creature and dance" as a
/// valid two-target phrase.
fn try_parse_exchange_control_targets(span: &str) -> Option<(TargetFilter, TargetFilter)> {
    // Quantified shape: "two target Xs" dispatched via nom. We peek for the
    // `"two target "` prefix with `alt((tag(...), tag(...)))` (plural handled by
    // `parse_target`'s QUANTIFIED_PREFIXES), then re-enter `parse_target` on the
    // full span so its quantifier path runs and returns a single filter that
    // applies to both slots.
    if alt((
        tag::<_, _, VerboseError<&str>>("two target "),
        tag("two other target "),
        tag("two another target "),
    ))
    .parse(span)
    .is_ok()
    {
        let (filter, remainder) = parse_target(span);
        if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
            return Some((filter.clone(), filter));
        }
    }

    // Compound shape: locate the top-level " and " connective with nom's
    // `take_until` (a combinator, not string-dispatch), then delegate each side
    // to `parse_exchange_slot`. `take_until` is structural — it splits the
    // span; all dispatch decisions happen inside `parse_exchange_slot` via nom.
    //
    // ASSUMPTION: Exchange-control slots do NOT contain an internal " and "
    // (e.g. no "creature with flying and first strike and target creature" in
    // printed Oracle text for this effect). `take_until` is first-occurrence
    // greedy, so a slot with internal " and " would misfire. No current card
    // triggers this; if such text appears, switch to a right-anchored split
    // or recognise per-slot terminators.
    let (right, (left, _)) = nom::sequence::pair(
        take_until::<_, _, VerboseError<&str>>(" and "),
        tag(" and "),
    )
    .parse(span)
    .ok()?;
    let target_a = parse_exchange_slot(left.trim())?;
    let target_b = parse_exchange_slot(right.trim())?;
    Some((target_a, target_b))
}

/// Parse a single exchange-control slot phrase. Returns the slot filter, or
/// `None` if the phrase isn't a recognised slot. The slot must be fully
/// consumed — a trailing remainder indicates the caller handed us malformed
/// input and we must fall through rather than silently accepting a partial
/// parse.
fn parse_exchange_slot(phrase: &str) -> Option<TargetFilter> {
    // Self-referential slot dispatch via nom: "this <type>" refers to the
    // source permanent and resolves to SelfRef regardless of the type word
    // (artifact, creature, enchantment …).
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("this ").parse(phrase) {
        if !rest.trim().is_empty() {
            return Some(TargetFilter::SelfRef);
        }
    }

    // Standard target slot: "target …" / "another target …" / "other target …".
    // parse_target absorbs all "target"/"another target"/"other target" prefixes.
    let (filter, remainder) = parse_target(phrase);
    if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
        return Some(filter);
    }
    None
}

/// CR 122.1: Parse "lose(s) all counters" / "target opponent loses
/// all counters" / "lose all counters" into an `Effect::LoseAllPlayerCounters`.
///
/// Two shapes are handled here:
/// 1. Bare predicate: "lose all counters" / "loses all counters" — the
///    "each opponent" / "each player" subject has already been stripped by
///    `strip_each_player_subject`, and the outer `player_scope` drives
///    per-player iteration. Target defaults to `Controller` so the iterator
///    addresses the iterating player (CR 608.2 player_scope rebinding).
/// 2. Explicit target: "target opponent loses all counters" /
///    "target player loses all counters" — `parse_target` lifts the typed
///    filter out of the subject; the effect resolves against that chosen
///    player.
fn try_parse_lose_all_player_counters(text: &str, lower: &str) -> Option<Effect> {
    // Case 1: bare predicate after subject-strip — "lose all counters" /
    // "loses all counters" (trailing period already stripped by the dispatch).
    let bare = lower.trim().trim_end_matches('.').trim();
    let bare_tail = alt((
        tag::<_, _, VerboseError<&str>>("loses all counters"),
        tag("lose all counters"),
    ))
    .parse(bare);
    if let Ok((rest, _)) = bare_tail {
        if rest.trim().is_empty() {
            return Some(Effect::LoseAllPlayerCounters {
                target: TargetFilter::Controller,
            });
        }
    }

    // Case 2: explicit subject — "target opponent loses all counters" /
    // "target player loses all counters". Strip the " loses all counters" /
    // " lose all counters" suffix (structural slice of a known trailing
    // literal, not parsing dispatch), then hand the subject prefix to
    // `parse_target`.
    let trimmed = text.trim_end_matches('.').trim();
    let trimmed_lower = trimmed.to_lowercase();
    let subject_len = trimmed_lower
        .strip_suffix(" loses all counters")
        .or_else(|| trimmed_lower.strip_suffix(" lose all counters"))
        .map(str::len)?;
    let subject = trimmed[..subject_len].trim();
    let (filter, remainder) = parse_target(subject);
    if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
        return Some(Effect::LoseAllPlayerCounters { target: filter });
    }

    None
}

/// CR 122.1: Parse "get/gets a/an/N [type] counter(s)" into a GivePlayerCounter AST.
/// Handles patterns like:
/// - "get a poison counter"
/// - "gets two experience counters"
/// - "get ten rad counters"
fn try_parse_player_counter(lower: &str) -> Option<ImperativeFamilyAst> {
    // Strip "get/gets " prefix
    let (rest, _) = alt((tag::<_, _, VerboseError<&str>>("gets "), tag("get ")))
        .parse(lower)
        .ok()?;

    // Must end with "counter" or "counters"
    let (before_counter, plural) = if let Some(s) = rest.strip_suffix(" counters") {
        (s, true)
    } else {
        let s = rest.strip_suffix(" counter")?;
        (s, false)
    };

    // Parse quantity + counter kind from the remaining text.
    // Patterns: "a poison" / "an experience" / "two rad" / "10 poison"
    let (count, counter_kind) =
        if let Ok((kind, _)) = nom_primitives::parse_article.parse(before_counter) {
            (1u32, kind.trim())
        } else if let Ok((rest, n)) = nom_primitives::parse_number.parse(before_counter) {
            (n, rest.trim())
        } else {
            return None;
        };

    // Validate: counter kind should be a single word (no spaces) to avoid false positives
    // like "gets +1/+1 counter" which is an object counter, not a player counter.
    if counter_kind.is_empty() || counter_kind.contains('+') || counter_kind.contains('-') {
        return None;
    }

    // CR 122.1b: Map to typed PlayerCounterKind — reject anything that's an object counter.
    // Energy counters are NOT included — they use the dedicated GainEnergy effect.
    let kind = match counter_kind {
        "poison" => PlayerCounterKind::Poison,
        "experience" => PlayerCounterKind::Experience,
        "rad" => PlayerCounterKind::Rad,
        "ticket" => PlayerCounterKind::Ticket,
        _ => return None,
    };

    let _ = plural; // plural is just grammatical, doesn't affect semantics
    Some(ImperativeFamilyAst::GivePlayerCounter {
        counter_kind: kind,
        count: QuantityExpr::Fixed {
            value: count as i32,
        },
    })
}

/// CR 706: Parse die side count from "roll a dN" / "roll a six-sided die" patterns.
/// CR 705.1 + CR 107.1: Parse "flip N coins" / "flip X coins" / "flip two coins" —
/// the N-coin form. Delegates the count to `parse_count_expr`, covering digit,
/// word-number, and `X` forms uniformly. Returns None for "flip a coin" / "flip one coin"
/// so the caller falls back to `FlipCoin` (the existing 1-flip shape).
fn try_parse_flip_n_coins(lower: &str) -> Option<ImperativeFamilyAst> {
    let (rest, _) = alt((tag::<_, _, VerboseError<&str>>("flip "), tag("flips ")))
        .parse(lower)
        .ok()?;

    let (expr, after) = parse_count_expr(rest)?;

    // `parse_count_expr` returns the remainder with leading whitespace trimmed
    // ("five coins" → rest = "coins"), so we match "coins"/"coin" without the
    // leading space. Word-boundary termination rejects "coinsomething".
    // "flip a coin" is handled by FlipCoin; "flip one coin" would semantically
    // match but is never printed.
    let (after_noun, _) = alt((tag::<_, _, VerboseError<&str>>("coins"), tag("coin")))
        .parse(after)
        .ok()?;
    // structural: not dispatch — checks that the next char is a non-alphanumeric boundary.
    if !after_noun.is_empty() && !after_noun.starts_with(|c: char| !c.is_alphanumeric()) {
        return None;
    }

    // Reject count == 1 so "flip 1 coin" (if ever printed) prefers FlipCoin.
    if matches!(expr, QuantityExpr::Fixed { value: 1 }) {
        return None;
    }

    Some(ImperativeFamilyAst::FlipCoins { count: expr })
}

fn try_parse_roll_die_sides(lower: &str) -> Option<u8> {
    // Strip the "roll a " / "rolls a " prefix.
    let (rest, _) = alt((tag::<_, _, VerboseError<&str>>("roll a "), tag("rolls a ")))
        .parse(lower)
        .ok()?;
    // Numeric form: "d20", "d6", "d4"
    if let Ok((num_rest, _)) = tag::<_, _, VerboseError<&str>>("d").parse(rest) {
        if let Ok(sides) = num_rest.parse::<u8>() {
            return Some(sides);
        }
    }
    // CR 706: Word-form: "six-sided die", "four-sided die", etc.
    let (_, sides) = alt((
        value(
            4_u8,
            alt((
                tag::<_, _, VerboseError<&str>>("four-sided"),
                tag("4-sided"),
            )),
        ),
        value(6, alt((tag("six-sided"), tag("6-sided")))),
        value(8, alt((tag("eight-sided"), tag("8-sided")))),
        value(10, alt((tag("ten-sided"), tag("10-sided")))),
        value(12, alt((tag("twelve-sided"), tag("12-sided")))),
        value(20, alt((tag("twenty-sided"), tag("20-sided")))),
    ))
    .parse(rest)
    .ok()?;
    Some(sides)
}

/// CR 706.2: Try to parse a d20 result table line like "1—9 | Draw two cards"
/// or "20 | Search your library for a card". Returns `(min, max, effect_text)`.
pub(crate) fn try_parse_die_result_line(text: &str) -> Option<(u8, u8, &str)> {
    let trimmed = text.trim();

    // Find the pipe separator: "N—M | effect" or "N | effect"
    let (_, (range_part, effect_text)) = nom_primitives::split_once_on(trimmed, " | ").ok()?;
    let range_part = range_part.trim();
    let effect_text = effect_text.trim();

    // Parse range: "1—9" (em dash U+2014), "10—19", "20" (single value)
    let (min, max) = if let Some(dash_idx) = range_part.find('\u{2014}') {
        let min_str = &range_part[..dash_idx];
        let max_str = &range_part[dash_idx + '\u{2014}'.len_utf8()..];
        (min_str.parse::<u8>().ok()?, max_str.parse::<u8>().ok()?)
    } else {
        // Single value like "20"
        let val = range_part.parse::<u8>().ok()?;
        (val, val)
    };

    Some((min, max, effect_text))
}

/// CR 705: Try to parse "if you win the flip, [effect]" / "if you lose the flip, [effect]"
/// from Oracle text. Returns `(is_win, effect_text)`.
pub(crate) fn try_parse_coin_flip_branch(text: &str) -> Option<(bool, &str)> {
    const WIN: &str = "if you win the flip, ";
    const LOSE: &str = "if you lose the flip, ";
    if let Some(prefix) = text.get(..WIN.len()) {
        if prefix.eq_ignore_ascii_case(WIN) {
            return Some((true, &text[WIN.len()..]));
        }
    }
    if let Some(prefix) = text.get(..LOSE.len()) {
        if prefix.eq_ignore_ascii_case(LOSE) {
            return Some((false, &text[LOSE.len()..]));
        }
    }
    None
}

pub(super) fn lower_imperative_family_ast(ast: ImperativeFamilyAst) -> ParsedEffectClause {
    match ast {
        // CR 122.1 + CR 608.2c: Multi-typed counter list → PutCounter chain.
        // Intercepted here (rather than in lower_zone_counter_ast which returns
        // a bare Effect) because the chain requires a sub_ability linkage that
        // only ParsedEffectClause can express.
        ImperativeFamilyAst::ZoneCounter(ZoneCounterImperativeAst::PutCounterList {
            entries,
            target,
            multi_target,
        }) => lower_put_counter_list(entries, target, multi_target),
        // CR 115.1c + CR 601.2c: "Choose target X and target Y" — two
        // independent target slots. Lowered to a primary `Effect::TargetOnly`
        // (slot A) with a chained `TargetOnly` sub_ability (slot B). At
        // resolution time both targets are announced as part of the same cast
        // (CR 601.2c: "if the spell uses the word 'target' in multiple
        // places, the same object or player can be chosen once for each
        // instance"). Intercepted here because a bare `Effect` cannot express
        // the sub_ability chain — only `ParsedEffectClause` can. Sentences
        // following the targeting clause that reference the chosen objects
        // (e.g., Goblin Welder's "If both targets are still legal …") chain
        // further sub_abilities and resolve their target slots via
        // `TargetFilter::ParentTarget` walking the chain (CR 608.2c:
        // instructions are followed in order; later text may modify earlier
        // text).
        ImperativeFamilyAst::Structured(ImperativeAst::Choose(
            ChooseImperativeAst::TwoTargets { target_a, target_b },
        )) => {
            let mut clause = parsed_clause(Effect::TargetOnly { target: target_a });
            clause.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::TargetOnly { target: target_b },
            )));
            clause
        }
        // CR 701.23a + CR 107.1: Dual/N-way search ("a X card and a Y card") lowers
        // to a chain of independent `SearchLibrary` effects linked via sub_ability,
        // mirroring `lower_put_counter_list`. Intercepted here because the bare
        // `Effect` returned by `lower_search_and_creation_ast` cannot express a
        // chain — only `ParsedEffectClause.sub_ability` can.
        ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(
            SearchCreationImperativeAst::SearchLibrary {
                filter,
                count,
                reveal,
                target_player,
                up_to,
                selection_constraint,
                reference_target: Some(reference_target),
                extra_filters,
                multi_destination,
                multi_enter_tapped,
            },
        )) => lower_target_referenced_search_library(
            reference_target,
            filter,
            count,
            reveal,
            target_player,
            up_to,
            selection_constraint,
            extra_filters,
            multi_destination,
            multi_enter_tapped,
        ),
        // CR 701.23a + CR 107.1: Dual/N-way search ("a X card and a Y card") lowers
        // to a chain of independent `SearchLibrary` effects linked via sub_ability,
        // mirroring `lower_put_counter_list`. Intercepted here because the bare
        // `Effect` returned by `lower_search_and_creation_ast` cannot express a
        // chain — only `ParsedEffectClause.sub_ability` can.
        ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(
            SearchCreationImperativeAst::SearchLibrary {
                filter,
                count,
                reveal,
                target_player,
                up_to,
                selection_constraint,
                reference_target: None,
                extra_filters,
                multi_destination,
                multi_enter_tapped,
            },
        )) if !extra_filters.is_empty() => lower_multi_filter_search_library(
            filter,
            count,
            reveal,
            target_player,
            up_to,
            selection_constraint,
            extra_filters,
            multi_destination,
            multi_enter_tapped,
        ),
        ImperativeFamilyAst::Shuffle(ast) => lower_shuffle_ast(ast),
        // CR 701.41a: Support N → PutCounter with multi-target "up to N".
        // On permanents (is_other=true): "up to N other target creatures"
        // On instants/sorceries (is_other=false): "up to N target creatures"
        ImperativeFamilyAst::Support { count, is_other } => {
            let properties = if is_other {
                vec![crate::types::ability::FilterProp::Another]
            } else {
                vec![]
            };
            let target = TargetFilter::Typed(TypedFilter {
                type_filters: vec![crate::types::ability::TypeFilter::Creature],
                properties,
                ..Default::default()
            });
            let mut clause = parsed_clause(Effect::PutCounter {
                counter_type: "P1P1".to_string(),
                count: QuantityExpr::Fixed { value: 1 },
                target,
            });
            clause.multi_target = Some(MultiTargetSpec::fixed(0, count as usize));
            clause
        }
        // All other arms produce a bare Effect with no sub_ability chain.
        other => parsed_clause(lower_imperative_family_effect(other)),
    }
}

fn lower_imperative_family_effect(ast: ImperativeFamilyAst) -> Effect {
    match ast {
        ImperativeFamilyAst::Structured(ast) => lower_imperative_ast(ast),
        ImperativeFamilyAst::CostResource(ast) => lower_cost_resource_ast(ast),
        ImperativeFamilyAst::ZoneCounter(ast) => lower_zone_counter_ast(ast),
        ImperativeFamilyAst::Explore => Effect::Explore,
        ImperativeFamilyAst::Connive => Effect::Connive {
            target: TargetFilter::Any,
            count: 1,
        },
        ImperativeFamilyAst::ForceBlock => Effect::ForceBlock {
            target: TargetFilter::Any,
        },
        // CR 701.15a: Goad target creature. Subject injection fills target from parsed text.
        ImperativeFamilyAst::Goad => Effect::Goad {
            target: TargetFilter::Any,
        },
        // CR 701.12a: Exchange control of two permanents. The two slot filters
        // come from the parser; resolution reads ability.targets for the chosen
        // objects.
        ImperativeFamilyAst::ExchangeControl { target_a, target_b } => {
            Effect::ExchangeControl { target_a, target_b }
        }
        // CR 509.1c: Must be blocked — grant transient MustBeBlocked static via GenericEffect.
        // Uses AddStaticMode so the mode propagates through the layer system to
        // static_definitions, where combat.rs checks it.
        ImperativeFamilyAst::MustBeBlocked => {
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::MustBeBlocked)
                    .modifications(vec![ContinuousModification::AddStaticMode {
                        mode: StaticMode::MustBeBlocked,
                    }])],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            }
        }
        ImperativeFamilyAst::Investigate => Effect::Investigate,
        ImperativeFamilyAst::Learn => Effect::Learn,
        // CR 701.40a: Default subject is the controller ("you manifest..."). Subject
        // lowering for "its controller manifests..." routes through the dedicated
        // subject-predicate arm in `lower_subject_predicate_ast` below, which
        // constructs `Effect::Manifest { target: subject.affected, ... }` directly.
        ImperativeFamilyAst::Manifest { count } => Effect::Manifest {
            target: TargetFilter::Controller,
            count,
        },
        ImperativeFamilyAst::ManifestDread => Effect::ManifestDread,
        ImperativeFamilyAst::BecomeMonarch => Effect::BecomeMonarch,
        ImperativeFamilyAst::VentureIntoDungeon => Effect::VentureIntoDungeon,
        ImperativeFamilyAst::VentureIntoUndercity => Effect::VentureInto {
            dungeon: crate::game::dungeon::DungeonId::Undercity,
        },
        ImperativeFamilyAst::TakeTheInitiative => Effect::TakeTheInitiative,
        ImperativeFamilyAst::Proliferate => Effect::Proliferate,
        // CR 701.56a: Time travel.
        ImperativeFamilyAst::TimeTravel => Effect::TimeTravel,
        // CR 701.36a: Populate.
        ImperativeFamilyAst::Populate => Effect::Populate,
        // CR 701.30: Clash with an opponent.
        ImperativeFamilyAst::Clash => Effect::Clash,
        ImperativeFamilyAst::GainKeyword(effect) => effect,
        ImperativeFamilyAst::LoseKeyword(effect) => effect,
        ImperativeFamilyAst::LoseTheGame => Effect::LoseTheGame,
        ImperativeFamilyAst::WinTheGame => Effect::WinTheGame,
        ImperativeFamilyAst::RollDie { sides } => Effect::RollDie {
            sides,
            results: vec![],
        },
        ImperativeFamilyAst::FlipCoin => Effect::FlipCoin {
            win_effect: None,
            lose_effect: None,
        },
        ImperativeFamilyAst::FlipCoins { count } => Effect::FlipCoins {
            count,
            win_effect: None,
            lose_effect: None,
        },
        ImperativeFamilyAst::FlipCoinUntilLose => Effect::FlipCoinUntilLose {
            // Stub — subsequent "For each flip you won, ..." clauses are
            // consolidated into this by consolidate_die_and_coin_defs.
            win_effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "flip_coin_until_lose_stub".to_string(),
                    description: Some("pending consolidation".to_string()),
                },
            )),
        },
        ImperativeFamilyAst::Put(ast) => lower_put_ast(ast),
        ImperativeFamilyAst::YouMay { text } => super::parse_effect(&text),
        // CR 122.1: Player counter manipulation. Target is set by subject injection.
        ImperativeFamilyAst::GivePlayerCounter {
            counter_kind,
            count,
        } => Effect::GivePlayerCounter {
            counter_kind,
            count,
            target: TargetFilter::Controller,
        },
        // CR 506.4: Remove from combat.
        ImperativeFamilyAst::RemoveFromCombat(target) => Effect::RemoveFromCombat { target },
        // Shuffle and Support are handled in `lower_imperative_family_ast` directly.
        ImperativeFamilyAst::Shuffle(_) | ImperativeFamilyAst::Support { .. } => unreachable!(),
    }
}

pub(super) fn parse_zone_counter_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ZoneCounterImperativeAst> {
    if let Some(ast) = parse_destroy_ast(text, lower) {
        return Some(ast);
    }
    if let Some(ast) = parse_exile_ast(text, lower, ctx) {
        return Some(ast);
    }
    if let Some(ast) = parse_counter_ast(text, lower) {
        return Some(ast);
    }
    if tag::<_, _, VerboseError<&str>>("put ").parse(lower).is_ok()
        && nom_primitives::scan_contains(lower, "counter")
    {
        // Try move-counters first ("put its counters on ...")
        if let Some((
            Effect::MoveCounters {
                source,
                counter_type,
                target,
            },
            _rem,
        )) = super::counter::try_parse_move_counters(lower, text)
        {
            return Some(ZoneCounterImperativeAst::MoveCounters {
                source,
                counter_type,
                target,
            });
        }
        // CR 122.1: Multi-typed counter list ("put a flying counter, a first
        // strike counter, and a lifelink counter on that creature"). Must run
        // before the single-counter path — `try_parse_put_counter_chain` only
        // returns `Some` when it consumed >=2 entries, so single-counter cases
        // fall through untouched.
        if let Some((entries, target, _rem, multi_target)) =
            super::counter::try_parse_put_counter_chain(lower, text, ctx)
        {
            return Some(ZoneCounterImperativeAst::PutCounterList {
                entries,
                target,
                multi_target,
            });
        }
        // Then fixed-count put ("put N counter(s) on ...")
        // Detect "each"/"all" to route to PutCounterAll (mass placement without targeting).
        // CR 122.1: "on each" and "on all" indicate mass application. The "counter(s)"
        // anchor handles the common case; the bare "on each "/"on all " fallbacks
        // cover phrases where a quantity clause ("equal to its power") intervenes
        // between the counter noun and the target — e.g. Gruff Triplets:
        // "put a number of +1/+1 counters equal to its power on each creature you
        // control named ~".
        let is_all = nom_primitives::scan_contains(lower, "counter on each")
            || nom_primitives::scan_contains(lower, "counters on each")
            || nom_primitives::scan_contains(lower, "counter on all")
            || nom_primitives::scan_contains(lower, "counters on all")
            || nom_primitives::scan_contains(lower, "on each ")
            || nom_primitives::scan_contains(lower, "on all ");
        return match try_parse_put_counter(lower, text, ctx) {
            Some((
                Effect::PutCounter {
                    counter_type,
                    count,
                    target,
                },
                _remainder,
                multi_target,
            )) => {
                if is_all && multi_target.is_none() {
                    Some(ZoneCounterImperativeAst::PutCounterAll {
                        counter_type,
                        count,
                        target,
                    })
                } else {
                    Some(ZoneCounterImperativeAst::PutCounter {
                        counter_type,
                        count,
                        target,
                    })
                }
            }
            _ => None,
        };
    }
    if tag::<_, _, VerboseError<&str>>("remove ")
        .parse(lower)
        .is_ok()
        && nom_primitives::scan_contains(lower, "counter")
    {
        return match try_parse_remove_counter(lower, ctx) {
            Some(Effect::RemoveCounter {
                counter_type,
                count,
                target,
            }) => Some(ZoneCounterImperativeAst::RemoveCounter {
                counter_type,
                count,
                target,
            }),
            _ => None,
        };
    }
    // CR 122.5: "move [N] [type] counter(s) from [source] onto/to [target]"
    if tag::<_, _, VerboseError<&str>>("move ")
        .parse(lower)
        .is_ok()
        && nom_primitives::scan_contains(lower, "counter")
    {
        if let Some(Effect::MoveCounters {
            source,
            counter_type,
            target,
        }) = try_parse_move_counters_from(lower, ctx)
        {
            return Some(ZoneCounterImperativeAst::MoveCounters {
                source,
                counter_type,
                target,
            });
        }
    }
    None
}

pub(super) fn lower_zone_counter_ast(ast: ZoneCounterImperativeAst) -> Effect {
    match ast {
        ZoneCounterImperativeAst::Destroy { target, all } => {
            if all {
                Effect::DestroyAll {
                    target,
                    cant_regenerate: false,
                }
            } else {
                Effect::Destroy {
                    target,
                    cant_regenerate: false,
                }
            }
        }
        ZoneCounterImperativeAst::Exile {
            origin,
            target,
            all,
        } => {
            if all {
                Effect::ChangeZoneAll {
                    origin,
                    destination: Zone::Exile,
                    target,
                }
            } else {
                Effect::ChangeZone {
                    origin,
                    destination: Zone::Exile,
                    target,
                    owner_library: false,
                    enter_transformed: false,
                    under_your_control: false,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                }
            }
        }
        ZoneCounterImperativeAst::ExileTop { player, count } => Effect::ExileTop { player, count },
        ZoneCounterImperativeAst::Counter {
            target,
            source_static,
            unless_payment,
            all,
        } => {
            if all {
                // CR 701.6 + CR 405.1: Mass counter. Drops `source_static` and
                // `unless_payment` — neither has a corpus card combining them
                // with mass counter, and the runtime resolver does not honor
                // those slots on `Effect::CounterAll`. (Source-static is a
                // per-target-permanent silencing pattern; unless-pays is a
                // controller opt-out that doesn't apply to non-targeting
                // mass effects per CR 115.1.)
                Effect::CounterAll { target }
            } else {
                Effect::Counter {
                    target,
                    source_static: source_static.map(|s| *s),
                    unless_payment,
                }
            }
        }
        ZoneCounterImperativeAst::PutCounter {
            counter_type,
            count,
            target,
        } => Effect::PutCounter {
            counter_type,
            count,
            target,
        },
        // CR 122.1: PutCounterList is always intercepted upstream in
        // `lower_imperative_family_ast` because it lowers to a sub_ability
        // chain that a bare Effect can't express. If execution reaches here
        // (e.g., via `TargetedImperativeAst::ZoneCounterProxy` in a compound
        // action, which only carries single-counter variants), degrade
        // gracefully to the first entry rather than panicking.
        ZoneCounterImperativeAst::PutCounterList {
            mut entries,
            target,
            ..
        } => {
            if let Some((counter_type, count)) = entries.drain(..).next() {
                Effect::PutCounter {
                    counter_type,
                    count,
                    target,
                }
            } else {
                Effect::Unimplemented {
                    name: "put_counter_list_empty".to_string(),
                    description: None,
                }
            }
        }
        ZoneCounterImperativeAst::PutCounterAll {
            counter_type,
            count,
            target,
        } => Effect::PutCounterAll {
            counter_type,
            count,
            target,
        },
        ZoneCounterImperativeAst::RemoveCounter {
            counter_type,
            count,
            target,
        } => Effect::RemoveCounter {
            counter_type,
            count,
            target,
        },
        ZoneCounterImperativeAst::MoveCounters {
            source,
            counter_type,
            target,
        } => Effect::MoveCounters {
            source,
            counter_type,
            target,
        },
    }
}

/// CR 701.53a: Parse "incubate {N}" from Oracle text.
///
/// Handles numeric and "X" counts via shared `parse_count_expr`.
fn try_parse_incubate(lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("incubate ")
        .parse(lower)
        .ok()?;
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }

    // CR 716.1: Incubate's count is part of the keyword action; if it doesn't
    // parse, return None so the line lands in Unimplemented rather than
    // fabricating "incubate 1".
    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Incubate { count })
}

/// CR 701.47a: Parse "amass {Type} {N}" from Oracle text.
///
/// Handles all subtypes generically. The subtype is canonicalized from plural
/// to singular form (e.g., "Zombies" -> "Zombie") via `parse_subtype`.
fn try_parse_amass(text: &str, lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("amass ")
        .parse(lower)
        .ok()?;
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }

    // Parse subtype from original text (preserving case for parse_subtype)
    let original_rest = text[text.len() - rest.len()..].trim();
    let (subtype, consumed) = crate::parser::oracle_util::parse_subtype(original_rest)?;
    let remainder = rest[consumed..].trim();

    // CR 701.47a: Amass requires an explicit count after the subtype. If it
    // doesn't parse, surface as Unimplemented rather than amassing 1.
    let count = parse_count_expr(remainder).map(|(q, _)| q)?;

    Some(Effect::Amass { subtype, count })
}

/// CR 701.37a: Parse "monstrosity {N}" from Oracle text.
///
/// Used inside activated ability effect text (after the colon).
fn try_parse_monstrosity(lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("monstrosity ")
        .parse(lower)
        .ok()?;
    let rest = rest.trim().trim_end_matches('.');

    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Monstrosity { count })
}

/// CR 701.46a: Parse "adapt N" from Oracle text.
///
/// Used inside activated ability effect text (after the colon).
fn try_parse_adapt(lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("adapt ")
        .parse(lower)
        .ok()?;
    let rest = rest.trim().trim_end_matches('.');

    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Adapt { count })
}

/// CR 508.1d: Parse "attacks/attack [player] this turn/combat if able" as a temporary MustAttack.
///
/// Handles bare forms ("attacks this turn if able") and player-targeted forms
/// ("attacks you this turn if able", "attacks that opponent this combat if able",
/// "attacks target opponent this turn if able"). The player target is currently
/// not enforced at runtime — MustAttack forces the creature to attack if able,
/// but the specific-player constraint requires additional engine support.
///
/// Emits a `GenericEffect` with `StaticMode::MustAttack` and the appropriate duration.
fn try_parse_attack_if_able(lower: &str) -> Option<ImperativeFamilyAst> {
    use crate::types::statics::StaticMode;

    let trimmed = lower.trim_end_matches('.');

    // First try: bare forms without a player reference.
    let result: Result<(&str, Duration), nom::Err<VerboseError<&str>>> = alt((
        value(Duration::UntilEndOfTurn, tag("attacks this turn if able")),
        value(Duration::UntilEndOfTurn, tag("attack this turn if able")),
        value(
            Duration::UntilEndOfCombat,
            tag("attacks this combat if able"),
        ),
        value(
            Duration::UntilEndOfCombat,
            tag("attack this combat if able"),
        ),
        value(
            Duration::UntilEndOfCombat,
            tag("attacks that combat if able"),
        ),
        value(
            Duration::UntilEndOfCombat,
            tag("attack that combat if able"),
        ),
    ))
    .parse(trimmed);

    if let Ok((_, duration)) = result {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(StaticMode::MustAttack)],
            duration: Some(duration),
            target: None,
        }));
    }

    // Second try: player-targeted forms — "attacks [player] this turn/combat if able".
    // Strip verb prefix, skip over the player phrase, then match the duration suffix.
    let rest = if let Ok((r, _)) =
        alt((tag::<_, _, VerboseError<&str>>("attacks "), tag("attack "))).parse(trimmed)
    {
        r
    } else {
        return None;
    };

    // Match duration suffix: "this turn if able" or "this combat if able"
    let duration_suffix: Result<(&str, Duration), nom::Err<VerboseError<&str>>> = alt((
        value(Duration::UntilEndOfTurn, tag(" this turn if able")),
        value(Duration::UntilEndOfCombat, tag(" this combat if able")),
        value(Duration::UntilEndOfCombat, tag(" each combat if able")),
    ))
    .parse(rest);

    // If a duration suffix is found somewhere in the remaining text,
    // the player phrase is whatever sits between the verb and the suffix.
    if duration_suffix.is_err() {
        // Try scanning for the suffix by finding it anywhere after the verb
        for (suffix_tag, dur) in [
            (" this turn if able", Duration::UntilEndOfTurn),
            (" this combat if able", Duration::UntilEndOfCombat),
            (" each combat if able", Duration::UntilEndOfCombat),
        ] {
            if rest.ends_with(suffix_tag) {
                return Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
                    static_abilities: vec![StaticDefinition::new(StaticMode::MustAttack)],
                    duration: Some(dur),
                    target: None,
                }));
            }
        }
        return None;
    }

    let (_, duration) = duration_suffix.ok()?;

    Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::new(StaticMode::MustAttack)],
        duration: Some(duration),
        target: None,
    }))
}

/// Handles "can't be blocked [this turn]", "can't attack [this turn]", "can't block [this turn]",
/// and compound forms like "can't attack or block". These delegate to the subject.rs
/// static-granting machinery, wrapping the result in a `GenericEffect`.
fn try_parse_subjectless_cant(lower: &str) -> Option<ImperativeFamilyAst> {
    use crate::parser::oracle_effect::subject::parse_restriction_modes;

    let trimmed = lower.trim_end_matches('.');

    // Determine duration from the suffix: "this combat" → UntilEndOfCombat,
    // "this turn" (or bare) → UntilEndOfTurn.
    let (clean, duration) = if let Some(c) = trimmed.strip_suffix(" this combat") {
        (c, Duration::UntilEndOfCombat)
    } else if let Some(c) = trimmed.strip_suffix(" this turn") {
        (c, Duration::UntilEndOfTurn)
    } else {
        (trimmed, Duration::UntilEndOfTurn)
    };

    let modes = parse_restriction_modes(clean)?;
    let statics: Vec<StaticDefinition> = modes.into_iter().map(StaticDefinition::new).collect();
    Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
        static_abilities: statics,
        duration: Some(duration),
        target: None,
    }))
}

/// CR 701.39a: Parse "bolster N" from Oracle text.
fn try_parse_bolster(lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("bolster ")
        .parse(lower)
        .ok()?;
    let rest = rest.trim().trim_end_matches('.');

    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Bolster { count })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CR 701.27 + CR 608.2k: "transform it" / "convert itself" — bare object
    /// pronoun resolves to the trigger source. Building-block coverage for the
    /// full SelfRef pronoun surface so all DFC self-transform sub-abilities
    /// (Primal Amulet class) reach the Transform handler instead of falling
    /// through to Unimplemented.
    #[test]
    fn parse_transform_self_pronouns() {
        for input in [
            "transform it",
            "transform itself",
            "convert it",
            "convert itself",
        ] {
            let result = parse_utility_imperative_ast(input, input);
            let Some(UtilityImperativeAst::Transform { target }) = result else {
                panic!("{input}: expected Transform, got {result:?}");
            };
            assert!(
                matches!(target, TargetFilter::SelfRef),
                "{input}: target SelfRef, got {target:?}"
            );
        }
    }

    #[test]
    fn parse_attach_triggering_object_to_last_created_token() {
        let input = "attach it to the token";
        let result = parse_utility_imperative_ast(input, input);
        let Some(UtilityImperativeAst::Attach { attachment, target }) = result else {
            panic!("{input}: expected Attach, got {result:?}");
        };
        assert_eq!(attachment, TargetFilter::TriggeringSource);
        assert_eq!(target, TargetFilter::LastCreated);
    }

    #[test]
    fn parse_attach_target_equipment_to_self_pronoun() {
        let input = "attach up to one target Equipment you control to her";
        let lower = input.to_lowercase();
        let result = parse_utility_imperative_ast(input, &lower);
        let Some(UtilityImperativeAst::Attach { attachment, target }) = result else {
            panic!("{input}: expected Attach, got {result:?}");
        };
        assert_eq!(
            attachment,
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Equipment".to_string())
                    .controller(ControllerRef::You)
            )
        );
        assert_eq!(target, TargetFilter::SelfRef);
    }

    #[test]
    fn parse_attach_target_equipment_to_target_creature() {
        let input = "attach target Equipment you control to target creature you control";
        let lower = input.to_lowercase();
        let result = parse_utility_imperative_ast(input, &lower);
        let Some(UtilityImperativeAst::Attach { attachment, target }) = result else {
            panic!("{input}: expected Attach, got {result:?}");
        };
        assert_eq!(
            attachment,
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Equipment".to_string())
                    .controller(ControllerRef::You)
            )
        );
        assert_eq!(
            target,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
        );
    }

    #[test]
    fn parse_gain_life_equal_to_life_lost() {
        let text = "gain life equal to the life you've lost this turn";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        match result {
            Some(NumericImperativeAst::GainLife { amount }) => {
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::LifeLostThisTurn { .. }
                        }
                    ),
                    "Expected LifeLostThisTurn, got {amount:?}"
                );
            }
            other => panic!("Expected GainLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_earthbend_verb() {
        let text = "Earthbend 3 target land";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse 'earthbend' verb");
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::Animate {
                power,
                toughness,
                keywords,
                ..
            } => {
                assert_eq!(power, Some(3));
                assert_eq!(toughness, Some(3));
                assert!(keywords.contains(&crate::types::keywords::Keyword::Haste));
            }
            other => panic!("Expected Effect::Animate, got {other:?}"),
        }
    }

    // CR 701.21a + CR 608.2k: When the trigger body is "you (may) sacrifice
    // [filter]", the actor hint piped through ParseContext.actor must default
    // the parsed `TargetFilter::Typed.controller` to ControllerRef::You so the
    // resolver restricts the prompt to the actor's permanents — sacrificing
    // requires controlling the permanent, never an opponent's.
    #[test]
    fn parse_sacrifice_defaults_controller_to_you_actor() {
        let text = "sacrifice a non-Demon creature";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::You),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => assert_eq!(
                    tf.controller,
                    Some(ControllerRef::You),
                    "Promise of Aclazotz: controller must default to You, got {tf:?}"
                ),
                other => panic!("expected Typed target, got {other:?}"),
            },
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    // CR 701.21a: Symmetric handling — "an opponent (may) sacrifices [filter]"
    // routes ControllerRef::Opponent into the parsed Sacrifice target.
    #[test]
    fn parse_sacrifice_defaults_controller_to_opponent_actor() {
        let text = "sacrifice a creature";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::Opponent),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => assert_eq!(tf.controller, Some(ControllerRef::Opponent)),
                other => panic!("expected Typed target, got {other:?}"),
            },
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn parse_sacrifice_preserves_choice_relative_clause() {
        let text = "sacrifice a permanent of their choice that shares a card type with it";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::Opponent),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                    assert!(tf.type_filters.iter().any(|type_filter| matches!(
                        type_filter,
                        crate::types::ability::TypeFilter::Permanent
                    )));
                    assert!(tf.properties.iter().any(|prop| matches!(
                        prop,
                        crate::types::ability::FilterProp::SharesQuality {
                            quality: crate::types::ability::SharedQuality::CardType,
                            reference: Some(reference),
                            relation: crate::types::ability::SharedQualityRelation::Shares,
                        } if matches!(reference.as_ref(), TargetFilter::ParentTarget)
                    )));
                }
                other => panic!("expected Typed target, got {other:?}"),
            },
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn parse_each_opponent_may_sacrifice_preserves_choice_relative_clause() {
        let def = super::super::parse_effect_chain(
            "each opponent may sacrifice a permanent of their choice that shares a card type with it",
            AbilityKind::Spell,
        );

        assert!(def.optional, "expected optional sacrifice");
        assert_eq!(
            def.player_scope,
            Some(crate::types::ability::PlayerFilter::Opponent)
        );
        match &*def.effect {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                    assert!(tf.type_filters.iter().any(|type_filter| matches!(
                        type_filter,
                        crate::types::ability::TypeFilter::Permanent
                    )));
                    assert!(tf.properties.iter().any(|prop| matches!(
                        prop,
                        crate::types::ability::FilterProp::SharesQuality {
                            quality: crate::types::ability::SharedQuality::CardType,
                            reference: Some(reference),
                            relation: crate::types::ability::SharedQualityRelation::Shares,
                        } if matches!(reference.as_ref(), TargetFilter::ParentTarget)
                    )));
                }
                other => panic!("expected Typed target, got {other:?}"),
            },
            other => panic!("expected Sacrifice effect, got {other:?}"),
        }
    }

    // CR 701.21a: An explicit controller phrase in the target text must NOT be
    // overwritten by the actor default. "Sacrifice a creature an opponent
    // controls" stays Some(Opponent) even when ctx.actor = Some(You).
    #[test]
    fn parse_sacrifice_preserves_explicit_controller() {
        let text = "sacrifice a creature an opponent controls";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::You),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => assert_eq!(
                    tf.controller,
                    Some(ControllerRef::Opponent),
                    "explicit controller must be preserved, got {tf:?}"
                ),
                other => panic!("expected Typed target, got {other:?}"),
            },
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    // Regression guard: without an actor hint (ctx.actor = None), the legacy
    // `controller: None` behavior is preserved. Establishes that the default is
    // strictly opt-in — non-trigger contexts (activated abilities) still rely on
    // the existing `ability.controller` resolver path.
    #[test]
    fn parse_sacrifice_without_actor_leaves_controller_unset() {
        let text = "sacrifice a creature";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext::default();
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => assert!(
                    tf.controller.is_none(),
                    "no actor hint should leave controller unset, got {tf:?}"
                ),
                other => panic!("expected Typed target, got {other:?}"),
            },
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    // CR 722.1: Mindslaver's declarative "You control target player during that
    // player's next turn" must route through the ControlNextTurn combinator.
    #[test]
    fn parse_mindslaver_control_next_turn() {
        let text = "You control target player during that player's next turn.";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(
            result.is_some(),
            "Should parse Mindslaver's 'you control ...' declarative"
        );
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::ControlNextTurn {
                target,
                grant_extra_turn_after,
            } => {
                assert!(matches!(target, TargetFilter::Player));
                assert!(!grant_extra_turn_after);
            }
            other => panic!("Expected Effect::ControlNextTurn, got {other:?}"),
        }
    }

    // CR 722.1: variant that grants an extra turn afterward (e.g., Emrakul-style).
    #[test]
    fn parse_control_next_turn_with_extra_turn_tail() {
        let text = "You control target player during that player's next turn. \
                    After that turn, that player takes an extra turn.";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some());
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::ControlNextTurn {
                grant_extra_turn_after,
                ..
            } => assert!(grant_extra_turn_after),
            other => panic!("Expected Effect::ControlNextTurn, got {other:?}"),
        }
    }

    // Regression guard: Mindslaver Toolkit's "Target opponent gains control of"
    // still parses to GainControl (not ControlNextTurn) after the refactor.
    #[test]
    fn parse_gain_control_of_not_control_next_turn() {
        let text = "Target opponent gains control of Mindslaver Toolkit";
        let lower = text.to_lowercase();
        // Subject-strip happens upstream; imperative dispatcher sees the
        // stripped form "gain control of Mindslaver Toolkit".
        let stripped = "gain control of Mindslaver Toolkit";
        let stripped_lower = stripped.to_lowercase();
        let result =
            parse_targeted_action_ast(stripped, &stripped_lower, &mut ParseContext::default());
        assert!(result.is_some());
        let effect = lower_targeted_action_ast(result.unwrap());
        assert!(
            matches!(effect, Effect::GainControl { .. }),
            "Expected GainControl, got {effect:?}"
        );
        let _ = (text, lower);
    }

    #[test]
    fn parse_airbend_verb() {
        let text = "Airbend target creature {2}";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse 'airbend' verb");
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::GrantCastingPermission { permission, .. } => {
                assert!(
                    matches!(
                        permission,
                        crate::types::ability::CastingPermission::ExileWithAltCost { ref cost, .. }
                            if matches!(cost, crate::types::mana::ManaCost::Cost { generic: 2, .. })
                    ),
                    "Expected ExileWithAltCost with {{2}}, got {permission:?}"
                );
            }
            other => panic!("Expected Effect::GrantCastingPermission, got {other:?}"),
        }
    }

    #[test]
    fn parse_airbend_up_to_one_other_target_creature_or_spell() {
        let text = "Airbend up to one other target creature or spell {2}";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default())
            .expect("Should parse airbend");
        let effect = lower_targeted_action_ast(result);
        match effect {
            Effect::GrantCastingPermission {
                permission,
                target: crate::types::ability::TargetFilter::Or { filters },
                ..
            } => {
                assert!(matches!(
                    permission,
                    crate::types::ability::CastingPermission::ExileWithAltCost { ref cost, .. }
                        if matches!(cost, crate::types::mana::ManaCost::Cost { generic: 2, .. })
                ));
                assert!(
                    filters.iter().any(|filter| matches!(
                        filter,
                        crate::types::ability::TargetFilter::Typed(tf)
                            if tf.type_filters.contains(&crate::types::ability::TypeFilter::Creature)
                                && tf.properties.contains(&crate::types::ability::FilterProp::Another)
                    )),
                    "expected creature branch with Another, got {filters:?}"
                );
                assert!(
                    filters.iter().any(|filter| matches!(
                        filter,
                        crate::types::ability::TargetFilter::Typed(tf)
                            if tf.type_filters.contains(&crate::types::ability::TypeFilter::Card)
                                && tf.properties.contains(&crate::types::ability::FilterProp::Another)
                    )),
                    "expected spell branch with Another, got {filters:?}"
                );
            }
            other => panic!(
                "Expected GrantCastingPermission with creature-or-spell target, got {other:?}"
            ),
        }
    }

    #[test]
    fn parse_earthbend_default_pt() {
        let text = "Earthbend target land";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some());
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::Animate {
                power, toughness, ..
            } => {
                assert_eq!(power, Some(0));
                assert_eq!(toughness, Some(0));
            }
            other => panic!("Expected Effect::Animate, got {other:?}"),
        }
    }

    #[test]
    fn parse_earthbend_no_explicit_target() {
        // After reminder text stripping, "Earthbend 2." has no explicit target.
        // Should default to "target land you control" per keyword definition.
        let text = "Earthbend 2.";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(
            result.is_some(),
            "Should parse 'earthbend' without explicit target"
        );
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::Animate {
                power,
                toughness,
                target,
                ..
            } => {
                assert_eq!(power, Some(2));
                assert_eq!(toughness, Some(2));
                assert_eq!(
                    target,
                    default_earthbend_target(),
                    "Should default to land you control"
                );
            }
            other => panic!("Expected Effect::Animate, got {other:?}"),
        }
    }

    /// CR 122.1: literal-N earthbend keeps the `Fixed` count path intact.
    #[test]
    fn earthbend_count_expr_literal_n() {
        let (target, count) = parse_earthbend_count_expr("2", "2");
        assert_eq!(count, QuantityExpr::Fixed { value: 2 });
        assert_eq!(target, default_earthbend_target());
    }

    /// CR 122.1: Toph's "earthbend X, where X is the number of experience
    /// counters you have" produces a typed PlayerCounter ref, not Fixed 0.
    #[test]
    fn earthbend_count_expr_x_with_player_counter_tail() {
        let tail = "x, where x is the number of experience counters you have";
        let (_, count) = parse_earthbend_count_expr(tail, tail);
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCounter {
                    kind: PlayerCounterKind::Experience,
                    scope: crate::types::ability::CountScope::Controller,
                },
            }
        );
    }

    /// CR 107.3a + CR 601.2b: bare "earthbend X" without a where-clause defers
    /// to the spell-cost X resolution path (Variable("X")), not Fixed 0.
    #[test]
    fn earthbend_count_expr_bare_x_falls_through_to_variable() {
        let (_, count) = parse_earthbend_count_expr("x", "x");
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            }
        );
    }

    #[test]
    fn parse_lose_half_their_life_rounded_up_trace() {
        // Class-level trace: make sure "lose half their life, rounded up"
        // produces a typed HalfRounded amount at the imperative level.
        let text = "lose half their life, rounded up";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(result.is_some(), "Should parse; got {result:?}");
        match result.unwrap() {
            NumericImperativeAst::LoseLife { amount } => {
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::HalfRounded {
                            rounding: crate::types::ability::RoundingMode::Up,
                            ..
                        }
                    ),
                    "Expected HalfRounded(Up), got {amount:?}"
                );
            }
            other => panic!("Expected LoseLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_lose_life_equal_to_mana_value() {
        let text = "loses life equal to that card's mana value";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(result.is_some(), "Should parse 'loses life equal to'");
        match result.unwrap() {
            NumericImperativeAst::LoseLife { amount } => {
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::Ref {
                            qty: QuantityRef::EventContextSourceManaValue
                        }
                    ),
                    "Expected EventContextSourceManaValue, got {amount:?}"
                );
            }
            other => panic!("Expected LoseLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_gain_life_equal_to_power() {
        let text = "gain life equal to its power";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(result.is_some(), "Should parse 'gain life equal to'");
        match result.unwrap() {
            NumericImperativeAst::GainLife { amount } => {
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::Ref {
                            qty: QuantityRef::EventContextSourcePower
                        }
                    ),
                    "Expected EventContextSourcePower, got {amount:?}"
                );
            }
            other => panic!("Expected GainLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_amass_zombies_2() {
        let result = try_parse_amass("amass Zombies 2", "amass zombies 2");
        assert!(result.is_some(), "Should parse 'amass Zombies 2'");
        match result.unwrap() {
            Effect::Amass { subtype, count } => {
                assert_eq!(subtype, "Zombie");
                assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
            }
            other => panic!("Expected Amass, got {other:?}"),
        }
    }

    #[test]
    fn parse_amass_orcs_3() {
        let result = try_parse_amass("amass Orcs 3", "amass orcs 3");
        assert!(result.is_some(), "Should parse 'amass Orcs 3'");
        match result.unwrap() {
            Effect::Amass { subtype, count } => {
                assert_eq!(subtype, "Orc");
                assert!(matches!(count, QuantityExpr::Fixed { value: 3 }));
            }
            other => panic!("Expected Amass, got {other:?}"),
        }
    }

    #[test]
    fn parse_amass_zombies_x() {
        let result = try_parse_amass("amass Zombies X", "amass zombies x");
        assert!(result.is_some(), "Should parse 'amass Zombies X'");
        match result.unwrap() {
            Effect::Amass { subtype, count } => {
                assert_eq!(subtype, "Zombie");
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Variable { .. }
                    }
                ));
            }
            other => panic!("Expected Amass, got {other:?}"),
        }
    }

    #[test]
    fn parse_monstrosity_4() {
        let result = try_parse_monstrosity("monstrosity 4.");
        assert!(result.is_some(), "Should parse 'monstrosity 4.'");
        match result.unwrap() {
            Effect::Monstrosity { count } => {
                assert!(matches!(count, QuantityExpr::Fixed { value: 4 }));
            }
            other => panic!("Expected Monstrosity, got {other:?}"),
        }
    }

    #[test]
    fn parse_monstrosity_x() {
        let result = try_parse_monstrosity("monstrosity x.");
        assert!(result.is_some(), "Should parse 'monstrosity X.'");
        match result.unwrap() {
            Effect::Monstrosity { count } => {
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Variable { .. }
                    }
                ));
            }
            other => panic!("Expected Monstrosity, got {other:?}"),
        }
    }

    #[test]
    fn parse_get_a_poison_counter() {
        let result = try_parse_player_counter("get a poison counter");
        assert!(result.is_some(), "Should parse 'get a poison counter'");
        match result.unwrap() {
            ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            } => {
                assert_eq!(counter_kind, PlayerCounterKind::Poison);
                assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    #[test]
    fn parse_gets_two_experience_counters() {
        let result = try_parse_player_counter("gets two experience counters");
        assert!(
            result.is_some(),
            "Should parse 'gets two experience counters'"
        );
        match result.unwrap() {
            ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            } => {
                assert_eq!(counter_kind, PlayerCounterKind::Experience);
                assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    #[test]
    fn parse_get_three_rad_counters() {
        let result = try_parse_player_counter("get three rad counters");
        assert!(result.is_some(), "Should parse 'get three rad counters'");
        match result.unwrap() {
            ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            } => {
                assert_eq!(counter_kind, PlayerCounterKind::Rad);
                assert!(matches!(count, QuantityExpr::Fixed { value: 3 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    #[test]
    fn parse_get_an_experience_counter() {
        let result = try_parse_player_counter("get an experience counter");
        assert!(result.is_some(), "Should parse 'get an experience counter'");
        match result.unwrap() {
            ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            } => {
                assert_eq!(counter_kind, PlayerCounterKind::Experience);
                assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    #[test]
    fn parse_player_counter_rejects_plus1_counter() {
        // "+1/+1 counter" is an object counter, not a player counter
        let result = try_parse_player_counter("gets a +1/+1 counter");
        assert!(
            result.is_none(),
            "Should NOT parse '+1/+1 counter' as player counter"
        );
    }

    #[test]
    fn parse_player_counter_rejects_unknown_type() {
        // "charge counter" is an object counter, not a known player counter
        let result = try_parse_player_counter("get a charge counter");
        assert!(
            result.is_none(),
            "Should NOT parse unknown counter type as player counter"
        );
    }

    #[test]
    fn parse_additional_phase_phase() {
        let text = "there is an additional combat phase after this phase";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse additional combat phase");
        let effect = lower_imperative_family_effect(result.unwrap());
        assert!(
            matches!(
                effect,
                Effect::AdditionalPhase {
                    phase: Phase::BeginCombat,
                    after: Phase::EndCombat,
                    ref followed_by,
                    ..
                } if followed_by.is_empty()
            ),
            "Expected AdditionalPhase without main phase, got {effect:?}"
        );
    }

    #[test]
    fn parse_additional_phase_with_main_phase() {
        let text = "there is an additional combat phase followed by an additional main phase";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse additional combat + main");
        let effect = lower_imperative_family_effect(result.unwrap());
        assert!(
            matches!(
                effect,
                Effect::AdditionalPhase {
                    phase: Phase::BeginCombat,
                    after: Phase::EndCombat,
                    ref followed_by,
                    ..
                } if followed_by == &vec![Phase::PostCombatMain]
            ),
            "Expected AdditionalPhase with main phase, got {effect:?}"
        );
    }

    #[test]
    fn parse_additional_upkeep_step() {
        let text = "get an additional upkeep step after this step";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse additional upkeep step");
        let effect = lower_imperative_family_effect(result.unwrap());
        assert!(
            matches!(
                effect,
                Effect::AdditionalPhase {
                    phase: Phase::Upkeep,
                    after: Phase::Upkeep,
                    ref followed_by,
                    ..
                } if followed_by.is_empty()
            ),
            "Expected AdditionalPhase for upkeep step, got {effect:?}"
        );
    }

    #[test]
    fn parse_after_this_phase_additional_phase() {
        let text = "after this phase, there is an additional combat phase";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse 'after this phase' variant");
    }

    #[test]
    fn parse_discard_your_hand() {
        let text = "discard your hand";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Discard { count, .. }) => {
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::HandSize {
                                player: PlayerScope::Controller
                            }
                        }
                    ),
                    "Expected HandSize ref, got {count:?}"
                );
            }
            other => panic!("Expected Discard with HandSize, got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_their_hand() {
        let text = "discard their hand";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Discard { count, .. }) => {
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::HandSize {
                                player: PlayerScope::Controller
                            }
                        }
                    ),
                    "Expected HandSize ref, got {count:?}"
                );
            }
            other => panic!("Expected Discard with HandSize, got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_a_card_regression() {
        let text = "discard a card";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Discard { count, .. }) => {
                assert!(
                    matches!(count, QuantityExpr::Fixed { value: 1 }),
                    "Expected Fixed(1), got {count:?}"
                );
            }
            other => panic!("Expected Discard with Fixed(1), got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_two_cards_regression() {
        let text = "discard two cards";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Discard { count, .. }) => {
                assert!(
                    matches!(count, QuantityExpr::Fixed { value: 2 }),
                    "Expected Fixed(2), got {count:?}"
                );
            }
            other => panic!("Expected Discard with Fixed(2), got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_unless_creature() {
        let text = "discard two cards unless you discard a creature card";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Discard {
                count,
                unless_filter,
                ..
            }) => {
                assert!(
                    matches!(count, QuantityExpr::Fixed { value: 2 }),
                    "Expected Fixed(2), got {count:?}"
                );
                assert!(
                    unless_filter.is_some(),
                    "Expected unless_filter for creature"
                );
            }
            other => panic!("Expected Discard with unless_filter, got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_unless_pirate() {
        let text = "discard two cards unless you discard a Pirate card";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Discard {
                count,
                unless_filter,
                ..
            }) => {
                assert!(
                    matches!(count, QuantityExpr::Fixed { value: 2 }),
                    "Expected Fixed(2), got {count:?}"
                );
                assert!(
                    unless_filter.is_some(),
                    "Expected unless_filter for Pirate subtype"
                );
            }
            other => panic!("Expected Discard with unless_filter, got {other:?}"),
        }
    }

    #[test]
    fn parse_support_on_spell() {
        // CR 701.41a: Support N on an instant/sorcery — "up to N target creatures"
        let text = "support 2";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext::default(); // No subject = spell context
        let ast = parse_imperative_family_ast(text, &lower, &mut ctx);
        assert!(
            matches!(
                &ast,
                Some(ImperativeFamilyAst::Support {
                    count: 2,
                    is_other: false
                })
            ),
            "Expected Support {{ count: 2, is_other: false }}, got {ast:?}"
        );
        let clause = lower_imperative_family_ast(ast.unwrap());
        assert!(
            matches!(
                &clause.effect,
                Effect::PutCounter { counter_type, count: QuantityExpr::Fixed { value: 1 }, .. }
                if counter_type == "P1P1"
            ),
            "Expected PutCounter P1P1, got {:?}",
            clause.effect
        );
        assert_eq!(clause.multi_target, Some(MultiTargetSpec::fixed(0, 2)));
        // Spell support should NOT have Another property
        if let Effect::PutCounter {
            target: TargetFilter::Typed(tf),
            ..
        } = &clause.effect
        {
            assert!(
                !tf.properties
                    .contains(&crate::types::ability::FilterProp::Another),
                "Spell support should not use 'other'"
            );
        }
    }

    #[test]
    fn parse_support_on_permanent() {
        // CR 701.41a: Support N on a permanent — "up to N other target creatures"
        let text = "support 3";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            subject: Some(TargetFilter::SelfRef),
            ..Default::default()
        };
        let ast = parse_imperative_family_ast(text, &lower, &mut ctx);
        assert!(
            matches!(
                &ast,
                Some(ImperativeFamilyAst::Support {
                    count: 3,
                    is_other: true
                })
            ),
            "Expected Support {{ count: 3, is_other: true }}, got {ast:?}"
        );
        let clause = lower_imperative_family_ast(ast.unwrap());
        // Permanent support should have Another property
        if let Effect::PutCounter {
            target: TargetFilter::Typed(tf),
            ..
        } = &clause.effect
        {
            assert!(
                tf.properties
                    .contains(&crate::types::ability::FilterProp::Another),
                "Permanent support should use 'other'"
            );
        }
        assert_eq!(clause.multi_target, Some(MultiTargetSpec::fixed(0, 3)));
    }

    #[test]
    fn parse_choose_one_of_them() {
        let text = "choose one of them";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromTrackedSet { count, chooser }) => {
                assert_eq!(count, 1);
                assert_eq!(chooser, Chooser::Controller);
            }
            other => panic!("Expected FromTrackedSet, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_two_of_those_cards() {
        let text = "choose two of those cards";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromTrackedSet { count, chooser }) => {
                assert_eq!(count, 2);
                assert_eq!(chooser, Chooser::Controller);
            }
            other => panic!("Expected FromTrackedSet with count=2, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_anaphoric_opponent() {
        let text = "an opponent chooses one of them";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromTrackedSet { count, chooser }) => {
                assert_eq!(count, 1);
                assert_eq!(chooser, Chooser::Opponent);
            }
            other => panic!("Expected FromTrackedSet with Opponent chooser, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_anaphoric_you_choose() {
        let text = "you choose one of those cards";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromTrackedSet { count, chooser }) => {
                assert_eq!(count, 1);
                assert_eq!(chooser, Chooser::Controller);
            }
            other => panic!("Expected FromTrackedSet, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_up_to_two_of_them() {
        let text = "choose up to two of them";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromTrackedSet { count, chooser }) => {
                assert_eq!(count, 2);
                assert_eq!(chooser, Chooser::Controller);
            }
            other => panic!("Expected FromTrackedSet with count=2, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_creature_they_control() {
        // Imperial Edict pattern: "choose a creature they control"
        let text = "choose a creature they control";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::TargetOnly { target }) => {
                // Should extract creature filter with controller
                assert!(
                    matches!(target, TargetFilter::Typed { .. }),
                    "Expected Typed filter, got {target:?}"
                );
            }
            Some(ChooseImperativeAst::Reparse { .. }) => {
                // Also acceptable — reparse path handles "they control"
            }
            other => panic!("Expected TargetOnly or Reparse for 'they control', got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_from_among_cataclysm_pattern() {
        // Cataclysm: "choose from among the permanents they control an artifact, ..."
        let text =
            "choose from among the permanents they control an artifact, a creature, an enchantment, and a land";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::CategoryAndSacrificeRest {
                categories,
                chooser_scope,
            }) => {
                assert_eq!(
                    categories,
                    vec![
                        CoreType::Artifact,
                        CoreType::Creature,
                        CoreType::Enchantment,
                        CoreType::Land
                    ]
                );
                assert_eq!(
                    chooser_scope,
                    crate::types::ability::CategoryChooserScope::EachPlayerSelf
                );
            }
            other => panic!("Expected CategoryAndSacrificeRest, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_from_among_gearhulk_pattern() {
        // Cataclysmic Gearhulk: "choose an artifact, a creature, ... from among ..."
        let text = "choose an artifact, a creature, an enchantment, and a planeswalker from among the nonland permanents they control";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::CategoryAndSacrificeRest {
                categories,
                chooser_scope,
            }) => {
                assert_eq!(
                    categories,
                    vec![
                        CoreType::Artifact,
                        CoreType::Creature,
                        CoreType::Enchantment,
                        CoreType::Planeswalker
                    ]
                );
                assert_eq!(
                    chooser_scope,
                    crate::types::ability::CategoryChooserScope::EachPlayerSelf
                );
            }
            other => panic!("Expected CategoryAndSacrificeRest, got {other:?}"),
        }
    }

    /// CR 115.1c + CR 601.2c: Goblin Welder activated ability.
    /// "Choose target artifact a player controls and target artifact card in
    /// that player's graveyard." must yield two distinct target slots.
    #[test]
    fn parse_choose_two_targets_goblin_welder() {
        use crate::types::ability::{FilterProp, TypeFilter};
        let text = "choose target artifact a player controls and target artifact card in that player's graveyard";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::TwoTargets { target_a, target_b }) => {
                let tf_a = match &target_a {
                    TargetFilter::Typed(tf) => tf,
                    other => panic!("target_a should be Typed, got {other:?}"),
                };
                assert_eq!(
                    tf_a.type_filters,
                    vec![TypeFilter::Artifact],
                    "target_a should be Artifact"
                );
                let tf_b = match &target_b {
                    TargetFilter::Typed(tf) => tf,
                    other => panic!("target_b should be Typed, got {other:?}"),
                };
                assert!(
                    tf_b.type_filters.contains(&TypeFilter::Artifact)
                        || tf_b.type_filters.contains(&TypeFilter::Card),
                    "target_b should reference an artifact card, got type_filters={:?}",
                    tf_b.type_filters
                );
                // CR 400.1: The second slot must anchor to the graveyard zone
                // via `FilterProp::InZone(Graveyard)` — the canonical zone
                // marker on a `TargetFilter::Typed`. The `parse_zone_qual`
                // combinator emits this for "in/from <player>'s graveyard"
                // suffixes (verified for Goblin Engineer's correct AST in
                // the L9-12b brief).
                assert!(
                    tf_b.properties.iter().any(|p| matches!(
                        p,
                        FilterProp::InZone {
                            zone: Zone::Graveyard
                        }
                    )),
                    "target_b should carry FilterProp::InZone(Graveyard); got properties={:?}",
                    tf_b.properties
                );
            }
            other => panic!("Expected TwoTargets, got {other:?}"),
        }
    }

    /// CR 115.1c + CR 601.2c: TwoTargets lowering must emit a primary
    /// `TargetOnly` for slot A with a chained `TargetOnly` sub_ability for
    /// slot B so both targets are announced at activation.
    #[test]
    fn lower_choose_two_targets_emits_chained_target_only() {
        use crate::types::ability::TypeFilter;
        let ast = ImperativeFamilyAst::Structured(ImperativeAst::Choose(
            ChooseImperativeAst::TwoTargets {
                target_a: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Artifact],
                    ..Default::default()
                }),
                target_b: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Card],
                    ..Default::default()
                }),
            },
        ));
        let clause = lower_imperative_family_ast(ast);
        match &clause.effect {
            Effect::TargetOnly {
                target: TargetFilter::Typed(tf),
            } => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Artifact]);
            }
            other => panic!("primary effect should be TargetOnly(Artifact), got {other:?}"),
        }
        let sub = clause
            .sub_ability
            .as_ref()
            .expect("TwoTargets must produce a chained sub_ability for slot B");
        match &*sub.effect {
            Effect::TargetOnly {
                target: TargetFilter::Typed(tf),
            } => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Card]);
            }
            other => panic!("sub_ability effect should be TargetOnly(Card), got {other:?}"),
        }
        assert!(
            sub.sub_ability.is_none(),
            "TwoTargets sub_ability should not chain further; resolution-time semantics live in higher-level continuation parsing"
        );
    }

    /// CR 115.1c regression: single-target "choose target X" must keep
    /// emitting the existing `TargetOnly` AST — the two-target detector must
    /// not steal single-slot wordings.
    #[test]
    fn parse_choose_single_target_unchanged() {
        let text = "choose a creature they control";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::TargetOnly { .. })
            | Some(ChooseImperativeAst::Reparse { .. }) => {
                // Either path is acceptable — what matters is that we did NOT
                // accidentally promote this to TwoTargets.
            }
            Some(ChooseImperativeAst::TwoTargets { .. }) => {
                panic!("single-target wording must not be promoted to TwoTargets")
            }
            other => {
                panic!("Expected TargetOnly or Reparse for single-target wording, got {other:?}")
            }
        }
    }

    /// CR 115.1c + CR 608.2c regression: "target X and put a counter on it"
    /// must NOT be split into two target slots. The "and ..." continuation
    /// is a compound action handled by `try_split_targeted_compound`, not a
    /// second target slot. (This wording is not actually a "choose" form,
    /// but the unit-level guard ensures the two-target detector requires
    /// "and target Y" specifically — not just any "and" continuation.)
    #[test]
    fn parse_choose_target_and_non_target_continuation() {
        // "Choose target creature and put a +1/+1 counter on it" — the
        // second clause is NOT a target slot. The two-target detector must
        // refuse this and let single-target dispatch claim it.
        let text = "choose target creature and put a +1/+1 counter on it";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        // Any non-TwoTargets variant (TargetOnly with the creature filter, or
        // Reparse routing into the compound-action splitter) is fine — the
        // contract is just "no false positive on TwoTargets".
        if let Some(ChooseImperativeAst::TwoTargets { .. }) = result {
            panic!("non-target 'and ...' continuation must not be promoted to TwoTargets");
        }
    }

    #[test]
    fn lower_choose_anaphoric_to_choose_from_zone() {
        let ast = ChooseImperativeAst::FromTrackedSet {
            count: 3,
            chooser: Chooser::Opponent,
        };
        let effect = lower_choose_ast(ast);
        match effect {
            Effect::ChooseFromZone {
                count,
                zone,
                chooser,
                up_to,
                constraint,
            } => {
                assert_eq!(count, 3);
                assert_eq!(zone, Zone::Exile);
                assert_eq!(chooser, Chooser::Opponent);
                assert!(!up_to);
                assert!(constraint.is_none());
            }
            other => panic!("Expected ChooseFromZone, got {other:?}"),
        }
    }

    #[test]
    fn parse_incubate_fixed() {
        let result = try_parse_incubate("incubate 3");
        assert!(result.is_some(), "Should parse 'incubate 3'");
        match result.unwrap() {
            Effect::Incubate { count } => {
                assert_eq!(count, QuantityExpr::Fixed { value: 3 });
            }
            other => panic!("Expected Incubate, got {other:?}"),
        }
    }

    #[test]
    fn parse_incubate_x() {
        let result = try_parse_incubate("incubate x");
        assert!(result.is_some(), "Should parse 'incubate x'");
        match result.unwrap() {
            Effect::Incubate { count } => {
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Variable { .. }
                        }
                    ),
                    "Expected Ref(Variable), got {count:?}"
                );
            }
            other => panic!("Expected Incubate, got {other:?}"),
        }
    }

    #[test]
    fn parse_attack_this_turn_if_able() {
        let result = try_parse_attack_if_able("attacks this turn if able");
        assert!(result.is_some(), "Should parse 'attacks this turn if able'");
        match result.unwrap() {
            ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
                static_abilities,
                duration,
                ..
            }) => {
                assert_eq!(
                    static_abilities[0].mode,
                    crate::types::statics::StaticMode::MustAttack
                );
                assert_eq!(duration, Some(Duration::UntilEndOfTurn));
            }
            other => panic!("Expected GenericEffect with MustAttack, got {other:?}"),
        }
    }

    #[test]
    fn parse_attack_this_combat_if_able() {
        let result = try_parse_attack_if_able("attack this combat if able");
        assert!(
            result.is_some(),
            "Should parse 'attack this combat if able'"
        );
        match result.unwrap() {
            ImperativeFamilyAst::GainKeyword(Effect::GenericEffect { duration, .. }) => {
                assert_eq!(duration, Some(Duration::UntilEndOfCombat));
            }
            other => panic!("Expected GenericEffect, got {other:?}"),
        }
    }

    /// CR 400.6 + CR 701.24a: "shuffle the cards from your hand into your
    /// library" — Whirlpool Drake class. The phrase names every card in the
    /// hand, so the lowered AST must be ChangeZoneAllToLibrary (mass move),
    /// not a TargetedChangeZoneToLibrary where "the cards" would be read as
    /// a pronoun target.
    #[test]
    fn parse_shuffle_cards_from_your_hand_into_your_library() {
        let text = "shuffle the cards from your hand into your library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origins }) => {
                assert_eq!(origins, vec![Zone::Hand]);
            }
            other => panic!("Expected ChangeZoneAllToLibrary Hand, got {other:?}"),
        }
    }

    /// Sibling coverage: the same structural phrase with a different zone
    /// ("from your graveyard into your library") must also route to the mass
    /// path — confirms the combinator generalizes across zones.
    #[test]
    fn parse_shuffle_cards_from_your_graveyard_into_your_library() {
        let text = "shuffle the cards from your graveyard into your library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origins }) => {
                assert_eq!(origins, vec![Zone::Graveyard]);
            }
            other => panic!("Expected ChangeZoneAllToLibrary Graveyard, got {other:?}"),
        }
    }

    /// Possessive variance: "shuffle the cards from their hand into their
    /// library" (opponent-facing phrasing) — same structure, different
    /// possessive.
    #[test]
    fn parse_shuffle_cards_from_their_hand_into_their_library() {
        let text = "shuffle the cards from their hand into their library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origins }) => {
                assert_eq!(origins, vec![Zone::Hand]);
            }
            other => panic!("Expected ChangeZoneAllToLibrary Hand, got {other:?}"),
        }
    }

    #[test]
    fn parse_shuffle_hand_and_graveyard_into_your_library() {
        let text = "shuffle your hand and graveyard into your library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origins }) => {
                assert_eq!(origins, vec![Zone::Hand, Zone::Graveyard]);
            }
            other => panic!("Expected ChangeZoneAllToLibrary Hand+Graveyard, got {other:?}"),
        }
    }

    #[test]
    fn lower_shuffle_hand_and_graveyard_into_change_zone_all_chain() {
        let clause = lower_shuffle_ast(ShuffleImperativeAst::ChangeZoneAllToLibrary {
            origins: vec![Zone::Hand, Zone::Graveyard],
        });
        assert!(matches!(
            &clause.effect,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Hand),
                destination: Zone::Library,
                target: TargetFilter::Controller,
            }
        ));

        let graveyard = clause
            .sub_ability
            .as_deref()
            .expect("hand move should chain graveyard move");
        assert!(matches!(
            &*graveyard.effect,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Library,
                target: TargetFilter::Controller,
            }
        ));

        let shuffle = graveyard
            .sub_ability
            .as_deref()
            .expect("graveyard move should chain shuffle");
        assert!(matches!(
            &*shuffle.effect,
            Effect::Shuffle {
                target: TargetFilter::Controller,
            }
        ));
        assert!(shuffle.sub_ability.is_none());
    }

    /// CR 400.7 + CR 701.23: Multi-zone same-name exile combinator covers
    /// the whole sibling class (Deadly Cover-Up, Lost Legacy, Cranial
    /// Extraction, Memoricide, Surgical Extraction). Both "with that name"
    /// and "with the same name as that card" forms are accepted.
    #[test]
    fn parse_multi_zone_same_name_exile_pattern() {
        let positives = [
            "search its owner's graveyard, hand, and library for any number of cards with that name and exile them",
            "search target player's graveyard, hand, and library for any number of cards with that name and exile them",
            "search target player's graveyard, hand, and library for all cards with that name and exile them",
            "search its owner's graveyard, hand, and library for any number of cards with the same name as that card and exile them",
            "search their graveyard, hand, and library for a card with that name and exile them",
        ];
        for text in positives {
            assert!(
                try_parse_multi_zone_same_name_exile(text).is_some(),
                "expected match for: {text}"
            );
        }

        let negatives = [
            // Library-only — handled by the regular SearchLibrary branch.
            "search your library for a card",
            // Two-zone permutation we don't recognize (deliberate scope cut).
            "search target player's graveyard and library for any number of cards with that name and exile them",
            // Different action verb after — single-zone search-and-put-into-hand.
            "search your library for a basic land card and put it into your hand",
        ];
        for text in negatives {
            assert!(
                try_parse_multi_zone_same_name_exile(text).is_none(),
                "expected no match for: {text}"
            );
        }
    }

    #[test]
    fn parse_search_creation_lowering_emits_change_zone_all_with_same_name_as_parent_target() {
        use crate::types::ability::FilterProp;
        let text = "search its owner's graveyard, hand, and library for any number of cards with that name and exile them";
        let ast = parse_search_and_creation_ast(text, text, &mut ParseContext::default())
            .expect("multi-zone same-name exile must parse");
        let effect = lower_search_and_creation_ast(ast);
        match effect {
            Effect::ChangeZoneAll {
                origin,
                destination,
                target,
            } => {
                assert!(
                    origin.is_none(),
                    "origin must be None — zones come from filter"
                );
                assert_eq!(destination, Zone::Exile);
                let TargetFilter::Typed(tf) = target else {
                    panic!("Expected Typed target, got {target:?}");
                };
                let zones_ok = tf.properties.iter().any(|p| {
                    matches!(p, FilterProp::InAnyZone { zones }
                        if zones == &vec![Zone::Graveyard, Zone::Hand, Zone::Library])
                });
                let same_name_ok = tf
                    .properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::SameNameAsParentTarget));
                assert!(zones_ok, "InAnyZone[GY,Hand,Lib] missing");
                assert!(same_name_ok, "SameNameAsParentTarget missing");
            }
            other => panic!("Expected ChangeZoneAll, got {other:?}"),
        }
    }

    /// CR 113.3 + CR 604.1: `gain "<quoted ability>"` in a sub_ability context
    /// produces a `GenericEffect` wrapping a `GrantTrigger` modification when
    /// the quoted text starts with `When`/`Whenever`/`At …`. Used by Rabid
    /// Attack: `+1/+0 and gain "When this creature dies, draw a card."` until
    /// end of turn.
    #[test]
    fn gain_quoted_trigger_ability_until_end_of_turn() {
        let effect =
            try_parse_gain_quoted_ability("gain \"When this creature dies, draw a card.\"")
                .expect("expected gain-quoted-ability to parse");
        let Effect::GenericEffect {
            static_abilities,
            duration,
            ..
        } = effect
        else {
            panic!("expected GenericEffect, got something else");
        };
        assert_eq!(duration, Some(Duration::UntilEndOfTurn));
        let static_def = static_abilities
            .first()
            .expect("static_abilities must contain the granted modification");
        let grant_trigger = static_def
            .modifications
            .iter()
            .find(|m| matches!(m, ContinuousModification::GrantTrigger { .. }));
        assert!(
            grant_trigger.is_some(),
            "expected a GrantTrigger modification, got modifications: {:?}",
            static_def.modifications
        );
    }

    /// `try_parse_gain_quoted_ability` must NOT swallow bare keyword grants —
    /// those belong to `try_parse_gain_keyword`. Returning `None` here lets
    /// the dispatcher's `or_else` try the bare-keyword path first.
    #[test]
    fn gain_quoted_ability_returns_none_for_bare_keyword() {
        assert!(
            try_parse_gain_quoted_ability("gain flying until end of turn").is_none(),
            "no quote marks → not a quoted-ability candidate"
        );
    }
}
