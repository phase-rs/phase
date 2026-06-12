//! Meld (CR 701.42 / CR 712.4) — parser combinators for the meld instigator's
//! own/control gate and `exile-them-then-meld` effect clause.
//!
//! A meld instigator's ability is a triggered (Gisela / Graf Rats) or activated
//! (Hanweir Battlements) ability whose effect text, after self-reference
//! normalization (self → `~`), reads:
//!
//! ```text
//! if you both own and control ~ and a creature named [partner],
//!     exile them, then meld them into [result].
//! ```
//!
//! This module owns two combinators, composed from `tag`/`take_until` and the
//! shared named+type filter parser (`parse_target`):
//!
//! 1. [`parse_meld_gate`] — recognizes the `"if you both own and control ..."`
//!    gate, returning the two-conjunct `TriggerCondition::And` (CR 701.42b own +
//!    control of BOTH halves) and the partner card name.
//! 2. [`parse_meld_effect_clause`] — recognizes the `"exile them, then meld them
//!    into [result]"` effect clause, returning
//!    `Effect::Meld { source, partner, result }` (`source` supplied by the parse
//!    context and `partner` supplied by the gate via
//!    `ParseContext::pending_meld_partner`).

use nom::bytes::complete::{tag, take_until};
use nom::Parser;

use crate::parser::oracle_ir::context::ParseContext;
use crate::parser::oracle_nom::error::OracleError;
use crate::parser::oracle_target::parse_target;
use crate::types::ability::{
    ControllerRef, Effect, FilterProp, TargetFilter, TriggerCondition, TypedFilter,
};

/// The fixed sentinel that separates the gate from the meld effect clause.
const MELD_SENTINEL: &str = ", exile them, then meld them into ";

/// The meld-specific signature substring present in BOTH entry shapes — the
/// gate-bearing activated text ("..., exile them, then meld them into R") and
/// the bare triggered residual ("exile them, then meld them into R"). Callers
/// use this as a cheap byte-substring fast-reject before driving the nom-based
/// gate/effect parse, so the ~6 meld cards are the only ones that pay for the
/// full parse attempt. This is a perf guard, not parsing dispatch: a positive
/// hit still routes through `parse_meld_gate` / `parse_meld_effect_clause`,
/// which remain nom-combinator-based and remain the sole authority on whether
/// the text actually forms a meld clause.
pub(crate) const MELD_EFFECT_MARKER: &str = "meld them into ";

/// CR 701.42b: A `Typed` filter carrying only the `FilterProp::Owned { You }`
/// ownership constraint, to AND with a self-reference filter.
fn owned_you_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter {
        type_filters: Vec::new(),
        controller: Some(ControllerRef::You),
        properties: vec![FilterProp::Owned {
            controller: ControllerRef::You,
        }],
    })
}

/// CR 701.42b: Build a `ControlCount { minimum: 1 }` conjunct requiring the
/// controller to both OWN (`FilterProp::Owned { You }`) and CONTROL (the
/// `ControlCount` evaluator's `obj.controller == controller` check) a single
/// object matching `filter`.
fn own_and_control_one(filter: TargetFilter) -> TriggerCondition {
    TriggerCondition::ControlCount {
        minimum: 1,
        filter: with_owned_you(filter),
    }
}

/// CR 701.42b: Add the `FilterProp::Owned { You }` ownership constraint to a
/// filter (the `ControlCount` evaluator already enforces control). A `Typed`
/// filter gains the property directly; any other filter (e.g. `SelfRef`) is
/// AND-composed with an ownership-only `Typed` filter so the own-and-control
/// check still applies to it.
fn with_owned_you(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            if !typed.properties.iter().any(is_owned_you) {
                typed.properties.push(FilterProp::Owned {
                    controller: ControllerRef::You,
                });
            }
            typed.controller = Some(ControllerRef::You);
            TargetFilter::Typed(typed)
        }
        other => TargetFilter::And {
            filters: vec![other, owned_you_filter()],
        },
    }
}

fn is_owned_you(prop: &FilterProp) -> bool {
    matches!(
        prop,
        FilterProp::Owned {
            controller: ControllerRef::You
        }
    )
}

/// Extract the `FilterProp::Named { name }` value from a parsed filter, if any.
fn named_of(filter: &TargetFilter) -> Option<String> {
    let TargetFilter::Typed(typed) = filter else {
        return None;
    };
    typed.properties.iter().find_map(|p| match p {
        FilterProp::Named { name } => Some(name.clone()),
        _ => None,
    })
}

/// CR 701.42b: Parse the meld own/control gate from a meld instigator's effect
/// text. On success returns the trigger-level intervening-if condition
/// (`TriggerCondition::And` of the self + partner own/control conjuncts), the
/// partner card name, and the residual effect text (`"exile them, then meld them
/// into [result]"`) so the caller can drive effect-clause parsing.
///
/// `self_ref` names the instigator: `"~"` for the triggered/normalized forms, or
/// `"this land"`/`"this creature"` for the activated/source-referential forms —
/// handled uniformly by `parse_target`.
pub(crate) fn parse_meld_gate(effect_text: &str) -> Option<(TriggerCondition, String, String)> {
    let lower = effect_text.to_lowercase();
    // Anchor on the gate prefix and isolate the gate body up to the fixed
    // sentinel via `take_until` (nom composition — not a substring scan).
    let (after_prefix, _) = tag::<_, _, OracleError<'_>>("if you both own and control ")
        .parse(lower.as_str())
        .ok()?;
    let prefix_len = lower.len() - after_prefix.len();
    let (_after_body, gate_body): (&str, &str) = take_until::<_, _, OracleError<'_>>(MELD_SENTINEL)
        .parse(after_prefix)
        .ok()?;
    // CR 701.42b: the bare own/control gate is a SINGLE clause ("...own and
    // control ~ and a creature named P"). Reject any gate body that spans a
    // sentence boundary before the meld sentinel — that signals the more complex
    // optional-cost meld form ("...named P, you may pay {C}. If you do, exile
    // them, then meld them into R", e.g. Vanille / Fang), whose "you may pay"
    // additional cost and "If you do" condition this bare-gate combinator does
    // NOT model. Returning None defers that form to baseline parsing rather than
    // silently swallowing its optional-cost clause (a coverage-honesty gap).
    if take_until::<_, _, OracleError<'_>>(".")
        .parse(gate_body)
        .is_ok()
    {
        return None;
    }
    // Recover original-case slices by byte offset (ASCII card text → 1:1 lower).
    let body_start = prefix_len;
    let body_end = prefix_len + gate_body.len();
    let gate_body_orig = &effect_text[body_start..body_end];

    // gate body = "<self> and <partner clause>". Parse the self half first.
    let (self_filter, after_self) = parse_target(gate_body_orig);
    let after_self = after_self.trim_start();
    // Consume the conjunction joining the two named halves with a nom `tag`
    // (case-insensitive: drive it on the lowercased remainder, then recover the
    // original-case partner clause by byte offset).
    let after_self_lower = after_self.to_lowercase();
    let (conj, _) = tag::<_, _, OracleError<'_>>("and ")
        .parse(after_self_lower.as_str())
        .ok()?;
    let partner_clause = &after_self[after_self.len() - conj.len()..];
    let (partner_filter, _partner_rest) = parse_target(partner_clause);
    let partner_name = named_of(&partner_filter)?;

    let condition = TriggerCondition::And {
        conditions: vec![
            own_and_control_one(self_filter),
            own_and_control_one(partner_filter),
        ],
    };

    // Residual effect text begins at the sentinel; consume the leading ", "
    // boundary with a nom `tag` (the sentinel always starts with it). If the
    // boundary is somehow absent, fall back to the raw tail.
    let residual_tail = &effect_text[body_end..];
    let residual = tag::<_, _, OracleError<'_>>(", ")
        .parse(residual_tail)
        .map(|(rest, _)| rest)
        .unwrap_or(residual_tail)
        .trim_start();
    Some((condition, partner_name, residual.to_string()))
}

/// CR 701.42a: Parse the meld effect clause `"exile them, then meld them into
/// [result]"` into `Effect::Meld { source, partner, result }`. The source name
/// is the enclosing card name; the partner name is supplied by the gate via
/// `ctx.pending_meld_partner` (the gate carries it in its `ControlCount`
/// conjunct; the effect clause names only `them` + result).
/// Returns `None` if the clause shape does not match or no partner is staged.
pub(crate) fn parse_meld_effect_clause(text: &str, ctx: &ParseContext) -> Option<Effect> {
    let lower = text.to_lowercase();
    let (after, _) = tag::<_, _, OracleError<'_>>("exile them, then meld them into ")
        .parse(lower.as_str())
        .ok()?;
    let consumed = lower.len() - after.len();
    // The result name runs to the end of the sentence — terminate at the first
    // `.` via a nom `take_until` so a trailing sentence ("It enters tapped and
    // attacking." / "Activate only as a sorcery.") is never swallowed.
    let result_orig = &text[consumed..];
    let result_name = take_until::<_, _, OracleError<'_>>(".")
        .parse(result_orig)
        .map(|(_, name)| name)
        .unwrap_or(result_orig)
        .trim();
    if result_name.is_empty() {
        return None;
    }
    let partner = ctx.pending_meld_partner.clone()?;
    let source = ctx.card_name.clone()?;
    Some(Effect::Meld {
        source,
        partner,
        result: result_name.to_string(),
    })
}
