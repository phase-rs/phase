use crate::parser::oracle_nom::error::{OracleError, OracleResult};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::{one_of, space1};
use nom::combinator::{all_consuming, eof, map, not, opt, peek, rest, value};
use nom::error::ParseError;
use nom::sequence::{preceded, terminated};
use nom::Parser;

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
use crate::parser::oracle_nom::bridge::{nom_on_lower, nom_parse_lower, split_once_on_lower};
use crate::parser::oracle_nom::primitives as nom_primitives;
use crate::parser::oracle_static::{
    parse_continuous_modifications, parse_quoted_ability_modifications,
};
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, BounceSelection, CategoryChooserScope, ChoiceType,
    Chooser, ContinuousModification, ControllerRef, CopyRetargetPermission, Duration, Effect,
    FilterProp, LibraryPosition, MultiTargetSpec, OutsideGameSourcePool, PaymentCost, PlayerScope,
    PreventionAmount, PreventionScope, PtStat, PtValue, QuantityExpr, QuantityRef,
    SearchSelectionConstraint, StaticDefinition, TargetFilter, TypeFilter, TypedFilter, ZoneOwner,
};
use crate::types::card_type::CoreType;
use crate::types::phase::Phase;
use crate::types::player::PlayerCounterKind;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::super::oracle_target::{
    parse_target, parse_target_with_ctx, parse_target_with_syntax, parse_type_phrase,
    resolve_pronoun_target, TargetSyntax,
};
use super::super::oracle_util::{
    contains_possessive, contains_self_or_object_pronoun, parse_count_expr, parse_mana_symbols,
    parse_ordinal, split_around, starts_with_possessive, TextPair,
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

/// CR 115.1 + Whitemane Lion ruling: True iff the filter constrains by a
/// controller scope (`controller: Some(...)`) at the top level (or inside an
/// `And`/`Or`/`Not` composition). This is the precondition for treating a
/// non-targeted bounce as a controller-choice EffectZoneChoice instead of a
/// no-op — without a controller scope, the resolver has no eligible-set to
/// enumerate. Mirrors the shape of `filter_targets_zone` in `effects/bounce.rs`.
pub(super) fn filter_has_controller_scope(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.controller.is_some(),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_has_controller_scope)
        }
        TargetFilter::Not { filter } => filter_has_controller_scope(filter),
        _ => false,
    }
}

fn parse_dig_library_owner(rest_lower: &str) -> TargetFilter {
    if preceded(
        take_until::<_, _, OracleError<'_>>("target player's library"),
        tag::<_, _, OracleError<'_>>("target player's library"),
    )
    .parse(rest_lower)
    .is_ok()
    {
        return TargetFilter::Player;
    }

    if preceded(
        take_until::<_, _, OracleError<'_>>("that player's library"),
        tag::<_, _, OracleError<'_>>("that player's library"),
    )
    .parse(rest_lower)
    .is_ok()
    {
        return TargetFilter::ParentTarget;
    }

    // CR 608.2c + CR 400.3: "that library" — anaphoric to a library
    // identified earlier in the instruction (Chaos Warp: owner's library
    // after shuffle).
    if preceded(
        take_until::<_, _, OracleError<'_>>("that library"),
        tag::<_, _, OracleError<'_>>("that library"),
    )
    .parse(rest_lower)
    .is_ok()
    {
        return TargetFilter::ParentTargetOwner;
    }

    TargetFilter::Controller
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
    tag::<_, _, OracleError<'_>>(" during that player's next turn")
        .parse(rem_lower.as_str())
        .ok()?;
    let rem_after_during = &rem[" during that player's next turn".len()..];
    let rem_after_during_lower = rem_after_during.to_ascii_lowercase();
    let (_tail, grant_extra_turn_after) = if let Ok((tail, _)) = alt((
        tag::<_, _, OracleError<'_>>(". after that turn, that player takes an extra turn"),
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
    if let Ok((rem, _)) = tag::<_, _, OracleError<'_>>("x").parse(lower_rest) {
        // CR 122.1: "X, where X is the number of <kind> counters <possessor>".
        if let Ok((rem2, qty)) = preceded(
            tag::<_, _, OracleError<'_>>(", where x is "),
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
            && tag::<_, _, OracleError<'_>>("then ")
                .parse(trimmed)
                .is_err()
            && tag::<_, _, OracleError<'_>>("and ").parse(trimmed).is_err()
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
        tag::<_, _, OracleError<'_>>("cards equal to "),
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
        tag::<_, _, OracleError<'_>>("that many cards"),
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

fn parse_life_verb_remainder<'a>(
    text: &'a str,
    lower: &str,
    verb: &str,
    third_person: &str,
) -> Option<&'a str> {
    let you_verb = format!("you {verb} ");
    let bare_verb = format!("{verb} ");
    let direct_third_person = format!("{third_person} ");
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value(
            (),
            alt((
                tag(you_verb.as_str()),
                tag(bare_verb.as_str()),
                tag(direct_third_person.as_str()),
            )),
        )
        .parse(input)
    }) {
        return Some(rest);
    }

    let subject_predicate = format!(" {third_person} ");
    nom_on_lower(text, lower, |input| {
        let (rest, _) = (
            take_until::<_, _, OracleError<'_>>(subject_predicate.as_str()),
            tag(subject_predicate.as_str()),
        )
            .parse(input)?;
        Ok((rest, ()))
    })
    .map(|(_, rest)| rest)
}

fn parse_life_equal_quantity(after_verb_lower: &str) -> Option<QuantityExpr> {
    let (qty_text, _) = tag::<_, _, OracleError<'_>>("life equal to ")
        .parse(after_verb_lower)
        .ok()?;
    let qty_text = qty_text.trim_end_matches('.').trim();
    if let Some(qty) = crate::parser::oracle_quantity::parse_event_context_quantity(qty_text) {
        return Some(qty);
    }
    crate::parser::oracle_quantity::parse_quantity_ref(qty_text)
        .map(|qty| QuantityExpr::Ref { qty })
}

/// CR 119.3 + CR 102.1: "gain 1 life for each player" (a/an/1) → the count of
/// every player in the game (`PlayerFilter::All`). Narrow to the one-life-per
/// form, the only shape that occurs (Benediction of Moons); other multipliers
/// fall through to the generic quantity paths. `text` is the remainder after
/// the "gain" verb, lowercased.
fn parse_gain_life_per_player(after_gain_lower: &str) -> Option<QuantityExpr> {
    // `(1|a|an) life for each player(s)` — `players` first so the longer tag
    // wins; the optional trailing period is consumed so `all_consuming` accepts
    // both the sentence-final and mid-clause forms.
    all_consuming((
        alt((
            tag::<_, _, OracleError<'_>>("1 life for each "),
            tag("a life for each "),
            tag("an life for each "),
        )),
        alt((tag("players"), tag("player"))),
        opt(tag(".")),
    ))
    .parse(after_gain_lower.trim())
    .ok()
    .map(|_| QuantityExpr::Ref {
        qty: QuantityRef::PlayerCount {
            filter: crate::types::ability::PlayerFilter::All,
        },
    })
}

pub(super) fn parse_numeric_imperative_ast(
    text: &str,
    lower: &str,
) -> Option<NumericImperativeAst> {
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| value((), tag("draw ")).parse(input))
        .or_else(|| {
            nom_on_lower(text, lower, |input| {
                let (rest, _) = (
                    take_until::<_, _, OracleError<'_>>(" draws "),
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
        // CR 121.1: When the verb committed but the quantity phrase
        // can't be classified, return None so the line surfaces as
        // `Effect::Unimplemented` upstream. Silently substituting Fixed{1} hides
        // dynamic-quantity gaps from the coverage report.
        let count = parse_count_expr(rest).map(|(q, _)| q)?;
        return Some(NumericImperativeAst::Draw { count, up_to });
    }

    if let Some(after_gain) = parse_life_verb_remainder(text, lower, "gain", "gains") {
        let after_lower = after_gain.to_ascii_lowercase();
        // CR 119.3 + CR 102.1: "gain N life for each player" — the life gained is
        // the number of players (the per-each amount is 1). Benediction of Moons.
        // Probed before the bare-quantity fallback, which would otherwise parse
        // "1" and silently drop the "for each player" multiplier.
        if let Some(amount) = parse_gain_life_per_player(&after_lower) {
            return Some(NumericImperativeAst::GainLife { amount });
        }
        // CR 119.3: Handle "life equal to {quantity}" — dynamic amount from game state.
        // CR 119.3: target-relative quantity refs ("target creature's
        // power/toughness/mana value"). Mirrors LoseLife. Soul's Grace,
        // Heron's Grace Champion, Lifeblood Hydra, etc.
        if let Some(amount) = parse_life_equal_quantity(after_lower.as_str()) {
            return Some(NumericImperativeAst::GainLife { amount });
        }
        // CR 119.3: "gain that much life" / "gain that many life" —
        // amount is the triggering event's amount (Exquisite Blood). Extract the
        // amount phrase before " life" and route through the event-context
        // quantity parser so "that much" resolves to `EventContextAmount`
        // rather than defaulting to 1.
        // CR 614.1a: First try the full phrase including any " life plus N" /
        // " life minus N" suffix. The Offset-aware combinator in
        // `parse_event_context_quantity` recognises the post-quantifier noun
        // ("that much life plus 1" / "that many life minus 2"), so cards
        // like Heron of Hope, Angel of Vitality, Leyline of Hope must be
        // probed BEFORE the bare-quantifier path strips " life" via
        // `take_until` (which would discard the offset clause).
        //
        // Strip the trailing " instead" rider via PATTERNS.md §2a:
        // `terminated(take_until(...), opt(tag(...)))` — the parser stops at
        // the suffix and consumes it (when present), leaving the body for
        // the offset combinator. Falls back to the trimmed full phrase
        // when the suffix is absent (Exquisite Blood-class non-replacement
        // gain-life).
        let full_phrase = after_lower.trim_end_matches('.').trim();
        let full_phrase_no_instead = terminated(
            take_until::<_, _, OracleError<'_>>(" instead"),
            opt(tag(" instead")),
        )
        .parse(full_phrase)
        .map(|(_rem, body)| body.trim_end())
        .unwrap_or(full_phrase);
        if let Some(qty) =
            crate::parser::oracle_quantity::parse_event_context_quantity(full_phrase_no_instead)
        {
            return Some(NumericImperativeAst::GainLife { amount: qty });
        }
        let amount_phrase = take_until::<_, _, OracleError<'_>>(" life")
            .parse(after_lower.as_str())
            .map(|(_, before)| before.trim())
            .unwrap_or(full_phrase);
        if let Some(qty) =
            crate::parser::oracle_quantity::parse_event_context_quantity(amount_phrase)
        {
            return Some(NumericImperativeAst::GainLife { amount: qty });
        }
        // CR 119.3: GainLife committed but quantity phrase unclassified —
        // surface as Unimplemented rather than fabricating Fixed{1}.
        let amount = parse_count_expr(after_gain).map(|(q, _)| q)?;
        return Some(NumericImperativeAst::GainLife { amount });
    }

    if let Some(after_lose) = parse_life_verb_remainder(text, lower, "lose", "loses") {
        let after_lower = after_lose.to_ascii_lowercase();
        if let Some(expr) = try_parse_half_life_amount(after_lower.as_str()) {
            return Some(NumericImperativeAst::LoseLife { amount: expr });
        }
        // CR 119.3: Handle "life equal to {quantity}" — dynamic amount from game state.
        // CR 119.3: target-relative quantity refs ("target creature's
        // power/toughness/mana value", etc.) — Final Punishment, Tomb
        // Blade-class drain, Genesis of the Daleks. Delegates to the
        // shared `parse_quantity_ref` building block.
        if let Some(amount) = parse_life_equal_quantity(after_lower.as_str()) {
            return Some(NumericImperativeAst::LoseLife { amount });
        }
        // CR 119.3: "lose that much life" / "lose that many life" —
        // amount is the triggering event's amount. Probe for event-context phrases
        // before falling back to the numeric last-word extractor.
        if let Ok((_, before_life)) =
            take_until::<_, _, OracleError<'_>>("life").parse(after_lower.as_str())
        {
            let amount_phrase = take_until::<_, _, OracleError<'_>>(" life")
                .parse(after_lower.as_str())
                .map(|(_, before)| before.trim())
                .unwrap_or_else(|_: nom::Err<OracleError<'_>>| {
                    after_lower.trim_end_matches('.').trim()
                });
            if let Some(qty) =
                crate::parser::oracle_quantity::parse_event_context_quantity(amount_phrase)
            {
                return Some(NumericImperativeAst::LoseLife { amount: qty });
            }
            if let Some((amount, remainder)) = parse_count_expr(amount_phrase) {
                if remainder.trim().is_empty() {
                    return Some(NumericImperativeAst::LoseLife { amount });
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
/// `DivideRounded` expression by delegating to the shared quantity combinator.
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
    // Delegate to the shared "half ..." combinator. This picks up the
    // possessive inner ref AND the rounding suffix in one call.
    let (_, expr) =
        super::super::oracle_nom::quantity::parse_half_rounded(lower.trim_start()).ok()?;
    Some(expr)
}

/// CR 606.3: Recognize the printed Chain Veil class — a single ability that
/// raises the per-permanent CR 606.3 loyalty-activation cap by N for every
/// planeswalker the controller controls. Two surface forms are accepted:
///
///   - "for each planeswalker you control, you may activate one of its loyalty
///     abilities once this turn as though none of its loyalty abilities have
///     been activated this turn." — the printed Chain Veil wording. The
///     "for each planeswalker you control" preamble identifies the beneficiaries
///     (each planeswalker gets +1 cap), not a repeat count;
///     `strip_for_each_prefix` bails out on this pattern so the imperative
///     dispatch sees the full text here.
///   - "activate each planeswalker's loyalty ability an additional time this
///     turn" / "an additional N times this turn" — future-proof generalization
///     after the outer "you may " has been stripped by
///     `strip_optional_effect_prefix`. Numeric variant supports cards that grant
///     more than one extra activation in a single resolution.
fn parse_grant_extra_loyalty_activations(lower: &str) -> Option<QuantityExpr> {
    if let Ok((_, amount)) = parse_chain_veil_for_each_form(lower) {
        return Some(amount);
    }
    parse_each_planeswalker_additional_form(lower)
        .map(|(_, amount)| amount)
        .ok()
}

/// CR 606.3: "For each planeswalker you control, you may activate one of its
/// loyalty abilities once this turn as though none of its loyalty abilities
/// have been activated this turn." — printed Chain Veil text. Always grants
/// +1 per planeswalker (the printed wording has no numeric variant).
fn parse_chain_veil_for_each_form(
    lower: &str,
) -> nom::IResult<&str, QuantityExpr, OracleError<'_>> {
    let (rest, _) = (
        tag("for each "),
        tag("planeswalker"),
        tag(" you control, "),
        parse_single_loyalty_permission,
    )
        .parse(lower)?;
    let (rest, _) = opt(tag(".")).parse(rest)?;
    Ok((rest, QuantityExpr::Fixed { value: 1 }))
}

fn parse_single_loyalty_permission(lower: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    let (rest, _) = (
        tag("you may activate one of its "),
        tag("loyalty abilities"),
        tag(" once this turn"),
    )
        .parse(lower)?;
    let (rest, _) = opt((
        tag(" as though none of its "),
        tag("loyalty abilities"),
        tag(" have been activated this turn"),
    ))
    .parse(rest)?;
    Ok((rest, ()))
}

/// CR 606.3: "activate each planeswalker's loyalty ability an additional time
/// this turn" / "an additional N times this turn" — generalization after
/// `strip_optional_effect_prefix` has consumed the outer "you may ".
fn parse_each_planeswalker_additional_form(
    lower: &str,
) -> nom::IResult<&str, QuantityExpr, OracleError<'_>> {
    let (rest, _) = (
        tag("activate "),
        tag("each planeswalker"),
        tag("'s "),
        tag("loyalty ability "),
    )
        .parse(lower)?;
    alt((
        // "an additional N times this turn" → +N
        map(
            (
                tag::<_, _, OracleError<'_>>("an additional "),
                nom_primitives::parse_number,
                tag(" times this turn"),
            ),
            |(_, n, _)| QuantityExpr::Fixed { value: n as i32 },
        ),
        // "an additional time this turn" → +1
        value(
            QuantityExpr::Fixed { value: 1 },
            tag::<_, _, OracleError<'_>>("an additional time this turn"),
        ),
    ))
    .parse(rest)
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
            player: TargetFilter::Controller,
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
/// computed against. Recurses through `DivideRounded` / `Multiply` / `Offset`
/// wrappers since the filter belongs to the innermost ObjectCount; returns
/// `None` for expressions that carry no filter (Fixed, Variable(X), etc.).
fn extract_object_count_filter(expr: &QuantityExpr) -> Option<TargetFilter> {
    match expr {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } => Some(filter.clone()),
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
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
pub(crate) fn parse_discard_card_filter(tail: &str) -> Option<TargetFilter> {
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

fn strip_sacrifice_count_suffix(target_text: &str) -> String {
    let trimmed = target_text.trim_start();
    let lower = trimmed.to_lowercase();
    if nom_on_lower(trimmed, &lower, |input| {
        value(
            (),
            (
                alt((
                    tag::<_, _, OracleError<'_>>(", rounded up"),
                    tag(", rounded down"),
                    tag(", round up"),
                    tag(", round down"),
                )),
                opt(tag(".")),
                eof,
            ),
        )
        .parse(input)
    })
    .is_some()
    {
        String::new()
    } else {
        target_text.to_string()
    }
}

/// CR 701.21a + CR 107.1c: Parse "one or more [objects]" / "any number of
/// [objects]" sacrifice choices. `UpTo(ObjectCount(filter))` provides the
/// dynamic upper bound; `min_count` carries the printed lower bound — `1` for
/// "one or more", and `0` for "any number of" (CR 107.1c: a player choosing
/// "any number" may choose any positive number or zero).
pub(super) fn parse_one_or_more_sacrifice(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<(QuantityExpr, TargetFilter, usize)> {
    let lower = text.to_lowercase();
    let (min_count, filter_text) = nom_on_lower(text, &lower, |input| {
        alt((
            value(1usize, tag("one or more ")),
            value(0usize, tag("any number of ")),
        ))
        .parse(input)
    })?;
    let target_text = strip_sacrifice_count_suffix(&strip_sacrifice_choice_marker(
        strip_article(filter_text.trim_start()).trim_end_matches('.'),
    ));
    let (mut target, remainder) = parse_type_phrase(target_text.trim());
    if !remainder.trim().is_empty() || matches!(target, TargetFilter::Any) {
        return None;
    }
    apply_actor_default(&mut target, ctx);
    let count = QuantityExpr::up_to(QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: target.clone(),
        },
    });
    Some((count, target, min_count))
}

/// CR 701.21a + CR 609.3: "sacrifice all <filter>" carries a mandatory
/// count equal to the eligible object pool. This lets the sacrifice resolver's
/// existing mandatory-all fast path perform every legal sacrifice without a
/// one-card special case.
pub(super) fn parse_all_sacrifice<'a>(
    text: &'a str,
    ctx: &mut ParseContext,
) -> Option<(QuantityExpr, TargetFilter, &'a str)> {
    let lower = text.to_lowercase();
    let ((), rest) = nom_on_lower(text, &lower, |input| value((), tag("all ")).parse(input))?;
    let (mut target, rem) = parse_target_with_ctx(rest.trim_start(), ctx);
    if matches!(target, TargetFilter::Any) {
        return None;
    }
    apply_actor_default(&mut target, ctx);
    let count = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: target.clone(),
        },
    };
    Some((count, target, rem))
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
        let (target, _rem) = parse_target_with_ctx(rest, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::TapAll { target });
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("untap all "), tag("untap each ")))).parse(input)
    }) {
        let (target, _rem) = parse_target_with_ctx(rest, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::UntapAll { target });
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("goad all "), tag("goad each ")))).parse(input)
    }) {
        let (target, _rem) = parse_target_with_ctx(rest, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::GoadAll { target });
    }
    // CR 701.16a: "sacrifice [count] <filter> [of their choice]" —
    // delegates to `parse_count_expr` so "a"/"an"/"X"/"half the permanents
    // they control" all flow through one authority. "Of their choice" is
    // the default per CR 701.16b (the sacrificing player chooses); strip
    // it as a confirmation suffix rather than bleeding into the filter.
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("sacrifice ")).parse(input)
    }) {
        if let Some((count, target, _rem)) = parse_all_sacrifice(rest, ctx) {
            #[cfg(debug_assertions)]
            assert_no_compound_remainder(_rem, text);
            return Some(TargetedImperativeAst::Sacrifice {
                target,
                count,
                min_count: 0,
            });
        }
        if let Some((count, target, min_count)) = parse_one_or_more_sacrifice(rest, ctx) {
            return Some(TargetedImperativeAst::Sacrifice {
                target,
                count,
                min_count,
            });
        }
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
        let target_text = strip_sacrifice_count_suffix(&strip_sacrifice_choice_marker(target_text));
        // CR 107.2: Skip `parse_target` on an empty remainder — the count
        // subsumed the filter ("sacrifice half the permanents they control
        // of their choice"), so there is nothing left to classify. Avoids
        // emitting a `target-fallback` parse warning for a well-formed parse.
        //
        // CR 608.2k: When the remainder is a bare object pronoun ("it",
        // "itself", "them", "him", "her") AND the parse context carries an
        // explicit trigger subject, resolve the pronoun against that subject
        // instead of defaulting to `ParentTarget`. On a self-ETB trigger
        // ("When Phlage enters, sacrifice it unless it escaped"; "When
        // Azorius Herald enters, sacrifice it unless {U} was spent to cast it")
        // the subject is `SelfRef` and there is no outer targeted object, so
        // "it" binds to the source permanent. For context-free parses (e.g.
        // the populate anaphor chain "populate. … sacrifice it at the
        // beginning of the next end step") the antecedent is set later by
        // `rewrite_parent_target_to_last_created`, so we must preserve
        // `ParentTarget` when no subject is provided.
        //
        // **Helper choice:** routes through `resolve_it_pronoun`, NOT
        // `resolve_pronoun_target`. The latter returns `ParentTarget` for
        // `Some(SelfRef)` subjects to support parent-target chains
        // ("tap target creature. exile it") — but the sacrifice imperative
        // here is a trigger sub-effect with no parent-target chain in scope.
        // Switching to `resolve_pronoun_target` would break the Azorius
        // Herald / Balduvian Horde / Phlage class. See the
        // `self_etb_sacrifice_it_anaphor_binds_to_self_ref` regression test
        // in `oracle_trigger.rs` for the lock-in.
        let target = if target_text.trim().is_empty() {
            TargetFilter::Any
        } else if ctx.subject.is_some() && is_bare_object_pronoun(target_text.trim()) {
            resolve_it_pronoun(ctx)
        } else {
            let (target, _rem) = parse_target_with_ctx(&target_text, ctx);
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
        return Some(TargetedImperativeAst::Sacrifice {
            target,
            count,
            min_count: 0,
        });
    }
    // Simple targeted verbs: tap, untap — parse target after verb prefix
    if let Some((verb, rest)) = nom_on_lower(text, lower, |input| {
        alt((value("tap", tag("tap ")), value("untap", tag("untap ")))).parse(input)
    }) {
        let (target_text, _) = super::strip_optional_target_prefix(strip_article(rest));
        let (target, _rem) = parse_target_with_ctx(target_text, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return match verb {
            "tap" => Some(TargetedImperativeAst::Tap { target }),
            "untap" => Some(TargetedImperativeAst::Untap { target }),
            _ => unreachable!(),
        };
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| value((), tag("goad ")).parse(input))
    {
        let (target_text, _) = super::strip_optional_target_prefix(strip_article(rest));
        let (target, _rem) = parse_target_with_ctx(target_text, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::Goad { target });
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
            tag::<_, _, OracleError<'_>>("that card"),
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
            match tag::<_, _, OracleError<'_>>("up to ").parse(after_discard) {
                Ok((rest, _)) => (rest, true),
                Err(_) => (after_discard, false),
            };
        // Strip "all the cards in " / "all cards in " prefix compositionally for
        // patterns like "discard all the cards in your hand" / "discards all cards in their hand".
        let after_discard = alt((
            tag::<_, _, OracleError<'_>>("all the cards in "),
            tag("all cards in "),
        ))
        .parse(after_discard)
        .map(|(rest, _)| rest)
        .unwrap_or(after_discard);
        // CR 109.5 + CR 115.10: Whole-hand discard. The possessive pronoun
        // disambiguates the hand-size owner. "Your hand" is the caster's hand
        // (`Controller`); "their hand" / "his or her hand" is the subject's
        // hand, represented here as `Target`. Under an each-player wrapper,
        // `rewrite_player_scope_refs` rewrites that target-scoped hand-size to
        // `ScopedPlayer`, so Windfall-style effects bind to the iterating
        // player instead of the caster.
        if let Ok((_, hand_owner)) = terminated(
            alt((
                value(
                    PlayerScope::Controller,
                    tag::<_, _, OracleError<'_>>("your"),
                ),
                value(PlayerScope::Target, tag("their")),
                value(PlayerScope::Target, tag("his or her")),
            )),
            tag(" hand"),
        )
        .parse(after_discard)
        {
            return Some(TargetedImperativeAst::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize { player: hand_owner },
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
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("any number of ").parse(after_discard) {
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
    // cards from all graveyards to the battlefield") routes to ChangeZoneAll
    // so the resolver scans the full object set. Mirrors the `tap all`/`tap
    // each` precheck arms above plus the bare `tag("return ")` arm's
    // destination routing.
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
        let (trailing_target_text, trailing_dest) = super::strip_return_destination_ext(rest);
        let (leading_target_text, leading_dest) = super::strip_leading_return_destination_ext(rest);
        let (target_text, dest) = if leading_dest.is_some() {
            (leading_target_text, leading_dest)
        } else {
            (trailing_target_text, trailing_dest)
        };
        let (is_mass, target_text) = if let Some((_, rest)) =
            nom_on_lower(target_text, &target_text.to_ascii_lowercase(), |input| {
                value((), alt((tag("all "), tag("each ")))).parse(input)
            }) {
            (true, rest)
        } else {
            (is_mass, target_text)
        };
        let counted_return = parse_count_expr(target_text).and_then(|(mut count, after_count)| {
            let filter = extract_object_count_filter(&count)?;
            if nom_primitives::scan_contains(rest_lower, "rounded up") {
                count = match count {
                    QuantityExpr::DivideRounded {
                        inner,
                        divisor,
                        rounding: _,
                    } => QuantityExpr::DivideRounded {
                        inner,
                        divisor,
                        rounding: crate::types::ability::RoundingMode::Up,
                    },
                    other => other,
                };
            } else if nom_primitives::scan_contains(rest_lower, "rounded down") {
                count = match count {
                    QuantityExpr::DivideRounded {
                        inner,
                        divisor,
                        rounding: _,
                    } => QuantityExpr::DivideRounded {
                        inner,
                        divisor,
                        rounding: crate::types::ability::RoundingMode::Down,
                    },
                    other => other,
                };
            }
            if after_count.trim().is_empty() {
                Some((filter, count))
            } else {
                None
            }
        });
        let count = counted_return.as_ref().map(|(_, count)| count.clone());
        // CR 115.1 + Whitemane Lion ruling: Use `parse_target_with_syntax` so
        // the "target"-keyword vs descriptor discriminator flows back from
        // this parse alone, with no cross-clause residue to clear.
        let (target, target_syntax, _count_for_shape) = match counted_return {
            Some((target, c)) => (target, TargetSyntax::TargetKeyword, c),
            None => {
                let (target, _rem, syntax) = parse_target_with_syntax(target_text, ctx);
                #[cfg(debug_assertions)]
                assert_no_compound_remainder(_rem, text);
                (target, syntax, QuantityExpr::Fixed { value: 0 })
            }
        };
        // CR 115.1: A bounce resolves at-resolution iff the Oracle text omitted
        // the word "target" AND the filter has a controller scope to enumerate
        // against (Whitemane Lion's "a creature you control" — the controller
        // picks at resolution time via EffectZoneChoice).
        let selection = if matches!(target_syntax, TargetSyntax::Descriptor)
            && filter_has_controller_scope(&target)
        {
            BounceSelection::AtResolution
        } else {
            BounceSelection::Targeted
        };
        let is_mass = is_mass || count.is_some();
        let origin = super::infer_origin_zone(rest_lower);

        // CR 400.7: Single-object battlefield destinations use ChangeZone;
        // mass destinations use ChangeZoneAll. Only pure mass-bounce
        // (battlefield⇒hand, no graveyard/library origin) promotes to
        // `ReturnAll` ⇒ `Effect::BounceAll`.
        return match dest {
            Some(d) if d.zone == Zone::Battlefield => {
                if is_mass && d.enter_with_counters.is_empty() {
                    Some(TargetedImperativeAst::ReturnAllToZone {
                        target,
                        origin,
                        destination: Zone::Battlefield,
                        enters_under: d.enters_under,
                        enter_tapped: d.enter_tapped,
                    })
                } else {
                    Some(TargetedImperativeAst::ReturnToBattlefield {
                        target,
                        origin,
                        enter_transformed: d.transformed,
                        enters_under: d.enters_under,
                        enter_tapped: d.enter_tapped,
                        enters_attacking: d.enters_attacking,
                        enter_with_counters: d.enter_with_counters,
                    })
                }
            }
            Some(d) if d.zone == Zone::Hand => {
                // Mass return only when the source zone is implicit (battlefield):
                // "return all <filter> from your graveyard to your hand" must
                // remain `ChangeZone { origin: Graveyard, destination: Hand }`,
                // not `BounceAll` (whose resolver only scans the battlefield).
                if is_mass && origin.is_none() {
                    Some(TargetedImperativeAst::ReturnAll { target, count })
                } else if is_mass {
                    Some(TargetedImperativeAst::ReturnAllToZone {
                        target,
                        origin,
                        destination: Zone::Hand,
                        enters_under: None,
                        enter_tapped: false,
                    })
                } else {
                    Some(TargetedImperativeAst::Return { target, selection })
                }
            }
            Some(d) => {
                if is_mass {
                    Some(TargetedImperativeAst::ReturnAllToZone {
                        target,
                        origin,
                        destination: d.zone,
                        enters_under: None,
                        enter_tapped: false,
                    })
                } else {
                    Some(TargetedImperativeAst::ReturnToZone {
                        target,
                        origin,
                        destination: d.zone,
                    })
                }
            }
            // No explicit destination phrase. "Return all <filter>" with no
            // "to ..." tail still defaults to owner's hand for the
            // mass-bounce class. Single-target "return <X>" likewise defaults
            // to hand — preserves the pre-existing behavior.
            None => {
                if is_mass {
                    Some(TargetedImperativeAst::ReturnAll { target, count })
                } else {
                    Some(TargetedImperativeAst::Return { target, selection })
                }
            }
        };
    }
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("fight ")).parse(input))
    {
        let (target_text, _) = super::strip_optional_target_prefix(rest);
        let (target, _rem) = parse_target_with_ctx(target_text, ctx);
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
        let (target, _rem) = parse_target_with_ctx(target_text, ctx);
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
        let (target, after_target) = parse_target_with_ctx(target_text, ctx);
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
        TargetedImperativeAst::Goad { target } => Effect::Goad { target },
        TargetedImperativeAst::GoadAll { target } => Effect::GoadAll { target },
        TargetedImperativeAst::Sacrifice {
            target,
            count,
            min_count,
        } => Effect::Sacrifice {
            target,
            count,
            min_count,
        },
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
        TargetedImperativeAst::Return { target, selection } => Effect::Bounce {
            target,
            destination: None,
            selection,
        },
        // CR 400.7 + CR 611.2c: "Return all/each [filter]" mass-bounce — the
        // resolver iterates every matching permanent. Class filter is preserved
        // as-is; single-object refs (SelfRef / TriggeringSource / AttachedTo /
        // ParentTarget) cannot reach this AST variant because the bare
        // `tag("return ")` arm above handles those.
        TargetedImperativeAst::ReturnAll { target, count } => Effect::BounceAll {
            target,
            destination: None,
            count,
        },
        // CR 400.7: Return to battlefield is a zone change, not a bounce.
        TargetedImperativeAst::ReturnToBattlefield {
            target,
            origin,
            enter_transformed,
            enters_under,
            enter_tapped,
            enters_attacking,
            enter_with_counters,
        } => Effect::ChangeZone {
            origin,
            destination: Zone::Battlefield,
            target,
            owner_library: false,
            enter_transformed,
            enters_under,
            enter_tapped,
            enters_attacking,
            up_to: false,
            enter_with_counters,
            face_down_profile: None,
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
            enters_under: None,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            face_down_profile: None,
        },
        TargetedImperativeAst::ReturnAllToZone {
            target,
            origin,
            destination,
            enters_under,
            enter_tapped,
        } => {
            let origin = if matches!(target, TargetFilter::ExiledBySource) {
                Some(Zone::Exile)
            } else {
                origin
            };
            Effect::ChangeZoneAll {
                origin,
                destination,
                target,
                enters_under,
                enter_tapped,
                face_down_profile: None,
            }
        }
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
                // CR 611.2a: bound to the ability controller by
                // `grant_permission::resolve`.
                granted_to: None,
                resolution_cleanup: None,
                duration: None,
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
    fn run(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
        // search <possessive> graveyard, hand, and library
        let (input, _) = tag::<_, _, OracleError<'_>>("search ").parse(input)?;
        let (input, _) = alt((
            tag::<_, _, OracleError<'_>>("its owner's "),
            tag("its controller's "),
            tag("their "),
            tag("that player's "),
            tag("target player's "),
            tag("target opponent's "),
            tag("an opponent's "),
            tag("your "),
        ))
        .parse(input)?;
        let (input, _) = alt((
            tag::<_, _, OracleError<'_>>("graveyard, hand, and library"),
            tag("graveyard, hand and library"),
            tag("graveyard, library, and hand"),
            tag("hand, graveyard, and library"),
            tag("library, graveyard, and hand"),
        ))
        .parse(input)?;
        // for [any number of] cards with that name and exile them
        let (input, _) = tag::<_, _, OracleError<'_>>(" for ").parse(input)?;
        let (input, _) = alt((
            value((), tag::<_, _, OracleError<'_>>("any number of cards")),
            value((), tag("all cards")),
            value((), tag("a card")),
        ))
        .parse(input)?;
        // Match the trailing same-name suffix. The name source is either a
        // previously-named card ("with that name", Lost Legacy / Deadly Cover-Up)
        // or the spell's exiled/countered target referenced by its card type
        // ("with the same name as that card/creature/land/spell/…", Surgical
        // Extraction / Eradicate / Crumble to Dust / Counterbore / Deicide). The
        // card-type is composed as one `alt` axis rather than enumerated as full
        // strings; all forms lower to the same `SameNameAsParentTarget` effect.
        let (input, _) = alt((
            value(
                (),
                tag::<_, _, OracleError<'_>>(" with that name and exile them"),
            ),
            value(
                (),
                (
                    tag::<_, _, OracleError<'_>>(" with the same name as that "),
                    alt((
                        tag("creature"),
                        tag("permanent"),
                        tag("planeswalker"),
                        tag("artifact"),
                        tag("enchantment"),
                        tag("land"),
                        tag("spell"),
                        tag("card"),
                    )),
                    tag(" and exile them"),
                ),
            ),
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
    if let Some(ast) = parse_search_outside_game_ast(lower, ctx) {
        return Some(ast);
    }
    if let Some((_, _)) = nom_on_lower(text, lower, |input| value((), tag("seek ")).parse(input)) {
        let details = super::parse_seek_details(lower, ctx);
        return Some(SearchCreationImperativeAst::Seek {
            filter: details.filter,
            count: details.count,
            from_top: details.from_top,
            destination: details.destination,
            enter_tapped: details.enter_tapped,
            extra_filters: details.extra_filters,
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
        // CR 701.23a: God-Pharaoh's-Gift-class multi-zone tutors ("search your
        // graveyard, hand, and/or library for ...") — the word after the
        // possessive is a non-library zone, so `starts_with_possessive` misses
        // them; the zone-list detector routes them through the same lowering.
        || super::parse_multi_search_zones(lower).is_some()
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
            split: details.split,
            source_zones: details.source_zones,
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
        } else if tag::<_, _, OracleError<'_>>("x").parse(rest_lower).is_ok() {
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            }
        } else {
            QuantityExpr::Fixed { value: 1 }
        };
        return Some(SearchCreationImperativeAst::Dig {
            count,
            reveal,
            player: parse_dig_library_owner(rest_lower),
        });
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
        return Some(SearchCreationImperativeAst::Dig {
            count,
            reveal,
            player: TargetFilter::Controller,
        });
    }
    if let Some((_, _)) = nom_on_lower(text, lower, |input| value((), tag("create ")).parse(input))
    {
        return match try_parse_token(lower, text, ctx) {
            // `owner` is absorbed by `..`: this search/creation path always
            // yields the `TargetFilter::Controller` default, and the lowering
            // below re-emits it. A "target [player] creates" subject is lifted
            // by `inject_subject_target` after lowering.
            Some(Effect::CopyTokenOf {
                target,
                source_filter,
                enters_attacking,
                tapped,
                count,
                extra_keywords,
                additional_modifications,
                ..
            }) => Some(SearchCreationImperativeAst::CopyTokenOf {
                target,
                count,
                source_filter,
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
                supertypes,
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
                    supertypes,
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

fn parse_search_outside_game_ast(
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<SearchCreationImperativeAst> {
    // CR 400.11 + CR 400.11a + CR 701.23j: The outside-the-game pool (a player's
    // sideboard / wishboard) is selected by any of three surface verbs:
    //   - "reveal a … card you own from outside the game …" — Burning Wish,
    //     Cunning Wish, Living Wish (reveal to hand).
    //   - "play a … card you own from outside the game this turn" — Wish (M19),
    //     which grants the controller permission to play the chosen card.
    //   - "cast a … card you own from outside the game …" — same play class for
    //     spells specifically.
    // All three pull from the same pool (CR 400.11b brings the chosen card into
    // the game); the verb only changes whether the card is revealed and whether
    // a "this turn" play window is attached. Routing the play/cast forms here —
    // rather than letting them fall to `try_parse_cast_effect` — is what keeps
    // Wish from misparsing into a `CastFromZone` that targets an in-game
    // permanent the controller already owns (issue #1976).
    //
    // CR 406.3 + CR 400.11: Two source pools may appear under a single
    // "reveal … or choose a face-up … card you own in exile" disjunction
    // (Karn, the Great Creator; Coax from the Blind Eternities). The
    // controller picks one card from the union of (a) the owned outside-the-
    // game collection and (b) face-up exile cards they own matching the
    // filter. The destination clause may be inline (" and put it into your
    // hand") or a sibling sentence handled by the chain splitter.
    // (filter_text, destination, source_pool, reveal) — extracted into a
    // type alias so the parser's return type stays under the
    // clippy::type_complexity threshold.
    type OutsideGameParseFields<'a> = (&'a str, Zone, OutsideGameSourcePool, bool);
    fn parse_clause<'a>(
        input: &'a str,
    ) -> Result<(&'a str, OutsideGameParseFields<'a>), nom::Err<OracleError<'a>>> {
        // CR 701.20 / CR 701.23j vs CR 305.1 + CR 601.2a: The verb determines
        // whether the chosen card is revealed. "reveal" makes it public;
        // "play"/"cast" bring it into the game to be played without revealing.
        let (rest, reveal) = alt((
            value(true, tag("reveal ")),
            value(false, tag("play ")),
            value(false, tag("cast ")),
        ))
        .parse(input)?;
        // Article: "a "/"an " — varies by filter head noun (artifact → an).
        let (rest, _) = alt((tag("a "), tag("an "))).parse(rest)?;
        // Outside-the-game branch is mandatory and yields filter_text. Use a
        // leading-space-free anchor so the bare "a card …" form (Wish, no type
        // adjective) captures an empty filter region that lowers to
        // `TargetFilter::Any`; typed forms ("sorcery card …") capture the
        // adjective with a trailing space that the filter parser trims.
        let (rest, filter_text) = take_until("card you own from outside the game").parse(rest)?;
        let (rest, _) = tag("card you own from outside the game").parse(rest)?;
        // Optional face-up exile disjunction. Re-uses the same filter phrase
        // (Karn and Coax both repeat the filter literally in both branches);
        // we discard the second filter_text since the outside-game one is
        // canonical for the unified pool's filter.
        let (rest, face_up_exile_branch) = opt(parse_face_up_exile_branch).parse(rest)?;
        let source_pool = if face_up_exile_branch.is_some() {
            OutsideGameSourcePool::SideboardAndFaceUpExile
        } else {
            OutsideGameSourcePool::Sideboard
        };
        // Optional inline destination clause. When absent, the destination
        // arrives as a follow-up "Put that card into your hand." chunk
        // routed through the chain splitter into a ChangeZone sub-ability.
        let (rest, destination) = opt(alt((
            value(Zone::Hand, tag(" and put it into your hand")),
            value(Zone::Hand, tag(" and put that card into your hand")),
        )))
        .parse(rest)?;
        // CR 611.2a: The play/cast forms carry a trailing "this turn" window.
        // The card is brought to hand (CR 400.11b) where it can be played; the
        // window is consumed here so the clause parses to EOF cleanly rather
        // than leaving an unparsed tail that would re-route the whole line.
        let (rest, _) = opt(tag(" this turn")).parse(rest)?;
        let (rest, _) = opt(tag(".")).parse(rest)?;
        let (rest, _) = eof.parse(rest)?;
        Ok((
            rest,
            (
                filter_text,
                destination.unwrap_or(Zone::Hand),
                source_pool,
                reveal,
            ),
        ))
    }

    // CR 406.3: A "face-up ... card you own in exile" branch refers to an
    // in-game exile-zone card that is visible by default unless an effect
    // exiled it face down.
    // " or choose a face-up <filter> card you own in exile". English
    // grammar always pairs "a face-up" (the head noun begins with the
    // consonant /f/), so no "an face-up" variant exists in MTGJSON.
    fn parse_face_up_exile_branch(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
        let (rest, _) = tag(" or choose a face-up ").parse(input)?;
        let (rest, _filter_text) = take_until(" card you own in exile").parse(rest)?;
        let (rest, _) = tag(" card you own in exile").parse(rest)?;
        Ok((rest, ()))
    }

    let (_, (filter_text, destination, source_pool, reveal)) = parse_clause(lower).ok()?;
    let filter = super::search::parse_search_filter(filter_text, ctx);
    Some(SearchCreationImperativeAst::SearchOutsideGame {
        filter,
        count: QuantityExpr::Fixed { value: 1 },
        reveal,
        destination,
        up_to: true,
        source_pool,
    })
}

/// CR 400.11 + CR 400.11a + CR 701.23j: Route "play/cast a … card you own from
/// outside the game …" (Wish, M19) to the outside-game pool selector, lowering
/// the AST to `Effect::SearchOutsideGame`. The verb-keyed imperative dispatcher
/// only sends "reveal"/"search" lines through `parse_search_and_creation_ast`,
/// so the "play"/"cast" surface forms must be probed explicitly before the
/// generic `try_parse_cast_effect` parser — otherwise Wish misparses into a
/// `CastFromZone` that targets an in-game permanent (issue #1976).
pub(super) fn try_parse_play_from_outside_game(
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<Effect> {
    let ast = parse_search_outside_game_ast(lower, ctx)?;
    Some(lower_search_and_creation_ast(ast))
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
            split,
            source_zones,
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
            split,
            source_zones,
        },
        SearchCreationImperativeAst::SearchOutsideGame {
            filter,
            count,
            reveal,
            destination,
            up_to,
            source_pool,
        } => Effect::SearchOutsideGame {
            filter,
            count: if up_to {
                QuantityExpr::up_to(count)
            } else {
                count
            },
            reveal,
            destination,
            source_pool,
        },
        SearchCreationImperativeAst::Dig {
            count,
            reveal,
            player,
        } => Effect::Dig {
            player,
            count,
            destination: None,
            keep_count: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: None,
            reveal,
            enter_tapped: false,
        },
        SearchCreationImperativeAst::CopyTokenOf {
            target,
            count,
            source_filter,
            enters_attacking,
            tapped,
            extra_keywords,
            additional_modifications,
        } => Effect::CopyTokenOf {
            target,
            owner: TargetFilter::Controller,
            source_filter,
            enters_attacking,
            tapped,
            count,
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
            // CR 205.4a: Preserve parsed token supertypes (legendary/snow).
            supertypes: token.supertypes,
            static_abilities: token.static_abilities,
            enter_with_counters: vec![],
        },
        SearchCreationImperativeAst::Seek {
            filter,
            count,
            from_top,
            destination,
            enter_tapped,
            extra_filters: _,
        } => Effect::Seek {
            filter,
            count,
            from_top,
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
            enters_under: None,
            enter_tapped: false,
            face_down_profile: None,
        },
    }
}

pub(super) fn parse_hand_reveal_ast(
    text: &str,
    lower: &str,
    _ctx: &mut ParseContext,
) -> Option<HandRevealImperativeAst> {
    // CR 406.6: Private look at source-linked exile (Scroll Rack) — no "hand" in phrase.
    if let Some((_, after_look_at)) =
        nom_on_lower(text, lower, |input| value((), tag("look at ")).parse(input))
    {
        let after_look_at_lower = &lower[lower.len() - after_look_at.len()..];
        if alt((
            tag::<_, _, OracleError<'_>>("the exiled cards"),
            tag("the cards exiled this way"),
        ))
        .parse(after_look_at_lower)
        .is_ok()
        {
            return Some(HandRevealImperativeAst::LookAt {
                target: TargetFilter::ExiledBySource,
                count: None,
                random: false,
            });
        }
    }

    if nom_on_lower(text, lower, |input| value((), tag("look at ")).parse(input)).is_some()
        && nom_primitives::scan_contains(lower, "hand")
    {
        // CR 400.1/400.2 + CR 508.5 + CR 608.2c: Possessive hand phrases are
        // player references, not object targets. Map the reusable player axes
        // explicitly so combat-trigger forms like "defending player's hand" and
        // random-card forms like "a card at random in target player's hand" do
        // not fall back to parsing "card" as the effect target.
        if let Some(((target, count, random), _)) = nom_on_lower(text, lower, |input| {
            let (rest, _) = tag("look at ").parse(input)?;
            let (rest, random_count) = opt(value(
                QuantityExpr::Fixed { value: 1 },
                alt((
                    tag::<_, _, OracleError<'_>>("a card at random in "),
                    tag("one card at random in "),
                )),
            ))
            .parse(rest)?;
            let (rest, target) = parse_hand_possessive_target(rest)?;
            Ok((rest, (target, random_count.clone(), random_count.is_some())))
        }) {
            return Some(HandRevealImperativeAst::LookAt {
                target,
                count,
                random,
            });
        }

        let (_, after_look_at) =
            nom_on_lower(text, lower, |input| value((), tag("look at ")).parse(input))?;
        let (target, _) = parse_target(after_look_at);
        return Some(HandRevealImperativeAst::LookAt {
            target,
            count: None,
            random: false,
        });
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
        tag::<_, _, OracleError<'_>>("it"),
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
        return Some(HandRevealImperativeAst::RevealAll {
            card_filter: parse_hand_reveal_card_filter(after_reveal_lower),
        });
    }

    None
}

fn parse_hand_reveal_card_filter(after_reveal_lower: &str) -> TargetFilter {
    let Ok((after_all, _)) = tag::<_, _, OracleError<'_>>("all ").parse(after_reveal_lower) else {
        return TargetFilter::None;
    };
    let Ok((rest, descriptor)) = take_until::<_, _, OracleError<'_>>(" cards").parse(after_all)
    else {
        return TargetFilter::None;
    };
    if alt((
        tag::<_, _, OracleError<'_>>(" cards in "),
        tag(" cards from "),
    ))
    .parse(rest)
    .is_err()
    {
        return TargetFilter::None;
    }
    if descriptor.trim().is_empty() {
        return TargetFilter::Any;
    }
    let singular = format!("{} card", descriptor.trim());
    let (filter, rem) = parse_type_phrase(&singular);
    if rem.trim().is_empty() && matches!(filter, TargetFilter::Typed(_)) {
        filter
    } else {
        TargetFilter::None
    }
}

pub(super) fn lower_hand_reveal_ast(ast: HandRevealImperativeAst) -> Effect {
    match ast {
        HandRevealImperativeAst::LookAt {
            target,
            count,
            random,
        } => Effect::RevealHand {
            target,
            card_filter: TargetFilter::None,
            count,
            random,
            choice_optional: false,
        },
        HandRevealImperativeAst::RevealAll { card_filter } => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter,
            count: None,
            random: false,
            choice_optional: false,
        },
        HandRevealImperativeAst::RevealPartial { count } => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter: TargetFilter::None,
            count: Some(count),
            random: false,
            choice_optional: false,
        },
        // CR 701.20a: Back-reference reveal — distinct from RevealHand (zone-wide).
        // ParentTarget binds at runtime to the parent ability's affected IDs.
        HandRevealImperativeAst::RevealBackRef => Effect::Reveal {
            target: TargetFilter::ParentTarget,
        },
    }
}

fn parse_hand_possessive_target(input: &str) -> nom::IResult<&str, TargetFilter, OracleError<'_>> {
    alt((
        value(TargetFilter::Controller, tag("your hand")),
        value(TargetFilter::Player, tag("target player's hand")),
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            tag("target opponent's hand"),
        ),
        value(TargetFilter::TriggeringPlayer, tag("that player's hand")),
        value(TargetFilter::TriggeringPlayer, tag("their hand")),
        value(
            TargetFilter::DefendingPlayer,
            tag("defending player's hand"),
        ),
    ))
    .parse(input)
}

pub(super) fn parse_choose_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ChooseImperativeAst> {
    if let Some(ast) = try_parse_choose_from_zone(lower, ctx) {
        return Some(ast);
    }

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

fn try_parse_choose_from_zone(lower: &str, ctx: &mut ParseContext) -> Option<ChooseImperativeAst> {
    type E<'a> = OracleError<'a>;

    let (_, choice_text) = alt((
        preceded(tag::<_, _, E>("choose "), rest),
        preceded(tag("you choose "), rest),
    ))
    .parse(lower)
    .ok()?;

    if nom_primitives::scan_contains(choice_text, "at random") {
        return None;
    }

    let (filter_prefix, (zone_owner, zones), zone_suffix) =
        nom_primitives::scan_preceded(choice_text, parse_choose_zone_connector)?;
    if filter_prefix.trim().is_empty() {
        return None;
    }

    // The post-zone suffix ("with mana value 3 or greater") is an optional
    // search-filter restriction. `parse_search_filter`'s suffix dispatch is
    // strict: an unmodeled clause ("without blitz", "that hasn't been chosen")
    // emits a `search-filter-suffix unmatched` TargetFallback diagnostic. Probe
    // the prefix+suffix on a throwaway context first; only fold the suffix into
    // the real parse when it classifies cleanly. An unmodeled suffix is dropped
    // (the type filter still parses) rather than surfacing a false regression.
    let bare_filter_text = strip_choose_article(filter_prefix.trim())?.to_string();
    let suffix = zone_suffix.trim();
    let filter_text = if suffix.is_empty() {
        bare_filter_text
    } else {
        let with_suffix = format!("{bare_filter_text} {suffix}");
        let mut probe = ParseContext::default();
        super::search::parse_search_filter(&with_suffix, &mut probe);
        let suffix_classified = !probe.diagnostics.iter().any(|d| {
            matches!(
                d,
                OracleDiagnostic::TargetFallback { context, .. }
                    if context == "search-filter-suffix unmatched"
            )
        });
        if suffix_classified {
            with_suffix
        } else {
            bare_filter_text
        }
    };

    let filter = super::search::parse_search_filter(&filter_text, ctx);
    Some(ChooseImperativeAst::FromZone {
        count: 1,
        zones,
        zone_owner,
        filter,
        chooser: Chooser::Controller,
        up_to: false,
    })
}

fn strip_choose_article(input: &str) -> Option<&str> {
    type E<'a> = OracleError<'a>;

    alt((
        preceded(tag::<_, _, E>("a "), rest),
        preceded(tag("an "), rest),
        preceded(tag("one "), rest),
    ))
    .parse(input)
    .map(|(_, stripped)| stripped)
    .ok()
}

fn parse_choose_zone_connector(
    input: &str,
) -> nom::IResult<&str, (ZoneOwner, Vec<Zone>), OracleError<'_>> {
    type E<'a> = OracleError<'a>;

    preceded(
        alt((tag::<_, _, E>("in "), tag("from "))),
        alt((
            map(preceded(tag("your "), parse_choose_zone_list), |zones| {
                (ZoneOwner::Controller, zones)
            }),
            map(
                preceded(tag("that player's "), parse_choose_zone_list),
                |zones| (ZoneOwner::TargetedPlayer, zones),
            ),
            map(
                preceded(tag("target opponent's "), parse_choose_zone_list),
                |zones| (ZoneOwner::TargetedPlayer, zones),
            ),
            map(
                preceded(tag("an opponent's "), parse_choose_zone_list),
                |zones| (ZoneOwner::Opponent, zones),
            ),
        )),
    )
    .parse(input)
}

fn parse_choose_zone_list(input: &str) -> nom::IResult<&str, Vec<Zone>, OracleError<'_>> {
    type E<'a> = OracleError<'a>;

    let (rest, first) = parse_choose_zone(input)?;
    let (rest, second) = opt(preceded(tag::<_, _, E>(" or "), parse_choose_zone)).parse(rest)?;
    let mut zones = vec![first];
    if let Some(second) = second {
        zones.push(second);
    }
    Ok((rest, zones))
}

fn parse_choose_zone(input: &str) -> nom::IResult<&str, Zone, OracleError<'_>> {
    type E<'a> = OracleError<'a>;

    alt((
        value(Zone::Graveyard, tag::<_, _, E>("graveyard")),
        value(Zone::Library, tag("library")),
        value(Zone::Hand, tag("hand")),
        value(Zone::Exile, tag("exile")),
    ))
    .parse(input)
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
    type E<'a> = OracleError<'a>;

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
    type E<'a> = OracleError<'a>;

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
        ChooseImperativeAst::CategoryAndSacrificeRest {
            categories,
            choose_filter,
            sacrifice_filter,
            ..
        } => ChooseImperativeAst::CategoryAndSacrificeRest {
            categories,
            chooser_scope: CategoryChooserScope::ControllerForAll,
            choose_filter,
            sacrifice_filter,
        },
        other => other,
    })
}

/// CR 110.4: The six permanent types — the expansion of the "of each permanent type" idiom.
const PERMANENT_TYPE_CATEGORIES: [CoreType; 6] = [
    CoreType::Artifact,
    CoreType::Battle,
    CoreType::Creature,
    CoreType::Enchantment,
    CoreType::Land,
    CoreType::Planeswalker,
];

/// CR 101.4 + CR 701.21a: Parse the "from among ... an artifact, a creature, ..."
/// or "an artifact, a creature, ... from among ..." pattern after "choose " has been stripped.
///
/// Handles two word orders:
/// 1. "from among the permanents they control an artifact, a creature, ..." (Cataclysm)
/// 2. "an artifact, a creature, ... from among ..." (Cataclysmic Gearhulk)
///
/// Parser structure (nom combinators):
/// - `tag("from among")` detects pattern 1
/// - `parse_category_list_prefix` consumes pattern 2's category list and returns the remainder
/// - Category list: `parse_category_item` composed with comma + "and" separator
fn parse_category_and_sacrifice_rest(rest_lower: &str) -> Option<ChooseImperativeAst> {
    type E<'a> = OracleError<'a>;

    // Pattern 3 (Liliana, Dreadhorde General): generalized "a permanent [they/you/that
    // player] control[s] of each permanent type" — no enumerated category list, no
    // "from among". CR 110.4 + CR 701.21a: expands to the six permanent types; "the
    // rest" are sacrificed. Matching only through "of each permanent type" and
    // discarding the remainder mirrors patterns 1 and 2 (no "sacrifice" token matched).
    if let Ok((rest, _)) = tag::<_, _, E>("a permanent ").parse(rest_lower) {
        let controller = alt((
            tag::<_, _, E>("they control"),
            tag("you control"),
            tag("that player controls"),
        ))
        .parse(rest);
        if let Ok((rest, _)) = controller {
            if tag::<_, _, E>(" of each permanent type")
                .parse(rest)
                .is_ok()
            {
                return Some(ChooseImperativeAst::CategoryAndSacrificeRest {
                    categories: PERMANENT_TYPE_CATEGORIES.to_vec(),
                    chooser_scope: CategoryChooserScope::EachPlayerSelf,
                    choose_filter: permanent_filter(),
                    sacrifice_filter: permanent_filter(),
                });
            }
        }
    }

    // Pattern 1: "from among the permanents [they/that player] control[s] an artifact, ..."
    if let Ok((after_from_among, _)) = tag::<_, _, E>("from among ").parse(rest_lower) {
        // Skip past "the permanents they control" / "the permanents that player controls"
        // to find the category list.
        let (categories_text, choose_filter) = parse_choose_domain(after_from_among).ok()?;
        let categories = parse_category_list(categories_text)?;
        return Some(ChooseImperativeAst::CategoryAndSacrificeRest {
            categories,
            chooser_scope: CategoryChooserScope::EachPlayerSelf,
            sacrifice_filter: choose_filter.clone(),
            choose_filter,
        });
    }

    // Pattern 2: "an artifact, a creature, ... from among [the nonland] permanents they control"
    if let Ok((after_categories, categories)) = parse_category_list_prefix(rest_lower) {
        let (_, choose_filter) = preceded(
            preceded(opt(tag::<_, _, E>(",")), tag(" from among ")),
            parse_choose_domain,
        )
        .parse(after_categories)
        .ok()?;
        return Some(ChooseImperativeAst::CategoryAndSacrificeRest {
            categories,
            chooser_scope: CategoryChooserScope::EachPlayerSelf,
            sacrifice_filter: choose_filter.clone(),
            choose_filter,
        });
    }

    None
}

fn permanent_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::permanent())
}

fn nonland_permanent_filter() -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::permanent().with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
    )
}

/// Parse "the permanents they control" / "the [nonland] permanents that player controls"
/// domains and return the category-list remainder plus the domain filter.
fn parse_choose_domain(input: &str) -> OracleResult<'_, TargetFilter> {
    type E<'a> = OracleError<'a>;

    let (rest, _) = tag::<_, _, E>("the ").parse(input)?;
    let (rest, nonland) = opt(tag::<_, _, E>("nonland ")).parse(rest)?;
    let (rest, _) = tag::<_, _, E>("permanents ").parse(rest)?;
    let (rest, _) = alt((
        tag::<_, _, E>("they control"),
        tag("you control"),
        tag("that player controls"),
    ))
    .parse(rest)?;
    let (rest, _) = opt(alt((tag::<_, _, E>(", "), tag(" ")))).parse(rest)?;
    Ok((
        rest,
        if nonland.is_some() {
            nonland_permanent_filter()
        } else {
            permanent_filter()
        },
    ))
}

/// Parse a comma-separated category list: "an artifact, a creature, an enchantment, and a land"
/// Uses nom combinators for each category item.
fn parse_category_list(input: &str) -> Option<Vec<CoreType>> {
    type E<'a> = OracleError<'a>;

    let (remaining, categories) = parse_category_list_prefix(input).ok()?;
    let remaining = remaining.trim_start();
    if remaining.is_empty()
        || tag::<_, _, E>(", then ").parse(remaining).is_ok()
        || tag::<_, _, E>(". then ").parse(remaining).is_ok()
        || tag::<_, _, E>(".").parse(remaining).is_ok()
    {
        return Some(categories);
    }

    None
}

fn parse_category_list_prefix(input: &str) -> OracleResult<'_, Vec<CoreType>> {
    type E<'a> = OracleError<'a>;

    let (mut remaining, first) = parse_category_item(input)?;
    let mut categories = vec![first];

    while let Ok((after_separator, _)) = alt((
        tag::<_, _, E>(", and "),
        tag(", "),
        tag(" and "),
        tag("and "),
    ))
    .parse(remaining)
    {
        let Ok((after_item, core_type)) = parse_category_item(after_separator) else {
            break;
        };
        categories.push(core_type);
        remaining = after_item;
    }

    Ok((remaining, categories))
}

fn parse_category_item(input: &str) -> OracleResult<'_, CoreType> {
    type E<'a> = OracleError<'a>;

    let (input, _) = alt((tag::<_, _, E>("an "), tag("a "))).parse(input)?;
    parse_core_type_name(input).ok_or_else(|| {
        nom::Err::Error(OracleError::from_error_kind(
            input,
            nom::error::ErrorKind::Alt,
        ))
    })
}

/// Parse a core type name from lowercase text using nom combinators.
fn parse_core_type_name(input: &str) -> Option<(&str, CoreType)> {
    type E<'a> = OracleError<'a>;

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
            random: false,
            choice_optional,
        },
        // CR 700.2: Anaphoric "choose N of them/those" → select from the tracked set
        // populated by the preceding effect (RevealTop, RevealHand, ExileTop, etc.).
        ChooseImperativeAst::FromTrackedSet { count, chooser } => Effect::ChooseFromZone {
            count,
            zone: Zone::Exile,
            additional_zones: Vec::new(),
            zone_owner: ZoneOwner::Controller,
            filter: None,
            chooser,
            up_to: false,
            constraint: None,
        },
        ChooseImperativeAst::FromZone {
            count,
            zones,
            zone_owner,
            filter,
            chooser,
            up_to,
        } => {
            let mut zones = zones.into_iter();
            let zone = zones.next().unwrap_or(Zone::Hand);
            Effect::ChooseFromZone {
                count,
                zone,
                additional_zones: zones.collect(),
                zone_owner,
                filter: Some(filter),
                chooser,
                up_to,
                constraint: None,
            }
        }
        // CR 101.4 + CR 701.21a: Multi-category permanent selection + sacrifice rest.
        ChooseImperativeAst::CategoryAndSacrificeRest {
            categories,
            chooser_scope,
            choose_filter,
            sacrifice_filter,
        } => Effect::ChooseAndSacrificeRest {
            categories,
            chooser_scope,
            choose_filter,
            sacrifice_filter,
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
    ctx: &mut ParseContext,
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
                let rest_lower = &lower[lower.len() - rest.len()..];
                let (target, _rem) = if let Some((target, rem_lower)) =
                    parse_copy_stack_ability_target(rest_lower)
                {
                    let rem = &rest[rest.len() - rem_lower.len()..];
                    (target, rem)
                } else {
                    parse_target(rest)
                };
                #[cfg(debug_assertions)]
                assert_no_compound_remainder(_rem, text);
                Some(UtilityImperativeAst::Copy { target })
            }
            _ => unreachable!(),
        };
    }
    if let Some((attachment_text, target_text)) = nom_on_lower(text, lower, |input| {
        let (input, _) = tag("unattach all ").parse(input)?;
        let (input, attachment) = terminated(take_until(" from "), tag(" from ")).parse(input)?;
        Ok((input, attachment.to_string()))
    }) {
        let (attachment, attachment_rem) = parse_type_phrase(attachment_text.trim());
        let (target, target_rem) = parse_target_with_ctx(target_text, ctx);
        if attachment_rem.trim().is_empty() && target_rem.trim().is_empty() {
            return Some(UtilityImperativeAst::UnattachAll { attachment, target });
        }
    }
    // CR 701.27 + CR 701.28: "transform" and "convert" are equivalent game actions.
    // CR 608.2k: the bare-pronoun and self-deictic arms ("transform it" /
    // "transform itself" / "transform this creature") split into two anaphor
    // classes:
    //   • Self-deictic ("~"/"this <type>") always binds to the source.
    //   • Bare object pronoun ("it"/"itself") binds via `resolve_it_pronoun`,
    //     which returns `TriggeringSource` when the parse context carries a
    //     non-self trigger subject (Serpent's Soul-Jar pattern, issue #319),
    //     and `SelfRef` otherwise (Primal Amulet self-trigger).
    if matches!(
        lower,
        "transform"
            | "transform ~"
            | "transform this"
            | "transform this creature"
            | "transform this permanent"
            | "transform this artifact"
            | "transform this land"
            | "convert"
            | "convert ~"
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
    if matches!(
        lower,
        "transform it" | "transform itself" | "convert it" | "convert itself"
    ) {
        // CR 608.2k: bare object pronoun resolves via the same dispatch as
        // exile/destroy — typed trigger subject → TriggeringSource (Werewolf
        // packs class), self-ref/any/none → ParentTarget (Primal Amulet's
        // self-trigger preserves source via the empty-targets fallback at
        // CR 608.2c). The diagnostic-only pronoun arg is uniform across the
        // bare object pronoun family per `is_bare_object_pronoun`.
        return Some(UtilityImperativeAst::Transform {
            target: resolve_pronoun_target(ctx, "it"),
        });
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("transform "), tag("convert ")))).parse(input)
    }) {
        // CR 608.2k: thread `ctx` so dynamic targets like "transform that
        // creature" / "transform target permanent" resolve anaphors via the
        // same trigger-subject machinery.
        let (target, _) = parse_target_with_ctx(rest, ctx);
        if !matches!(target, TargetFilter::Any) {
            return Some(UtilityImperativeAst::Transform { target });
        }
    }
    // CR 613.4d: switch power and toughness — two surface forms (sibling branches):
    //   - prepositional: "switch the power and toughness of <target>" (Inversion
    //     Behemoth class — supports the "(each of) any number of target X"
    //     distribution, with multi_target recovered by
    //     `extract_switch_pt_multi_target` in the post-parse fixup. Authorizing
    //     rule for variable-count targeting: CR 115.1d.)
    //   - possessive: "switch <target>'s power and toughness" (single-target
    //     class — Inversion of Fortune, Twiddle's siblings).
    // Try prepositional first so the more specific "the power and toughness of"
    // shape is consumed before the bare "switch <target>" form runs.
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("switch the power and toughness of ")).parse(input)
    }) {
        // Strip the optional "each of " and "any number of " distribution
        // prefixes so `parse_target` sees a bare target phrase. The quantifier
        // itself is recovered as a `MultiTargetSpec` in mod.rs via
        // `extract_switch_pt_multi_target` (parallel to the DealDamage / Double
        // counter fixups). Walking the lowercased view in lock-step with the
        // original text preserves casing for `parse_target`.
        let rest_lower = rest.to_ascii_lowercase();
        let mut consumed = 0usize;
        if let Ok((after, _)) = tag::<_, _, OracleError<'_>>("each of ").parse(rest_lower.as_str())
        {
            consumed = rest_lower.len() - after.len();
        }
        let after_each_lower = &rest_lower[consumed..];
        if let Ok((after, _)) =
            tag::<_, _, OracleError<'_>>("any number of ").parse(after_each_lower)
        {
            consumed += after_each_lower.len() - after.len();
        }
        let target_text = &rest[consumed..];
        let (target, rem) = parse_target_with_ctx(target_text, ctx);
        let rem_lower = rem.trim_start().to_ascii_lowercase();
        // The trailing duration ("until end of turn") is stripped upstream by
        // `strip_trailing_duration`; in that case `rem` is empty. Accept either
        // form so the branch also matches when this parser is invoked directly
        // on text that retains the duration (e.g. unit tests).
        let rem_after_duration = tag::<_, _, OracleError<'_>>("until end of turn")
            .parse(rem_lower.as_str())
            .map(|(rest, _)| rest)
            .unwrap_or(rem_lower.as_str());
        let mut terminal = alt((
            value((), eof),
            value((), all_consuming(tag::<_, _, OracleError<'_>>("."))),
        ));
        if terminal.parse(rem_after_duration).is_ok() {
            return Some(UtilityImperativeAst::SwitchPT { target });
        }
    }
    // CR 613.4d: "switch [target]'s power and toughness"
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("switch ")).parse(input))
    {
        let (target, rem) = parse_target(rest);
        // Consume "'s power and toughness" or " power and toughness" suffix
        let rem_lower = rem.to_lowercase();
        if tag::<_, _, OracleError<'_>>("'s power and toughness")
            .parse(rem_lower.as_str())
            .is_ok()
            || tag::<_, _, OracleError<'_>>(" power and toughness")
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
            let (target, _target_rem) = parse_attach_recipient(&target_text, ctx);
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
        // CR 608.2k: same anaphor dispatch as the explicit-attach arm above.
        let (target, _rem) = parse_target_with_ctx(after_to, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(UtilityImperativeAst::Attach {
            attachment: TargetFilter::SelfRef,
            target,
        });
    }
    None
}

fn parse_copy_stack_ability_target(input: &str) -> Option<(TargetFilter, &str)> {
    let (input, _) = opt(tag::<_, _, OracleError<'_>>("target "))
        .parse(input)
        .ok()?;
    let (input, _) = alt((
        tag::<_, _, OracleError<'_>>("activated or triggered ability"),
        tag("triggered or activated ability"),
        tag("activated ability"),
        tag("triggered ability"),
    ))
    .parse(input)
    .ok()?;
    let (input, _) = nom::character::complete::multispace0::<_, OracleError<'_>>(input).ok()?;
    if let Ok((rem, _)) = tag::<_, _, OracleError<'_>>("you control").parse(input) {
        return Some((
            TargetFilter::StackAbility {
                controller: Some(ControllerRef::You),
            },
            rem,
        ));
    }
    if input.is_empty()
        || all_consuming(tag::<_, _, OracleError<'_>>("."))
            .parse(input)
            .is_ok()
    {
        return Some((TargetFilter::StackAbility { controller: None }, input));
    }
    None
}

pub(super) fn stack_ability_filter_from_text(input: &str) -> TargetFilter {
    let controller = if nom_primitives::scan_contains(input, "you control") {
        Some(ControllerRef::You)
    } else if nom_primitives::scan_contains(input, "opponents control")
        || nom_primitives::scan_contains(input, "opponent controls")
        || nom_primitives::scan_contains(input, "opponent's control")
    {
        Some(ControllerRef::Opponent)
    } else {
        None
    };
    TargetFilter::StackAbility { controller }
}

fn parse_explicit_targeted_attach(
    input: &str,
) -> nom::IResult<&str, (String, String), OracleError<'_>> {
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

fn parse_attach_recipient<'a>(text: &'a str, ctx: &mut ParseContext) -> (TargetFilter, &'a str) {
    // CR 608.2k: thread `ctx` so "attach this Equipment to it" in trigger
    // bodies binds "it" to the triggering subject (Ancestral Katana —
    // "Whenever a Samurai or Warrior you control attacks alone … attach this
    // Equipment to it"). Pre-existing "her" / "him" → SelfRef carve-out is
    // preserved for legacy attach-to-self phrasings (e.g. "attach to her").
    let (target, rest) = parse_target_with_ctx(text, ctx);
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
) -> nom::IResult<&str, (TargetFilter, TargetFilter), OracleError<'_>> {
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
            let rest = tag::<_, _, OracleError<'_>>("regenerate ")
                .parse(&*lower)
                .map(|(r, _)| r)
                .unwrap_or(&lower);
            let (target, _) = parse_target(rest);
            Effect::Regenerate { target }
        }
        UtilityImperativeAst::Copy { target } => Effect::CopySpell {
            target,
            // Step 4 continuation absorption upgrades this to MayChooseNewTargets
            // when "you may choose new targets for the copy" follows (CR 707.10c).
            retarget: CopyRetargetPermission::KeepOriginalTargets,
            copier: None,
        },
        UtilityImperativeAst::Transform { target } => Effect::Transform { target },
        UtilityImperativeAst::Attach { attachment, target } => {
            Effect::Attach { attachment, target }
        }
        UtilityImperativeAst::UnattachAll { attachment, target } => {
            Effect::UnattachAll { attachment, target }
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
    let rest = tag::<_, _, OracleError<'_>>("prevent ")
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
    let amount = if tag::<_, _, OracleError<'_>>("all ").parse(rest).is_ok() {
        PreventionAmount::All
    } else if let Ok((after_next, _)) = tag::<_, _, OracleError<'_>>("the next ").parse(rest) {
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

    // CR 609.7 + CR 615.2: prevention scoped to a chosen source (a targeted
    // spell). "Prevent all damage target instant or sorcery spell would deal
    // this turn" (Dromoka's Command) restricts the shield to a SINGLE chosen
    // source object, not a blanket recipient-scoped prevent-all. When this
    // matches, the recipient `target` collapses to `Any` and the source scope
    // is carried as the `damage_source_filter`.
    if let Some(source_filter) = parse_prevent_source_scope(text, &lower) {
        // CR 609.7a: the source is chosen when the effect is created and applies
        // even after the spell leaves the stack. The chosen spell lands in
        // `ability.targets[0]` (the recipient `Any` surfaces no slot), so the
        // shield captures it via `ParentTargetSlot { index: 0 }`.
        return Effect::PreventDamage {
            amount,
            amount_dynamic: None,
            target: TargetFilter::Any,
            scope,
            damage_source_filter: Some(TargetFilter::And {
                filters: vec![TargetFilter::ParentTargetSlot { index: 0 }, source_filter],
            }),
        };
    }

    // Determine target
    let target = if nom_primitives::scan_contains(rest, "any target") {
        TargetFilter::Any
    } else if nom_primitives::scan_contains(rest, "target creature")
        || nom_primitives::scan_contains(rest, "target permanent")
    {
        // Extract the target from the text
        let tp = TextPair::new(text, &lower);
        if let Ok((_, before)) = take_until::<_, _, OracleError<'_>>("target ").parse(tp.lower) {
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

    let damage_source_filter = parse_prevent_damage_source_filter(text, &lower);

    // CR 615.11 + CR 107.3i: `amount_dynamic` (the "prevent X … where X is
    // <quantity>" override) is populated at chunk level by
    // `apply_where_x_effect_expression`, not here — the chunk machinery
    // strips the trailing "where x is …" binding before this parser ever
    // sees it. `amount` is the Next(1) fallback used when no dynamic clause
    // is present (or when the binding fails to resolve to a known quantity).
    Effect::PreventDamage {
        amount,
        amount_dynamic: None,
        target,
        scope,
        damage_source_filter,
    }
}

/// CR 615.1: Optional trailing "by [source-filter]" on prevent clauses
/// (Arachnogenesis: "by non-Spider creatures").
fn parse_prevent_damage_source_filter(text: &str, lower: &str) -> Option<TargetFilter> {
    let (_, filter_text) = nom_on_lower(text, lower, |input| {
        value(
            (),
            (take_until::<_, _, OracleError<'_>>(" by "), tag(" by ")),
        )
        .parse(input)
    })?;
    let filter_text = filter_text.trim().trim_end_matches('.');
    let (filter, rem) = parse_type_phrase(filter_text);
    if rem.trim().is_empty() && matches!(filter, TargetFilter::Typed(_)) {
        Some(filter)
    } else {
        None
    }
}

/// CR 609.7 + CR 615.2: Detect the source-scoped prevent form — "prevent
/// [all/the next N] damage **target `<source>`** would deal this turn"
/// (Dromoka's Command: "Prevent all damage target instant or sorcery spell
/// would deal this turn").
///
/// This is distinct from the recipient form ("...damage **to** target
/// creature"), which the caller already handles. The disambiguator is the
/// trailing `" would deal"` clause: a source-scoped prevent names what the
/// chosen object *deals*, whereas the recipient form names what is dealt *to*
/// the target. We isolate the region before `" would deal"`, take the source
/// descriptor after `"target "` (both via the `split_once_on_lower` combinator
/// bridge — `take_until`+`tag`, never ad-hoc string dispatch), and run
/// `parse_target` on it. We accept ONLY when the parsed filter resolves to a
/// choosable stack source (a spell on the stack) — `parse_target` is the
/// detector.
///
/// Returns the parsed `<source>` filter (e.g. an `Or`/`And` of stack-spell
/// `Typed` legs) on match, or `None` to fall through to the recipient/by-source
/// logic.
fn parse_prevent_source_scope(text: &str, lower: &str) -> Option<TargetFilter> {
    // Isolate the slice that precedes " would deal" (the source phrase region).
    // `split_once_on_lower` composes `take_until`+`tag` internally and returns
    // original-case slices, so the borrowed remainder escapes cleanly.
    let (before_would_deal, _) = split_once_on_lower(text, lower, " would deal")?;
    // Within that region, the source descriptor follows the "target " keyword.
    let region_lower = before_would_deal.to_lowercase();
    let (_, source_descriptor) = split_once_on_lower(before_would_deal, &region_lower, "target ")?;
    let (source_filter, rem) = parse_target(source_descriptor);
    // The candidate must consume the entire source descriptor and resolve to a
    // choosable spell on the stack — otherwise this is not the source-scoped
    // form (e.g. "target creature" recipient).
    if rem.trim().is_empty() && filter_is_choosable_stack_source(&source_filter) {
        Some(source_filter)
    } else {
        None
    }
}

/// CR 609.7a: A source-scope prevent's chosen source must be a choosable object
/// on the stack — a spell (`StackSpell`, or a `Typed` filter scoped `InZone
/// Stack`), possibly wrapped in `And`/`Or`/`Not` (e.g. "instant or sorcery
/// spell" → `Or` of two stack-spell legs). Mirrors `targeting.rs`'s
/// `filter_targets_stack_spells`, kept local so the parser stays dependency-free
/// of the runtime targeting module.
fn filter_is_choosable_stack_source(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::StackSpell => true,
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::InZone { zone } if *zone == Zone::Stack)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_is_choosable_stack_source)
        }
        TargetFilter::Not { filter } => filter_is_choosable_stack_source(filter),
        _ => false,
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

/// CR 611.2c + CR 702.10: Coalesce a subjectless continuous body that combines a
/// P/T pump with one or more keyword/ability grants ("get +2/+0 and gain haste
/// until end of turn") into a single `Effect::GenericEffect`, mirroring the
/// subject-bound `build_continuous_clause` so both paths emit the same mods.
///
/// Returns `None` for a pure pump body (no non-P/T modification) so the caller
/// falls through to the existing bare-`Effect::Pump` numeric arm unchanged, and
/// `None` for a keyword-only body (handled by `try_parse_gain_keyword`).
fn coalesce_pump_with_modifications(body_text: &str) -> Option<Effect> {
    let (without_duration, duration) = super::strip_trailing_duration(body_text);
    let modifications = parse_continuous_modifications(without_duration);
    let has_pt = modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::AddPower { .. } | ContinuousModification::AddToughness { .. }
        )
    });
    let has_non_pt = modifications.iter().any(|m| {
        !matches!(
            m,
            ContinuousModification::AddPower { .. } | ContinuousModification::AddToughness { .. }
        )
    });
    if !(has_pt && has_non_pt) {
        return None;
    }
    let duration = duration.or(Some(Duration::UntilEndOfTurn));
    Some(Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(modifications)
            .description(body_text.to_string())],
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
    tag::<_, _, OracleError<'_>>("put ").parse(lower).ok()?;

    if nom_on_lower(text, lower, |input| {
        value(
            (),
            (
                tag("put "),
                tag("that many "),
                tag("cards "),
                tag("from the top of your library "),
                tag("into your hand"),
            ),
        )
        .parse(input)
    })
    .is_some()
    {
        return Some(PutImperativeAst::PutTopCardsIntoHandMatchingExileCount);
    }

    if let Ok((after, _)) = tag::<_, _, OracleError<'_>>("put the top ").parse(lower) {
        if nom_primitives::scan_contains(lower, "graveyard") {
            let count = nom_primitives::parse_number
                .parse(after)
                .map(|(_, n)| n)
                .unwrap_or(1);
            return Some(PutImperativeAst::Mill { count });
        }
    }

    // "put X on top of Y's library" — specific position, no auto-shuffle.
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

    // "put that card on top" / "put it on top" / "put them on top" —
    // abbreviated form used after "shuffle" in search-and-put-on-top tutors (41 cards).
    if lower.ends_with("on top") {
        return Some(PutImperativeAst::TopOfLibrary);
    }

    // "put X on the bottom of Y's library" — specific position without
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

    // "put that card on the bottom" / "put it on the bottom" —
    // abbreviated form without "of Y's library".
    if lower.ends_with("on the bottom") {
        return Some(PutImperativeAst::BottomOfLibrary);
    }

    // "put X into Y's library Nth from the top" —
    // specific positional placement (God-Eternals, Approach, Bury in Books).
    if let Ok((_, before_from)) = take_until::<_, _, OracleError<'_>>("from the top").parse(lower) {
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

    if let Some((effect, choice_count)) = super::try_parse_put_zone_change_parts(lower, text) {
        return match effect {
            Effect::ChangeZoneAll {
                origin,
                destination,
                target,
                enters_under,
                enter_tapped,
                ..
            } => Some(PutImperativeAst::ZoneChangeAll {
                origin,
                destination,
                target,
                enters_under,
                enter_tapped,
            }),
            Effect::ChangeZone {
                origin,
                destination,
                target,
                enters_under,
                enter_tapped,
                enter_transformed,
                enters_attacking,
                up_to,
                enter_with_counters,
                ..
            } => Some(PutImperativeAst::ZoneChange {
                origin,
                destination,
                target,
                enters_under,
                enter_tapped,
                enter_transformed,
                enters_attacking,
                up_to,
                choice_count: choice_count.map(Box::new),
                enter_with_counters,
            }),
            _ => None,
        };
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
        PutImperativeAst::ZoneChangeAll {
            origin,
            destination,
            target,
            enters_under,
            enter_tapped,
        } => Effect::ChangeZoneAll {
            origin,
            destination,
            target,
            enters_under,
            enter_tapped,
            face_down_profile: None,
        },
        PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
            enters_under,
            enter_tapped,
            enter_transformed,
            enters_attacking,
            up_to,
            choice_count: _,
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
                    enters_under: None,
                    enter_tapped,
                    face_down_profile: None,
                }
            } else {
                Effect::ChangeZone {
                    origin,
                    destination,
                    target,
                    owner_library: false,
                    enter_transformed,
                    enters_under,
                    enter_tapped,
                    // CR 508.4: Propagated from the inline-tail patcher in
                    // `try_parse_put_zone_change` (Kaalia / Ilharg class).
                    enters_attacking,
                    up_to,
                    enter_with_counters,
                    face_down_profile: None,
                }
            }
        }
        // Place at a specific position — uses move_to_library_position,
        // not ChangeZone which shuffles the destination library. `count` defaults to
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
        PutImperativeAst::PutTopCardsIntoHandMatchingExileCount => Effect::Mill {
            count: QuantityExpr::Ref {
                qty: QuantityRef::CardsExiledBySource,
            },
            target: TargetFilter::Controller,
            destination: Zone::Hand,
        },
    }
}

/// Parse "put that many {type} counter(s) on {target}" — dynamic counter count from event context.
/// CR 120.1: "that many" references the amount from the triggering event (e.g., damage dealt).
/// Produces PutCounter with count=0 as a sentinel for event-context resolution.
fn try_parse_that_many_counters(lower: &str, ctx: &mut ParseContext) -> Option<Effect> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("put that many ")
        .parse(lower)
        .ok()?;
    // Counter type (e.g. "+1/+1", "charge", "loyalty", "double strike").
    // CR 122.1 + CR 122.1b: route through the shared `parse_counter_type_typed`
    // combinator so multi-word keyword counter names ("first strike", "double
    // strike") canonicalize to `CounterType::Keyword(...)` instead of being
    // truncated at the first whitespace.
    let (after_type, counter_type) = nom_primitives::parse_counter_type_typed(rest).ok()?;

    // Skip "counter" or "counters" keyword
    let after_type = after_type.trim_start();
    let after_counter = alt((tag::<_, _, OracleError<'_>>("counters"), tag("counter")))
        .parse(after_type)
        .map(|(r, _)| r)
        .unwrap_or(after_type)
        .trim_start();

    // Parse target after "on"
    let target = if let Ok((on_rest, _)) = tag::<_, _, OracleError<'_>>("on ").parse(after_counter)
    {
        if alt((tag::<_, _, OracleError<'_>>("~"), tag("this ")))
            .parse(on_rest)
            .is_ok()
        {
            TargetFilter::SelfRef
        } else if alt((tag::<_, _, OracleError<'_>>("it"), tag("itself")))
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

    // "That many" resolves from trigger event context at runtime.
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
    let (rest, _) = tag::<_, _, OracleError<'_>>("remove ").parse(lower).ok()?;
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
fn parse_possessive_determiner(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
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

fn parse_shuffle_origin_zones(input: &str) -> nom::IResult<&str, Vec<Zone>, OracleError<'_>> {
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
/// CR 400.6 + CR 701.24c: Recognizes whole-zone bulk moves like Whirlpool Drake's
/// "shuffle the cards from your hand into your library" and Midnight Clock's
/// "shuffle your hand and graveyard into your library". The origin-zone phrase
/// names every card in each listed zone — no targeting or filtering — so the
/// resulting AST lowers to `ChangeZoneAll` (not `ChangeZone`).
///
/// Supports zones: hand, graveyard, exile. Returns None for any other structure.
fn parse_mass_zones_to_library(lower: &str) -> Option<Vec<Zone>> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("shuffle ").parse(lower).ok()?;
    let (rest, _) = opt(preceded(
        tag::<_, _, OracleError<'_>>("the cards "),
        alt((tag("from "), tag("in "))),
    ))
    .parse(rest)
    .ok()?;
    // "{possessive} "
    let (rest, _) = parse_possessive_determiner(rest).ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" ").parse(rest).ok()?;
    let (rest, origins) = parse_shuffle_origin_zones(rest).ok()?;
    // " into {possessive} library"
    let (rest, _) = tag::<_, _, OracleError<'_>>(" into ").parse(rest).ok()?;
    let (rest, _) = parse_possessive_determiner(rest).ok()?;
    let (_rest, _) = tag::<_, _, OracleError<'_>>(" library").parse(rest).ok()?;
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
    if tag::<_, _, OracleError<'_>>("shuffle")
        .parse(lower)
        .is_err()
        || !nom_primitives::scan_contains(lower, "library")
    {
        return None;
    }

    // CR 608.2c + CR 701.24a: "shuffle {possessive} library" — exact
    // library-shuffle forms only. Compound forms like "shuffle your graveyard
    // into your library" fall through to the zone-change parser below.
    if let Ok((_, target)) = parse_shuffle_library_target(lower) {
        return Some(ShuffleImperativeAst::ShuffleLibrary { target });
    }
    // CR 701.24c + CR 400.3: "shuffle <pronoun> into <possessive> library" —
    // covers "shuffle it into its owner's library" (Cavalier cycle), "shuffle
    // ~ into its owner's library" (Green Sun's Zenith, Beacon cycle, Nexus of
    // Fate — self-referential tutors that shuffle themselves back), "shuffle
    // that card into its owner's library" (search-then-shuffle tutors),
    // "shuffle them into their owners' libraries" (compound subject). Both
    // pronoun (it/them/that card/those cards/~) and possessive (its owner's /
    // their owner's / their owners' / your) are classified via nom combinators
    // so the lowered `ChangeZone` carries the correct `target` (SelfRef vs
    // ParentTarget) and `owner_library` flag. The `~` token is produced by
    // `normalize_card_name_refs` for self-references; it is handled by
    // `contains_self_or_object_pronoun` here (not `contains_object_pronoun`)
    // because the anaphoric/self-reference distinction matters at other call
    // sites (compound action splitting in `try_split_targeted_compound`).
    if contains_self_or_object_pronoun(lower, "shuffle", "into")
        || contains_self_or_object_pronoun(lower, "shuffles", "into")
    {
        // Pronoun classification. Walk word-boundaries, peel "shuffle"/
        // "shuffles" + " ", then alt() over the five recognized references.
        // "it" / "~" → SelfRef (singular, anaphoric to the source object;
        // "~" is the self-reference token from `normalize_card_name_refs`);
        // "them" / "that card" / "those cards" → ParentTarget (refers to a
        // previously-bound target). The fall-through "SelfRef" arm only
        // engages when the outer `contains_self_or_object_pronoun` guard
        // somehow matched a pronoun the inner combinator didn't recognize —
        // defensive and also matches the existing "shuffle this creature
        // into …" form.
        let target = nom_primitives::scan_at_word_boundaries(lower, |input| {
            let (rest, _) =
                alt((tag::<_, _, OracleError<'_>>("shuffle "), tag("shuffles "))).parse(input)?;
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
                value((), tag::<_, _, OracleError<'_>>("its owner's library")),
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
    // CR 701.24c + CR 400.3: "shuffle <descriptive target> into <possessive>
    // library" — covers "shuffle enchanted creature into its owner's library"
    // (Dramatic Accusation, Stay Hidden Stay Silent) and any future card that
    // names a non-pronoun target phrase before "into ... library". The target
    // is parsed by `parse_target` which handles "enchanted creature",
    // "equipped creature", "target creature", etc.
    // Placed after the pronoun path (which handles it/them/~/that card) so
    // pronoun forms are not accidentally consumed by `parse_target`.
    if let Some(((target_phrase, owner_library), _)) = nom_on_lower(lower, lower, |input| {
        let (input, _) = tag::<_, _, OracleError<'_>>("shuffle ").parse(input)?;
        let (input, target_phrase) = take_until(" into ").parse(input)?;
        not(take_until::<_, _, OracleError<'_>>(" from ")).parse(target_phrase)?;
        let (input, _) = tag(" into ").parse(input)?;
        let (input, owner_library) = alt((
            value(true, tag("its owner's library")),
            value(true, tag("their owner's library")),
            value(true, tag("their owners' libraries")),
        ))
        .parse(input)?;
        Ok((input, (target_phrase.to_string(), owner_library)))
    }) {
        let (target, _) = parse_target(&target_phrase);
        if !matches!(target, TargetFilter::Any) {
            return Some(ShuffleImperativeAst::ChangeZoneToLibrary {
                target,
                owner_library,
            });
        }
    }
    // CR 701.24c + CR 400.6: "shuffle the cards from {possessive} {zone} into
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
    // CR 701.24c: "shuffle target card from your graveyard into your library" —
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

fn parse_shuffle_library_target(
    input: &str,
) -> crate::parser::oracle_nom::error::OracleResult<'_, TargetFilter> {
    let (input, _) = tag("shuffle ").parse(input)?;
    let (input, target) = alt((
        value(TargetFilter::Controller, tag("your")),
        value(
            TargetFilter::Owner,
            alt((tag("his or her owner's"), tag("its owner's"))),
        ),
        value(
            TargetFilter::ParentTarget,
            alt((
                tag("that player's"),
                tag("his or her"),
                tag("their"),
                tag("that"),
            )),
        ),
    ))
    .parse(input)?;
    let (input, _) = tag(" library").parse(input)?;
    let (input, _) = eof(input)?;

    Ok((input, target))
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
            // CR 701.24c + CR 400.3: `target` and `owner_library` are
            // populated by `parse_shuffle_ast`'s combinator-based pronoun /
            // possessive classification. See the construction site for the
            // detection grammar and the TODO on the `Shuffle` sub-target.
            let effect = Effect::ChangeZone {
                origin: None,
                destination: Zone::Library,
                target,
                owner_library,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
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
                enters_under: None,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
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
    entries: Vec<(crate::types::counter::CounterType, QuantityExpr)>,
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

/// CR 701.23a + CR 701.23h: Lower a multi-filter library search ("a X card
/// and a Y card [and a Z card ...], put them onto the battlefield [tapped],
/// then shuffle") into a single `SearchLibrary` when each printed filter asks
/// for one card. The prompt exposes the union of legal cards while
/// `SearchSelectionConstraint::MatchEachFilter` requires the submitted set to
/// be assignable to each printed description.
///
/// Non-unit counts, `up to`, or additional group constraints keep the older
/// chained lowering below so those semantics remain explicit until they can be
/// represented as a composed selection constraint.
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
    if !up_to
        && selection_constraint == SearchSelectionConstraint::None
        && matches!(count, QuantityExpr::Fixed { value: 1 })
    {
        let filters = std::iter::once(primary_filter)
            .chain(extra_filters)
            .collect::<Vec<_>>();
        return parsed_clause(Effect::SearchLibrary {
            filter: TargetFilter::Or {
                filters: filters.clone(),
            },
            count: QuantityExpr::Fixed {
                value: filters.len() as i32,
            },
            reveal,
            target_player,
            selection_constraint: SearchSelectionConstraint::MatchEachFilter { filters },
            split: None,
            source_zones: vec![crate::types::zones::Zone::Library],
        });
    }

    // Build the chain right-to-left so each link owns its successor. The chain
    // ends at a `SearchLibrary` (the last extra filter) so the outer intrinsic
    // continuation can append the terminal `ChangeZone` for that last search.
    let change_zone_effect = || Effect::ChangeZone {
        origin: Some(Zone::Library),
        destination,
        target: TargetFilter::Any,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        face_down_profile: None,
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
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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
        split: None,
        source_zones: vec![crate::types::zones::Zone::Library],
    });
    clause.sub_ability = tail;
    clause
}

pub(super) fn lower_multi_filter_seek(
    primary_filter: TargetFilter,
    count: QuantityExpr,
    from_top: Option<usize>,
    destination: Zone,
    enter_tapped: bool,
    extra_filters: Vec<TargetFilter>,
) -> ParsedEffectClause {
    let mut tail: Option<Box<AbilityDefinition>> = None;
    for extra_filter in extra_filters.into_iter().rev() {
        let mut seek_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Seek {
                filter: extra_filter,
                count: count.clone(),
                from_top,
                destination,
                enter_tapped,
            },
        );
        seek_def.sub_ability = tail;
        tail = Some(Box::new(seek_def));
    }

    let mut clause = parsed_clause(Effect::Seek {
        filter: primary_filter,
        count,
        from_top,
        destination,
        enter_tapped,
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
            split: None,
            source_zones: vec![crate::types::zones::Zone::Library],
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
    // CR 400.3: When the parent `ChangeZone` routes to the target's owner's
    // library (`owner_library: true`), the implicit shuffle must randomize that
    // same library — not the spell controller's (Chaos Warp stolen-permanent case).
    let shuffle_target = match &effect {
        Effect::ChangeZone {
            owner_library: true,
            ..
        } => TargetFilter::ParentTargetOwner,
        _ => TargetFilter::Controller,
    };
    let shuffle = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Shuffle {
            target: shuffle_target,
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
        unless_pay: None,
    }
}

fn change_zone_all_to_library_effect(origin: Zone) -> Effect {
    Effect::ChangeZoneAll {
        origin: Some(origin),
        destination: Zone::Library,
        target: TargetFilter::Controller,
        enters_under: None,
        enter_tapped: false,
        face_down_profile: None,
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

pub(super) fn parse_destroy_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ZoneCounterImperativeAst> {
    if nom_on_lower(text, lower, |input| {
        value((), alt((tag("destroy all "), tag("destroy each ")))).parse(input)
    })
    .is_some()
    {
        let (_, rest) = nom_on_lower(text, lower, |input| value((), tag("destroy ")).parse(input))?;
        // CR 608.2k: thread `ctx` so bare "it"/"them" anaphors bind to the
        // triggering subject ("Whenever a creature dies, destroy it" class).
        let (target, _rem) = parse_target_with_ctx(rest, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(ZoneCounterImperativeAst::Destroy { target, all: true });
    }
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("destroy ")).parse(input))
    {
        // CR 608.2k: see comment above — anaphor binding via parse_target_with_ctx.
        let (target, _rem) = parse_target_with_ctx(rest, ctx);
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
    fn inner(i: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
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
    ctx: &mut ParseContext,
) -> Option<ZoneCounterImperativeAst> {
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("exile the top ").parse(lower) {
        let (initial_count, remainder) =
            if let Ok((rem, n)) = nom_primitives::parse_number.parse(rest) {
                (QuantityExpr::Fixed { value: n as i32 }, rem.trim_start())
            } else if let Ok((rem, _)) = tag::<_, _, OracleError<'_>>("x").parse(rest) {
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
        // CR 608.2: "their library" inside an outer "each player" / "each opponent"
        // player_scope iteration binds to the iterating player. The chunk_ctx
        // surfaces this via `relative_player_scope = ScopedPlayer`, which
        // `that_player_library_filter` already maps to `TargetFilter::ScopedPlayer`.
        // For the third-person "each player's library" alternate phrasing,
        // return `ScopedPlayer`; effect-chain lowering lifts that sentinel into
        // `player_scope: All` so the existing scoped resolver exiles from each
        // player's own library without target selection.
        // CR 400.12 + CR 115.1: "target opponent's library" / "target player's
        // library" are zone-as-operand phrases on a *chosen* player. They
        // reuse the canonical leaves produced by `parse_target` for the
        // bare phrases ("target opponent" → Typed{controller: Opponent},
        // "target player" → TargetFilter::Player) so the runtime selector
        // applies the same legality/targeting machinery used elsewhere
        // (Maralen, Fae Ascendant ETB; Court of Locthwain ETB).
        let target_opponent_filter =
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
        for (pattern, player) in [
            ("card of your library", TargetFilter::Controller),
            ("cards of your library", TargetFilter::Controller),
            ("card of that player's library", that_player.clone()),
            ("cards of that player's library", that_player.clone()),
            ("card of their library", that_player.clone()),
            ("cards of their library", that_player.clone()),
            (
                "card of target opponent's library",
                target_opponent_filter.clone(),
            ),
            (
                "cards of target opponent's library",
                target_opponent_filter.clone(),
            ),
            ("card of target player's library", TargetFilter::Player),
            ("cards of target player's library", TargetFilter::Player),
            ("card of each player's library", TargetFilter::ScopedPlayer),
            ("cards of each player's library", TargetFilter::ScopedPlayer),
        ] {
            if let Ok((after_lib, _)) = tag::<_, _, OracleError<'_>>(pattern).parse(remainder) {
                // CR 406.3: Detect the "face down" suffix Oracle text uses to
                // mark hidden-information exiles (Necropotence / Bomat Courier
                // / Asmodeus the Archfiend / Knowledge Vault class). The
                // resolver propagates this to the moved object's `face_down`
                // flag so `visibility.rs` redacts the card for non-owners.
                let (after_lib, face_down) = strip_exile_top_face_down(after_lib);
                // CR 107.3i: Optional ", where x is <quantity expr>" suffix
                // overrides the leading `Variable { "X" }` binding with the
                // dynamic quantity expression. Mirrors the
                // try_parse_token_enters_with_counters / put-counters-on-token
                // followup patterns. Without this, the trigger has no chosen X
                // (it's an ETB-triggered ability, not a cast), and the count
                // would default to 0 at resolution time.
                let count = resolve_exile_top_where_x_binding(after_lib, initial_count);
                return Some(ZoneCounterImperativeAst::ExileTop {
                    player,
                    count,
                    face_down,
                });
            }
        }

        // CR 701.13: Exile — "exile the top card[s]" with NO "of <player>'s
        // library" qualifier is an implicit-controller top-of-library exile
        // (Urza, Lord High Artificer's {5}; Bloodsoaked Insight; the
        // "shuffle ..., then exile the top card" class). The twelve qualified
        // suffix patterns above run FIRST so a real "of <player>'s library"
        // continuation is never shadowed by this fallback.
        // CR 701.24: Shuffle — after a shuffle the library order is RANDOM, so
        // the exiled top card is determined by post-shuffle order; the player
        // makes NO selection. This deterministic top-of-library ExileTop is the
        // rules-correct shape — NOT a library-wide ChangeZone (which would offer
        // a tutor prompt). The ABSENCE of a selection prompt is correct.
        let head = remainder.trim_start();
        if let Ok((after_noun, _)) = alt((
            tag::<_, _, OracleError<'_>>("cards"),
            tag::<_, _, OracleError<'_>>("card"),
        ))
        .parse(head)
        {
            // Require this is a clause boundary, NOT a qualified continuation
            // (" of <player>'s library", already handled above). `not(tag(" of "))`
            // rejects the qualified form on the raw tail; the boundary check
            // accepts EOF or a sentence/clause terminator (after an optional
            // single separator space).
            let not_qualified = not(tag::<_, _, OracleError<'_>>(" of "))
                .parse(after_noun)
                .is_ok();
            let boundary = after_noun.strip_prefix(' ').unwrap_or(after_noun); // allow-noncombinator: trim one optional separator before nom boundary peek
            let at_boundary = alt((
                value((), eof::<_, OracleError<'_>>),
                value((), peek(one_of(".,"))),
            ))
            .parse(boundary)
            .is_ok();
            if not_qualified && at_boundary {
                // CR 406.3: honor a trailing "face down" suffix exactly as the
                // twelve qualified patterns do.
                let (after_lib, face_down) = strip_exile_top_face_down(after_noun);
                // CR 107.3c: honor a trailing ", where x is <expr>" binding —
                // the text defines X, so the controller doesn't choose it.
                let count = resolve_exile_top_where_x_binding(after_lib, initial_count);
                return Some(ZoneCounterImperativeAst::ExileTop {
                    player: TargetFilter::Controller,
                    count,
                    face_down,
                });
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
            enter_with_counters: vec![],
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
            enter_with_counters: vec![],
        });
    }

    // "exile <filter> from your hand [with <counter suffix>]"
    // Mirror the original arm's `terminated(take_until, tag)`; the ONLY change
    // vs. the original is dropping the trailing `eof` from the terminator so a
    // post-hand tail is allowed. `terminated` returns the `take_until` slice
    // (the card filter) and consumes " from your hand"; the parser remainder
    // (`after_hand`) is the post-hand tail.
    if let Ok((after_hand, (_, filter_text))) = (
        opt(nom_primitives::parse_article),
        terminated(take_until(" from your hand"), tag(" from your hand")),
    )
        .parse(rest_lower)
    {
        let (mut target, rem) = parse_type_phrase(filter_text.trim());
        // `after_hand` is the parser remainder AFTER "from your hand": empty/"."
        // (no-tail case) or " with a number of … counters on it equal to …".
        let enter_with_counters = super::parse_with_counters_suffix(after_hand);
        let tail_clean = after_hand.trim().trim_start_matches('.').trim();
        // Guard against silently dropping an unrecognized trailing clause: the
        // arm declines unless the tail is empty or fully consumed as a counter
        // suffix. An unconsumed tail falls through to the generic exile path.
        let tail_is_consumed = tail_clean.is_empty() || !enter_with_counters.is_empty();
        if rem.trim().is_empty() && !matches!(target, TargetFilter::Any) && tail_is_consumed {
            attach_controller_if_absent(&mut target, ControllerRef::You);
            if let TargetFilter::Typed(typed) = &mut target {
                typed
                    .properties
                    .push(FilterProp::InZone { zone: Zone::Hand });
            }
            return Some(ZoneCounterImperativeAst::Exile {
                origin: Some(Zone::Hand),
                target,
                all: false,
                enter_with_counters,
            });
        }
    }

    // CR 608.2c: "exile a nonland card from among them" selects from the
    // previous effect's published set, then moves that selected card to exile.
    // The destination is the effect's job; the origin is intentionally open
    // because the tracked set already identifies the exact object(s).
    if let Ok((after_among, (_, filter_text))) = (
        opt(nom_primitives::parse_article),
        terminated(
            take_until(" from among "),
            alt((tag(" from among them"), tag(" from among those cards"))),
        ),
    )
        .parse(rest_lower)
    {
        let (target, rem) = parse_type_phrase(filter_text.trim());
        let tail_clean = after_among.trim().trim_start_matches('.').trim(); // allow-noncombinator: punctuation cleanup after typed terminator
        if rem.trim().is_empty() && !matches!(target, TargetFilter::Any) && tail_clean.is_empty() {
            return Some(ZoneCounterImperativeAst::Exile {
                origin: None,
                target: TargetFilter::TrackedSetFiltered {
                    id: crate::types::identifiers::TrackedSetId(0),
                    filter: Box::new(target),
                },
                all: false,
                enter_with_counters: vec![],
            });
        }
    }

    // CR 608.2k: thread `ctx` through so bare "it"/"them" anaphors in trigger
    // bodies ("Whenever an Elf you control dies, exile it") bind to the
    // triggering subject via `resolve_pronoun_target`, not the ability source.
    // Issue #319: Serpent's Soul-Jar exiled itself instead of the dying Elf.
    let (parsed_target, _rem) = parse_target_with_ctx(rest_text, ctx);
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
        enter_with_counters: vec![],
    })
}

pub(super) fn that_player_library_filter(ctx: &ParseContext) -> TargetFilter {
    if matches!(ctx.relative_player_scope, Some(ControllerRef::ScopedPlayer)) {
        return TargetFilter::ScopedPlayer;
    }
    if matches!(ctx.relative_player_scope, Some(ControllerRef::TargetPlayer)) {
        return TargetFilter::TriggeringPlayer;
    }

    match &ctx.subject {
        Some(TargetFilter::Typed(tf)) if tf.type_filters.is_empty() && tf.controller.is_some() => {
            TargetFilter::TriggeringPlayer
        }
        Some(TargetFilter::Player) => TargetFilter::TriggeringPlayer,
        _ => TargetFilter::ParentTarget,
    }
}

/// CR 406.3: Strip a trailing "face down" suffix from the remainder of an
/// `exile the top ... library` body. Returns `(remaining_text, face_down)`.
/// The Oracle text places "face down" immediately after the library clause
/// and before any subsequent dynamic-count or follow-on phrase (Necropotence,
/// Bomat Courier, Asmodeus the Archfiend, Knowledge Vault). Matching at this
/// boundary keeps the downstream `where x is` and "those cards" lowering
/// paths untouched.
fn strip_exile_top_face_down(after_lib: &str) -> (&str, bool) {
    let trimmed = after_lib.trim_start();
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("face down").parse(trimmed) {
        // Word-boundary check after the nom tag accepted: clause
        // terminators (EOF, '.', ',') or a separator (' ') keep us from
        // bleeding into a larger identifier. Not a parser dispatch — the
        // dispatch was the `tag` above; this is post-tag validation.
        match rest.chars().next() {
            None | Some('.') | Some(',') | Some(' ') => return (rest, true),
            _ => {}
        }
    }
    (after_lib, false)
}

/// CR 107.3i: Resolve a `", where x is <quantity expr>"` suffix that follows an
/// `Effect::ExileTop` body, overriding the leading `Variable { "X" }` binding
/// with the dynamic quantity expression. Mirrors the X-binding suffix logic
/// already used by `try_parse_token_enters_with_counters` for declarative
/// "the token enters with X +1/+1 counters on it, where X is …" cards.
///
/// Trigger contexts have no chosen X (cast cost-X-paid only exists on a spell
/// cast), so the parse-time substitution is the only way the resolver sees the
/// dynamic count. When the suffix is missing or the inner phrase fails to
/// parse to a known quantity, the original count (typically `Variable { "X" }`
/// or `Fixed { value: N }`) is returned unchanged.
fn resolve_exile_top_where_x_binding(after_lib: &str, initial_count: QuantityExpr) -> QuantityExpr {
    let trimmed = after_lib
        .trim_start()
        .trim_start_matches([',', '.', ' '])
        .trim_start();
    let Ok((rest_where, _)) = alt((
        tag::<_, _, OracleError<'_>>("where x is "),
        tag("where X is "),
    ))
    .parse(trimmed) else {
        return initial_count;
    };
    let qty_text = rest_where
        .trim_end()
        .trim_end_matches(['.', ','])
        .trim_end();
    if let Some(expr) = crate::parser::oracle_quantity::parse_event_context_quantity(qty_text) {
        return expr;
    }
    if let Some(qty) = crate::parser::oracle_quantity::parse_quantity_ref(qty_text) {
        return QuantityExpr::Ref { qty };
    }
    initial_count
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
    fn abilities_head(i: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
        preceded(nom::character::complete::multispace0, tag("abilities")).parse(i)
    }
    let abilities_match = nom_primitives::scan_contains(rest, "activated or triggered ability")
        || (mass_consumed && abilities_head(rest).is_ok());
    if abilities_match {
        // CR 118.12: Parse "unless pays" even for ability counters.
        let unless_pay = super::parse_unless_payment(rest).map(super::counter_unless_pay_modifier);
        return Some(ZoneCounterImperativeAst::Counter {
            target: stack_ability_filter_from_text(rest),
            source_rider: None,
            unless_pay,
            all: mass_consumed,
        });
    }

    // CR 701.6a + CR 115.1: "Counter target <stack-object phrase>" where the
    // phrase is a multi-way disjunction of spells and/or activated/triggered
    // abilities — e.g. Louisoix's Sacrifice's "activated ability, triggered
    // ability, or noncreature spell". Bare `parse_target` cannot recognize the
    // "activated ability" disjunct (it is not a card type) and silently drops
    // the noncreature restriction, yielding a degenerate empty-`type_filters`
    // stack filter. Strip the leading "target " token (nom `tag`, not string
    // matching) and try the dedicated stack-object combinator first; it
    // composes the ability-kind axis with an optional restricted-spell tail.
    let stack_phrase = opt(tag::<_, _, OracleError<'_>>("target "))
        .parse(rest)
        .map(|(after, _)| after)
        .unwrap_or(rest);
    if let Ok((_, stack_target)) =
        crate::parser::oracle_nom::target::parse_stack_object_target(stack_phrase)
    {
        let unless_pay = super::parse_unless_payment(rest).map(super::counter_unless_pay_modifier);
        return Some(ZoneCounterImperativeAst::Counter {
            target: stack_target,
            source_rider: None,
            unless_pay,
            all: mass_consumed,
        });
    }

    // CR 107.3i + CR 202.3 + CR 608.2b: A trailing "where X is <expression>"
    // defining clause (Spellstutter Sprite: "counter target spell with mana
    // value X or less, where X is the number of Faeries you control") binds
    // the literal X in the type filter's `Cmc` bound. Without this
    // substitution the bound stays `QuantityRef::Variable("X")` with no
    // defining expression, collapses to 0 at resolution, and the target
    // legality re-check at CR 608.2b (target legality is verified again as a
    // spell or ability resolves) fails every legal spell. The strip-and-apply
    // pattern mirrors the Birthing Ritual callsite (see
    // `oracle_effect/mod.rs:16859`).
    let (without_where_tp, where_x_expression) =
        super::strip_trailing_where_x(crate::parser::oracle_util::TextPair::new(rest_orig, rest));
    let (target, _rem) = parse_target(without_where_tp.original);
    #[cfg(debug_assertions)]
    assert_no_compound_remainder(_rem, text);
    // `parse_target("... spell with mana value X or less")` already scopes
    // the spell phrase to the stack through the shared target parser. Keep this
    // path on that building block and only apply the trailing X definition.
    let target = super::apply_where_x_to_filter(target, where_x_expression.as_deref());
    // CR 118.12: Parse "unless its controller pays {X}" for conditional counters
    let unless_pay = super::parse_unless_payment(rest).map(super::counter_unless_pay_modifier);
    Some(ZoneCounterImperativeAst::Counter {
        target,
        source_rider: None,
        unless_pay,
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
    use nom::combinator::recognize;
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
    if let Ok((tail, _)) = tag::<_, _, OracleError<'_>>("life equal to ").parse(rest) {
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
    if terminated(tag::<_, _, OracleError<'_>>("x life"), word_boundary)
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
        terminated(tag::<_, _, OracleError<'_>>(" life"), word_boundary),
    )
        .parse(rest)
    {
        return Some(QuantityExpr::Fixed { value: n as i32 });
    }

    None
}

fn parse_mana_and_life_payment(rest_orig: &str) -> Option<PaymentCost> {
    let (mana_cost, after_mana) = parse_mana_symbols(rest_orig.trim())?;
    let after_mana_lower = after_mana.to_lowercase();
    let (_, after_and) = nom_on_lower(after_mana, &after_mana_lower, |input| {
        value(
            (),
            preceded(nom::character::complete::multispace0, tag("and ")),
        )
        .parse(input)
    })?;
    let amount = parse_pay_life_amount(after_and.trim_start())?;
    Some(PaymentCost::AbilityCost {
        cost: AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana { cost: mana_cost },
                AbilityCost::PayLife { amount },
            ],
        },
    })
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
        if let Some(cost) = parse_mana_and_life_payment(rest_orig) {
            return Some(CostResourceImperativeAst::Pay { cost });
        }
        // CR 118.8 + CR 119.4: `pay <amount> life` — literal count, X variable,
        // or dynamic reference (`pay life equal to its power`). Dispatched with
        // nom combinators over the post-"pay " remainder.
        if let Some(amount) = parse_pay_life_amount(rest) {
            return Some(CostResourceImperativeAst::Pay {
                cost: PaymentCost::Life { amount },
            });
        }
        // CR 107.14: "pay any amount of {E}" → variable energy payment
        if let Ok((_, _)) = tag::<_, _, OracleError<'_>>("any amount of {e}").parse(rest) {
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
        // CR 118.1 + CR 107.3: "pay any amount of mana" → variable generic
        // mana payment. Join forces uses the total paid this way as X for the
        // following effect chain.
        if let Ok((_, _)) = tag::<_, _, OracleError<'_>>("any amount of mana").parse(rest) {
            return Some(CostResourceImperativeAst::Pay {
                cost: PaymentCost::Mana {
                    cost: crate::types::mana::ManaCost::Cost {
                        shards: vec![crate::types::mana::ManaCostShard::X],
                        generic: 0,
                    },
                },
            });
        }
        // "pay an amount of {e} equal to ..." → dynamic energy payment.
        // Delegates to the single-authority combinator
        // `parse_dynamic_energy_unless_cost`; the `Variable("X")` fallback
        // below covers "equal to" tails the quantity parser cannot resolve.
        if tag::<_, _, OracleError<'_>>("an amount of {e} equal to ")
            .parse(rest)
            .is_ok()
        {
            if let Some(amount) = super::parse_dynamic_energy_unless_cost(rest) {
                return Some(CostResourceImperativeAst::Pay {
                    cost: PaymentCost::Energy { amount },
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
    // CR 106.4 + CR 505.1: Dispatch to the mana detector for a bare "add …"
    // clause OR a subject-led mana clause ("the active player adds …", "that
    // player adds …") — `try_parse_add_mana_effect` strips the subject itself.
    if nom_on_lower(text, lower, |input| {
        alt((
            value((), tag("add ")),
            value(
                (),
                (
                    alt((tag("the active player "), tag("that player "))),
                    tag("adds "),
                ),
            ),
        ))
        .parse(input)
    })
    .is_some()
    {
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
                damage_source: None,
            } => Some(CostResourceImperativeAst::Damage {
                amount,
                target,
                all: true,
            }),
            // DealDamage with damage_source, DamageEachPlayer, DamageAll with
            // a non-default damage_source/player_filter — pass through directly.
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
                    damage_source: None,
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

/// CR 500.8 + CR 510.2: Quantity for "<N> additional <step/phase>s". The
/// scanner advances along word boundaries and tries a single composed
/// combinator at each position:
///   `quantifier ~ " additional"` where `quantifier` =
///     `tag("that many")` → event-bound (`QuantityRef::EventContextAmount`)
///   | `parse_number`        → literal N (e.g. "two additional combat phases")
///
/// Anything else — including the article forms "an"/"a"/"the" already parsed
/// elsewhere as 1 — falls through to the singular default
/// `QuantityExpr::Fixed { value: 1 }`. Anchoring on `" additional"` keeps the
/// helper agnostic to surrounding sentence shapes ("you get that many
/// additional upkeep steps after this phase", "after this phase, there is an
/// additional combat phase").
fn parse_additional_phase_count(lower: &str) -> QuantityExpr {
    fn count_combinator(input: &str) -> OracleResult<'_, QuantityExpr> {
        let event_bound = value(
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            tag("that many"),
        );
        let literal = map(nom_primitives::parse_number, |n| QuantityExpr::Fixed {
            value: n as i32,
        });
        terminated(alt((event_bound, literal)), tag(" additional")).parse(input)
    }

    let mut remaining = lower;
    while !remaining.is_empty() {
        if let Ok((_rest, qty)) = count_combinator(remaining) {
            return qty;
        }
        // Advance to the next word boundary so the combinator stays anchored
        // to candidate quantifier positions.
        remaining = remaining
            .find(' ')
            .map_or("", |i| remaining[i + 1..].trim_start());
    }
    QuantityExpr::Fixed { value: 1 }
}

pub(super) fn parse_imperative_family_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ImperativeFamilyAst> {
    let first_word = lower.split_whitespace().next().unwrap_or("");

    // CR 724.1: "end the turn" (Time Stop, Sundial of the Infinite, Obeka,
    // Glorious End, Discontinuity, Day's Undoing). Whole-phrase imperative
    // with no target; parse it as an anchored nom production rather than a
    // substring scan so unrelated clauses cannot accidentally match it.
    if all_consuming(terminated(
        tag::<_, _, OracleError<'_>>("end the turn"),
        opt(tag(".")),
    ))
    .parse(lower.trim())
    .is_ok()
    {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::EndTheTurn));
    }

    // CR 724.2: "end the combat phase" (Mandate of Peace). Whole-phrase
    // imperative with no target; anchored nom production mirroring the
    // "end the turn" parse so unrelated clauses cannot accidentally match it.
    if all_consuming(terminated(
        tag::<_, _, OracleError<'_>>("end the combat phase"),
        opt(tag(".")),
    ))
    .parse(lower.trim())
    .is_ok()
    {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::EndCombatPhase));
    }

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
            count: parse_additional_phase_count(lower),
        }));
    }
    if nom_primitives::scan_contains(lower, "additional upkeep step") {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::AdditionalPhase {
            target: TargetFilter::Controller,
            phase: Phase::Upkeep,
            after: Phase::Upkeep,
            followed_by: vec![],
            count: parse_additional_phase_count(lower),
        }));
    }

    // CR 606.3: "activate each planeswalker's loyalty ability an additional
    // time this turn" — The Chain Veil class. The outer "you may" is
    // stripped by `strip_optional_effect_prefix`, leaving the imperative
    // "activate ..." form here. The grant raises the per-permanent CR 606.3
    // activation cap by `amount` for the rest of the turn (the resolver lives
    // in `effects::grant_extra_loyalty_activations`).
    //
    // Quantity axes accepted:
    //   - "an additional time"        → +1
    //   - "an additional <number> times" → +N (future-proofs cards that grant
    //     more than one additional activation in a single ability).
    if let Some(amount) = parse_grant_extra_loyalty_activations(lower) {
        return Some(ImperativeFamilyAst::GainKeyword(
            Effect::GrantExtraLoyaltyActivations {
                amount,
                target: TargetFilter::Controller,
            },
        ));
    }

    // CR 722.1: "You control target player during that player's next turn"
    // (Mindslaver / Word of Command class). "You" is the spell/ability controller
    // in a declarative sentence (not an imperative verb), so this bypasses the
    // first_word dispatch below. Delegates to the ControlNextTurn combinator
    // in `parse_targeted_action_ast`.
    if tag::<_, _, OracleError<'_>>("you control ")
        .parse(lower)
        .is_ok()
    {
        if let Some(ast) = parse_targeted_action_ast(text, lower, ctx) {
            return Some(ImperativeFamilyAst::Structured(ImperativeAst::Targeted(
                ast,
            )));
        }
    }

    // CR 614.9 + CR 614.1a + CR 615: One-shot "the next time [source] would deal
    // [combat] damage [to X] this turn, [modify/redirect] instead" damage
    // replacement (Desperate Gambit, Soltari Guerrillas, Beacon of Destiny, Jade
    // Monolith, Goblin Psychopath). The text begins with "the next time" — not a
    // verb — so it is intercepted here, alongside the other non-verb-led effects
    // above, before the first-word verb dispatch. The detector is the prefix
    // combinator inside `parse_oneshot_damage_replacement`; on failure it returns
    // `None` and we fall through.
    if let Some(effect) = crate::parser::oracle_replacement::parse_oneshot_damage_replacement(lower)
    {
        return Some(ImperativeFamilyAst::GainKeyword(effect));
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
            // "draw that many cards" / "draw that many cards minus one" →
            // EventContextAmount-based quantities, bypassing numeric AST which
            // can only represent fixed u32 counts.
            let count = tag::<_, _, OracleError<'_>>("draw ")
                .parse(lower)
                .ok()
                .and_then(|(tail_lower, _)| {
                    let tail = &text[text.len() - tail_lower.len()..];
                    super::super::oracle_quantity::parse_event_context_quantity(tail)
                })
            .unwrap_or(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            });
            Some(ImperativeFamilyAst::GainKeyword(Effect::Draw {
                count,
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
        "prevent" | "regenerate" | "copy" | "attach" | "unattach" | "switch" => {
            parse_utility_imperative_ast(text, lower, ctx)
                .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Utility(ast)))
        }
        // CR 701.27 + CR 701.28: "transform" and "convert" are equivalent game actions.
        "transform" | "transforms" | "convert" | "converts" => {
            parse_utility_imperative_ast(text, lower, ctx)
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
            if tag::<_, _, OracleError<'_>>("manifest dread")
                .parse(lower)
                .is_ok()
            {
                Some(ImperativeFamilyAst::ManifestDread)
            } else if let Ok((rest, _)) =
                tag::<_, _, OracleError<'_>>("manifest the top ").parse(lower)
            {
                // CR 701.40a: "manifest the top card of your library"
                // or "manifest the top N cards of your/that player's library"
                let parsed = alt((
                    value(
                        QuantityExpr::Fixed { value: 1 },
                        alt((
                            tag::<_, _, OracleError<'_>>("card "),
                            tag("cards "),
                        )),
                    ),
                    map(nom_primitives::parse_number, |n| QuantityExpr::Fixed {
                        value: n as i32,
                    }),
                ))
                .parse(rest);

                let (count, after_count) = if let Ok((after_count, count)) = parsed {
                    let after_count = if matches!(&count, QuantityExpr::Fixed { value: 1 }) {
                        after_count
                    } else if let Ok((after_cards, _)) = preceded(
                        tag::<_, _, OracleError<'_>>(" "),
                        alt((tag("card "), tag("cards "))),
                    )
                    .parse(after_count)
                    {
                        after_cards
                    } else {
                        after_count
                    };
                    (count, after_count)
                } else {
                    (QuantityExpr::Fixed { value: 1 }, rest)
                };

                let target = if tag::<_, _, OracleError<'_>>("of your library")
                    .parse(after_count)
                    .is_ok()
                {
                    TargetFilter::Controller
                } else if tag::<_, _, OracleError<'_>>("of that player's library")
                    .parse(after_count)
                    .is_ok()
                {
                    that_player_library_filter(ctx)
                } else {
                    TargetFilter::Controller
                };
                Some(ImperativeFamilyAst::Manifest { target, count })
            } else {
                None
            }
        }
        "proliferate" => Some(ImperativeFamilyAst::Proliferate),
        // CR 701.56a: "time travel" / "time travel N times"
        "time" => {
            if tag::<_, _, OracleError<'_>>("time travel")
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
            if tag::<_, _, OracleError<'_>>("clash with an opponent")
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
            let rest = alt((tag::<_, _, OracleError<'_>>("blight "), tag("blight")))
                .parse(lower)
                .map(|(r, _)| r)
                .unwrap_or("");
            let count = nom_primitives::parse_number
                .parse(rest.trim())
                .map(|(_, n)| n)
                .unwrap_or(1);
            Some(ImperativeFamilyAst::GainKeyword(Effect::BlightEffect {
                count,
            }))
        }
        // Forage keyword action (CR 701.61a)
        "forage" => Some(ImperativeFamilyAst::GainKeyword(Effect::Forage)),
        // Collect evidence N keyword action (CR 702.163a)
        "collect" => {
            if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("collect evidence ").parse(lower)
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
            let rest = alt((tag::<_, _, OracleError<'_>>("endure "), tag("endures ")))
                .parse(lower)
                .map(|(r, _)| r)
                .unwrap_or("");
            // CR 701.63b: "endure X" degrades to 0 (nothing happens). parse_number_or_x
            // maps a bare "x" to 0; the unwrap fallback only applies when no count token
            // is present at all.
            let count = nom_primitives::parse_number_or_x
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
            let (rest, _) = tag::<_, _, OracleError<'_>>("support ")
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
            if let Ok((_, ast)) = value::<_, _, OracleError<'_>, _>(
                ImperativeFamilyAst::FlipCoinUntilLose,
                alt((
                    tag::<_, _, OracleError<'_>>("flip a coin until you lose a flip"),
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
            value::<_, _, OracleError<'_>, _>(
                ImperativeFamilyAst::FlipCoin,
                alt((
                    tag::<_, _, OracleError<'_>>("flip a coin"),
                    tag("flips a coin"),
                )),
            )
            .parse(lower)
            .ok()
            .map(|(_, ast)| ast)
        }
        // CR 701.52: "roll to visit your Attractions" (not a generic d20/d6 roll).
        "roll" | "rolls" => {
            if nom_parse_lower(lower, |input| {
                value(
                    ImperativeFamilyAst::RollToVisitAttractions,
                    (
                        alt((tag("roll"), tag("rolls"))),
                        tag(" to visit your attractions"),
                        eof,
                    ),
                )
                .parse(input)
            })
            .is_some()
            {
                Some(ImperativeFamilyAst::RollToVisitAttractions)
            } else if let Some(ast) = try_parse_roll_n_dice(lower) {
                // CR 706.1: "roll two six-sided dice" / "roll X d12" — the
                // multi-dice form. Tried before the single-die path; returns
                // None for count == 1 so "roll a d6" / "roll one d6" falls through.
                Some(ast)
            } else {
                try_parse_roll_die_with_modifier(lower).map(|(sides, modifier)| {
                    ImperativeFamilyAst::RollDie {
                        count: QuantityExpr::Fixed { value: 1 },
                        sides,
                        modifier,
                    }
                })
            }
        }
        // CR 701.51b: "open an Attraction" / "open two Attractions"
        "open" | "opens" => parse_open_attraction_imperative(lower),
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
                tag::<_, _, OracleError<'_>>("venture into the undercity"),
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
        // CR 726.1: "take the initiative"
        "take" | "takes" => {
            if alt((
                value((), tag::<_, _, OracleError<'_>>("take the initiative")),
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
                value(PhaseDir::Out, tag::<_, _, OracleError<'_>>("phase out")),
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
                if let Ok((mass_rest, _)) = alt((
                    tag::<_, _, OracleError<'_>>("all "),
                    tag::<_, _, OracleError<'_>>("each "),
                ))
                .parse(rest)
                {
                    let (target, _) = parse_target(mass_rest);
                    return Some(ImperativeFamilyAst::GainKeyword(Effect::GoadAll {
                        target,
                    }));
                }
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
            // CR 701.12a: "exchange <player>'s life total with ~'s power/toughness"
            // (Tree of Perdition, Tree of Redemption, Evra) — checked before
            // "exchange control of" since the two shapes share only the verb.
            if let Some((player, stat)) = try_parse_exchange_life_with_stat(lower) {
                return Some(ImperativeFamilyAst::ExchangeLifeWithStat { player, stat });
            }
            let (rest, _) = tag::<_, _, OracleError<'_>>("exchange control of ")
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
            if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("must be blocked").parse(lower) {
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
            } else if let Some(effect) =
                try_parse_gain_keyword(text).or_else(|| try_parse_gain_quoted_ability(text))
            {
                // CR 113.3 + CR 604.1: grant a keyword ability (or, via
                // try_parse_gain_quoted_ability, a quoted ability) to an object.
                // Checked BEFORE the life-gain branch because the bare
                // `scan_contains(lower, "life")` guard below also matches
                // keywords whose name contains "life" — e.g. "gain lifelink",
                // which otherwise misrouted to the numeric life-gain parser and
                // fell through to Unimplemented. `try_parse_gain_keyword`
                // returns `None` unless it actually finds a keyword, so genuine
                // life-gain clauses ("gain 3 life") still fall through below.
                Some(ImperativeFamilyAst::GainKeyword(effect))
            } else if nom_primitives::scan_contains(lower, "life") {
                parse_numeric_imperative_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)))
            } else {
                None
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

        // "look" → exiled/hand targets (step 4) → "look at the top" (step 5)
        "look" => parse_hand_reveal_ast(text, lower, ctx) // allow-noncombinator: pre-existing match dispatch; exiled-card look must precede library-top search
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::HandReveal(ast)))
            .or_else(|| {
                parse_search_and_creation_ast(text, lower, ctx)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(ast)))
            }),

        // "gets"/"get" → try player counter first, then the subjectless
        // pump+keyword coalescer (CR 611.2c: keeps "gain haste" attached to the
        // P/T pump as one GenericEffect), then bare numeric pump (step 3). Pure
        // "get +N/+M" bodies coalesce to None and fall through to the numeric arm.
        "gets" | "get" => try_parse_player_counter(lower)
            .or_else(|| coalesce_pump_with_modifications(text).map(ImperativeFamilyAst::GainKeyword))
            .or_else(|| {
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
            // CR 611.2c: subjectless pump+keyword body ("get +2/+0 and gain
            // haste ...") — coalesce into one GenericEffect so the keyword grant
            // is not dropped by the bare-Pump numeric arm. Pure pump → None.
            if let Some(effect) = coalesce_pump_with_modifications(text) {
                return Some(ImperativeFamilyAst::GainKeyword(effect));
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

/// CR 701.12a: Parse "exchange <player>'s life total with ~'s power/toughness"
/// (Tree of Perdition, Tree of Redemption, Evra, Halcyon Witness) into the
/// exchanged player filter and the source stat. Returns `None` for any other
/// "exchange" shape so the caller falls through to "exchange control of".
///
/// The whole clause must be consumed (only a trailing terminator may remain) so
/// unrelated "exchange" phrasings don't match partially.
fn try_parse_exchange_life_with_stat(lower: &str) -> Option<(TargetFilter, PtStat)> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("exchange ")
        .parse(lower)
        .ok()?;
    let (rest, player) = parse_exchange_life_player(rest)?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("life total with ")
        .parse(rest)
        .ok()?;
    // Source possessive: "~'s " (self-reference normalization) or the literal
    // "this creature's " form, both naming the ability's source permanent.
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("~'s "),
        tag("this creature's "),
    ))
    .parse(rest)
    .ok()?;
    let (rest, stat) = alt((
        value(PtStat::Toughness, tag::<_, _, OracleError<'_>>("toughness")),
        value(PtStat::Power, tag("power")),
    ))
    .parse(rest)
    .ok()?;
    if !rest
        .trim_start()
        .trim_end_matches(['.', ';'])
        .trim()
        .is_empty()
    {
        return None;
    }
    Some((player, stat))
}

/// CR 119: Parse the player whose life total is exchanged. "your" binds to the
/// ability's controller (no target); "target opponent's" / "target player's"
/// declare a player target. Returns the filter and the remaining text after the
/// possessive.
fn parse_exchange_life_player(input: &str) -> Option<(&str, TargetFilter)> {
    alt((
        value(
            TargetFilter::Controller,
            tag::<_, _, OracleError<'_>>("your "),
        ),
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            tag("target opponent's "),
        ),
        value(TargetFilter::Player, tag("target player's ")),
    ))
    .parse(input)
    .ok()
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
        tag::<_, _, OracleError<'_>>("two target "),
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
    let (right, (left, _)) =
        nom::sequence::pair(take_until::<_, _, OracleError<'_>>(" and "), tag(" and "))
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
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("this ").parse(phrase) {
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
        tag::<_, _, OracleError<'_>>("loses all counters"),
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
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("gets "), tag("get ")))
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
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("flip "), tag("flips ")))
        .parse(lower)
        .ok()?;

    let (expr, after) = parse_count_expr(rest)?;

    // `parse_count_expr` returns the remainder with leading whitespace trimmed
    // ("five coins" → rest = "coins"), so we match "coins"/"coin" without the
    // leading space. Word-boundary termination rejects "coinsomething".
    // "flip a coin" is handled by FlipCoin; "flip one coin" would semantically
    // match but is never printed.
    let (after_noun, _) = alt((tag::<_, _, OracleError<'_>>("coins"), tag("coin")))
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

/// CR 706.1: Parse "roll N six-sided dice" / "roll X d12" / "roll two d6" —
/// the multi-dice form. Mirrors `try_parse_flip_n_coins`: strip the
/// "roll "/"rolls " prefix, take the count via `parse_count_expr` (digit,
/// word-number, and `X` forms), then parse the die size from the remainder.
/// Returns None for count == 1 so "roll a d6" / "roll one d6" falls through
/// to the existing single-die path.
fn try_parse_roll_n_dice(lower: &str) -> Option<ImperativeFamilyAst> {
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("roll "), tag("rolls ")))
        .parse(lower)
        .ok()?;

    // `parse_count_expr` returns the remainder with leading whitespace trimmed
    // ("two six-sided dice" → after = "six-sided dice").
    let (expr, after) = parse_count_expr(rest)?;

    // CR 706.1: Reject count == 1 so "roll a d6" / "roll one d6" prefers the
    // single-die `RollDie { count: Fixed(1), .. }` path.
    if matches!(expr, QuantityExpr::Fixed { value: 1 }) {
        return None;
    }

    let (sides, rest_after_sides) = parse_die_sides_with_rest(after)?;
    // CR 706.1: The remainder must be only the plural/singular die noun
    // (possibly with trailing punctuation) — a wider clause means this isn't a
    // bare multi-dice roll, so fall through to higher-level chain parsing.
    let rest_after_sides = rest_after_sides
        .trim_start()
        .trim_end_matches(['.', ',', ';'])
        .trim();
    if !rest_after_sides.is_empty() {
        return None;
    }

    Some(ImperativeFamilyAst::RollDie {
        count: expr,
        sides,
        // CR 706.2: multi-dice forms with a result-shifting modifier are
        // vanishingly rare and parsed via the table path; the bare form has
        // no modifier.
        modifier: None,
    })
}

/// CR 706.1a: Returns `(sides, remainder)`. The remainder is the slice immediately after
/// the consumed die phrase, with whitespace untrimmed. Callers needing to
/// attach trailing modifiers / clauses can branch on the remainder shape.
fn try_parse_roll_die_sides_with_rest(lower: &str) -> Option<(u8, &str)> {
    // Strip the single-die article/count prefix.
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("roll a "),
        tag("rolls a "),
        tag("roll one "),
        tag("rolls one "),
    ))
    .parse(lower)
    .ok()?;
    parse_die_sides_with_rest(rest)
}

/// CR 706.1a: Parse a die size (`dN` numeric form or `N-sided`/word form) from
/// the text immediately following the count/article, returning `(sides,
/// remainder)`. Shared by the single-die (`roll a dN`) and multi-dice (`roll N
/// dN`) parsers so both pick up every die-size spelling uniformly. The
/// trailing " die"/" dice" noun is consumed when present.
fn parse_die_sides_with_rest(rest: &str) -> Option<(u8, &str)> {
    // CR 706.1a: Numeric form — consume "d" followed by the longest run of
    // ASCII digits. This permits trailing text like " and add the number of
    // cards in your hand" or terminating punctuation.
    if let Ok((after_d, _)) = tag::<_, _, OracleError<'_>>("d").parse(rest) {
        let digit_end = after_d
            .bytes()
            .position(|b| !b.is_ascii_digit())
            .unwrap_or(after_d.len());
        if digit_end > 0 {
            if let Ok(sides) = after_d[..digit_end].parse::<u8>() {
                return Some((sides, &after_d[digit_end..]));
            }
        }
    }
    // CR 706.1a: Word-form — "six-sided die", "four-sided die", etc.
    let (after_word, sides) = alt((
        value(
            4_u8,
            alt((tag::<_, _, OracleError<'_>>("four-sided"), tag("4-sided"))),
        ),
        value(6, alt((tag("six-sided"), tag("6-sided")))),
        value(8, alt((tag("eight-sided"), tag("8-sided")))),
        value(10, alt((tag("ten-sided"), tag("10-sided")))),
        value(12, alt((tag("twelve-sided"), tag("12-sided")))),
        value(20, alt((tag("twenty-sided"), tag("20-sided")))),
    ))
    .parse(rest)
    .ok()?;
    // Consume the optional " dice"/" die" noun so it doesn't leak into the
    // modifier-detection path. " dice" (plural, multi-dice form) is tried
    // before " die" so it isn't mis-consumed as " die" + "ce". Tolerant of
    // absence ("roll a six-sided").
    let after_word = alt((
        value((), tag::<_, _, OracleError<'_>>(" dice")),
        value((), tag(" die")),
        value((), tag("")),
    ))
    .parse(after_word)
    .ok()
    .map(|(rest, _)| rest)
    .unwrap_or(after_word);
    Some((sides, after_word))
}

/// CR 701.51b: "open N attraction(s)" after the open/opens prefix.
fn parse_open_attractions_count_imperative(input: &str) -> OracleResult<'_, ImperativeFamilyAst> {
    let (rest, count) = nom_primitives::parse_number(input)?;
    let (rest, _) = space1(rest)?;
    let (rest, _) = alt((tag("attractions"), tag("attraction"))).parse(rest)?;
    Ok((rest, ImperativeFamilyAst::OpenAttractions { count }))
}

/// CR 701.51b: "open an Attraction" / "open two Attractions".
fn parse_open_attraction_imperative(lower: &str) -> Option<ImperativeFamilyAst> {
    nom_parse_lower(lower, |input| {
        map(
            (
                alt((tag("open "), tag("opens "))),
                alt((
                    value(
                        ImperativeFamilyAst::OpenAttractions { count: 1 },
                        tag("an attraction"),
                    ),
                    value(
                        ImperativeFamilyAst::OpenAttractions { count: 1 },
                        tag("a attraction"),
                    ),
                    parse_open_attractions_count_imperative,
                )),
                opt(nom::bytes::complete::take_while(|c: char| {
                    c == '.' || c == ','
                })),
                eof,
            ),
            |(_, ast, _, _)| ast,
        )
        .parse(input)
    })
}

/// CR 706 + CR 706.2: Try to parse a full `"roll a d{N}"` clause, including
/// an optional trailing `" and (add|subtract) {quantity}"` modifier that the
/// resolver applies to the natural roll before result-table lookup.
///
/// Returns `(sides, modifier)` on success. The modifier is `None` when the
/// remainder is empty or only contains trailing punctuation; otherwise the
/// remainder must shape as a recognized add/subtract clause.
fn try_parse_roll_die_with_modifier(
    lower: &str,
) -> Option<(u8, Option<crate::types::ability::DieRollModifier>)> {
    let (sides, rest) = try_parse_roll_die_sides_with_rest(lower)?;
    let rest = rest.trim_end_matches(['.', ',', ';']).trim();
    if rest.is_empty() {
        return Some((sides, None));
    }
    // Modifier shapes: "and add X", "and subtract X". Anything else means the
    // clause is wider than just a roll — let the dispatch fall through to
    // higher-level chain parsing (e.g., "roll a d20 for each player").
    let (after_and, _) = tag::<_, _, OracleError<'_>>("and ").parse(rest).ok()?;
    let (modifier_text, sign) = alt((
        value(true, tag::<_, _, OracleError<'_>>("add ")),
        value(false, tag("subtract ")),
    ))
    .parse(after_and)
    .ok()?;
    let value = crate::parser::oracle_quantity::parse_quantity_ref(modifier_text)?;
    let modifier = if sign {
        crate::types::ability::DieRollModifier::Add {
            value: crate::types::ability::QuantityExpr::Ref { qty: value },
        }
    } else {
        crate::types::ability::DieRollModifier::Subtract {
            value: crate::types::ability::QuantityExpr::Ref { qty: value },
        }
    };
    Some((sides, Some(modifier)))
}

/// CR 706.2: Try to parse a d20 result table line like "1—9 | Draw two cards",
/// "20 | Search your library for a card", or "15+ | Scry X, then draw X cards".
/// Returns `(min, max, effect_text)`. Open-ended upper bounds ("15+") set
/// `max = u8::MAX` so any modifier-boosted roll above the printed lower bound
/// resolves to this branch — see CR 706.2 on modifier-shifted results
/// (Diviner's Portent, Gale's Redirection, etc.).
pub(crate) fn try_parse_die_result_line(text: &str) -> Option<(u8, u8, &str)> {
    let trimmed = text.trim();

    // Find the pipe separator: "N—M | effect", "N+ | effect", or "N | effect"
    let (_, (range_part, effect_text)) = nom_primitives::split_once_on(trimmed, " | ").ok()?;
    let range_part = range_part.trim();
    let effect_text = effect_text.trim();

    // Parse range: "1—9" (em dash U+2014), "10—19", "15+" (open-ended upper),
    // or "20" (single value).
    let (min, max) = if let Some(dash_idx) = range_part.find('\u{2014}') {
        let min_str = &range_part[..dash_idx];
        let max_str = &range_part[dash_idx + '\u{2014}'.len_utf8()..];
        (min_str.parse::<u8>().ok()?, max_str.parse::<u8>().ok()?)
    // allow-noncombinator: CR 706.2 "N+" open-ended upper bound — single-char structural suffix on a pre-tokenized numeric range slice; the surrounding nom split already isolated `range_part` off the pipe delimiter (Pattern 3 in PATTERNS.md).
    } else if let Some(min_str) = range_part.strip_suffix('+') {
        (min_str.trim().parse::<u8>().ok()?, u8::MAX)
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
        // CR 118.12: A Counter with an "unless [player] pays [cost]" modifier
        // — intercepted here so the modifier propagates to
        // `ParsedEffectClause.unless_pay`. The Effect itself becomes the
        // modifier-free `Effect::Counter`; runtime resolution flows through
        // the unified `ResolvedAbility.unless_pay` pipeline rather than a
        // counter-specific bespoke branch.
        ImperativeFamilyAst::ZoneCounter(ZoneCounterImperativeAst::Counter {
            target,
            source_rider,
            unless_pay: Some(unless_pay),
            all,
        }) => {
            let effect = if all {
                // CR 701.6 + CR 405.1: Mass counter drops both source_rider
                // and unless_pay (no corpus card combines them with mass
                // counter, and mass counter is non-targeting per CR 115.1).
                Effect::CounterAll { target }
            } else {
                Effect::Counter {
                    target,
                    source_rider,
                }
            };
            let mut clause = parsed_clause(effect);
            // For mass counter, unless_pay is meaningless — drop it.
            if !all {
                clause.unless_pay = Some(unless_pay);
            }
            clause
        }
        // CR 122.1 + CR 608.2c: Multi-typed counter list → PutCounter chain.
        // Intercepted here (rather than in lower_zone_counter_ast which returns
        // a bare Effect) because the chain requires a sub_ability linkage that
        // only ParsedEffectClause can express.
        ImperativeFamilyAst::Put(PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
            enters_under,
            enter_tapped,
            enter_transformed,
            enters_attacking,
            up_to,
            choice_count: Some(choice_count),
            enter_with_counters,
        }) => {
            let effect = lower_put_ast(PutImperativeAst::ZoneChange {
                origin,
                destination,
                target,
                enters_under,
                enter_tapped,
                enter_transformed,
                enters_attacking,
                up_to,
                choice_count: Some(choice_count.clone()),
                enter_with_counters,
            });
            let mut clause = parsed_clause(effect);
            clause.multi_target = Some(*choice_count);
            clause
        }
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
                // Reference-target searches are not cultivate-class splits.
                split: _,
                // Reference-target searches are library-only (default).
                source_zones: _,
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
                // Multi-filter searches handle destinations per-filter, not via split.
                split: _,
                // Multi-filter searches are library-only (default).
                source_zones: _,
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
        ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(
            SearchCreationImperativeAst::Seek {
                filter,
                count,
                from_top,
                destination,
                enter_tapped,
                extra_filters,
            },
        )) if !extra_filters.is_empty() => lower_multi_filter_seek(
            filter,
            count,
            from_top,
            destination,
            enter_tapped,
            extra_filters,
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
                counter_type: crate::types::counter::CounterType::Plus1Plus1,
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
        ImperativeFamilyAst::ForceAttack {
            duration,
            required_player,
        } => Effect::ForceAttack {
            target: TargetFilter::Any,
            required_player,
            duration,
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
        ImperativeFamilyAst::ExchangeLifeWithStat { player, stat } => {
            Effect::ExchangeLifeWithStat { player, stat }
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
        ImperativeFamilyAst::Manifest { target, count } => Effect::Manifest { target, count },
        ImperativeFamilyAst::ManifestDread => Effect::ManifestDread,
        ImperativeFamilyAst::BecomeMonarch => Effect::BecomeMonarch,
        ImperativeFamilyAst::VentureIntoDungeon => Effect::VentureIntoDungeon,
        ImperativeFamilyAst::VentureIntoUndercity => Effect::VentureInto {
            dungeon: crate::game::dungeon::DungeonId::Undercity,
        },
        ImperativeFamilyAst::TakeTheInitiative => Effect::TakeTheInitiative,
        ImperativeFamilyAst::OpenAttractions { count } => Effect::OpenAttractions { count },
        ImperativeFamilyAst::RollToVisitAttractions => Effect::RollToVisitAttractions,
        ImperativeFamilyAst::Proliferate => Effect::Proliferate,
        // CR 701.56a: Time travel.
        ImperativeFamilyAst::TimeTravel => Effect::TimeTravel,
        // CR 701.36a: Populate.
        ImperativeFamilyAst::Populate => Effect::Populate,
        // CR 701.30: Clash with an opponent.
        ImperativeFamilyAst::Clash => Effect::Clash,
        ImperativeFamilyAst::GainKeyword(effect) => effect,
        ImperativeFamilyAst::LoseKeyword(effect) => effect,
        ImperativeFamilyAst::LoseTheGame => Effect::LoseTheGame { target: None },
        ImperativeFamilyAst::WinTheGame => Effect::WinTheGame { target: None },
        ImperativeFamilyAst::RollDie {
            count,
            sides,
            modifier,
        } => Effect::RollDie {
            count,
            sides,
            results: vec![],
            modifier,
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

/// CR 122.1: Detect a *mass* counter placement ("on each" / "on all") within a
/// single counter-placement clause. The caller MUST pass only the primary
/// clause text — never a full compound string — otherwise a trailing "on each"
/// conjunct would wrongly promote a targeted primary clause.
pub(super) fn counter_placement_is_mass(clause_lower: &str) -> bool {
    nom_primitives::scan_contains(clause_lower, "counter on each")
        || nom_primitives::scan_contains(clause_lower, "counters on each")
        || nom_primitives::scan_contains(clause_lower, "counter on all")
        || nom_primitives::scan_contains(clause_lower, "counters on all")
        || nom_primitives::scan_contains(clause_lower, "on each ")
        || nom_primitives::scan_contains(clause_lower, "on all ")
}

/// CR 122.1 + CR 608.2c: In a distributive "put a number of +1/+1 counters on
/// EACH ... equal to THAT CREATURE's <stat>" (Canopy Gargantuan), "that
/// creature" is the per-iteration recipient — each object receives counters
/// equal to its OWN stat. The shared `parse_event_context_refs` lowers "that
/// creature's <stat>" to `ObjectScope::CostPaidObject` (correct in trigger
/// bodies, where it refers to the triggering object), so for a mass placement
/// we rebind it to `ObjectScope::Recipient`; `resolve_add_all` then re-evaluates
/// the count per object. A genuine cost referent ("the SACRIFICED creature's
/// power") carries a cost/zone-change participle and is left as
/// `CostPaidObject` — `resolve_add_all` resolves that once and applies it
/// uniformly per CR 608.2k.
fn rebind_distributive_recipient_count(count: QuantityExpr, lower: &str) -> QuantityExpr {
    let is_cost_referent = nom_primitives::scan_contains(lower, "sacrificed")
        || nom_primitives::scan_contains(lower, "exiled")
        || nom_primitives::scan_contains(lower, "discarded")
        || nom_primitives::scan_contains(lower, "milled");
    if is_cost_referent {
        return count;
    }
    rebind_costpaid_scope_to_recipient(count)
}

/// Walk a `QuantityExpr` and rebind `ObjectScope::CostPaidObject` on the object
/// P/T/mana-value leaves to `ObjectScope::Recipient`. Recurses through the
/// arithmetic wrappers ("that creature's toughness plus 1", "twice that
/// creature's power") so the whole class composes.
fn rebind_costpaid_scope_to_recipient(expr: QuantityExpr) -> QuantityExpr {
    use crate::types::ability::ObjectScope::{CostPaidObject, Recipient};
    match expr {
        QuantityExpr::Ref { qty } => QuantityExpr::Ref {
            qty: match qty {
                QuantityRef::Power {
                    scope: CostPaidObject,
                } => QuantityRef::Power { scope: Recipient },
                QuantityRef::Toughness {
                    scope: CostPaidObject,
                } => QuantityRef::Toughness { scope: Recipient },
                QuantityRef::ObjectManaValue {
                    scope: CostPaidObject,
                } => QuantityRef::ObjectManaValue { scope: Recipient },
                other => other,
            },
        },
        QuantityExpr::Offset { inner, offset } => QuantityExpr::Offset {
            inner: Box::new(rebind_costpaid_scope_to_recipient(*inner)),
            offset,
        },
        QuantityExpr::ClampMin { inner, minimum } => QuantityExpr::ClampMin {
            inner: Box::new(rebind_costpaid_scope_to_recipient(*inner)),
            minimum,
        },
        QuantityExpr::Multiply { factor, inner } => QuantityExpr::Multiply {
            factor,
            inner: Box::new(rebind_costpaid_scope_to_recipient(*inner)),
        },
        QuantityExpr::DivideRounded {
            inner,
            divisor,
            rounding,
        } => QuantityExpr::DivideRounded {
            inner: Box::new(rebind_costpaid_scope_to_recipient(*inner)),
            divisor,
            rounding,
        },
        QuantityExpr::Sum { exprs } => QuantityExpr::Sum {
            exprs: exprs
                .into_iter()
                .map(rebind_costpaid_scope_to_recipient)
                .collect(),
        },
        QuantityExpr::UpTo { max } => QuantityExpr::UpTo {
            max: Box::new(rebind_costpaid_scope_to_recipient(*max)),
        },
        QuantityExpr::Power { base, exponent } => QuantityExpr::Power {
            base,
            exponent: Box::new(rebind_costpaid_scope_to_recipient(*exponent)),
        },
        QuantityExpr::Difference { left, right } => QuantityExpr::Difference {
            left: Box::new(rebind_costpaid_scope_to_recipient(*left)),
            right: Box::new(rebind_costpaid_scope_to_recipient(*right)),
        },
        QuantityExpr::Fixed { value } => QuantityExpr::Fixed { value },
    }
}

pub(super) fn parse_zone_counter_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ZoneCounterImperativeAst> {
    if let Some(ast) = parse_destroy_ast(text, lower, ctx) {
        return Some(ast);
    }
    if let Some(ast) = parse_exile_ast(text, lower, ctx) {
        return Some(ast);
    }
    if let Some(ast) = parse_counter_ast(text, lower) {
        return Some(ast);
    }
    if tag::<_, _, OracleError<'_>>("put ").parse(lower).is_ok()
        && nom_primitives::scan_contains(lower, "counter")
    {
        // Try move-counters first ("put its counters on ...")
        if let Some((
            Effect::MoveCounters {
                source,
                counter_type,
                count,
                mode,
                selection,
                target,
            },
            _rem,
        )) = super::counter::try_parse_move_counters(lower, text, ctx)
        {
            return Some(ZoneCounterImperativeAst::MoveCounters {
                source,
                counter_type,
                count,
                mode,
                selection,
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
        let is_all = counter_placement_is_mass(lower);
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
                        count: rebind_distributive_recipient_count(count, lower),
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
    if tag::<_, _, OracleError<'_>>("remove ").parse(lower).is_ok()
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
    if tag::<_, _, OracleError<'_>>("move ").parse(lower).is_ok()
        && nom_primitives::scan_contains(lower, "counter")
    {
        if let Some(Effect::MoveCounters {
            source,
            counter_type,
            count,
            mode,
            selection,
            target,
        }) = try_parse_move_counters_from(lower, ctx)
        {
            return Some(ZoneCounterImperativeAst::MoveCounters {
                source,
                counter_type,
                count,
                mode,
                selection,
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
            enter_with_counters,
        } => {
            if all {
                // `ChangeZoneAll` has no counter slot; mass exile never carries
                // a "with counters" clause (all five non-from-hand construction
                // sites pass `vec![]`).
                Effect::ChangeZoneAll {
                    origin,
                    destination: Zone::Exile,
                    target,
                    enters_under: None,
                    enter_tapped: false,
                    face_down_profile: None,
                }
            } else {
                Effect::ChangeZone {
                    origin,
                    destination: Zone::Exile,
                    target,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters,
                    face_down_profile: None,
                }
            }
        }
        ZoneCounterImperativeAst::ExileTop {
            player,
            count,
            face_down,
        } => Effect::ExileTop {
            player,
            count,
            face_down,
        },
        ZoneCounterImperativeAst::Counter {
            target,
            source_rider,
            // CR 118.12: An unless-pay-bearing Counter is intercepted in
            // `lower_imperative_family_ast` so the modifier flows into
            // `ParsedEffectClause.unless_pay`. By the time we reach this
            // bare-Effect lowering site, `unless_pay` is always None — the
            // ParsedEffectClause-aware paths consumed it. This is the
            // sub_ability-less fallback for the (rare) case the Counter is
            // routed through `TargetedImperativeAst::ZoneCounterProxy`
            // directly without going through `lower_imperative_family_ast`.
            unless_pay: _,
            all,
        } => {
            if all {
                // CR 701.6 + CR 405.1: Mass counter. Drops `source_rider` —
                // no corpus card combines a source_rider with mass counter,
                // and the runtime resolver does not honor that slot on
                // `Effect::CounterAll`. (The rider is a per-target-permanent
                // silence/destroy follow-up.)
                Effect::CounterAll { target }
            } else {
                Effect::Counter {
                    target,
                    source_rider,
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
            count,
            mode,
            selection,
            target,
        } => Effect::MoveCounters {
            source,
            counter_type,
            count,
            mode,
            selection,
            target,
        },
    }
}

/// CR 701.53a: Parse "incubate {N}" from Oracle text.
///
/// Handles numeric and "X" counts via shared `parse_count_expr`.
fn try_parse_incubate(lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("incubate ")
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
    let (rest, _) = tag::<_, _, OracleError<'_>>("amass ").parse(lower).ok()?;
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
    let (rest, _) = tag::<_, _, OracleError<'_>>("monstrosity ")
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
    let (rest, _) = tag::<_, _, OracleError<'_>>("adapt ").parse(lower).ok()?;
    let rest = rest.trim().trim_end_matches('.');

    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Adapt { count })
}

/// CR 508.1d: Parse "attacks/attack [player] this turn/combat if able" requirements.
///
/// Bare forms ("attacks this turn if able") emit a temporary `MustAttack`.
/// Player-bound "attacks you ..." forms emit `ForceAttack`, whose resolver binds
/// "you" to the resolving ability controller and grants `MustAttackPlayer`.
fn try_parse_attack_if_able(lower: &str) -> Option<ImperativeFamilyAst> {
    let trimmed = lower.trim_end_matches('.');

    // First try: bare forms without a player reference.
    // verb axis × phase axis (PATTERNS.md §8b): factor "attack(s)" out front,
    // then map the phase clause to its duration.
    let result: Result<(&str, Duration), nom::Err<OracleError<'_>>> = (
        alt((tag("attacks"), tag("attack"))),
        preceded(
            tag(" "),
            alt((
                value(Duration::UntilEndOfTurn, tag("this turn if able")),
                value(
                    Duration::UntilEndOfCombat,
                    alt((tag("this combat if able"), tag("that combat if able"))),
                ),
            )),
        ),
    )
        .map(|(_, duration)| duration)
        .parse(trimmed);

    if let Ok((_, duration)) = result {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
            static_abilities: vec![must_attack_static_definition()],
            duration: Some(duration),
            target: None,
        }));
    }

    let targeted: Result<(&str, (TargetFilter, Duration)), nom::Err<OracleError<'_>>> = (
        alt((tag("attacks"), tag("attack"))),
        preceded(
            tag(" "),
            // CR 508.1d: the required player. "you" binds the resolving ability
            // controller; "that player" references the opponent chosen earlier
            // in the same resolution — Ruhan of the Fomori, Raving Dead, Knight
            // Rampager ("choose an opponent at random. This creature attacks
            // that player this combat if able."). CR 608.2c: the choose+attack
            // resolve together, so the resolution-scoped ChosenPlayer { index }
            // (read from chosen_players) is the correct reference, not the
            // durable SourceChosenPlayer. The opponent choice is the single
            // preceding choice in every card of this class, so index 0.
            alt((
                value(TargetFilter::Controller, tag("you")),
                value(
                    TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::ChosenPlayer { index: 0 }),
                    ),
                    tag("that player"),
                ),
            )),
        ),
        preceded(
            tag(" "),
            alt((
                value(Duration::UntilEndOfTurn, tag("this turn if able")),
                value(
                    Duration::UntilEndOfCombat,
                    alt((
                        tag("this combat if able"),
                        tag("that combat if able"),
                        tag("each combat if able"),
                    )),
                ),
            )),
        ),
    )
        .map(|(_, required_player, duration)| (required_player, duration))
        .parse(trimmed);

    if let Ok((rest, (required_player, duration))) = targeted {
        if rest.is_empty() {
            return Some(ImperativeFamilyAst::ForceAttack {
                duration,
                required_player,
            });
        }
    }

    None
}

/// CR 508.1d: Build the `StaticDefinition` for a transient "attacks if able"
/// requirement. `Effect::GenericEffect` resolution snapshots only
/// `static_def.modifications` (the `mode` field is inert for a transient grant —
/// `snapshot_transient_modifications` never reads it), so the `MustAttack` mode
/// must be carried by an explicit `AddStaticMode` modification to actually reach
/// the layer system and `combat.rs` enforcement. Mirrors the block path's
/// `ImperativeFamilyAst::MustBeBlocked` lowering.
fn must_attack_static_definition() -> StaticDefinition {
    use crate::types::statics::StaticMode;
    StaticDefinition::new(StaticMode::MustAttack).modifications(vec![
        ContinuousModification::AddStaticMode {
            mode: StaticMode::MustAttack,
        },
    ])
}

/// CR 508.1d / CR 509.1c: True iff `lower` (already lowercased, trimmed) is a
/// recognized *standalone* combat requirement — "attack(s) [player] this
/// turn/combat if able" or "must be blocked [this turn] [if able]". Used by
/// `split_clause_sequence` to gate the trailing-conjunct split of
/// "gains <keyword> until end of turn and <combat requirement>" so the
/// requirement reaches its existing standalone parser. Composes the existing
/// recognizers as Some/None classifiers; their produced AST is discarded.
pub(crate) fn is_standalone_combat_requirement(lower: &str) -> bool {
    let trimmed = lower.trim().trim_end_matches('.').trim();
    if try_parse_attack_if_able(trimmed).is_some() {
        return true;
    }
    // CR 509.1c: "must be blocked [this turn] [if able]" — mirrors the
    // imperative `"must"` verb arm.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("must be blocked").parse(trimmed) {
        let rest = rest.trim();
        return rest.is_empty()
            || rest == "this turn if able"
            || rest == "if able"
            || rest == "this turn";
    }
    false
}

/// Handles "can't be blocked [this turn]", "can't attack [this turn]", "can't block [this turn]",
/// and compound forms like "can't attack or block". These delegate to the subject.rs
/// static-granting machinery, wrapping the result in a `GenericEffect`.
fn try_parse_subjectless_cant(lower: &str) -> Option<ImperativeFamilyAst> {
    use crate::parser::oracle_effect::subject::{
        parse_restriction_modes, static_mode_needs_grant_propagation,
    };

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

    // CR 702.18a / 702.11a: "can't be the target [of ...]" granted to the target
    // for a duration is a Shroud / Hexproof keyword grant (Vines of Vastwood). Map
    // it to the keyword so the targeting check applies the correct controller scope
    // (Hexproof leaves the controller able to target), reusing the enforced keyword
    // path rather than a scope-less rule static.
    if let Some(scope) = crate::parser::oracle_keyword::classify_cant_be_targeted(clean) {
        let keyword = match scope {
            crate::parser::oracle_keyword::CantBeTargetedScope::AnyPlayer => {
                crate::types::keywords::Keyword::Shroud
            }
            crate::parser::oracle_keyword::CantBeTargetedScope::OpponentsOnly => {
                crate::types::keywords::Keyword::Hexproof
            }
        };
        return Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .modifications(vec![ContinuousModification::AddKeyword { keyword }])],
            duration: Some(duration),
            target: None,
        }));
    }

    let modes = parse_restriction_modes(clean)?;
    let statics: Vec<StaticDefinition> = modes
        .into_iter()
        .map(|mode| {
            // CR 508.1d + CR 509.1a + CR 509.1b (issue #327): Duration-scoped
            // combat restriction modes must carry an `AddStaticMode`
            // modification so the transient continuous effect propagates
            // them onto the recipient's `static_definitions` at layer-apply
            // time. Without this, the runtime block / attack check never
            // sees the rule. Mirrors the injection in
            // `subject::build_restriction_clause`.
            let needs_propagation = static_mode_needs_grant_propagation(&mode);
            let mut def = StaticDefinition::new(mode.clone());
            if needs_propagation {
                def = def.modifications(vec![ContinuousModification::AddStaticMode { mode }]);
            }
            def
        })
        .collect();
    Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
        static_abilities: statics,
        duration: Some(duration),
        target: None,
    }))
}

/// CR 701.39a: Parse "bolster N" from Oracle text.
fn try_parse_bolster(lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("bolster ").parse(lower).ok()?;
    let rest = rest.trim().trim_end_matches('.');

    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Bolster { count })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed_leg(filter: &TargetFilter) -> Option<&TypedFilter> {
        match filter {
            TargetFilter::Typed(tf) => Some(tf),
            TargetFilter::And { filters } => filters.iter().find_map(typed_leg),
            _ => None,
        }
    }

    fn is_stack_spell_leg(filter: &TargetFilter) -> bool {
        match filter {
            TargetFilter::StackSpell => true,
            TargetFilter::And { filters } => filters.iter().any(is_stack_spell_leg),
            _ => false,
        }
    }

    fn has_type(tf: &TypedFilter, ty: TypeFilter) -> bool {
        tf.type_filters.iter().any(|candidate| candidate == &ty)
    }

    fn has_prop(tf: &TypedFilter, prop: FilterProp) -> bool {
        tf.properties.iter().any(|candidate| candidate == &prop)
    }

    /// CR 122.1: `counter_placement_is_mass` recognizes "on each"/"on all"
    /// mass placements and rejects targeted ("on target") or anaphoric
    /// ("on it") single-object placements.
    #[test]
    fn counter_placement_is_mass_predicate() {
        assert!(counter_placement_is_mass(
            "a +1/+1 counter on each creature"
        ));
        assert!(counter_placement_is_mass(
            "a loyalty counter on all planeswalkers"
        ));
        assert!(!counter_placement_is_mass(
            "a +1/+1 counter on target creature"
        ));
        assert!(!counter_placement_is_mass("a stun counter on it"));
    }

    /// Issue #408 — "Counter target activated ability, triggered ability, or
    /// noncreature spell" (Louisoix's Sacrifice) must parse to the full
    /// three-way disjunction, not a degenerate empty-`type_filters` stack
    /// filter. The noncreature restriction must survive on the spell leg.
    #[test]
    fn parse_counter_louisoix_three_way_disjunction() {
        let text = "Counter target activated ability, triggered ability, or noncreature spell.";
        let ast = parse_counter_ast(text, &text.to_lowercase())
            .expect("Louisoix's Sacrifice counter clause should parse");
        let ZoneCounterImperativeAst::Counter { target, .. } = ast else {
            panic!("expected a Counter AST");
        };
        let TargetFilter::Or { filters } = &target else {
            panic!("expected Or {{ ability, noncreature-spell }}, got {target:?}");
        };
        assert!(
            filters
                .iter()
                .any(|f| matches!(f, TargetFilter::StackAbility { controller: None })),
            "missing the activated/triggered ability disjunct: {target:?}"
        );
        let spell_leg = filters
            .iter()
            .find_map(typed_leg)
            .expect("missing the typed noncreature-spell disjunct");
        assert!(
            has_type(spell_leg, TypeFilter::Non(Box::new(TypeFilter::Creature))),
            "the spell leg must EXCLUDE creature spells (noncreature restriction)"
        );
        assert!(
            has_prop(spell_leg, FilterProp::InZone { zone: Zone::Stack }),
            "the spell leg must be pinned to the stack zone"
        );
    }

    /// Issue #899 regression — "counter target spell with mana value X or
    /// less, where X is the number of Faeries you control" (Spellstutter
    /// Sprite) must resolve the `Cmc` bound to the defining `where X is …`
    /// expression rather than leaving it as the bare `Variable("X")` that
    /// collapses to 0 at resolution. Building-block coverage: this exercises
    /// `strip_trailing_where_x` + `apply_where_x_to_filter` composed under
    /// `parse_counter_ast`, so every counter-target-spell-with-where-X card
    /// (Faerie Trickery, Filigree Sages variants, etc.) is covered by the
    /// same path.
    /// CR 107.3i (shared X on an object) + CR 202.3 (mana value). Target
    /// legality re-check on resolution (CR 608.2b) is exercised by the
    /// integration tests in
    /// `tests/integration/spellstutter_sprite_counter_with_x.rs`.
    #[test]
    fn parse_counter_target_spell_with_where_x_cmc_bound() {
        let text = "counter target spell with mana value X or less, where X is the number of Faeries you control.";
        let ast = parse_counter_ast(text, &text.to_lowercase())
            .expect("Spellstutter Sprite counter clause should parse");
        let ZoneCounterImperativeAst::Counter { target, .. } = ast else {
            panic!("expected a Counter AST");
        };
        let typed = typed_leg(&target).unwrap_or_else(|| {
            panic!("expected a typed leg under the stack constraint, got {target:?}")
        });
        let cmc = typed
            .properties
            .iter()
            .find_map(|p| match p {
                FilterProp::Cmc { comparator, value } => Some((comparator, value)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a Cmc bound in the typed leg, got {typed:?}"));
        assert_eq!(
            *cmc.0,
            crate::types::ability::Comparator::LE,
            "Spellstutter's 'or less' clause must parse as a <= mana-value bound"
        );
        let cmc = cmc.1;
        let QuantityExpr::Ref { qty } = cmc else {
            panic!("expected the Cmc bound to be a dynamic Ref, got {cmc:?}");
        };
        let QuantityRef::ObjectCount { filter } = qty else {
            panic!("expected the Cmc bound to resolve to ObjectCount(Faeries you control), got {qty:?} — the where-X binding was dropped");
        };
        let object_typed = match filter {
            TargetFilter::Typed(tf) => tf,
            other => panic!("expected ObjectCount over a Typed filter, got {other:?}"),
        };
        assert!(
            object_typed
                .type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Faerie"))),
            "ObjectCount filter must include the Faerie subtype: {object_typed:?}"
        );
        assert_eq!(
            object_typed.controller,
            Some(ControllerRef::You),
            "ObjectCount filter must be controller-scoped to You: {object_typed:?}"
        );
    }

    /// Issue #408 regression guard — a plain "Counter target spell"
    /// (Counterspell) must still yield a stack-spell filter; the new
    /// stack-object combinator must not steal the simple case.
    #[test]
    fn parse_counter_plain_spell_unchanged() {
        let text = "Counter target spell.";
        let ast = parse_counter_ast(text, &text.to_lowercase())
            .expect("\"Counter target spell\" should parse");
        let ZoneCounterImperativeAst::Counter { target, .. } = ast else {
            panic!("expected a Counter AST");
        };
        // "Counter target spell" → a stack-pinned spell filter (StackSpell or
        // a stack-constrained Typed Card filter). It must NOT be an Or with an
        // ability disjunct.
        assert!(
            !matches!(&target, TargetFilter::Or { .. }),
            "plain \"Counter target spell\" must not gain an ability disjunct: {target:?}"
        );
        let is_spell_filter = is_stack_spell_leg(&target)
            || typed_leg(&target)
                .is_some_and(|tf| has_prop(tf, FilterProp::InZone { zone: Zone::Stack }));
        assert!(
            is_spell_filter,
            "plain \"Counter target spell\" must yield a stack-spell filter, got {target:?}"
        );
    }

    #[test]
    fn parse_outside_game_wish_reveal_to_hand() {
        let ability = super::super::parse_effect_chain(
            "You may reveal a sorcery card you own from outside the game and put it into your hand. Exile ~.",
            AbilityKind::Spell,
        );
        assert!(
            !ability.optional,
            "the reveal choice is optional; the self-exile sentence is mandatory"
        );
        assert!(matches!(&*ability.effect, Effect::SearchOutsideGame { .. }));
        assert!(matches!(
            ability.sub_ability.as_deref().map(|sub| &*sub.effect),
            Some(Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            })
        ));

        let effect = super::super::parse_effect(
            "reveal a sorcery card you own from outside the game and put it into your hand",
        );
        match effect {
            Effect::SearchOutsideGame {
                filter,
                count,
                reveal,
                destination,
                source_pool,
            } => {
                assert_eq!(count, QuantityExpr::up_to(QuantityExpr::Fixed { value: 1 }));
                assert!(reveal);
                assert_eq!(destination, Zone::Hand);
                assert!(
                    !source_pool.includes_face_up_exile(),
                    "legacy single-branch wording must default to sideboard-only"
                );
                match filter {
                    TargetFilter::Typed(typed) => {
                        assert!(typed.type_filters.contains(&TypeFilter::Sorcery));
                    }
                    other => panic!("expected sorcery filter, got {other:?}"),
                }
            }
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }

        let effect = super::super::parse_effect(
            "reveal a creature card you own from outside the game and put it into your hand",
        );
        match effect {
            Effect::SearchOutsideGame { filter, .. } => match filter {
                TargetFilter::Typed(typed) => {
                    assert!(typed.type_filters.contains(&TypeFilter::Creature));
                }
                other => panic!("expected creature filter, got {other:?}"),
            },
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }
    }

    /// CR 406.3 + CR 400.11: Karn, the Great Creator's -2 reveals OR pulls
    /// from face-up exile. Verifies the Karn-class disjunction sets
    /// `source_pool: OutsideGameSourcePool::SideboardAndFaceUpExile`, identifies the artifact filter, and
    /// captures Hand as the destination. The trailing "Put that card into
    /// your hand." sentence (CR 608.2c anaphoric "that card") is absorbed
    /// into the parent effect's destination by the chain splitter, so no
    /// separate ChangeZone sub-ability is built.
    #[test]
    fn parse_outside_game_karn_minus_two() {
        let ability = super::super::parse_effect_chain(
            "You may reveal an artifact card you own from outside the game or choose a face-up artifact card you own in exile. Put that card into your hand.",
            AbilityKind::Activated,
        );
        match &*ability.effect {
            Effect::SearchOutsideGame {
                filter,
                destination,
                source_pool,
                reveal,
                ..
            } => {
                assert!(
                    source_pool.includes_face_up_exile(),
                    "Karn-class disjunction must set source_pool"
                );
                assert!(reveal);
                assert_eq!(*destination, Zone::Hand);
                match filter {
                    TargetFilter::Typed(typed) => {
                        assert!(
                            typed.type_filters.contains(&TypeFilter::Artifact),
                            "artifact filter must be recognized from the outside-game branch"
                        );
                    }
                    other => panic!("expected artifact filter, got {other:?}"),
                }
            }
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }
    }

    /// CR 406.3 + CR 400.11: Coax from the Blind Eternities — same Karn-class
    /// disjunction with an Eldrazi subtype filter. Confirms the parser is
    /// filter-agnostic and the disjunction trigger is structural, not
    /// keyword-specific.
    #[test]
    fn parse_outside_game_coax_blind_eternities() {
        let ability = super::super::parse_effect_chain(
            "You may reveal an Eldrazi card you own from outside the game or choose a face-up Eldrazi card you own in exile. Put that card into your hand.",
            AbilityKind::Spell,
        );
        match &*ability.effect {
            Effect::SearchOutsideGame {
                filter,
                source_pool,
                destination,
                ..
            } => {
                assert!(
                    source_pool.includes_face_up_exile(),
                    "Coax disjunction must set source_pool"
                );
                assert_eq!(*destination, Zone::Hand);
                // CR 205.3m: Eldrazi is a creature subtype; the search filter
                // carries it as TypeFilter::Subtype within `type_filters`.
                match filter {
                    TargetFilter::Typed(typed) => {
                        let has_eldrazi = typed.type_filters.iter().any(|tf| {
                            matches!(tf, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("eldrazi"))
                        });
                        assert!(
                            has_eldrazi,
                            "expected Eldrazi subtype filter, got {:?}",
                            typed.type_filters
                        );
                    }
                    other => panic!("expected typed filter, got {other:?}"),
                }
            }
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }
    }

    /// Regression: legacy single-branch "reveal a … from outside the game"
    /// wording (Wish, Cunning Wish, Burning Wish) must still parse with
    /// `source_pool: OutsideGameSourcePool::Sideboard` so non-Karn wishboard cards keep their
    /// sideboard-only resolution path.
    #[test]
    fn parse_outside_game_legacy_single_branch_still_works() {
        let effect = super::super::parse_effect(
            "reveal a sorcery card you own from outside the game and put it into your hand",
        );
        match effect {
            Effect::SearchOutsideGame {
                source_pool,
                destination,
                ..
            } => {
                assert!(
                    !source_pool.includes_face_up_exile(),
                    "legacy wishboard text must NOT enable face-up exile"
                );
                assert_eq!(destination, Zone::Hand);
            }
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }
    }

    /// Regression (issue #1976): Wish (M19) — "You may play a card you own from
    /// outside the game this turn." must parse to `SearchOutsideGame` (the
    /// sideboard / wishboard pool, CR 400.11 + CR 400.11a + CR 701.23j), NOT a
    /// `CastFromZone` that targets an in-game permanent the controller already
    /// owns. The bare "a card" filter (no type adjective) must widen to
    /// `TargetFilter::Any`, the destination defaults to Hand (CR 400.11b), the
    /// play verb suppresses the reveal, and the choice is optional ("up to").
    #[test]
    fn parse_outside_game_wish_play_from_sideboard() {
        let ability = super::super::parse_effect_chain(
            "You may play a card you own from outside the game this turn.",
            AbilityKind::Spell,
        );
        match &*ability.effect {
            Effect::SearchOutsideGame {
                filter,
                count,
                reveal,
                destination,
                source_pool,
            } => {
                assert_eq!(
                    *filter,
                    TargetFilter::Any,
                    "bare \"a card\" must widen to any card"
                );
                assert!(count.is_up_to(), "Wish's play choice is optional (up to 1)");
                assert!(!*reveal, "the play form does not reveal the chosen card");
                assert_eq!(*destination, Zone::Hand);
                assert!(
                    !source_pool.includes_face_up_exile(),
                    "Wish is sideboard-only, not a Karn-class disjunction"
                );
            }
            other => panic!(
                "Wish must parse to SearchOutsideGame, not a permanent-targeting cast; got {other:?}"
            ),
        }

        // End-to-end through the canonical card pipeline: the full Wish card
        // must yield a single SearchOutsideGame ability, never a CastFromZone.
        let parsed = crate::parser::oracle::parse_oracle_text(
            "You may play a card you own from outside the game this turn.",
            "Wish",
            &[],
            &["Sorcery".to_string()],
            &[],
        );
        assert!(
            parsed
                .abilities
                .iter()
                .any(|ability| matches!(&*ability.effect, Effect::SearchOutsideGame { .. })),
            "Wish card pipeline must produce SearchOutsideGame, got {:?}",
            parsed
                .abilities
                .iter()
                .map(|ability| &ability.effect)
                .collect::<Vec<_>>()
        );
        assert!(
            !parsed
                .abilities
                .iter()
                .any(|ability| matches!(&*ability.effect, Effect::CastFromZone { .. })),
            "Wish must not parse to a permanent-targeting CastFromZone"
        );
    }

    /// Regression (issue #1976): the "cast a … card you own from outside the
    /// game" sibling surface form (Cunning Wish class for spells) routes through
    /// the same outside-game pool, carrying its type filter.
    #[test]
    fn parse_outside_game_cast_typed_from_sideboard() {
        let effect = super::super::parse_effect(
            "You may cast an instant card you own from outside the game this turn.",
        );
        match effect {
            Effect::SearchOutsideGame {
                filter,
                source_pool,
                reveal,
                ..
            } => {
                assert!(!reveal, "the cast form does not reveal");
                assert!(!source_pool.includes_face_up_exile());
                let typed = typed_leg(&filter).expect("instant filter must be typed");
                assert!(
                    typed.type_filters.contains(&TypeFilter::Instant),
                    "expected instant filter, got {:?}",
                    typed.type_filters
                );
            }
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }
    }

    /// CR 701.27 + CR 608.2c + CR 608.2k: "transform it" / "convert itself" —
    /// bare object pronoun resolves to `ParentTarget` when no trigger subject
    /// is set on the parse context. At resolution time, `ParentTarget` with
    /// empty `ability.targets` falls back to the source per CR 608.2c
    /// (`targeting::resolved_targets`), so DFC self-transform sub-abilities
    /// (Primal Amulet class) still target the source. The trigger-subject
    /// anaphor case (typed subject → `TriggeringSource`) is exercised at the
    /// `parse_target_with_ctx` layer in `oracle_target` tests.
    #[test]
    fn parse_transform_self_pronouns() {
        for input in [
            "transform it",
            "transform itself",
            "convert it",
            "convert itself",
        ] {
            let result = parse_utility_imperative_ast(input, input, &mut ParseContext::default());
            let Some(UtilityImperativeAst::Transform { target }) = result else {
                panic!("{input}: expected Transform, got {result:?}");
            };
            assert!(
                matches!(target, TargetFilter::ParentTarget),
                "{input}: expected ParentTarget, got {target:?}"
            );
        }
    }

    #[test]
    fn parse_attach_triggering_object_to_last_created_token() {
        let input = "attach it to the token";
        let result = parse_utility_imperative_ast(input, input, &mut ParseContext::default());
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
        let result = parse_utility_imperative_ast(input, &lower, &mut ParseContext::default());
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
        let result = parse_utility_imperative_ast(input, &lower, &mut ParseContext::default());
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
    fn parse_unattach_all_equipment_from_target_creature() {
        let input = "unattach all Equipment from target creature";
        let lower = input.to_lowercase();
        let result = parse_utility_imperative_ast(input, &lower, &mut ParseContext::default());
        let Some(UtilityImperativeAst::UnattachAll { attachment, target }) = result else {
            panic!("{input}: expected UnattachAll, got {result:?}");
        };
        assert_eq!(
            attachment,
            TargetFilter::Typed(TypedFilter::default().subtype("Equipment".to_string()))
        );
        assert_eq!(target, TargetFilter::Typed(TypedFilter::creature()));
    }

    #[test]
    fn parse_unattach_all_decomposes_attachment_type_and_pronoun_target() {
        let input = "unattach all Equipment from it";
        let lower = input.to_lowercase();
        let result = parse_utility_imperative_ast(input, &lower, &mut ParseContext::default());
        let Some(UtilityImperativeAst::UnattachAll { attachment, target }) = result else {
            panic!("{input}: expected UnattachAll, got {result:?}");
        };
        assert_eq!(
            attachment,
            TargetFilter::Typed(TypedFilter::default().subtype("Equipment".to_string()))
        );
        assert!(matches!(target, TargetFilter::ParentTarget));
    }

    /// CR 608.2k regression — issue #319 sibling.
    /// "attach ~ to it" inside a typed-subject trigger ("Whenever a Samurai
    /// or Warrior you control attacks alone … attach this Equipment to it"
    /// — Ancestral Katana) must bind "it" to the triggering creature, not
    /// the parent ability target. Verifies the ctx-threaded attach path.
    #[test]
    fn parse_attach_self_to_it_in_typed_trigger_binds_triggering_source() {
        for input in ["attach ~ to it", "attach this equipment to it"] {
            let mut ctx = ParseContext {
                subject: Some(TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(
                            TypedFilter::creature()
                                .controller(ControllerRef::You)
                                .subtype("Samurai".to_string()),
                        ),
                        TargetFilter::Typed(
                            TypedFilter::creature()
                                .controller(ControllerRef::You)
                                .subtype("Warrior".to_string()),
                        ),
                    ],
                }),
                ..Default::default()
            };
            let result = parse_utility_imperative_ast(input, input, &mut ctx);
            let Some(UtilityImperativeAst::Attach {
                attachment: _,
                target,
            }) = result
            else {
                panic!("{input}: expected Attach, got {result:?}");
            };
            assert_eq!(target, TargetFilter::TriggeringSource, "{input}");
        }
    }

    #[test]
    fn parse_exile_from_your_hand_preserves_type_phrase_filter() {
        let input = "exile a nonartifact, nonland card from your hand";
        let lower = input.to_lowercase();
        let result = parse_zone_counter_ast(input, &lower, &mut ParseContext::default());
        let Some(ZoneCounterImperativeAst::Exile {
            origin: Some(Zone::Hand),
            target: TargetFilter::Typed(filter),
            all: false,
            enter_with_counters,
        }) = result
        else {
            panic!("{input}: expected hand-origin typed exile, got {result:?}");
        };

        assert!(enter_with_counters.is_empty());
        assert_eq!(filter.controller, Some(ControllerRef::You));
        assert!(filter.type_filters.contains(&TypeFilter::Card));
        assert!(filter
            .type_filters
            .contains(&TypeFilter::Non(Box::new(TypeFilter::Artifact))));
        assert!(filter
            .type_filters
            .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))));
        assert!(filter
            .properties
            .contains(&FilterProp::InZone { zone: Zone::Hand }));
    }

    #[test]
    fn parse_exile_all_creatures_and_spacecraft() {
        let input = "exile all creatures and Spacecraft";
        let lower = input.to_lowercase();
        let result = parse_zone_counter_ast(input, &lower, &mut ParseContext::default());
        let Some(ZoneCounterImperativeAst::Exile {
            origin: None,
            target: TargetFilter::Or { filters },
            all: true,
            enter_with_counters,
        }) = result
        else {
            panic!("{input}: expected mass exile type union, got {result:?}");
        };

        assert!(enter_with_counters.is_empty());
        assert_eq!(filters.len(), 2);
        assert_eq!(filters[0], TargetFilter::Typed(TypedFilter::creature()));
        assert_eq!(
            filters[1],
            TargetFilter::Typed(TypedFilter::default().subtype("Spacecraft".to_string()))
        );
    }

    #[test]
    fn parse_exile_from_your_hand_handles_article_variants() {
        for (input, expected_type) in [
            ("exile an instant card from your hand", TypeFilter::Instant),
            ("exile a creature card from your hand", TypeFilter::Creature),
        ] {
            let lower = input.to_lowercase();
            let result = parse_zone_counter_ast(input, &lower, &mut ParseContext::default());
            let Some(ZoneCounterImperativeAst::Exile {
                origin: Some(Zone::Hand),
                target: TargetFilter::Typed(filter),
                all: false,
                enter_with_counters: _,
            }) = result
            else {
                panic!("{input}: expected hand-origin typed exile, got {result:?}");
            };

            assert_eq!(filter.controller, Some(ControllerRef::You));
            assert!(filter.type_filters.contains(&expected_type));
            assert!(filter
                .properties
                .contains(&FilterProp::InZone { zone: Zone::Hand }));
        }
    }

    #[test]
    fn parse_exile_from_hand_with_dynamic_counter_suffix() {
        use crate::types::ability::{ObjectScope, QuantityExpr, QuantityRef, TypeFilter};
        use crate::types::counter::CounterType;
        // The Eleventh Doctor: "exile a card from your hand with a number of
        // time counters on it equal to its mana value."
        let input =
            "exile a card from your hand with a number of time counters on it equal to its mana value";
        let lower = input.to_lowercase();
        let result = parse_zone_counter_ast(input, &lower, &mut ParseContext::default());
        let Some(ZoneCounterImperativeAst::Exile {
            origin: Some(Zone::Hand),
            target: TargetFilter::Typed(filter),
            all: false,
            enter_with_counters,
        }) = result
        else {
            panic!("{input}: expected hand-origin typed exile, got {result:?}");
        };
        assert_eq!(filter.controller, Some(ControllerRef::You));
        assert!(filter.type_filters.contains(&TypeFilter::Card));
        assert!(filter
            .properties
            .contains(&FilterProp::InZone { zone: Zone::Hand }));
        assert_eq!(
            enter_with_counters,
            vec![(
                CounterType::Time,
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Recipient,
                    },
                },
            )],
        );
    }

    #[test]
    fn parse_exile_from_hand_unrecognized_tail_attributes_no_counters() {
        // Negative: an unrecognized `with …` tail that is NOT a counter suffix
        // makes the from-hand arm's `tail_is_consumed` guard fail, so the arm
        // declines and the parse falls through to the generic exile path.
        // The key invariant: an unrecognized tail must NEVER be misinterpreted
        // as a counter suffix — `enter_with_counters` stays empty regardless of
        // which arm produces the AST.
        let input = "exile a card from your hand with flying";
        let lower = input.to_lowercase();
        let result = parse_zone_counter_ast(input, &lower, &mut ParseContext::default());
        let Some(ZoneCounterImperativeAst::Exile {
            enter_with_counters,
            ..
        }) = result
        else {
            panic!("{input}: expected an Exile AST, got {result:?}");
        };
        assert!(
            enter_with_counters.is_empty(),
            "unrecognized `with` tail must not yield counters: {enter_with_counters:?}"
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

    #[test]
    fn parse_sacrifice_one_or_more_uses_filtered_up_to_count() {
        let text = "sacrifice one or more Treasures";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::You),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice {
                target,
                count,
                min_count,
            } => {
                assert_eq!(min_count, 1);
                match &target {
                    TargetFilter::Typed(tf) => {
                        assert_eq!(tf.controller, Some(ControllerRef::You));
                        assert!(tf.type_filters.iter().any(|type_filter| matches!(
                            type_filter,
                            crate::types::ability::TypeFilter::Subtype(subtype)
                                if subtype == "Treasure"
                        )));
                    }
                    other => panic!("expected Typed target, got {other:?}"),
                }
                match count {
                    QuantityExpr::UpTo { max } => match *max {
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { filter },
                        } => assert_eq!(filter, target),
                        other => panic!("expected ObjectCount max, got {other:?}"),
                    },
                    other => panic!("expected UpTo count, got {other:?}"),
                }
            }
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn parse_sacrifice_all_uses_filtered_object_count() {
        let text = "sacrifice all permanents you control";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext::default();
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice {
                target,
                count,
                min_count,
            } => {
                assert_eq!(min_count, 0);
                match &target {
                    TargetFilter::Typed(tf) => {
                        assert_eq!(tf.controller, Some(ControllerRef::You));
                        assert!(tf
                            .type_filters
                            .iter()
                            .any(|type_filter| matches!(type_filter, TypeFilter::Permanent)));
                    }
                    other => panic!("expected Typed target, got {other:?}"),
                }
                match count {
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    } => assert_eq!(filter, target),
                    other => panic!("expected ObjectCount count, got {other:?}"),
                }
            }
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn parse_sacrifice_all_applies_actor_default_to_count_filter() {
        let text = "sacrifice all permanents";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::ParentTargetController),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice { target, count, .. } => {
                let TargetFilter::Typed(tf) = &target else {
                    panic!("expected Typed target, got {target:?}");
                };
                assert_eq!(tf.controller, Some(ControllerRef::ParentTargetController));
                match count {
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    } => assert_eq!(filter, target),
                    other => panic!("expected ObjectCount count, got {other:?}"),
                }
            }
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    /// Issue #967: "sacrifice any number of creatures, each with power 1 or
    /// less" — the comma+"each" distributive linker between the collective
    /// type word and the per-object property suffix dropped the power filter
    /// entirely (the parser stopped at the comma, leaving `, each with...`
    /// unconsumed; the type-phrase fallback then produced
    /// `Effect::Sacrifice { target: TargetFilter::Typed(Creature), count: 1 }`
    /// — no power constraint, fixed count). CR 208.1: the per-object power
    /// comparison applies via the existing P/T suffix combinator.
    #[test]
    fn parse_sacrifice_any_number_creatures_comma_each_power_filter_attached() {
        use crate::types::ability::{Comparator, FilterProp, PtStat, PtValueScope};

        for text in [
            "sacrifice any number of creatures, each with power 1 or less",
            "sacrifice any number of creatures each with power 1 or less",
        ] {
            let lower = text.to_lowercase();
            let mut ctx = ParseContext {
                actor: Some(ControllerRef::You),
                ..Default::default()
            };
            let result =
                parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
            let Effect::Sacrifice { target, count, .. } = lower_targeted_action_ast(result) else {
                panic!("expected Effect::Sacrifice for {text:?}");
            };
            let TargetFilter::Typed(ref tf) = target else {
                panic!("expected Typed filter for {text:?}, got {target:?}");
            };
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "missing Creature type for {text:?}",
            );
            let has_pt = tf.properties.iter().any(|p| {
                matches!(
                    p,
                    FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Current,
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 1 },
                    }
                )
            });
            assert!(
                has_pt,
                "missing PtComparison(Power, Current, LE, 1) for {text:?}: {:?}",
                tf.properties,
            );
            match count {
                QuantityExpr::UpTo { max } => match *max {
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    } => assert_eq!(filter, target, "ObjectCount filter mismatch for {text:?}"),
                    other => panic!("expected ObjectCount max for {text:?}, got {other:?}"),
                },
                other => panic!("expected UpTo count for {text:?}, got {other:?}"),
            }
        }
    }

    /// Issue #967 follow-up: Angelic Aberration's "each with base power or
    /// toughness 1 or less" disjunctive variant. The same comma-each linker
    /// must allow the `power or toughness` disjunction (CR 208 + CR 208.4b)
    /// to attach correctly with the `Base` scope qualifier.
    #[test]
    fn parse_sacrifice_any_number_creatures_comma_each_base_pt_disjunction() {
        use crate::types::ability::{Comparator, FilterProp, PtStat, PtValueScope};

        let text = "sacrifice any number of creatures, each with base power or toughness 1 or less";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::You),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        let Effect::Sacrifice { target, .. } = lower_targeted_action_ast(result) else {
            panic!("expected Effect::Sacrifice");
        };
        let TargetFilter::Typed(ref tf) = target else {
            panic!("expected Typed filter, got {target:?}");
        };
        // Disjunctive `power or toughness ≤ 1` ⇒ `AnyOf {
        //   PtComparison(Power, Base, LE, 1),
        //   PtComparison(Toughness, Base, LE, 1),
        // }`.
        let has_disj = tf.properties.iter().any(|p| {
            let FilterProp::AnyOf { props } = p else {
                return false;
            };
            let want_power = FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Base,
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 1 },
            };
            let want_tough = FilterProp::PtComparison {
                stat: PtStat::Toughness,
                scope: PtValueScope::Base,
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 1 },
            };
            props.contains(&want_power) && props.contains(&want_tough)
        });
        assert!(
            has_disj,
            "missing AnyOf[Power Base LE 1, Toughness Base LE 1], got {:?}",
            tf.properties,
        );
    }

    // Issue #458: "sacrifice any number of <filter>" — Scapeshift class.
    // CR 107.1c: "any number" includes zero, so `min_count` is 0 (vs. 1 for
    // "one or more"). The dynamic `UpTo(ObjectCount)` ceiling is unchanged.
    #[test]
    fn parse_sacrifice_any_number_of_lands_uses_zero_min_count() {
        let text = "sacrifice any number of lands";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::You),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice {
                target,
                count,
                min_count,
            } => {
                assert_eq!(min_count, 0, "\"any number\" includes zero (CR 107.1c)");
                match count {
                    QuantityExpr::UpTo { max } => match *max {
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { filter },
                        } => assert_eq!(filter, target),
                        other => panic!("expected ObjectCount max, got {other:?}"),
                    },
                    other => panic!("expected UpTo count, got {other:?}"),
                }
            }
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
                    assert_eq!(tf.controller, Some(ControllerRef::You));
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
    fn parse_mass_return_to_battlefield_after_leading_destination() {
        let text = "Return to the battlefield all artifact and creature cards in your graveyard that were put there from the battlefield this turn";
        let lower = text.to_lowercase();
        let ast = parse_targeted_action_ast(text, &lower, &mut ParseContext::default())
            .expect("mass return should parse");
        let effect = lower_targeted_action_ast(ast);
        match effect {
            Effect::ChangeZoneAll {
                origin,
                destination,
                target: TargetFilter::Or { filters },
                enters_under: None,
                enter_tapped,
                face_down_profile: None,
            } => {
                assert_eq!(origin, None);
                assert_eq!(destination, Zone::Battlefield);
                assert!(!enter_tapped);
                assert_eq!(filters.len(), 2);
                for filter in filters {
                    let TargetFilter::Typed(typed) = filter else {
                        panic!("expected typed OR leg, got {filter:?}");
                    };
                    assert_eq!(typed.controller, Some(ControllerRef::You));
                    assert!(typed.properties.contains(&FilterProp::InZone {
                        zone: Zone::Graveyard
                    }));
                    assert!(typed.properties.iter().any(|prop| matches!(
                        prop,
                        FilterProp::ZoneChangedThisTurn {
                            from: Some(Zone::Battlefield),
                            to: Some(Zone::Graveyard),
                        }
                    )));
                }
            }
            other => panic!("Expected ChangeZoneAll return to battlefield, got {other:?}"),
        }
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
                            if has_type(tf, crate::types::ability::TypeFilter::Creature)
                                && has_prop(tf, crate::types::ability::FilterProp::Another)
                    )),
                    "expected creature branch with Another, got {filters:?}"
                );
                assert!(
                    filters.iter().any(|filter| {
                        is_stack_spell_leg(filter)
                            && typed_leg(filter).is_some_and(|tf| {
                                has_prop(tf, crate::types::ability::FilterProp::Another)
                            })
                    }),
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
        // produces a typed DivideRounded amount at the imperative level.
        let text = "lose half their life, rounded up";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(result.is_some(), "Should parse; got {result:?}");
        match result.unwrap() {
            NumericImperativeAst::LoseLife { amount } => {
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::DivideRounded {
                            rounding: crate::types::ability::RoundingMode::Up,
                            ..
                        }
                    ),
                    "Expected DivideRounded(Up), got {amount:?}"
                );
            }
            other => panic!("Expected LoseLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_sacrifice_half_non_demon_permanents_lifts_count_filter() {
        let text = "sacrifice half the non-Demon permanents you control, rounded up";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Sacrifice { target, count, .. }) => {
                assert!(matches!(
                    count,
                    QuantityExpr::DivideRounded {
                        rounding: crate::types::ability::RoundingMode::Up,
                        ..
                    }
                ));
                let TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller: Some(ControllerRef::You),
                    ..
                }) = target
                else {
                    panic!("expected You-controlled typed target");
                };
                assert_eq!(
                    type_filters,
                    vec![
                        crate::types::ability::TypeFilter::Permanent,
                        crate::types::ability::TypeFilter::Non(Box::new(
                            crate::types::ability::TypeFilter::Subtype("Demon".to_string())
                        )),
                    ]
                );
            }
            other => panic!("Expected Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn parse_sacrifice_half_non_god_creatures_ignores_choice_rounding_suffix() {
        let text =
            "sacrifice half the non-God creatures they control of their choice, rounded down";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext::default();
        let result = parse_targeted_action_ast(text, &lower, &mut ctx);
        match result {
            Some(TargetedImperativeAst::Sacrifice { target, count, .. }) => {
                assert!(matches!(
                    count,
                    QuantityExpr::DivideRounded {
                        rounding: crate::types::ability::RoundingMode::Down,
                        ..
                    }
                ));
                assert!(ctx.diagnostics.is_empty(), "{:?}", ctx.diagnostics);
                let TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller: Some(ControllerRef::ScopedPlayer),
                    ..
                }) = target
                else {
                    panic!("expected ScopedPlayer-controlled typed target");
                };
                assert_eq!(
                    type_filters,
                    vec![
                        crate::types::ability::TypeFilter::Creature,
                        crate::types::ability::TypeFilter::Non(Box::new(
                            crate::types::ability::TypeFilter::Subtype("God".to_string())
                        )),
                    ]
                );
            }
            other => panic!("Expected Sacrifice, got {other:?}"),
        }
    }

    /// CR 608.2c — Yuriko, the Tiger's Shadow / Dark Confidant class. The bare
    /// anaphoric prefix "that card" in `"loses life equal to that card's mana
    /// value"` is an instruction-order referent: it points at the object
    /// introduced by an earlier `RevealTop` / `Mill` / `ChangeZone`
    /// instruction in the same ability. `classify_possessive_referent`
    /// therefore emits `ObjectScope::Demonstrative` (the noun-phrase
    /// back-reference, distinct from the pronoun "its"), whose runtime resolver
    /// reads `effect_context_object` first (the revealed card) and only then
    /// falls back to the trigger source and the cost-paid object. The dedicated
    /// variant keeps the subject-injection rewrite from rebinding this fixed
    /// antecedent.
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
                            qty: QuantityRef::ObjectManaValue {
                                scope: crate::types::ability::ObjectScope::Demonstrative
                            }
                        }
                    ),
                    "Expected ObjectManaValue {{ Demonstrative }}, got {amount:?}"
                );
            }
            other => panic!("Expected LoseLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_lose_life_equal_to_life_gained_this_turn() {
        let text = "loses life equal to the amount of life you gained this turn";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(
            result.is_some(),
            "Should parse third-person life loss whose amount references gained life"
        );
        match result.unwrap() {
            NumericImperativeAst::LoseLife { amount } => {
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::Ref {
                            qty: QuantityRef::LifeGainedThisTurn {
                                player: PlayerScope::Controller
                            }
                        }
                    ),
                    "Expected LifeGainedThisTurn {{ Controller }}, got {amount:?}"
                );
            }
            other => panic!("Expected LoseLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_lose_life_two_times_x() {
        let text = "lose two times X life";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(result.is_some(), "Should parse multiplied X life loss");
        match result.unwrap() {
            NumericImperativeAst::LoseLife { amount } => match amount {
                QuantityExpr::Multiply { factor, inner } => {
                    assert_eq!(factor, 2);
                    assert!(matches!(
                        *inner,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Variable { .. }
                        }
                    ));
                }
                other => panic!("Expected Multiply, got {other:?}"),
            },
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
                // CR 608.2k: bare anaphoric "its power" parses to the parse-time
                // marker `Anaphoric`; the parser remaps it to a concrete scope
                // where context allows, else it survives to runtime (resolving
                // identically to `CostPaidObject`).
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Power {
                                scope: crate::types::ability::ObjectScope::Anaphoric
                            }
                        }
                    ),
                    "Expected Power {{ CostPaidObject }}, got {amount:?}"
                );
            }
            other => panic!("Expected GainLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_gains_life_equal_to_power() {
        let text = "gains life equal to its power";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(
            result.is_some(),
            "Should parse stripped third-person gain-life predicates"
        );
        match result.unwrap() {
            NumericImperativeAst::GainLife { amount } => assert!(
                matches!(
                    amount,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: crate::types::ability::ObjectScope::Anaphoric
                        }
                    }
                ),
                "Expected Power {{ Anaphoric }}, got {amount:?}"
            ),
            other => panic!("Expected GainLife, got {other:?}"),
        }
    }

    /// CR 119.3 + CR 208.1: "loses life equal to its power plus its toughness" —
    /// Phthisis class. Both operands use Anaphoric scope so the enclosing
    /// clause's subject-injection applies identically to the individual
    /// "its power" / "its toughness" single-value forms. The destroy effect
    /// that precedes this clause sets `effect_context_object` to the destroyed
    /// creature's LKI, providing the correct runtime referent.
    #[test]
    fn parse_lose_life_equal_to_power_plus_toughness() {
        let text = "lose life equal to its power plus its toughness";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(
            result.is_some(),
            "Should parse 'lose life equal to its power plus its toughness'"
        );
        match result.unwrap() {
            NumericImperativeAst::LoseLife { amount } => match amount {
                QuantityExpr::Sum { exprs } => {
                    assert_eq!(exprs.len(), 2, "Sum should have two operands");
                    assert!(
                        matches!(
                            exprs[0],
                            QuantityExpr::Ref {
                                qty: QuantityRef::Power {
                                    scope: crate::types::ability::ObjectScope::Anaphoric
                                }
                            }
                        ),
                        "First operand should be Power(Anaphoric), got {:?}",
                        exprs[0]
                    );
                    assert!(
                        matches!(
                            exprs[1],
                            QuantityExpr::Ref {
                                qty: QuantityRef::Toughness {
                                    scope: crate::types::ability::ObjectScope::Anaphoric
                                }
                            }
                        ),
                        "Second operand should be Toughness(Anaphoric), got {:?}",
                        exprs[1]
                    );
                }
                other => panic!("Expected Sum, got {other:?}"),
            },
            other => panic!("Expected LoseLife, got {other:?}"),
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
    fn parse_pay_any_amount_of_mana_as_variable_mana_cost() {
        let text = "pay any amount of mana";
        let lower = text.to_lowercase();
        let Some(CostResourceImperativeAst::Pay {
            cost: PaymentCost::Mana { cost },
        }) = parse_cost_resource_ast(text, &lower, &mut ParseContext::default())
        else {
            panic!("expected variable mana PayCost");
        };
        let crate::types::mana::ManaCost::Cost { shards, generic } = cost else {
            panic!("expected concrete mana cost");
        };
        assert_eq!(generic, 0);
        assert!(matches!(
            shards.as_slice(),
            [crate::types::mana::ManaCostShard::X]
        ));
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

    /// CR 500.8 + CR 510.2: Obeka, Splitter of Seconds — "you get that many
    /// additional upkeep steps after this phase" must thread the triggering
    /// combat damage amount through `EventContextAmount`, not collapse it to
    /// the singular default.
    #[test]
    fn parse_obeka_that_many_additional_upkeep_steps_binds_event_amount() {
        let text = "you get that many additional upkeep steps after this phase";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
        let effect = lower_imperative_family_effect(
            result.expect("Obeka consequent should parse as AdditionalPhase"),
        );
        match effect {
            Effect::AdditionalPhase {
                phase: Phase::Upkeep,
                after: Phase::Upkeep,
                count:
                    QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                ..
            } => {}
            other => {
                panic!("expected AdditionalPhase with EventContextAmount count, got {other:?}")
            }
        }
    }

    #[test]
    fn parse_fixed_count_additional_upkeep_steps_binds_literal_count() {
        let text = "you get two additional upkeep steps after this phase";
        let lower = text.to_lowercase();
        let effect = lower_imperative_family_effect(
            parse_imperative_family_ast(text, &lower, &mut ParseContext::default())
                .expect("fixed-count additional upkeep should parse"),
        );
        match effect {
            Effect::AdditionalPhase {
                count: QuantityExpr::Fixed { value: 2 },
                ..
            } => {}
            other => panic!("expected count=Fixed(2) for fixed-count form, got {other:?}"),
        }
    }

    /// Singular "an additional upkeep step" must keep the legacy Fixed(1)
    /// semantics so Paradox Haze and similar cards still push exactly one
    /// extra step.
    #[test]
    fn parse_additional_upkeep_step_defaults_to_count_one() {
        let text = "get an additional upkeep step after this step";
        let lower = text.to_lowercase();
        let effect = lower_imperative_family_effect(
            parse_imperative_family_ast(text, &lower, &mut ParseContext::default())
                .expect("should parse singular additional upkeep"),
        );
        match effect {
            Effect::AdditionalPhase {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            } => {}
            other => panic!("expected count=Fixed(1) for singular form, got {other:?}"),
        }
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
        // CR 109.5 + CR 115.10: "their" in a discard imperative refers to
        // the subject, not the printed ability controller. The local
        // imperative parser represents that as `Target`; an outer
        // each-player scope rewrites it to `ScopedPlayer`.
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
                                player: PlayerScope::Target
                            }
                        }
                    ),
                    "Expected HandSize ref scoped to Target, got {count:?}"
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
                if *counter_type == crate::types::counter::CounterType::Plus1Plus1
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
    fn parse_choose_creature_card_in_your_hand() {
        let text = "choose a creature card in your hand";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromZone {
                zones,
                zone_owner,
                filter,
                ..
            }) => {
                assert_eq!(zones, vec![Zone::Hand]);
                assert_eq!(zone_owner, ZoneOwner::Controller);
                let TargetFilter::Typed(tf) = filter else {
                    panic!("expected Typed creature filter, got {filter:?}");
                };
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("Expected FromZone, got {other:?}"),
        }
    }

    /// D-09 regression: Riveteers Provocateur's "choose a creature card in your
    /// hand without blitz" has an unmodeled post-zone restriction ("without
    /// blitz"). The bare type filter (`Creature`) must still parse, but the
    /// unparseable suffix must NOT be force-fed to the strict search-filter
    /// suffix dispatch — doing so emits a `search-filter-suffix unmatched`
    /// TargetFallback diagnostic, which trips the parser-diagnostic ratchet.
    #[test]
    fn parse_choose_creature_card_with_unmodeled_post_zone_suffix_emits_no_diagnostic() {
        let text = "choose a creature card in your hand without blitz";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext::default();
        let result = parse_choose_ast(text, &lower, &mut ctx);
        match result {
            Some(ChooseImperativeAst::FromZone {
                zones,
                zone_owner,
                filter,
                ..
            }) => {
                assert_eq!(zones, vec![Zone::Hand]);
                assert_eq!(zone_owner, ZoneOwner::Controller);
                let TargetFilter::Typed(tf) = filter else {
                    panic!("expected Typed creature filter, got {filter:?}");
                };
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("Expected FromZone, got {other:?}"),
        }
        assert!(
            !ctx.diagnostics.iter().any(|d| matches!(
                d,
                OracleDiagnostic::TargetFallback { context, .. }
                    if context == "search-filter-suffix unmatched"
            )),
            "unmodeled post-zone suffix must not emit a search-filter-suffix \
             unmatched diagnostic, got {:?}",
            ctx.diagnostics
        );
    }

    #[test]
    fn parse_choose_instant_or_sorcery_card_in_hand_or_graveyard() {
        let text = "choose an instant or sorcery card in your hand or graveyard";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromZone {
                zones,
                zone_owner,
                filter,
                ..
            }) => {
                assert_eq!(zones, vec![Zone::Hand, Zone::Graveyard]);
                assert_eq!(zone_owner, ZoneOwner::Controller);
                let TargetFilter::Or { filters } = filter else {
                    panic!("expected Or instant/sorcery filter, got {filter:?}");
                };
                assert_eq!(filters.len(), 2);
            }
            other => panic!("Expected FromZone, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_targeted_players_graveyard_or_hand() {
        let text = "you choose a nonland card from that player's graveyard or hand";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromZone {
                zones,
                zone_owner,
                filter,
                ..
            }) => {
                assert_eq!(zones, vec![Zone::Graveyard, Zone::Hand]);
                assert_eq!(zone_owner, ZoneOwner::TargetedPlayer);
                let TargetFilter::Typed(tf) = filter else {
                    panic!("expected Typed nonland filter, got {filter:?}");
                };
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))));
            }
            other => panic!("Expected FromZone, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_random_card_in_graveyard_is_not_direct_choice() {
        let text = "choose a card at random in your graveyard";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        assert!(
            !matches!(result, Some(ChooseImperativeAst::FromZone { .. })),
            "random choices must not become player-directed ChooseFromZone prompts"
        );
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
                ..
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
    fn parse_choose_permanent_of_each_type_liliana_dreadhorde() {
        // Liliana, Dreadhorde General −9: "Each opponent chooses a permanent they
        // control of each permanent type and sacrifices the rest."
        // The "Each opponent" actor prefix is stripped upstream; this exercises the
        // post-strip body reaching parse_choose_ast.
        let text = "choose a permanent they control of each permanent type";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::CategoryAndSacrificeRest {
                categories,
                chooser_scope,
                ..
            }) => {
                assert_eq!(
                    categories,
                    vec![
                        CoreType::Artifact,
                        CoreType::Battle,
                        CoreType::Creature,
                        CoreType::Enchantment,
                        CoreType::Land,
                        CoreType::Planeswalker,
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
                choose_filter,
                sacrifice_filter,
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
                assert!(
                    matches!(
                        &choose_filter,
                        TargetFilter::Typed(TypedFilter {
                            type_filters,
                            ..
                        }) if type_filters.contains(&TypeFilter::Non(Box::new(TypeFilter::Land)))
                    ),
                    "Gearhulk choose_filter should be nonland permanent, got {choose_filter:?}"
                );
                assert_eq!(choose_filter, sacrifice_filter);
            }
            other => panic!("Expected CategoryAndSacrificeRest, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_from_among_gearhulk_pattern_with_comma() {
        let text = "choose an artifact, a creature, an enchantment, and a planeswalker, from among the nonland permanents they control";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::CategoryAndSacrificeRest {
                categories,
                choose_filter,
                sacrifice_filter,
                ..
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
                assert_eq!(choose_filter, sacrifice_filter);
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
                additional_zones,
                zone_owner,
                filter,
                chooser,
                up_to,
                constraint,
            } => {
                assert_eq!(count, 3);
                assert_eq!(zone, Zone::Exile);
                assert!(additional_zones.is_empty());
                assert_eq!(zone_owner, ZoneOwner::Controller);
                assert!(filter.is_none());
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

    #[test]
    fn parse_attack_you_this_combat_if_able() {
        let result = try_parse_attack_if_able("attacks you this combat if able");
        assert!(
            result.is_some(),
            "Should parse 'attacks you this combat if able'"
        );
        match result.unwrap() {
            ImperativeFamilyAst::ForceAttack {
                duration,
                required_player,
            } => {
                assert_eq!(duration, Duration::UntilEndOfCombat);
                assert_eq!(required_player, TargetFilter::Controller);
            }
            other => panic!("Expected ForceAttack, got {other:?}"),
        }
    }

    #[test]
    fn parse_attack_that_player_this_combat_if_able() {
        // CR 508.1d: Ruhan of the Fomori / Raving Dead / Knight Rampager —
        // "attacks that player" references the opponent chosen earlier in the
        // same resolution, lowered to ControllerRef::ChosenPlayer { index: 0 }.
        let result = try_parse_attack_if_able("attacks that player this combat if able")
            .expect("should parse 'attacks that player this combat if able'");
        match result {
            ImperativeFamilyAst::ForceAttack {
                duration,
                required_player,
            } => {
                assert_eq!(duration, Duration::UntilEndOfCombat);
                assert_eq!(
                    required_player.chosen_player_index(),
                    Some(0),
                    "that player must reference the chosen player at index 0, got {required_player:?}"
                );
            }
            other => panic!("Expected ForceAttack, got {other:?}"),
        }
    }

    #[test]
    fn ruhan_choose_opponent_then_force_attack_composes() {
        use crate::types::ability::{ChoiceType, Effect};

        // CR 608.2c: The full Ruhan-class trigger composes the opponent choice
        // with a forced attack at that opponent. Both resolve together, so the
        // sub-ability references the resolution-scoped ChosenPlayer { index }.
        let parsed = crate::parser::oracle::parse_oracle_text(
            "At the beginning of combat on your turn, choose an opponent at random. ~ attacks that player this combat if able.",
            "Ruhan of the Fomori",
            &[],
            &["Creature".to_string()],
            &[],
        );
        let trigger = parsed
            .triggers
            .first()
            .expect("should produce one triggered ability");
        let execute = trigger
            .execute
            .as_ref()
            .expect("trigger has an execute chain");
        assert!(
            matches!(
                &*execute.effect,
                Effect::Choose {
                    choice_type: ChoiceType::Opponent,
                    ..
                }
            ),
            "parent must be an opponent choice, got {:?}",
            execute.effect
        );
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("the force-attack must chain as the sub-ability");
        // The forced attacker is the source (~ → SelfRef), forced to attack the
        // resolution-scoped chosen opponent (chosen_players[0]).
        let Effect::ForceAttack {
            target,
            required_player,
            duration,
        } = &*sub.effect
        else {
            panic!("sub-ability must be a ForceAttack, got {:?}", sub.effect);
        };
        assert_eq!(*target, TargetFilter::SelfRef);
        assert_eq!(*duration, Duration::UntilEndOfCombat);
        assert_eq!(
            required_player.chosen_player_index(),
            Some(0),
            "must force attacking the chosen player, got {required_player:?}"
        );
    }

    /// CR 508.1d / CR 509.1c: the standalone-combat-requirement recognizer
    /// used to gate the conjunction split. Recognizes both attack and
    /// must-be-blocked forms, and rejects non-requirements.
    #[test]
    fn standalone_combat_requirement_recognizer() {
        assert!(is_standalone_combat_requirement(
            "attack this combat if able"
        ));
        assert!(is_standalone_combat_requirement(
            "attacks this turn if able"
        ));
        assert!(is_standalone_combat_requirement(
            "must be blocked this turn if able"
        ));
        assert!(is_standalone_combat_requirement("must be blocked if able"));
        // Not combat requirements.
        assert!(!is_standalone_combat_requirement("haste"));
        assert!(!is_standalone_combat_requirement("gains flying"));
        assert!(!is_standalone_combat_requirement("draw a card"));
    }

    /// CR 400.6 + CR 701.24c: "shuffle the cards from your hand into your
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

    /// CR 608.2c: "shuffle that library" — anaphoric reference to the
    /// player target bound earlier in the same instruction (Visions).
    #[test]
    fn parse_shuffle_that_library_resolves_to_parent_target() {
        let text = "shuffle that library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ShuffleLibrary { target }) => {
                assert_eq!(target, TargetFilter::ParentTarget);
            }
            other => panic!("Expected ShuffleLibrary {{ ParentTarget }}, got {other:?}"),
        }
    }

    #[test]
    fn parse_shuffle_possessive_library_siblings() {
        for text in [
            "shuffle that player's library",
            "shuffle their library",
            "shuffle his or her library",
        ] {
            let result = parse_shuffle_ast(text, text);
            match result {
                Some(ShuffleImperativeAst::ShuffleLibrary { target }) => {
                    assert_eq!(target, TargetFilter::ParentTarget);
                }
                other => {
                    panic!("Expected ShuffleLibrary {{ ParentTarget }} for {text}, got {other:?}")
                }
            }
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
                ..
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
                ..
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

    /// CR 701.24c + CR 400.3: "shuffle ~ into its owner's library" — the
    /// self-referential tail of Green Sun's Zenith, the Beacon cycle, Nexus
    /// of Fate, etc. The `~` token is produced by `normalize_card_name_refs`
    /// for the source card's own name; the AST must classify it as
    /// `TargetFilter::SelfRef` with `owner_library: true`, NOT
    /// `Unimplemented`. This is the building-block assertion that unlocks
    /// the entire self-shuffle class.
    #[test]
    fn parse_shuffle_self_ref_into_owners_library() {
        let text = "shuffle ~ into its owner's library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneToLibrary {
                target,
                owner_library,
            }) => {
                assert_eq!(target, TargetFilter::SelfRef);
                assert!(owner_library);
            }
            other => panic!("Expected ChangeZoneToLibrary SelfRef+owner_library, got {other:?}"),
        }
    }

    /// Sibling coverage: "shuffle ~ into your library" — same self-reference
    /// shape but possessive resolves to controller (`owner_library: false`),
    /// matching the variant cycle that names "your library" explicitly.
    #[test]
    fn parse_shuffle_self_ref_into_your_library() {
        let text = "shuffle ~ into your library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneToLibrary {
                target,
                owner_library,
            }) => {
                assert_eq!(target, TargetFilter::SelfRef);
                assert!(!owner_library);
            }
            other => panic!("Expected ChangeZoneToLibrary SelfRef+!owner_library, got {other:?}"),
        }
    }

    /// Regression: "shuffle their hand and graveyard into their library"
    /// (Echo of Eons after subject-stripping + deconjugation) must still route
    /// to `ChangeZoneAllToLibrary`, NOT the descriptive-target path.
    #[test]
    fn parse_shuffle_their_hand_and_graveyard_into_their_library() {
        let text = "shuffle their hand and graveyard into their library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origins }) => {
                assert_eq!(origins, vec![Zone::Hand, Zone::Graveyard]);
            }
            other => {
                panic!("Expected ChangeZoneAllToLibrary Hand+Graveyard, got {other:?}")
            }
        }
    }

    /// CR 701.24c + CR 400.3: "shuffle enchanted creature into its owner's
    /// library" — the descriptive-target path for Aura-based shuffle effects
    /// (Dramatic Accusation, Stay Hidden Stay Silent). The target is parsed by
    /// `parse_target` which resolves "enchanted creature" to a `Typed` filter
    /// with `EnchantedBy` property. `owner_library` is `true` because the
    /// possessive resolves to the card's owner.
    #[test]
    fn parse_shuffle_enchanted_creature_into_owners_library() {
        use crate::types::ability::FilterProp;
        let text = "shuffle enchanted creature into its owner's library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneToLibrary {
                target,
                owner_library,
            }) => {
                // "enchanted creature" → Typed filter with EnchantedBy property
                match &target {
                    TargetFilter::Typed(tf) => {
                        assert!(
                            tf.properties.contains(&FilterProp::EnchantedBy),
                            "expected EnchantedBy property, got {tf:?}"
                        );
                    }
                    other => panic!("Expected Typed filter with EnchantedBy, got {other:?}"),
                }
                assert!(owner_library, "expected owner_library = true");
            }
            other => {
                panic!("Expected ChangeZoneToLibrary with EnchantedBy target, got {other:?}")
            }
        }
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
            // CR 201.2a: "its controller's" possessive + card-type name source —
            // Eradicate ("that creature"), Crumble to Dust ("that land"),
            // Counterbore ("that spell"), Deicide ("its controller's … that card").
            "search its controller's graveyard, hand, and library for all cards with the same name as that creature and exile them",
            "search its controller's graveyard, hand, and library for any number of cards with the same name as that land and exile them",
            "search its controller's graveyard, hand, and library for all cards with the same name as that spell and exile them",
            "search its controller's graveyard, hand, and library for any number of cards with the same name as that card and exile them",
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
    fn parse_copy_stack_ability_target_preserves_unknown_qualifier_remainder() {
        let controlled =
            parse_copy_stack_ability_target("target activated or triggered ability you control")
                .expect("controlled stack ability target should parse");
        assert_eq!(controlled.1, "");
        assert!(matches!(
            controlled.0,
            TargetFilter::StackAbility {
                controller: Some(ControllerRef::You)
            }
        ));

        let unscoped = parse_copy_stack_ability_target("target triggered ability")
            .expect("unqualified stack ability target should parse");
        assert_eq!(unscoped.1, "");
        assert!(matches!(
            unscoped.0,
            TargetFilter::StackAbility { controller: None }
        ));

        assert!(
            parse_copy_stack_ability_target(
                "target activated or triggered ability you don't control"
            )
            .is_none(),
            "unknown qualifier must not widen to an unscoped StackAbility target"
        );
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
                ..
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
        assert_eq!(duration.as_ref(), Some(&Duration::UntilEndOfTurn));
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

    /// CR 122.1b: "put that many <keyword> counter(s) on <target>" must
    /// canonicalize multi-word keyword counter names ("double strike", "first
    /// strike") to `CounterType::Keyword(...)`. The previous
    /// `.find(whitespace)` slicing truncated at the first space, mapping
    /// "double strike" → `CounterType::Generic("double")`.
    #[test]
    fn that_many_counters_multi_word_keyword() {
        use crate::types::counter::CounterType;
        use crate::types::keywords::KeywordKind;
        let mut ctx = ParseContext::default();
        let effect =
            try_parse_that_many_counters("put that many double strike counters on it", &mut ctx)
                .expect("clause should parse");
        match effect {
            Effect::PutCounter {
                counter_type,
                count,
                ..
            } => {
                assert_eq!(
                    counter_type,
                    CounterType::Keyword(KeywordKind::DoubleStrike),
                    "multi-word keyword counter must canonicalize to Keyword(DoubleStrike)"
                );
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount
                        }
                    ),
                    "'that many' must resolve to event-context amount, got {count:?}"
                );
            }
            other => panic!("expected Effect::PutCounter, got {other:?}"),
        }
    }

    // --- coalesce_pump_with_modifications (CR 611.2c / 702.10) ---

    /// Returns true if any static ability in a GenericEffect carries `keyword`.
    fn generic_has_keyword(effect: &Effect, keyword: crate::types::keywords::Keyword) -> bool {
        match effect {
            Effect::GenericEffect {
                static_abilities, ..
            } => static_abilities.iter().any(|s| {
                s.modifications.iter().any(|m| {
                    matches!(m, ContinuousModification::AddKeyword { keyword: k } if *k == keyword)
                })
            }),
            _ => false,
        }
    }

    /// pump + haste body retains all three mods (AddPower, AddToughness,
    /// AddKeyword(Haste)) as one GenericEffect with `target: None` and
    /// UntilEndOfTurn.
    #[test]
    fn coalesce_pump_haste_retains_all_mods() {
        let effect = coalesce_pump_with_modifications("get +2/+0 and gain haste until end of turn")
            .expect("pump+keyword body should coalesce");
        match &effect {
            Effect::GenericEffect {
                static_abilities,
                duration,
                target,
            } => {
                assert_eq!(*target, None, "non-distributed body must not broadcast");
                assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
                let mods = &static_abilities[0].modifications;
                assert!(mods
                    .iter()
                    .any(|m| matches!(m, ContinuousModification::AddPower { value: 2 })));
                assert!(mods
                    .iter()
                    .any(|m| matches!(m, ContinuousModification::AddToughness { value: 0 })));
                assert!(generic_has_keyword(
                    &effect,
                    crate::types::keywords::Keyword::Haste
                ));
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
    }

    /// "lifelink" contains the substring "life", so the `gain` dispatch must
    /// route "gain lifelink" to the keyword-grant parser, not the numeric
    /// life-gain branch (which would fail and fall through to Unimplemented).
    /// Reaches cards like Blessing of Belzenlok ("…it also gains lifelink…").
    #[test]
    fn gain_lifelink_dispatches_to_keyword_grant_not_life() {
        let ast = parse_imperative_family_ast(
            "gain lifelink",
            "gain lifelink",
            &mut ParseContext::default(),
        )
        .expect("'gain lifelink' should parse");
        match ast {
            ImperativeFamilyAst::GainKeyword(effect) => assert!(
                generic_has_keyword(&effect, crate::types::keywords::Keyword::Lifelink),
                "expected a Lifelink keyword grant"
            ),
            _ => panic!("expected GainKeyword(Lifelink)"),
        }
    }

    /// CR 724.1: "end the turn" parses to the no-target `Effect::EndTheTurn`
    /// (Time Stop, Sundial of the Infinite, Obeka, Glorious End).
    #[test]
    fn end_the_turn_parses_to_end_the_turn_effect() {
        let ast = parse_imperative_family_ast(
            "end the turn",
            "end the turn",
            &mut ParseContext::default(),
        )
        .expect("'end the turn' should parse");
        assert!(
            matches!(ast, ImperativeFamilyAst::GainKeyword(Effect::EndTheTurn)),
            "expected Effect::EndTheTurn"
        );
    }

    /// CR 724.2: "end the combat phase" parses to the no-target
    /// `Effect::EndCombatPhase` (Mandate of Peace).
    #[test]
    fn end_the_combat_phase_parses_to_end_combat_phase_effect() {
        let ast = parse_imperative_family_ast(
            "end the combat phase",
            "end the combat phase",
            &mut ParseContext::default(),
        )
        .expect("'end the combat phase' should parse");
        assert!(
            matches!(
                ast,
                ImperativeFamilyAst::GainKeyword(Effect::EndCombatPhase)
            ),
            "expected Effect::EndCombatPhase"
        );
    }

    /// Regression: a genuine life-gain clause must still reach the numeric
    /// life-gain parser, not be captured by the keyword-grant branch.
    #[test]
    fn gain_life_still_dispatches_to_numeric_life_gain() {
        let ast =
            parse_imperative_family_ast("gain 3 life", "gain 3 life", &mut ParseContext::default())
                .expect("'gain 3 life' should parse");
        assert!(
            matches!(
                ast,
                ImperativeFamilyAst::Structured(ImperativeAst::Numeric(_))
            ),
            "expected numeric life-gain dispatch"
        );
    }

    /// Production-path regression for the same lifelink/life substring
    /// ambiguity, including default duration and SelfRef scope.
    #[test]
    fn effect_gain_lifelink_dispatches_to_keyword_grant() {
        let def = crate::parser::oracle_effect::parse_effect_chain(
            "gain lifelink until end of turn",
            AbilityKind::Spell,
        );
        let Effect::GenericEffect {
            static_abilities,
            duration,
            ..
        } = &*def.effect
        else {
            panic!(
                "expected GenericEffect for lifelink grant, got {:?}",
                def.effect
            );
        };
        assert_eq!(duration.as_ref(), Some(&Duration::UntilEndOfTurn));
        assert_eq!(
            static_abilities[0].affected,
            Some(TargetFilter::SelfRef),
            "bare creature-scoped keyword grant must remain SelfRef"
        );
        assert!(
            static_abilities[0].modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: crate::types::keywords::Keyword::Lifelink
                }
            )),
            "expected AddKeyword(Lifelink), got {:?}",
            static_abilities[0].modifications
        );
    }

    /// Pure pump body → None (falls through to the bare numeric Pump arm).
    #[test]
    fn coalesce_pure_pump_is_none() {
        assert!(coalesce_pump_with_modifications("get +2/+0").is_none());
    }

    /// Keyword-only body → None (handled by `try_parse_gain_keyword`).
    #[test]
    fn coalesce_keyword_only_is_none() {
        assert!(coalesce_pump_with_modifications("gain haste until end of turn").is_none());
    }

    /// No explicit duration on a pump+keyword body defaults to UntilEndOfTurn.
    #[test]
    fn coalesce_pump_keyword_defaults_until_end_of_turn() {
        let effect = coalesce_pump_with_modifications("get +1/+0 and gain flying")
            .expect("pump+keyword body should coalesce");
        match effect {
            Effect::GenericEffect { duration, .. } => {
                assert_eq!(duration, Some(Duration::UntilEndOfTurn));
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
    }

    /// Multi-keyword body keeps both granted keywords.
    #[test]
    fn coalesce_pump_multi_keyword() {
        let effect = coalesce_pump_with_modifications("get +1/+0 and gain flying and haste")
            .expect("pump+multi-keyword body should coalesce");
        assert!(generic_has_keyword(
            &effect,
            crate::types::keywords::Keyword::Haste
        ));
        assert!(generic_has_keyword(
            &effect,
            crate::types::keywords::Keyword::Flying
        ));
    }

    /// Integration: a subjectless pump+keyword body routed through
    /// `parse_imperative_family_ast` yields `GainKeyword(GenericEffect)`.
    #[test]
    fn imperative_family_pump_keyword_is_gain_keyword() {
        let text = "get +2/+0 and gain haste until end of turn";
        let mut ctx = ParseContext::default();
        let ast = parse_imperative_family_ast(text, &text.to_lowercase(), &mut ctx)
            .expect("pump+keyword body should parse");
        match ast {
            ImperativeFamilyAst::GainKeyword(effect) => {
                assert!(matches!(effect, Effect::GenericEffect { .. }));
            }
            other => panic!("expected GainKeyword(GenericEffect), got {other:?}"),
        }
    }

    /// Regression: a pure pump body still routes through the numeric Pump arm.
    #[test]
    fn imperative_family_pure_pump_still_numeric() {
        let text = "get +2/+0";
        let mut ctx = ParseContext::default();
        let ast = parse_imperative_family_ast(text, &text.to_lowercase(), &mut ctx)
            .expect("pump body should parse");
        assert!(
            matches!(
                ast,
                ImperativeFamilyAst::Structured(ImperativeAst::Numeric(_))
            ),
            "pure pump must remain a numeric imperative, got {ast:?}"
        );
    }

    /// CR 706 + CR 706.2: "Roll a d20" with no modifier parses to a bare RollDie.
    /// Issue #1675 — Canopy Gargantuan. "Put a number of +1/+1 counters on each
    /// other creature you control equal to that creature's toughness." rebinds
    /// the per-recipient "that creature's toughness" count to `Recipient` so the
    /// resolver re-evaluates it per object.
    #[test]
    fn put_counter_all_each_equal_to_that_creatures_toughness_rebinds_to_recipient() {
        use crate::types::ability::ObjectScope;
        let def = super::super::parse_effect_chain(
            "Put a number of +1/+1 counters on each other creature you control equal to that creature's toughness.",
            AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::PutCounterAll { count, .. } => assert!(
                matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Toughness {
                            scope: ObjectScope::Recipient
                        }
                    }
                ),
                "count must rebind to Recipient, got {count:?}"
            ),
            other => panic!("expected PutCounterAll, got {other:?}"),
        }
    }

    /// The rebind is gated: a genuine cost referent ("the sacrificed creature's
    /// power") stays `CostPaidObject` (resolved once, applied uniformly), so the
    /// rebind does not hijack cost-paid `*All` counts.
    #[test]
    fn rebind_distributive_recipient_count_leaves_cost_referent_alone() {
        use crate::types::ability::ObjectScope;
        let cost_paid = QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::CostPaidObject,
            },
        };
        let out = super::rebind_distributive_recipient_count(
            cost_paid.clone(),
            "put a +1/+1 counter on each creature you control equal to the sacrificed creature's power",
        );
        assert_eq!(
            out, cost_paid,
            "cost-referent count must NOT rebind to Recipient"
        );

        // And "that creature's power" (no cost participle) DOES rebind.
        let anaphor = QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::CostPaidObject,
            },
        };
        let rebound = super::rebind_distributive_recipient_count(
            anaphor,
            "put a +1/+1 counter on each creature you control equal to that creature's power",
        );
        assert!(matches!(
            rebound,
            QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Recipient
                }
            }
        ));
    }

    #[test]
    fn rebind_distributive_recipient_count_walks_all_quantity_wrappers() {
        use crate::types::ability::ObjectScope;

        let expr = QuantityExpr::Difference {
            left: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::CostPaidObject,
                },
            }),
            right: Box::new(QuantityExpr::Power {
                base: 2,
                exponent: Box::new(QuantityExpr::UpTo {
                    max: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::ObjectManaValue {
                            scope: ObjectScope::CostPaidObject,
                        },
                    }),
                }),
            }),
        };

        let rebound = super::rebind_distributive_recipient_count(
            expr,
            "put a number of +1/+1 counters on each creature you control equal to that creature's power",
        );

        let QuantityExpr::Difference { left, right } = rebound else {
            panic!("expected Difference, got {rebound:?}");
        };
        assert!(matches!(
            *left,
            QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Recipient
                }
            }
        ));

        let QuantityExpr::Power { exponent, .. } = *right else {
            panic!("expected Power, got {right:?}");
        };
        let QuantityExpr::UpTo { max } = *exponent else {
            panic!("expected UpTo, got {exponent:?}");
        };
        assert!(matches!(
            *max,
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Recipient
                }
            }
        ));
    }

    #[test]
    fn roll_a_d20_no_modifier_parses_to_roll_die() {
        let def = super::super::parse_effect_chain("Roll a d20.", AbilityKind::Spell);
        match &*def.effect {
            Effect::RollDie {
                count,
                sides,
                modifier,
                results,
            } => {
                assert_eq!(*sides, 20);
                assert_eq!(
                    *count,
                    crate::types::ability::QuantityExpr::Fixed { value: 1 }
                );
                assert!(modifier.is_none());
                assert!(results.is_empty());
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.2: "Roll a d20 and add the number of cards in your hand" parses
    /// to RollDie with an Add modifier referencing controller hand size.
    #[test]
    fn roll_a_d20_with_add_modifier_parses_to_roll_die_with_modifier() {
        let def = super::super::parse_effect_chain(
            "Roll a d20 and add the number of cards in your hand.",
            AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::RollDie {
                sides, modifier, ..
            } => {
                assert_eq!(*sides, 20);
                let m = modifier.as_ref().expect("expected Add modifier");
                match m {
                    crate::types::ability::DieRollModifier::Add { value } => match value {
                        QuantityExpr::Ref {
                            qty: QuantityRef::HandSize { player },
                        } => {
                            assert!(matches!(
                                player,
                                crate::types::ability::PlayerScope::Controller
                            ));
                        }
                        other => panic!("expected HandSize ref, got {other:?}"),
                    },
                    other => panic!("expected Add modifier, got {other:?}"),
                }
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.1a + CR 706.2: The single-die parser accepts word-count "one"
    /// as the same semantic count as article "a", including modifier clauses.
    #[test]
    fn roll_one_d20_with_add_modifier_parses_to_roll_die_with_modifier() {
        let def = super::super::parse_effect_chain(
            "Roll one d20 and add the number of cards in your hand.",
            AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::RollDie {
                count,
                sides,
                modifier,
                ..
            } => {
                assert_eq!(*sides, 20);
                assert_eq!(
                    *count,
                    crate::types::ability::QuantityExpr::Fixed { value: 1 }
                );
                assert!(matches!(
                    modifier,
                    Some(crate::types::ability::DieRollModifier::Add { .. })
                ));
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.2: "Roll a d20 and subtract the number of cards in your hand"
    /// parses to RollDie with a Subtract modifier. Mirrors the Add case to
    /// guarantee the sign path is wired.
    #[test]
    fn roll_a_d20_with_subtract_modifier_parses_to_roll_die_with_modifier() {
        let def = super::super::parse_effect_chain(
            "Roll a d20 and subtract the number of cards in your hand.",
            AbilityKind::Activated,
        );
        match &*def.effect {
            Effect::RollDie {
                sides, modifier, ..
            } => {
                assert_eq!(*sides, 20);
                let m = modifier.as_ref().expect("expected Subtract modifier");
                assert!(matches!(
                    m,
                    crate::types::ability::DieRollModifier::Subtract { .. }
                ));
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.2: "Roll a six-sided die" is the word-form variant — parses to
    /// RollDie { sides: 6 } so cards printed in the older "N-sided die"
    /// phrasing continue to work alongside the modern "dN" shorthand.
    #[test]
    fn roll_a_six_sided_die_parses_to_roll_die_six() {
        let def = super::super::parse_effect_chain("Roll a six-sided die.", AbilityKind::Spell);
        match &*def.effect {
            Effect::RollDie {
                sides, modifier, ..
            } => {
                assert_eq!(*sides, 6);
                assert!(modifier.is_none());
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.1: "Roll two six-sided dice" is the multi-dice word form —
    /// parses to `RollDie { count: Fixed(2), sides: 6 }`. Mirrors the
    /// `FlipCoins { count }` precedent for the dice axis.
    #[test]
    fn roll_two_six_sided_dice_parses_to_roll_die_count_two() {
        let def = super::super::parse_effect_chain("Roll two six-sided dice.", AbilityKind::Spell);
        match &*def.effect {
            Effect::RollDie {
                count,
                sides,
                modifier,
                ..
            } => {
                assert_eq!(*sides, 6);
                assert_eq!(
                    *count,
                    crate::types::ability::QuantityExpr::Fixed { value: 2 }
                );
                assert!(modifier.is_none());
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.1: The single-die path is unchanged — "Roll a six-sided die"
    /// still lowers to `count: Fixed(1)` so back-compat with existing
    /// single-die cards holds.
    #[test]
    fn roll_a_six_sided_die_lowers_to_count_one() {
        let def = super::super::parse_effect_chain("Roll a six-sided die.", AbilityKind::Spell);
        match &*def.effect {
            Effect::RollDie { count, sides, .. } => {
                assert_eq!(*sides, 6);
                assert_eq!(
                    *count,
                    crate::types::ability::QuantityExpr::Fixed { value: 1 }
                );
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.1: The `dN` shorthand also supports a leading count: "Roll two
    /// d6" → `count: Fixed(2), sides: 6`.
    #[test]
    fn roll_two_d6_parses_to_roll_die_count_two() {
        let def = super::super::parse_effect_chain("Roll two d6.", AbilityKind::Spell);
        match &*def.effect {
            Effect::RollDie { count, sides, .. } => {
                assert_eq!(*sides, 6);
                assert_eq!(
                    *count,
                    crate::types::ability::QuantityExpr::Fixed { value: 2 }
                );
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.1 + CR 107.3a: "Roll X twelve-sided dice" binds the count to the
    /// announced `X`, lowering to `count: Variable("X"), sides: 12`.
    #[test]
    fn roll_x_twelve_sided_dice_parses_to_variable_count() {
        let def = super::super::parse_effect_chain("Roll X twelve-sided dice.", AbilityKind::Spell);
        match &*def.effect {
            Effect::RollDie { count, sides, .. } => {
                assert_eq!(*sides, 12);
                assert!(matches!(
                    count,
                    crate::types::ability::QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::Variable { .. }
                    }
                ));
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.2: open-ended upper bound "N+" parses to (min=N, max=u8::MAX) so
    /// modifier-boosted rolls beyond the printed face count still resolve to
    /// the intended branch.
    #[test]
    fn die_result_line_open_ended_upper_bound() {
        assert_eq!(
            super::try_parse_die_result_line("15+ | Draw two cards."),
            Some((15, u8::MAX, "Draw two cards."))
        );
        assert_eq!(
            super::try_parse_die_result_line("1\u{2014}9 | Draw a card."),
            Some((1, 9, "Draw a card."))
        );
        assert_eq!(
            super::try_parse_die_result_line("20 | Win the game."),
            Some((20, 20, "Win the game."))
        );
    }

    /// CR 701.13 + CR 701.24: A suffix-less "exile the top card[s]" (no "of
    /// <player>'s library" qualifier) is an implicit-controller, deterministic
    /// top-of-library exile — the "shuffle ..., then exile the top card" class
    /// (Urza, Lord High Artificer's {5}). It must NOT fall through to the
    /// generic `tag("exile ")` path (which produces a library-wide tutor).
    /// Covers period-terminated, plural-count, and EOF-terminated forms.
    #[test]
    fn exile_the_top_card_no_qualifier_parses_controller_exile_top() {
        let mut ctx = ParseContext::default();

        let singular = parse_exile_ast("exile the top card.", "exile the top card.", &mut ctx)
            .expect("'exile the top card.' should parse as ExileTop");
        assert!(
            matches!(
                singular,
                ZoneCounterImperativeAst::ExileTop {
                    player: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 1 },
                    face_down: false,
                }
            ),
            "expected ExileTop(Controller, 1), got {singular:?}"
        );

        let plural = parse_exile_ast(
            "exile the top two cards.",
            "exile the top two cards.",
            &mut ctx,
        )
        .expect("'exile the top two cards.' should parse as ExileTop");
        assert!(
            matches!(
                plural,
                ZoneCounterImperativeAst::ExileTop {
                    player: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 2 },
                    face_down: false,
                }
            ),
            "expected ExileTop(Controller, 2), got {plural:?}"
        );

        let eof = parse_exile_ast("exile the top card", "exile the top card", &mut ctx)
            .expect("'exile the top card' (no period) should parse as ExileTop");
        assert!(
            matches!(
                eof,
                ZoneCounterImperativeAst::ExileTop {
                    player: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 1 },
                    face_down: false,
                }
            ),
            "expected ExileTop(Controller, 1) at EOF, got {eof:?}"
        );
    }

    /// Regression: the suffix-less branch must NOT shadow the qualified "of
    /// <player>'s library" patterns, which run first and take precedence.
    /// "of target opponent's library" → opponent filter; "of each player's
    /// library" → ScopedPlayer. Both must survive the new fallback.
    #[test]
    fn exile_the_top_qualified_library_patterns_take_precedence() {
        let mut ctx = ParseContext::default();

        let opponent = parse_exile_ast(
            "exile the top card of target opponent's library.",
            "exile the top card of target opponent's library.",
            &mut ctx,
        )
        .expect("qualified opponent-library phrase should parse");
        assert!(
            matches!(
                opponent,
                ZoneCounterImperativeAst::ExileTop {
                    player: TargetFilter::Typed(_),
                    ..
                }
            ),
            "expected opponent-typed ExileTop, got {opponent:?}"
        );

        let each = parse_exile_ast(
            "exile the top card of each player's library.",
            "exile the top card of each player's library.",
            &mut ctx,
        )
        .expect("qualified each-player-library phrase should parse");
        assert!(
            matches!(
                each,
                ZoneCounterImperativeAst::ExileTop {
                    player: TargetFilter::ScopedPlayer,
                    ..
                }
            ),
            "expected ScopedPlayer ExileTop, got {each:?}"
        );
    }

    /// CR 609.7 + CR 615.2: A source-scoped prevent ("prevent all damage target
    /// instant or sorcery spell would deal this turn" — Dromoka's Command)
    /// collapses the recipient `target` to `Any` and carries the chosen source
    /// as `damage_source_filter = And[ParentTargetSlot{0}, <stack-spell leaf>]`.
    #[test]
    fn prevent_source_scoped_spell_yields_parent_target_slot_filter() {
        let effect = parse_prevent_effect(
            "Prevent all damage target instant or sorcery spell would deal this turn.",
        );
        let Effect::PreventDamage {
            amount,
            target,
            scope,
            damage_source_filter,
            ..
        } = effect
        else {
            panic!("expected PreventDamage, got {effect:?}");
        };
        assert_eq!(amount, PreventionAmount::All);
        assert_eq!(target, TargetFilter::Any);
        assert_eq!(scope, PreventionScope::AllDamage);
        let Some(TargetFilter::And { filters }) = damage_source_filter else {
            panic!("expected And source filter, got {damage_source_filter:?}");
        };
        // First leg: the cast-time-chosen-source sentinel at slot 0.
        assert!(
            filters
                .iter()
                .any(|f| matches!(f, TargetFilter::ParentTargetSlot { index: 0 })),
            "expected ParentTargetSlot {{ index: 0 }} leg, got {filters:?}"
        );
        // Sibling leg: the choosable instant-or-sorcery stack-spell filter.
        let source_leaf = filters
            .iter()
            .find(|f| !matches!(f, TargetFilter::ParentTargetSlot { .. }))
            .expect("source leaf present");
        assert!(
            is_stack_spell_leg(source_leaf)
                || matches!(source_leaf, TargetFilter::Or { filters } if filters.iter().any(is_stack_spell_leg)),
            "source leaf must scope to a stack spell, got {source_leaf:?}"
        );
    }

    /// NEGATIVE: a recipient-scoped prevent ("prevent the next 3 damage to
    /// target creature") must stay recipient-targeted — `target: Typed(creature)`
    /// and `damage_source_filter: None` — proving the source-scope branch does
    /// not divert the recipient form.
    #[test]
    fn prevent_recipient_scoped_creature_not_diverted_to_source() {
        let effect = parse_prevent_effect(
            "Prevent the next 3 damage that would be dealt to target creature this turn.",
        );
        let Effect::PreventDamage {
            amount,
            target,
            damage_source_filter,
            ..
        } = effect
        else {
            panic!("expected PreventDamage, got {effect:?}");
        };
        assert_eq!(amount, PreventionAmount::Next(3));
        assert!(
            typed_leg(&target).is_some_and(|tf| has_type(tf, TypeFilter::Creature)),
            "recipient target must stay Typed(creature), got {target:?}"
        );
        assert_eq!(
            damage_source_filter, None,
            "recipient prevent must not carry a source filter"
        );
    }
}
