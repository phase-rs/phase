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
//! 1. [`parse_meld_gate`] — recognizes the `"if [leading condition and] you both
//!    own and control ..."` gate, returning the `TriggerCondition::And` (CR 603.4
//!    own + control of BOTH halves, optionally preceded by a leading game-state
//!    conjunct such as Titania's `"if there are four or more land cards in your
//!    graveyard"`, CR 603.4), the partner card name, and the residual effect text.
//!    The residual is either the direct `"exile them, then meld them into R"`
//!    (Gisela, Titania) or the optional-cost `"you may pay {C}. If you do, exile
//!    them, then meld them into R"` reflexive form (Vanille, CR 118.12).
//! 2. [`parse_meld_effect_clause`] — recognizes the `"exile them, then meld them
//!    into [result]"` effect clause, returning
//!    `Effect::Meld { source, partner, result }` (`source` supplied by the parse
//!    context and `partner` supplied by the gate via
//!    `ParseContext::pending_meld_partner`).

use nom::bytes::complete::{tag, take_until};
use nom::Parser;

use crate::parser::oracle_ir::context::ParseContext;
use crate::parser::oracle_nom::condition::parse_inner_condition;
use crate::parser::oracle_nom::error::OracleError;
use crate::parser::oracle_target::parse_target;
use crate::parser::oracle_trigger::static_condition_to_trigger_condition;
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

/// CR 603.4: Parse the meld own/control gate from a meld instigator's effect
/// text. On success returns the trigger-level intervening-"if" condition
/// (`TriggerCondition::And` of an optional leading game-state conjunct plus the
/// self + partner own/control conjuncts), the partner card name, and the residual
/// effect text so the caller can drive effect-clause parsing.
///
/// The residual is either the direct `"exile them, then meld them into [result]"`
/// (Gisela, Titania) or the optional-cost `"you may pay {C}. If you do, exile them,
/// then meld them into [result]"` reflexive form (Vanille, CR 118.12) — the caller
/// routes the latter through the shared "you may pay … if you do" machinery.
///
/// The self reference is `"~"` for the triggered/normalized forms; `parse_target`
/// handles it uniformly with the partner's `"a [type] named [name]"` clause.
pub(crate) fn parse_meld_gate(effect_text: &str) -> Option<(TriggerCondition, String, String)> {
    let lower = effect_text.to_lowercase();
    // (1) Isolate the gate region up to the fixed meld sentinel via `take_until`
    // (nom composition — not a substring scan). Absent sentinel ⇒ not a meld gate.
    // Bounding to the sentinel means a comma inside a legendary partner name
    // ("Bruna, the Fading Light") can never be mistaken for the sentinel's comma.
    let (_after_sentinel, gate_region): (&str, &str) =
        take_until::<_, _, OracleError<'_>>(MELD_SENTINEL)
            .parse(lower.as_str())
            .ok()?;
    let gate_len = gate_region.len();

    // (2) The meld gate is always the trigger's intervening-"if" body (CR 603.4).
    let (after_if, _) = tag::<_, _, OracleError<'_>>("if ")
        .parse(gate_region)
        .ok()?;

    // (3) A leading game-state condition may precede the own/control gate ("if
    // there are four or more land cards in your graveyard and you both own and
    // control ...", Titania). Split it off at the own/control anchor and parse it
    // through the shared condition combinator, prepending the resulting
    // intervening-"if" conjunct (CR 603.4). A non-empty leading slice that does
    // not fully parse defers the whole meld to baseline (returns None), so the
    // gate never silently drops an unrecognized rider. Absent ⇒ no leading
    // conjunct (Gisela, Vanille).
    let (after_lead, leading): (&str, &str) =
        take_until::<_, _, OracleError<'_>>("you both own and control ")
            .parse(after_if)
            .ok()?;
    let mut conditions: Vec<TriggerCondition> = Vec::new();
    if !leading.is_empty() {
        // Strip the conjunction joining the pre-isolated leading condition to the
        // gate; `parse_inner_condition` (nom) below is the actual dispatch.
        // allow-noncombinator: structural conjunction strip on an isolated slice.
        let leading_cond = leading.strip_suffix(" and ")?;
        let (rest, sc) = parse_inner_condition(leading_cond).ok()?;
        if !rest.trim().is_empty() {
            return None;
        }
        conditions.push(static_condition_to_trigger_condition(&sc)?);
    }

    // (4) Consume the own/control gate prefix.
    let (after_prefix, _) = tag::<_, _, OracleError<'_>>("you both own and control ")
        .parse(after_lead)
        .ok()?;

    // (5) Recover the original-case "<self> and <partner>" clause by byte offset
    // (ASCII card text → 1:1 lower) so `parse_target` preserves the partner's
    // printed name casing. All slices above are suffixes of `lower`, whose gate
    // region begins at offset 0, so the clause spans `[gate_len - after_prefix.len(),
    // gate_len)`.
    let self_partner_start = gate_len - after_prefix.len();
    let self_partner_orig = &effect_text[self_partner_start..gate_len];
    let (self_filter, after_self) = parse_target(self_partner_orig);
    let after_self = after_self.trim_start();
    // Consume the conjunction joining the two named halves with a nom `tag`
    // (case-insensitive: drive it on the lowercased remainder, then recover the
    // original-case partner clause by byte offset).
    let after_self_lower = after_self.to_lowercase();
    let (conj, _) = tag::<_, _, OracleError<'_>>("and ")
        .parse(after_self_lower.as_str())
        .ok()?;
    let partner_clause = &after_self[after_self.len() - conj.len()..];
    let (partner_filter, partner_rest) = parse_target(partner_clause);
    let partner_name = named_of(&partner_filter)?;

    // CR 603.4: the own/control gate for both halves is pushed into the trigger's
    // intervening-"if" condition (checked when the trigger event occurs and
    // re-checked as it resolves).
    conditions.push(own_and_control_one(self_filter));
    conditions.push(own_and_control_one(partner_filter));
    let condition = TriggerCondition::And { conditions };

    // (6) Residual effect text begins immediately after the partner name.
    // `partner_rest` is a suffix of the gate region ending at the sentinel, so the
    // partner name ends at `gate_len - partner_rest.len()`. For the direct form
    // (Gisela / Titania) `partner_rest` is empty ⇒ the residual starts at the
    // sentinel ("... , exile them, then meld them into R"). For the optional-cost
    // form (Vanille) `partner_rest` is the ", you may pay {C}. If you do…" tail,
    // so the residual carries the reflexive cost (CR 118.12) forward for the
    // caller's "you may pay … if you do" machinery. The leading ", " boundary is
    // consumed with a nom `tag`.
    let name_end = gate_len - partner_rest.len();
    let residual_tail = &effect_text[name_end..];
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Comparator, CountScope, QuantityExpr, QuantityRef, ZoneRef};

    // `parse_meld_gate` runs on the trigger's already-normalized effect text
    // (self-references folded to `~`). These inputs mirror that stage verbatim.

    fn control_count_conjuncts(cond: &TriggerCondition) -> usize {
        let TriggerCondition::And { conditions } = cond else {
            panic!("expected And, got {cond:?}");
        };
        conditions
            .iter()
            .filter(|c| matches!(c, TriggerCondition::ControlCount { minimum: 1, .. }))
            .count()
    }

    #[test]
    fn vanille_optional_cost_gate_defers_reflexive_cost_to_residual() {
        let text = "if you both own and control ~ and a creature named Fang, Fearless l'Cie, \
             you may pay {3}{B}{G}. If you do, exile them, then meld them into Ragnarok, \
             Divine Deliverance.";
        let (cond, partner, residual) = parse_meld_gate(text).expect("Vanille gate parses");
        assert_eq!(partner, "Fang, Fearless l'Cie");
        // The deleted period-guard used to reject this whole form (returning None);
        // the residual now carries the reflexive "you may pay … if you do" cost.
        assert_eq!(
            residual,
            "you may pay {3}{B}{G}. If you do, exile them, then meld them into Ragnarok, \
             Divine Deliverance."
        );
        let TriggerCondition::And { conditions } = &cond else {
            panic!("expected And, got {cond:?}");
        };
        assert_eq!(conditions.len(), 2, "self + partner, no leading condition");
        assert_eq!(control_count_conjuncts(&cond), 2);
    }

    #[test]
    fn titania_leading_condition_prepends_land_count_conjunct() {
        let text = "if there are four or more land cards in your graveyard and you both own \
             and control ~ and a land named Argoth, Sanctum of Nature, exile them, then meld \
             them into Titania, Gaea Incarnate.";
        let (cond, partner, residual) = parse_meld_gate(text).expect("Titania gate parses");
        assert_eq!(partner, "Argoth, Sanctum of Nature");
        assert_eq!(
            residual,
            "exile them, then meld them into Titania, Gaea Incarnate."
        );
        let TriggerCondition::And { conditions } = &cond else {
            panic!("expected And, got {cond:?}");
        };
        assert_eq!(conditions.len(), 3, "land-count + self + partner");
        assert_eq!(control_count_conjuncts(&cond), 2);
        // CR 603.4: the leading graveyard-land count is prepended as the first conjunct.
        match &conditions[0] {
            TriggerCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ZoneCardCount {
                                zone: ZoneRef::Graveyard,
                                scope: CountScope::Controller,
                                ..
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            } => {}
            other => panic!("first conjunct must be graveyard land-count GE 4, got {other:?}"),
        }
    }

    #[test]
    fn gisela_direct_gate_is_two_conjunct() {
        let text = "if you both own and control ~ and a creature named Bruna, the Fading Light, \
             exile them, then meld them into Brisela, Voice of Nightmares.";
        let (cond, partner, residual) = parse_meld_gate(text).expect("Gisela gate parses");
        assert_eq!(partner, "Bruna, the Fading Light");
        assert_eq!(
            residual,
            "exile them, then meld them into Brisela, Voice of Nightmares."
        );
        assert_eq!(control_count_conjuncts(&cond), 2);
        let TriggerCondition::And { conditions } = &cond else {
            panic!("expected And, got {cond:?}");
        };
        assert_eq!(conditions.len(), 2, "no leading condition");
    }

    #[test]
    fn non_meld_named_clause_without_sentinel_declines() {
        // A "creature named X, [pronoun clause]" that lacks the meld sentinel must
        // not be mistaken for a meld gate (CR 201.2 referential clause, not meld).
        let text = "if you both own and control ~ and a creature named Fang, Fearless l'Cie, \
             it gains flying until end of turn.";
        assert!(parse_meld_gate(text).is_none());
    }
}
