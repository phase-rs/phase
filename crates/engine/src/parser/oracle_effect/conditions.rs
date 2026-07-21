use std::str::FromStr;

use crate::parser::oracle_nom::error::{OracleError, OracleResult};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::char;
use nom::character::complete::multispace0;
use nom::combinator::{all_consuming, map, opt, peek, value};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::super::oracle_nom::bridge::{nom_on_lower, nom_parse_lower};
use super::super::oracle_nom::condition::{
    inject_controller_you, parse_cast_using_teamwork_phrase, parse_spell_target_superlative_suffix,
    parse_you_put_onto_battlefield_this_way_clause, parse_zone_changed_this_way_clause,
};
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_quantity::{canonicalize_quantity_ref, parse_cda_quantity};
use super::super::oracle_target::{parse_target, parse_type_phrase, parse_zone_word};
use super::super::oracle_util::{parse_comparison_suffix, parse_subtype, TextPair};
use super::sequence::parse_dig_from_among;
use super::{parse_effect_chain, scan_contains_phrase, ParseContext};
use crate::parser::oracle_ir::ast::ContinuationAst;
use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, AdditionalCostOrigin, CastManaObjectScope,
    CastManaSpentMetric, CastVariantPaid, CoinFlipResult, Comparator, ControllerRef, CountScope,
    DamageChannel, DigSource, Duration, Effect, EffectOutcomeSignal, FilterProp, GuessOutcome,
    ObjectScope, ParsedCondition, PlayerScope, PtStat, PtValueScope, QuantityExpr, QuantityRef,
    StaticCondition, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::phase::Phase;
use crate::types::zones::Zone;

/// Wrap `cond` in `AbilityCondition::Not` when `negated` is true; otherwise
/// return it unchanged. Replaces the per-leaf `negated: bool` fields that
/// existed before Π-N — call sites that previously emitted `Variant { ...,
/// negated }` now construct the positive variant and pass through this helper.
fn maybe_negate(cond: AbilityCondition, negated: bool) -> AbilityCondition {
    if negated {
        AbilityCondition::Not {
            condition: Box::new(cond),
        }
    } else {
        cond
    }
}

/// CR 702.171b: The runtime filter matching the saddled designation. Shared by
/// the affirmative and negated `SourceIsSaddled` bridges in
/// `static_condition_to_ability_condition` so both compose the same
/// `SourceMatchesFilter { Typed([IsSaddled]) }` shape.
fn source_saddled_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter {
        properties: vec![FilterProp::IsSaddled],
        ..Default::default()
    })
}

fn parse_creature_subtype_or_list_prefix(lower: &str) -> Option<(TargetFilter, &str)> {
    crate::parser::oracle_static::parse_subtype_or_list_insensitive_prefix(lower)
}

fn parse_creature_subtype_card_tail(lower: &str) -> Option<(TargetFilter, &str)> {
    let (subtype_filter, rest) = parse_creature_subtype_or_list_prefix(lower)?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" creature card")
        .parse(rest)
        .ok()?;
    Some((subtype_filter, rest))
}

fn parse_creature_subtype_type_tail(lower: &str) -> Option<TargetFilter> {
    let (subtype_filter, rest) = parse_creature_subtype_or_list_prefix(lower)?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" creature").parse(rest).ok()?;
    if rest.is_empty() {
        Some(subtype_filter)
    } else {
        None
    }
}

fn remainder_after_optional_comma(s: &str) -> &str {
    opt(tag::<_, _, OracleError<'_>>(", "))
        .parse(s)
        .map(|(rest, _)| rest)
        .unwrap_or(s)
}

/// CR 205.3: True when a `TypeFilter` references a subtype anywhere in its
/// structure (directly, behind a `Non` negation, or inside an `AnyOf`
/// disjunction). Used to distinguish the present-target subtype condition
/// ("it's a Goblin") — owned by `TargetMatchesFilter` — from CoreType phrases
/// the explicit CoreType match already routes to `RevealedHasCardType`.
fn type_filter_references_subtype(filter: &TypeFilter) -> bool {
    match filter {
        TypeFilter::Subtype(_) => true,
        TypeFilter::Non(inner) => type_filter_references_subtype(inner),
        TypeFilter::AnyOf(inners) => inners.iter().any(type_filter_references_subtype),
        _ => false,
    }
}

pub(crate) fn split_leading_conditional(text: &str) -> Option<(String, String)> {
    let lower = text.to_lowercase();
    let prefix_rest = parse_leading_conditional_prefix(&lower)?;
    let condition_start_idx = lower.len() - prefix_rest.len();

    let mut paren_depth = 0u32;
    let mut in_quotes = false;
    let bytes = text.as_bytes();

    for (idx, ch) in text.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => paren_depth += 1,
            ')' if !in_quotes => paren_depth = paren_depth.saturating_sub(1),
            ',' if !in_quotes
                && paren_depth == 0
                && idx >= condition_start_idx
                && !is_thousands_separator_comma(bytes, idx)
                && !comma_inside_if_creature_subtype_list(&lower, idx) =>
            {
                let condition_text = text[..idx].trim().to_string();
                let rest = text[idx + 1..].trim();
                if !rest.is_empty() {
                    return Some((condition_text, rest.to_string()));
                }
            }
            _ => {}
        }
    }

    None
}

fn parse_leading_conditional_prefix(lower: &str) -> Option<&str> {
    alt((
        tag::<_, _, OracleError<'_>>("then, if "),
        tag("then if "),
        tag("if "),
        // CR 508.6 + CR 608.2c: temporal "during any turn <cond>, <body>" gate
        // (Neyali, Neriv, Boros Strike-Captain) — the head names a turn-scoped
        // condition rather than "if", but splits and gates identically.
        tag("during any turn "),
        tag("during a turn "),
    ))
    .parse(lower)
    .ok()
    .map(|(rest, _)| rest)
}

/// True if the comma at `idx` is part of a numeric thousands-separator
/// (digit before, exactly three digits after, no fourth digit). This mirrors
/// the grouping that [`oracle_nom::primitives::parse_digit_number`] consumes,
/// so the conditional splitter does not bisect numeric literals like
/// "1,000" (e.g. A Good Thing's "if you have 1,000 or more life, ...").
/// CR 205.3m: Commas inside "if it's a Kraken, Leviathan, ... creature card"
/// separate subtypes, not the condition from the effect body.
fn comma_inside_if_creature_subtype_list(lower: &str, comma_idx: usize) -> bool {
    let Some(after_prefix) = parse_leading_conditional_prefix(lower) else {
        return false;
    };
    // CR 205.3m: "if it's a Kraken, Leviathan, ... creature card" — the legacy
    // `it's a [subtype] card` intro form, whose subtype span ends at " card".
    if let Ok((after_intro, _)) =
        alt((tag::<_, _, OracleError<'_>>("it's a "), tag("it's an "))).parse(after_prefix)
    {
        if let Some((_, after_type)) = parse_creature_subtype_card_tail(after_intro) {
            let subtype_start = lower.len() - after_intro.len();
            let subtype_end = lower.len() - after_type.len();
            if (subtype_start..subtype_end).contains(&comma_idx) {
                return true;
            }
        }
    }
    // CR 205.3m + CR 608.2c: target-anaphoric "that creature is a Mutant, Ninja,
    // or Turtle" (Turtle Van). The subtype-list commas separate disjuncts, not
    // the condition from the effect body. Compose the same subject + tense +
    // article + `parse_type_phrase` span used by
    // `parse_target_type_membership_condition` and test whether the comma falls
    // inside the matched span. The trailing predicate has no " card" anchor, so
    // the span ends where `parse_type_phrase` stops consuming type words.
    target_anaphoric_subtype_span(lower, after_prefix)
        .is_some_and(|(start, end)| (start..end).contains(&comma_idx))
}

/// CR 205.3m: Find the byte span of the subtype-disjunction predicate in a
/// target-anaphoric "<subject> is a <subtype list>" condition. Returns the
/// `[start, end)` offsets (relative to `lower`) of the parsed type phrase, or
/// `None` when the text is not this shape. `after_prefix` is `lower` with the
/// leading conditional prefix ("then if " / "if ") already stripped.
fn target_anaphoric_subtype_span(lower: &str, after_prefix: &str) -> Option<(usize, usize)> {
    let (after_subject, _) = parse_target_demonstrative_subject(after_prefix).ok()?;
    let (after_tense, _) = parse_target_anaphoric_tense_polarity(after_subject).ok()?;
    let (after_article, _) = opt(nom_primitives::parse_article).parse(after_tense).ok()?;
    let (filter, remainder) = crate::parser::oracle_target::parse_type_phrase(after_article);
    if remainder.len() == after_article.len() || matches!(filter, TargetFilter::Any) {
        return None;
    }
    let start = lower.len() - after_article.len();
    let end = lower.len() - remainder.len();
    Some((start, end))
}

fn is_thousands_separator_comma(bytes: &[u8], idx: usize) -> bool {
    // Need at least one preceding digit.
    if idx == 0 || !bytes[idx - 1].is_ascii_digit() {
        return false;
    }
    // Exactly three digits must follow.
    for offset in 1..=3 {
        match bytes.get(idx + offset) {
            Some(b) if b.is_ascii_digit() => {}
            _ => return false,
        }
    }
    // A fourth following digit invalidates the grouping (e.g. "1,0000").
    !matches!(bytes.get(idx + 4), Some(b) if b.is_ascii_digit())
}

pub(super) fn strip_leading_instead(text: &str) -> String {
    let lower = text.to_lowercase();
    if let Some(((), rest)) = nom_on_lower(text, &lower, |input| {
        value((), tag("instead ")).parse(input)
    }) {
        rest.to_string()
    } else {
        text.to_string()
    }
}

pub(crate) fn strip_leading_general_conditional(
    text: &str,
    ctx: &mut ParseContext,
) -> (Option<AbilityCondition>, String) {
    // CR 508.4 + CR 608.2c + CR 701.42: this condition contains an internal
    // comma before its second conjunct ("are attacking, and you both own and
    // control them"). Peel it through the shared condition production before
    // the generic first-comma splitter, then leave the imperative body to the
    // normal effect-chain parser.
    if let Some((condition, _, _, partner, body)) = super::meld::strip_live_pair_conditional(text) {
        ctx.pending_meld_partner = Some(partner);
        return (Some(condition), body);
    }
    // CR 603.4: an inline `If` in an activated ability has its normal English
    // meaning. Parse the shared own/control pair grammar as an AbilityCondition;
    // this covers Hanweir Battlements and Urza, Lord Protector without a
    // card-name dispatch.
    if let Some((condition, _, _, partner, body)) = super::meld::strip_owned_pair_conditional(text)
    {
        ctx.pending_meld_partner = Some(partner);
        return (Some(condition), body);
    }
    if let Some((condition_fragment, body)) = split_leading_conditional(text) {
        let condition_lower = condition_fragment.to_lowercase();
        let cond_text = nom_on_lower(&condition_fragment, &condition_lower, |i| {
            value(
                (),
                alt((
                    tag::<_, _, OracleError<'_>>("then, if "),
                    tag("then if "),
                    tag("if "),
                    tag("during any turn "),
                    tag("during a turn "),
                )),
            )
            .parse(i)
        })
        .map(|((), rest)| rest)
        .unwrap_or(&condition_fragment)
        .trim();

        let body_lower = body.to_lowercase();
        let player_damage_scry = tag::<_, _, OracleError<'_>>("scry ")
            .parse(body_lower.as_str())
            .is_ok()
            .then(|| parse_previous_effect_player_damage_condition(cond_text))
            .flatten();
        let effect_discard_drain = tag::<_, _, OracleError<'_>>("each opponent loses ")
            .parse(body_lower.as_str())
            .is_ok()
            .then(|| parse_effect_discard_instant_or_sorcery_condition(cond_text))
            .flatten();

        if let Some(condition) = player_damage_scry
            .or(effect_discard_drain)
            .or_else(|| try_nom_condition_as_ability_condition(cond_text, ctx))
            .or_else(|| parse_condition_text(cond_text))
            .or_else(|| parse_control_count_as_ability_condition(cond_text))
            .or_else(|| parse_and_conjunction_condition(cond_text, ctx))
        {
            return (Some(condition), body);
        }
    }
    (None, text.to_string())
}

/// CR 608.2c: Parse a top-level `"<cond> and <cond> [and …]"` conjunction into
/// `AbilityCondition::And`. Last-resort fallback — invoked only after the whole
/// condition text failed to parse as a single condition, so it does not
/// mis-split a phrase whose internal `" and "` belongs to one conjunct. Each
/// conjunct is parsed by the same single-condition dispatchers (no self-
/// recursion); if any conjunct fails, the whole conjunction fails.
/// Motivating card: Coiling Rebirth ("if the gift was promised and that
/// creature isn't legendary, …").
fn parse_and_conjunction_condition(
    cond_text: &str,
    ctx: &mut ParseContext,
) -> Option<AbilityCondition> {
    // structural top-level conjunction decomposition (each conjunct is parsed by
    // nom-backed dispatchers below), not parsing dispatch.
    let parts: Vec<&str> = cond_text.split(" and ").collect(); // allow-noncombinator: structural conjunction split, conjuncts parsed by nom dispatchers
    if parts.len() < 2 {
        return None;
    }
    let mut conditions = Vec::with_capacity(parts.len());
    for part in parts {
        let part = part.trim();
        let condition = try_nom_condition_as_ability_condition(part, ctx)
            .or_else(|| parse_condition_text(part))
            .or_else(|| parse_control_count_as_ability_condition(part))?;
        conditions.push(condition);
    }
    Some(AbilityCondition::And { conditions })
}

/// CR 608.2c + CR 608.2d: Strip a leading `"If <condition>, "` head ONLY when the
/// typed-condition strip already returned `None` AND the body that would follow
/// the head begins with `"you may "`. The unrepresentable condition is dropped
/// to preserve the optional choice on the body — issue #2277.
///
/// Without this fallback the `If <X>, ` head stays on the text, so the downstream
/// `strip_optional_effect_prefix` (which requires `"you may "` at position 0)
/// never fires and the optional flag is lost (e.g. Amareth's "If it shares a card
/// type with that permanent, you may reveal that card and put it into your hand",
/// Tithe's "If target opponent controls more lands than you, you may search …").
/// Dropping the condition is acceptable because the upstream `Condition_If`
/// swallow detector still flags these patterns as condition-unsupported — we are
/// fixing the OPTIONAL representation here, NOT the condition. The condition
/// stays correctly unrepresented; the may-choice is now preserved.
///
/// TRADE-OFF (read before extending): this is a deliberate rules-fidelity
/// regression at the AST layer. The produced sub-ability carries `optional:
/// true` with `condition: null`, so if such a card were ever executed it would
/// offer the may-choice *ungated* — strictly more permissive than the printed
/// `If <gate>` text. This is sound ONLY because `Condition_If` keeps the card
/// `supported == false`, which holds it out of the engine's production-execution
/// set. When a typed recognizer is later added for one of these conditions, the
/// typed strip will match first, this fallback will stop firing for that shape,
/// and the card transitions to a fully gated+optional AST in a single step.
///
/// Mandatory-body guard: this function is a no-op when the body does NOT start
/// with `"you may "`. That prevents turning, e.g.,
/// `"If you control a creature, draw a card"` into an unconditional draw.
///
/// Callers must invoke this ONLY after the typed strip returned `None` — the
/// function performs no typed-condition recognition itself.
pub(crate) fn strip_unrecognized_conditional_head_when_body_optional(text: &str) -> String {
    let Some((_condition_fragment, body)) = split_leading_conditional(text) else {
        return text.to_string();
    };
    let body_lower = body.to_lowercase();
    if nom_on_lower(&body, &body_lower, |i| {
        value((), tag::<_, _, OracleError<'_>>("you may ")).parse(i)
    })
    .is_none()
    {
        return text.to_string();
    }
    body
}

/// CR 702.33b + CR 702.33c + CR 702.33f: Recognize quantified or per-variant
/// kicker gating in a leading `"if [subject] was kicked …, [body]"` clause.
/// Returns the typed `AbilityCondition` and the residual body when matched.
///
/// Patterns covered (subject is consumed permissively up to "was kicked"):
/// - "if it was kicked twice, [body]"             → min_count = 2
/// - "if it was kicked three times, [body]"       → min_count = N (English/digit)
/// - "if it was kicked with its {COST} kicker, [body]"
///   → parser records the printed cost; synthesis maps it to the card's
///   positional `KickerVariant` once kicker declarations are visible.
fn strip_quantified_kicker_conditional(
    text: &str,
    lower: &str,
) -> Option<(AbilityCondition, String)> {
    // CR 603.4: Locate the "was kicked" anchor. Subject (~/it/this creature/
    // this spell) is consumed permissively — the typed shape is determined
    // entirely by what follows.
    let after_if = tag::<_, _, OracleError<'_>>("if ")
        .parse(lower)
        .ok()
        .map(|(rest, _)| rest)?;
    let (after_kicked, _) = take_until::<_, _, OracleError<'_>>("was kicked")
        .parse(after_if)
        .ok()?;
    let (after_kicked, _) = tag::<_, _, OracleError<'_>>("was kicked")
        .parse(after_kicked)
        .ok()?;

    // Branch 1: "was kicked with its {COST} kicker, [body]" — per-variant.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" with its ").parse(after_kicked) {
        let cost_start = text.len() - rest.len();
        let (rest, cost_text) = take_until::<_, _, OracleError<'_>>(" kicker, ")
            .parse(rest)
            .ok()?;
        let (rest, _) = tag::<_, _, OracleError<'_>>(" kicker, ").parse(rest).ok()?;
        let offset = text.len() - rest.len();
        let cost =
            parse_kicker_condition_mana_cost(&text[cost_start..cost_start + cost_text.len()])?;
        return Some((
            AbilityCondition::additional_cost_paid_kicker_cost(cost),
            text[offset..].to_string(),
        ));
    }

    // Branch 2: "was kicked twice, [body]" → min_count = 2.
    // CR 702.33b/c: "twice" is the printed form for kicked-N=2.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" twice, ").parse(after_kicked) {
        let offset = text.len() - rest.len();
        return Some((
            AbilityCondition::additional_cost_paid_n_times(2),
            text[offset..].to_string(),
        ));
    }

    // Branch 3: "was kicked N times, [body]" → min_count = N. Accepts both
    // English number words (one through twenty) and digit forms via
    // `nom_primitives::parse_number`. "one time" is unprinted but harmless.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" ").parse(after_kicked) {
        if let Ok((rest, n)) = nom_primitives::parse_number(rest) {
            if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" times, ").parse(rest) {
                let offset = text.len() - rest.len();
                return Some((
                    AbilityCondition::additional_cost_paid_n_times(n),
                    text[offset..].to_string(),
                ));
            }
        }
    }

    None
}

fn parse_kicker_condition_mana_cost(cost_text: &str) -> Option<ManaCost> {
    nom_primitives::parse_mana_cost
        .parse(cost_text.trim())
        .ok()
        .map(|(_, cost)| cost)
}

fn strip_alternative_mana_cost_conditional<'a>(text: &'a str, lower: &str) -> Option<&'a str> {
    // CR 118.9 + CR 608.2c: "If the {COST} cost was paid, [body]" — alternative
    // cost rider on spells like Baleful Mastery.
    let ((), after_prefix) =
        nom_on_lower(text, lower, |input| value((), tag("if the ")).parse(input))?;
    let (after_cost, _) = nom_primitives::parse_mana_cost(after_prefix).ok()?;
    let after_cost_lower = after_cost.to_lowercase();
    nom_on_lower(after_cost, &after_cost_lower, |input| {
        value((), tag(" cost was paid, ")).parse(input)
    })
    .map(|((), rest)| rest)
}

/// CR 205.3m: The PREDICATE form "the revealed card is the chosen type" — the
/// copular clause "<subject> is <the chosen type>", distinct from the adjectival
/// "of the chosen type" suffix (`oracle_target`). Combinators only.
fn parse_revealed_card_is_chosen_type(i: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            tag("the revealed card"),
            tag(" is "),
            tag("the chosen type"),
        ),
    )
    .parse(i)
}

/// CR 205.3m + CR 607.2d + CR 608.2h: A leading-if that DECLARES the ability's
/// target and gates it on the source's chosen creature type — "If target
/// <filter> is the chosen type, <body>" (A Killer Among Us). This grammar is
/// novel: the target is introduced INSIDE the condition, not by the body. If the
/// gate were emitted with `subject_slot: None` and the target left in the
/// condition, the body would declare no object target and the buff would land on
/// an unbound `TriggeringSource` (dead for an activated ability). So the captured
/// target phrase is HOISTED into the body's first bare object pronoun: ordinary
/// target parsing then declares it as slot 0, and the gate binds to slot 0.
///
/// Returns `(TargetMatchesFilter{ IsChosenCreatureType, subject_slot: Some(0) },
/// rewritten_body)` or `None` when the leading clause is not this shape.
pub(super) fn strip_target_declaring_chosen_type_conditional(
    text: &str,
) -> Option<(AbilityCondition, String)> {
    let lower = text.to_lowercase();
    let ((), after_if) = nom_on_lower(text, &lower, |i| {
        value((), tag::<_, _, OracleError<'_>>("if ")).parse(i)
    })?;
    // Parse the target phrase introduced by the condition ("target attacking
    // creature token"). Must be a single typed filter to carry the chosen-type
    // leg and to declare a slot-0 target when hoisted into the body.
    let (filter, remainder) = parse_target(after_if);
    let TargetFilter::Typed(mut typed) = filter else {
        return None;
    };
    // The copula must be exactly " is the chosen type, " — the present-tense
    // creature-type gate, distinct from the "of the chosen type" adjectival
    // suffix — with a trailing comma separating it from the body.
    let rem_lower = remainder.to_lowercase();
    let ((), body) = nom_on_lower(remainder, &rem_lower, |i| {
        value((), tag::<_, _, OracleError<'_>>(" is the chosen type, ")).parse(i)
    })?;
    // The literal target phrase consumed by `parse_target` (original case).
    let target_phrase = &after_if[..after_if.len() - remainder.len()];
    let rewritten_body = super::replace_first_object_pronoun(body, target_phrase)?;
    // CR 205.3e + CR 205.3m + CR 702.73a: gate the declared target on the
    // source's chosen creature type.
    typed.properties.push(FilterProp::IsChosenCreatureType);
    Some((
        AbilityCondition::TargetMatchesFilter {
            filter: TargetFilter::Typed(typed),
            use_lki: false,
            subject_slot: Some(0),
        },
        rewritten_body,
    ))
}

pub(super) fn strip_additional_cost_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();

    // CR 608.2c + CR 601.2b: Inverted additional-cost "instead" — "[body] instead
    // if <additional cost was paid>." (Cinder Strike: "It deals 4 damage to that
    // creature instead if this spell's additional cost was paid."). The forward
    // word order ("if <cost was paid>, [body] instead") is handled by the
    // leading-"if" arms below; this catches the trailing-condition order, which
    // the line-level `strip_instead_clause` defers into the chain as intra-chain.
    {
        let tp = TextPair::new(text, &lower);
        if let Some((before, after)) = tp.rsplit_around(" instead if ") {
            let cond_text = after.original.trim().trim_end_matches('.').trim();
            if let Some(condition) = parse_additional_cost_instead_condition_fragment(cond_text) {
                return (Some(condition), before.original.trim().to_string());
            }
        }
    }

    if let Some((_, rest)) = nom_on_lower(text, &lower, |i| {
        value((), tag("if the gift wasn't promised, ")).parse(i)
    }) {
        return (
            Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::additional_cost_paid_any()),
            }),
            rest.to_string(),
        );
    }

    if alt((tag::<_, _, OracleError<'_>>("if "), tag("then if ")))
        .parse(lower.as_str())
        .is_ok()
    {
        if let Ok((_, (_, rest))) =
            nom_primitives::split_once_on(lower.as_str(), " wasn't kicked, ")
                .or_else(|_| nom_primitives::split_once_on(lower.as_str(), " wasn't bargained, "))
        {
            let offset = text.len() - rest.len();
            return (
                Some(AbilityCondition::Not {
                    condition: Box::new(AbilityCondition::additional_cost_paid_any()),
                }),
                text[offset..].to_string(),
            );
        }
    }

    // CR 608.2c + CR 205.3m: "if this spell's additional cost was paid AND the
    // revealed card is the chosen type, <body>" (Celestial Reunion) — a
    // conjunction of the additional-cost-paid leg and a predicate on the found
    // card. Must be tried before the bare "…was paid, " arm below (which requires
    // the comma immediately after "paid" and would not match this compound form).
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| {
        let (i, _) = tag("if this spell's additional cost was paid and ").parse(i)?;
        let (i, _) = parse_revealed_card_is_chosen_type(i)?;
        let (i, _) = tag(", ").parse(i)?;
        Ok((i, ()))
    }) {
        return (
            Some(AbilityCondition::And {
                conditions: vec![
                    AbilityCondition::additional_cost_paid_any(),
                    AbilityCondition::TargetMatchesFilter {
                        filter: TypedFilter::creature()
                            .properties(vec![FilterProp::IsChosenCreatureType])
                            .into(),
                        use_lki: false,
                        subject_slot: None,
                    },
                ],
            }),
            rest.to_string(),
        );
    }

    let mut alternative_mana_cost_conditional = false;

    let body = if let Some(((), rest)) = nom_on_lower(text, &lower, |input| {
        value(
            (),
            alt((
                tag("if this spell's additional cost was paid, "),
                tag("if evidence was collected, "),
                tag("if the gift was promised, "),
            )),
        )
        .parse(input)
    }) {
        Some(rest.to_string())
    } else if let Some(rest) = strip_alternative_mana_cost_conditional(text, &lower) {
        alternative_mana_cost_conditional = true;
        Some(rest.to_string())
    } else if tag::<_, _, OracleError<'_>>("if ")
        .parse(lower.as_str())
        .is_ok()
    {
        // CR 702.33b/c + CR 702.33f: Quantified / per-variant kicker gating.
        // Try "kicked twice/N times" and "kicked with its {COST} kicker"
        // BEFORE the plain "was kicked" split so the more specific phrasings
        // take priority. Returns early with the typed condition.
        if let Some((cond, rest)) = strip_quantified_kicker_conditional(text, &lower) {
            return (Some(cond), rest);
        }
        // CR 601.2b/f: "if this spell was cast using teamwork, [body]" gates the
        // body specifically on the Teamwork additional-cost payment (origin
        // Teamwork), so a different optional/imposed additional cost on the same
        // spell does not satisfy it. The leading-"instead" form folds to
        // `AdditionalCostPaidInstead` via the shared `instead` handling below, so
        // only the non-instead form is peeled here.
        if let Ok((_, (_, rest))) =
            nom_primitives::split_once_on(lower.as_str(), " was cast using teamwork, ")
        {
            let is_instead = tag::<_, _, OracleError<'_>>("instead")
                .parse(rest.trim_start())
                .is_ok();
            if !is_instead {
                let offset = text.len() - rest.len();
                return (
                    Some(AbilityCondition::additional_cost_paid_origin(
                        AdditionalCostOrigin::Teamwork,
                    )),
                    text[offset..].to_string(),
                );
            }
        }
        nom_primitives::split_once_on(lower.as_str(), " was kicked, ")
            .or_else(|_| nom_primitives::split_once_on(lower.as_str(), " was bargained, "))
            .or_else(|_| nom_primitives::split_once_on(lower.as_str(), " was beheld, "))
            // CR 601.2b/f: Teamwork is an optional additional cast cost; "if this
            // spell was cast using teamwork" gates the body on the same
            // `additional_cost_paid` flag as kicker/bargain. The leading-"instead"
            // form (Cruel Alliance, Too Evil to Stay Dead) is folded to
            // `AdditionalCostPaidInstead` by the shared `instead` handling below.
            .or_else(|_| {
                nom_primitives::split_once_on(lower.as_str(), " was cast using teamwork, ")
            })
            .ok()
            .map(|(_, (_, rest))| {
                let offset = text.len() - rest.len();
                text[offset..].to_string()
            })
    } else {
        None
    };

    let tp = TextPair::new(text, &lower);
    if body.is_none() && scan_contains_phrase(&lower, "sneak cost was paid") {
        if let Some(after) = tp.strip_after("instead ") {
            return (
                Some(AbilityCondition::CastVariantPaidInstead {
                    variant: CastVariantPaid::Sneak,
                }),
                after.original.to_string(),
            );
        }
        // CR 702.190a: "if this spell's sneak cost was paid, [effect]" — non-"instead"
        // variant that gates a sub-ability on sneak payment.
        if let Some(after) = tp.strip_after("sneak cost was paid, ") {
            return (
                Some(AbilityCondition::CastVariantPaid {
                    variant: CastVariantPaid::Sneak,
                    subject: ObjectScope::Source,
                }),
                after.original.to_string(),
            );
        }
    }
    if body.is_none() && scan_contains_phrase(&lower, "ninjutsu cost was paid") {
        if let Some(after) = tp.strip_after("instead ") {
            return (
                Some(AbilityCondition::CastVariantPaidInstead {
                    variant: CastVariantPaid::Ninjutsu,
                }),
                after.original.to_string(),
            );
        }
        // CR 702.49: "if its ninjutsu cost was paid, [effect]" — non-"instead"
        // variant that gates a sub-ability on ninjutsu payment.
        if let Some(after) = tp.strip_after("ninjutsu cost was paid, ") {
            return (
                Some(AbilityCondition::CastVariantPaid {
                    variant: CastVariantPaid::Ninjutsu,
                    subject: ObjectScope::Source,
                }),
                after.original.to_string(),
            );
        }
    }
    if body.is_none() && scan_contains_phrase(&lower, "surge cost was paid") {
        if let Some(after) = tp.strip_after("instead ") {
            return (
                Some(AbilityCondition::CastVariantPaidInstead {
                    variant: CastVariantPaid::Surge,
                }),
                after.original.to_string(),
            );
        }
        // CR 702.117a: "if its surge cost was paid, [effect]" — non-"instead"
        // variant that gates a sub-ability on surge payment.
        if let Some(after) = tp.strip_after("surge cost was paid, ") {
            return (
                Some(AbilityCondition::CastVariantPaid {
                    variant: CastVariantPaid::Surge,
                    subject: ObjectScope::Source,
                }),
                after.original.to_string(),
            );
        }
    }
    if body.is_none() && scan_contains_phrase(&lower, "spectacle cost was paid") {
        if let Some(after) = tp.strip_after("instead ") {
            return (
                Some(AbilityCondition::CastVariantPaidInstead {
                    variant: CastVariantPaid::Spectacle,
                }),
                after.original.to_string(),
            );
        }
        // CR 702.137a: "if its spectacle cost was paid, [effect]" — non-"instead"
        // variant that gates a sub-ability on spectacle payment.
        if let Some(after) = tp.strip_after("spectacle cost was paid, ") {
            return (
                Some(AbilityCondition::CastVariantPaid {
                    variant: CastVariantPaid::Spectacle,
                    subject: ObjectScope::Source,
                }),
                after.original.to_string(),
            );
        }
    }
    if body.is_none() && scan_contains_phrase(&lower, "prowl cost was paid") {
        if let Some(after) = tp.strip_after("instead ") {
            return (
                Some(AbilityCondition::CastVariantPaidInstead {
                    variant: CastVariantPaid::Prowl,
                }),
                after.original.to_string(),
            );
        }
        // CR 702.76a: "if its prowl cost was paid, [effect]" — non-"instead"
        // variant that gates a sub-ability on prowl payment.
        if let Some(after) = tp.strip_after("prowl cost was paid, ") {
            return (
                Some(AbilityCondition::CastVariantPaid {
                    variant: CastVariantPaid::Prowl,
                    subject: ObjectScope::Source,
                }),
                after.original.to_string(),
            );
        }
    }

    match body {
        Some(body) => {
            let body_lower = body.to_lowercase();
            let (body, condition) = if let Some(stripped) = body_lower
                .strip_suffix(" instead")
                .map(|_| &body[..body.len() - " instead".len()])
            {
                (
                    stripped.to_string(),
                    AbilityCondition::AdditionalCostPaidInstead,
                )
            } else {
                let stripped = strip_leading_instead(&body);
                if stripped.len() < body.len() {
                    (stripped, AbilityCondition::AdditionalCostPaidInstead)
                } else if alternative_mana_cost_conditional {
                    (body, AbilityCondition::AlternativeManaCostPaid)
                } else {
                    (body, AbilityCondition::additional_cost_paid_any())
                }
            };
            (Some(condition), body)
        }
        None => (None, text.to_string()),
    }
}

/// Strip optional punctuation/space between a parsed reflexive clause and its body.
fn strip_reflexive_conditional_body_separator(input: &str) -> &str {
    opt(alt((
        tag::<_, _, OracleError<'_>>(", "),
        tag(". "),
        tag(" "),
    )))
    .parse(input)
    .map(|(rest, _)| rest)
    .unwrap_or(input)
}

/// CR 608.2d: Strip a leading "if they guessed wrong/right, " head into the
/// typed `EffectOutcomeSignal::Guessed { outcome }` outcome condition for an
/// `Effect::OpponentGuess` branch. Both polarities are positive tests: "wrong"
/// → `outcome: GuessOutcome::Incorrect`, "right" → `outcome: GuessOutcome::Correct`.
pub(super) fn strip_guess_outcome_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_ascii_lowercase();
    let parsed: Result<(&str, GuessOutcome), nom::Err<()>> = alt((
        value(
            GuessOutcome::Incorrect,
            tag::<_, _, ()>("if they guessed wrong, "),
        ),
        value(
            GuessOutcome::Incorrect,
            tag::<_, _, ()>("if they guess wrong, "),
        ),
        value(
            GuessOutcome::Correct,
            tag::<_, _, ()>("if they guessed right, "),
        ),
        value(
            GuessOutcome::Correct,
            tag::<_, _, ()>("if they guess right, "),
        ),
    ))
    .parse(lower.as_str());
    if let Ok((rest, outcome)) = parsed {
        let consumed = lower.len() - rest.len();
        let body = text[consumed..].trim_start().to_string();
        return (
            Some(AbilityCondition::EffectOutcome {
                signal: EffectOutcomeSignal::Guessed { outcome },
            }),
            body,
        );
    }
    (None, text.to_string())
}

pub(super) fn strip_if_you_do_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();

    // CR 603.12 + CR 608.2c: strip a leading reflexive-conditional connector
    // ("if you do, ", "when you do, ", "if that player doesn't, ", ...) and
    // return the corresponding AbilityCondition. Delegates to the shared
    // `parse_reflexive_conditional_connector` combinator in `oracle_nom::condition`
    // so the connector set stays in lockstep with the sequence-splitter's
    // sticky-detection consumer.
    if let Some((condition, rest)) = nom_on_lower(text, &lower, |input| {
        crate::parser::oracle_nom::condition::parse_reflexive_conditional_connector(input)
    }) {
        return (Some(condition), rest.to_string());
    }

    // CR 400.7 + CR 608.2c + CR 303.4f + CR 301.5b: "if a[n] [type] (is|was)
    // [verb] this way, [body]" — delegate to the shared
    // `parse_zone_changed_this_way_clause` combinator in `oracle_nom::condition`.
    // The combinator covers past + present tense, single-word imperatives
    // (destroyed/exiled/sacrificed/returned/discarded/milled/countered) AND
    // the multi-word "put onto the battlefield" verb, with subtype filters
    // (Aura/Equipment/...) via `parse_type_phrase`. Replaces the prior
    // hand-rolled past-tense / single-word / top-level-type-only matcher.
    if let Ok((rest, prefix)) = alt((
        value("if ", tag::<_, _, OracleError<'_>>("if ")),
        value("when ", tag("when ")),
    ))
    .parse(lower.as_str())
    {
        use crate::parser::oracle_nom::condition as nom_cond;
        // CR 400.7 + CR 608.2c: active-voice reflexive gates that resolve to a
        // full `AbilityCondition` (not just a type filter). These run under BOTH
        // "if" and "when" prefixes — "If you put an artifact onto the battlefield
        // this way" (Oviya), "If you put a Town card into your hand this way"
        // (Town Greeter), "If you control a Squirrel or returned a Squirrel card
        // to your hand this way" (Cache Grab), "If you draw one or more cards
        // this way" (Transcendent Archaic) — where previously only "when" reached
        // the active combinators.
        if let Ok((after_clause, condition)) =
            nom_cond::parse_you_control_or_returned_this_way_condition(rest)
                .or_else(|_| nom_cond::parse_you_put_into_hand_this_way_condition(rest))
                .or_else(|_| nom_cond::parse_you_put_counters_on_type_this_way_condition(rest))
                .or_else(|_| nom_cond::parse_you_draw_this_way_condition(rest))
        {
            let body_lower = strip_reflexive_conditional_body_separator(after_clause);
            let offset = text.len() - body_lower.len();
            return (Some(condition), text[offset..].to_string());
        }
        // CR 603.12 + CR 608.2c: active-voice "you put [type] onto the battlefield
        // this way" — also hoisted to run under "if" (Oviya, Spelunking).
        if let Ok((after_clause, (filter, _negated))) =
            nom_cond::parse_you_put_onto_battlefield_this_way_clause(rest)
        {
            let body_lower = strip_reflexive_conditional_body_separator(after_clause);
            let offset = text.len() - body_lower.len();
            return (
                Some(AbilityCondition::ZoneChangedThisWay { filter }),
                text[offset..].to_string(),
            );
        }
        if let Ok((after_clause, (filter, _negated))) =
            nom_cond::parse_zone_changed_this_way_clause(rest)
        {
            let body_lower = strip_reflexive_conditional_body_separator(after_clause);
            let offset = text.len() - body_lower.len();
            return (
                Some(AbilityCondition::ZoneChangedThisWay { filter }),
                text[offset..].to_string(),
            );
        }
        if prefix == "when " {
            // CR 603.12 + CR 701.9a: "when you discard a card this way, [body]" —
            // the reflexive gate created by a preceding "discard a card"
            // instruction (Talion's Messenger, The Ancient One). The discard's
            // hand → graveyard move publishes the card into
            // `state.last_zone_changed_ids`, which `ZoneChangedThisWay` checks.
            if let Ok((after_clause, (filter, _negated))) =
                crate::parser::oracle_nom::condition::parse_you_discard_this_way_clause(rest)
            {
                let body_lower = strip_reflexive_conditional_body_separator(after_clause);
                let offset = text.len() - body_lower.len();
                return (
                    Some(AbilityCondition::ZoneChangedThisWay { filter }),
                    text[offset..].to_string(),
                );
            }
            // CR 603.12 + CR 701.21a: "when you sacrifice one or more X this way,
            // [body]" — the reflexive gate created by a preceding "sacrifice
            // [quantifier] X" instruction (Nyssa of Traken). The sacrifice's
            // battlefield → graveyard move publishes the permanents into
            // `state.last_zone_changed_ids`, which `ZoneChangedThisWay` checks.
            if let Ok((after_clause, (filter, _negated))) =
                crate::parser::oracle_nom::condition::parse_you_sacrifice_this_way_clause(rest)
            {
                let body_lower = strip_reflexive_conditional_body_separator(after_clause);
                let offset = text.len() - body_lower.len();
                return (
                    Some(AbilityCondition::ZoneChangedThisWay { filter }),
                    text[offset..].to_string(),
                );
            }
        }
    }
    (None, text.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UnlessSuffixStrip {
    Absent,
    Parsed(AbilityCondition),
    Unrecognized { rider: String },
}

pub(super) fn strip_unless_entered_suffix(
    text: &str,
    ctx: &mut ParseContext,
) -> (UnlessSuffixStrip, String) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    for pattern in &[
        "unless ~ entered this turn",
        "unless this creature entered this turn",
    ] {
        if let Some((before, _)) = tp.split_around(pattern) {
            return (
                UnlessSuffixStrip::Parsed(AbilityCondition::Not {
                    condition: Box::new(AbilityCondition::SourceEnteredThisTurn),
                }),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }
    if let Some((effect_part, condition_part)) = lower.rsplit_once(" unless ") {
        let condition_text = condition_part.trim_end_matches('.');
        if let Some(cond) = try_nom_condition_as_unless(condition_text, ctx) {
            let effect_text = text[..effect_part.len()].trim().to_string();
            return (UnlessSuffixStrip::Parsed(cond), effect_text);
        }
        return (
            UnlessSuffixStrip::Unrecognized {
                rider: condition_text.to_string(),
            },
            text.to_string(),
        );
    }
    (UnlessSuffixStrip::Absent, text.to_string())
}

/// CR 607.2a + CR 608.2c: "unless it has the same name as another card exiled
/// this way" on an optional put-to-hand rider (Tainted Pact).
pub(super) fn strip_unless_shares_name_with_other_exiled_this_way(
    text: &str,
) -> Option<(String, AbilityCondition)> {
    const SUFFIX: &str = " unless it has the same name as another card exiled this way";
    let lower = text.to_lowercase();
    let (_, before) = all_consuming(terminated(
        take_until::<_, _, OracleError<'_>>(SUFFIX),
        tag::<_, _, OracleError<'_>>(SUFFIX),
    ))
    .parse(lower.as_str())
    .ok()?;
    let trimmed = text[..before.len()].trim_end().to_string();
    Some((
        trimmed,
        AbilityCondition::Not {
            condition: Box::new(AbilityCondition::TargetSharesNameWithOtherExiledThisWay {
                target: TargetFilter::ParentTarget,
            }),
        },
    ))
}

fn try_nom_condition_as_unless(
    condition_text: &str,
    ctx: &mut ParseContext,
) -> Option<AbilityCondition> {
    use crate::parser::oracle_nom::condition::parse_inner_condition;

    let (rest, inner) = parse_inner_condition(condition_text).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    let negated = StaticCondition::Not {
        condition: Box::new(inner),
    };
    static_condition_to_ability_condition(&negated, ctx)
}

pub(super) fn strip_cast_from_zone_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    // CR 603.4 + CR 601.2: Negated effect-level form — "if you didn't cast it
    // from your hand/graveyard/exile" → ¬(cast ∧ origin=X). This is the OPPOSITE
    // presupposition from the "anywhere other than X" arm below: a copy or a
    // reanimated object (`cast_from_zone == None`) correctly evaluates TRUE here,
    // so this arm stays a BARE `Not` with NO `∃cast` conjunct (do NOT add the
    // BB-FU4 And-wrap). The canonical "didn't cast it" cards (Phage the
    // Untouchable, Epochrasite) actually reach the engine via
    // `TriggerCondition::WasCast` (trigger intervening-if) and
    // `ReplacementCondition` (enters-with) respectively — not this effect-level
    // `AbilityCondition` arm, which remains for any genuine effect-level rider of
    // this shape. MUST precede the positive form to avoid partial prefix matching.
    if let Some((zone, rest)) = nom_on_lower(text, &lower, |input| {
        // Decompose into prefix + zone: the prefix accepts both the ASCII
        // (`didn't`) and curly (`didn’t`, U+2019) apostrophe used by Scryfall
        // printings; the zone is the shared owner-specific/exile alternation.
        let (input, _) = alt((
            tag("if you didn't cast it from "),
            tag("if you didn’t cast it from "),
        ))
        .parse(input)?;
        alt((
            value(Zone::Hand, tag("your hand")),
            value(Zone::Graveyard, tag("your graveyard")),
            value(Zone::Exile, tag("exile")),
        ))
        .parse(input)
    }) {
        let rest = remainder_after_optional_comma(rest);
        return (
            Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::WasCast { zone: Some(zone) }),
            }),
            rest.to_string(),
        );
    }
    // CR 603.4 + CR 601.2: Positive form — "if you cast it from your hand/exile/graveyard".
    if let Some((zone, rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            value(Zone::Hand, tag("if you cast it from your hand")),
            value(Zone::Exile, tag("if you cast it from exile")),
            value(Zone::Graveyard, tag("if you cast it from your graveyard")),
        ))
        .parse(input)
    }) {
        let rest = remainder_after_optional_comma(rest);
        return (
            Some(AbilityCondition::WasCast { zone: Some(zone) }),
            rest.to_string(),
        );
    }
    // CR 603.4 + CR 601.2a + CR 400.7: Passive-voice resolve-time cast-origin
    // rider — "[then ]if this spell was cast from [anywhere other than] {your
    // hand|your graveyard|exile}" (Antiquities on the Loose, Otterball Antics
    // flashback tokens). Subject is the RESOLVING SPELL (SELF_REF_PARSE_ONLY, not
    // ~-normalized). The leading "if " is retained in the text reaching this
    // stripper (existing arms embed it, :1029/:1051), so it is consumed here.
    // opt("then ") guards the SequentialSibling splitter leaving "Then" behind.
    // The negated "anywhere other than <zone>" branch MUST precede the bare
    // <zone> branch (prefix nesting).
    if let Some((condition, rest)) = nom_on_lower(text, &lower, |input| {
        let (input, _) = opt(tag("then ")).parse(input)?;
        let (input, _) = tag("if this spell was cast from ").parse(input)?;
        let zone = || {
            alt((
                value(Zone::Hand, tag("your hand")),
                value(Zone::Graveyard, tag("your graveyard")),
                value(Zone::Exile, tag("exile")),
            ))
        };
        alt((
            // CR 601.2a + CR 707.10 (resolved in BB-FU4): "was cast from anywhere
            // other than X" is a positive-cast presupposition — it asserts
            // (∃cast ∧ origin≠X), NOT merely origin≠X. Encode both conjuncts as
            // `And[WasCast{None}, Not(WasCast{Some(X)})]` so a spell COPY (which has
            // `cast_from_zone == None` per CR 707.10 — a copy isn't cast — and per
            // CR 400.7 has no cast provenance) short-circuits the `WasCast{None}`
            // conjunct to false instead of over-firing via the old `Not(false)=true`.
            // The `∃cast` conjunct is applied ONLY to this "anywhere other than"
            // producer; the "you didn't cast it from X" arm (:1049) is the opposite
            // presupposition and deliberately stays a bare `Not` (see its comment).
            map(preceded(tag("anywhere other than "), zone()), |z| {
                AbilityCondition::And {
                    conditions: vec![
                        AbilityCondition::WasCast { zone: None },
                        AbilityCondition::Not {
                            condition: Box::new(AbilityCondition::WasCast { zone: Some(z) }),
                        },
                    ],
                }
            }),
            map(zone(), |z| AbilityCondition::WasCast { zone: Some(z) }),
        ))
        .parse(input)
    }) {
        let rest = remainder_after_optional_comma(rest);
        return (Some(condition), rest.to_string());
    }
    (None, text.to_string())
}

fn type_filter_to_core_type(tf: &TypeFilter) -> Option<CoreType> {
    match tf {
        TypeFilter::Creature => Some(CoreType::Creature),
        TypeFilter::Land => Some(CoreType::Land),
        TypeFilter::Artifact => Some(CoreType::Artifact),
        TypeFilter::Enchantment => Some(CoreType::Enchantment),
        TypeFilter::Instant => Some(CoreType::Instant),
        TypeFilter::Sorcery => Some(CoreType::Sorcery),
        TypeFilter::Planeswalker => Some(CoreType::Planeswalker),
        TypeFilter::Battle => Some(CoreType::Battle),
        // CR 308.1: Kindred maps to its core type.
        TypeFilter::Kindred => Some(CoreType::Kindred),
        _ => None,
    }
}

/// Inverse of [`type_filter_to_core_type`]: map a `CoreType` to the `TypeFilter`
/// the engine uses to gate a present-target filter. Total over the card-type
/// `CoreType` set; mirrors the explicit arms of `type_filter_to_core_type`.
pub(super) fn core_type_to_type_filter(core: CoreType) -> TypeFilter {
    match core {
        CoreType::Creature => TypeFilter::Creature,
        CoreType::Land => TypeFilter::Land,
        CoreType::Artifact => TypeFilter::Artifact,
        CoreType::Enchantment => TypeFilter::Enchantment,
        CoreType::Instant => TypeFilter::Instant,
        CoreType::Sorcery => TypeFilter::Sorcery,
        CoreType::Planeswalker => TypeFilter::Planeswalker,
        CoreType::Battle => TypeFilter::Battle,
        // CR 308.1: Kindred maps to its dedicated type filter.
        CoreType::Kindred => TypeFilter::Kindred,
        // CR 110.1: any remaining card type maps to a Subtype-free typed filter
        // by its name; `Tribal`/`Plane`/etc. fall here and are gated by name.
        other => TypeFilter::Subtype(format!("{other:?}")),
    }
}

/// CR 608.2c + CR 109.2: Convert a *reveal-context* card-type condition into a
/// *present-target* card-type condition.
///
/// `strip_card_type_conditional` emits `RevealedHasCardType{Creature}` for the
/// anaphoric head "if it's a creature, it ..." because most users of that helper
/// gate on a card revealed/zone-changed by an earlier instruction (a reveal
/// context). But the damage-spell class (Disintegrate / Carbonize: "deals N
/// damage to any target. If it's a creature, it can't be regenerated this turn,
/// and if it would die this turn, exile it instead.") has NO revealed subject —
/// the "it" is the spell's chosen damage *target* on the battlefield. Under
/// CR 109.2 an unqualified card-type description (no "card"/"spell"/zone) refers
/// to a permanent of that type, i.e. the targeted object. Carrying the raw
/// `RevealedHasCardType{Creature}` would evaluate ALWAYS-FALSE here (no revealed
/// id → `evaluate_condition` returns false), silently dropping the riders even
/// for a creature target. `TargetMatchesFilter{Typed(creature), use_lki:true}`
/// instead evaluates against the ability's first object target (the damage
/// target): true for a creature, false for a planeswalker/player.
///
/// CR 608.2c (later text — "if it's a creature, it ..." — modifies the earlier
/// "deals N damage to any target") + CR 109.2 (the object/anaphor "it" is a
/// permanent of the named type). This is the same conversion the "permanent" arm
/// of `strip_card_type_conditional` (~786) and `parse_if_it_isnt_a_*` (~3109)
/// already perform for non-`CoreType` gates; here we extend it to the `CoreType`
/// gate that a damage anaphor produces.
///
/// Returns `None` for any condition that is not a single-`CoreType`
/// `RevealedHasCardType` with no additional/subtype filter (so non-type gates
/// fall through unchanged at the call site).
pub(super) fn card_type_condition_as_target_match(
    cond: &AbilityCondition,
) -> Option<AbilityCondition> {
    let AbilityCondition::RevealedHasCardType {
        card_types,
        additional_filter: None,
        subtype_filter: None,
    } = cond
    else {
        return None;
    };
    let [core] = card_types.as_slice() else {
        return None;
    };
    Some(AbilityCondition::TargetMatchesFilter {
        filter: TargetFilter::Typed(TypedFilter::new(core_type_to_type_filter(*core))),
        // CR 400.7: the damage target may already be dying/changing zones when the
        // riders evaluate; use last-known information so a creature that is being
        // destroyed this way still matches.
        use_lki: true,
        subject_slot: None,
    })
}

/// CR 608.2c: "If an instant or sorcery card is revealed this way, ..."
/// (Delver of Secrets class) — gates a sub_ability on the last revealed card's type.
fn parse_if_revealed_card_type_conditional(text: &str) -> Option<(AbilityCondition, String)> {
    let lower = text.to_lowercase();
    let (type_filters, remainder) = nom_on_lower(text, &lower, |input| {
        let (rest, _) = alt((
            tag::<_, _, OracleError<'_>>("if an "),
            tag::<_, _, OracleError<'_>>("if a "),
        ))
        .parse(input)?;
        let (rest, type_filters) = nom_quantity::parse_type_filter_list(rest)?;
        let (rest, _) = tag::<_, _, OracleError<'_>>(" card is revealed this way").parse(rest)?;
        Ok((rest, type_filters))
    })?;
    let core_types: Vec<CoreType> = type_filters
        .iter()
        .filter_map(type_filter_to_core_type)
        .collect();
    if core_types.is_empty() {
        return None;
    }
    Some((
        AbilityCondition::RevealedHasCardType {
            card_types: core_types,
            additional_filter: None,
            subtype_filter: None,
        },
        remainder_after_optional_comma(remainder).to_string(),
    ))
}

/// CR 608.2c + CR 406.6: "If the exiled card is a [type] card, ..." — gates a
/// sub_ability / repeat-process loop on the type of the card just moved by the
/// preceding step. `RevealedHasCardType` falls back to `last_zone_changed_ids`
/// when no reveal occurred, so the demonstrative "the exiled card" resolves to
/// the card the preceding `ChangeZone` step moved this resolution (Sin, Spira's
/// Punishment exiles a graveyard card, then this gates the repeat on its type).
fn parse_if_exiled_card_type_conditional(text: &str) -> Option<(AbilityCondition, String)> {
    let lower = text.to_lowercase();
    let (type_filters, remainder) = nom_on_lower(text, &lower, |input| {
        let (rest, _) = tag::<_, _, OracleError<'_>>("if the exiled card is a ").parse(input)?;
        let (rest, type_filters) = nom_quantity::parse_type_filter_list(rest)?;
        let (rest, _) = tag::<_, _, OracleError<'_>>(" card").parse(rest)?;
        Ok((rest, type_filters))
    })?;
    let core_types: Vec<CoreType> = type_filters
        .iter()
        .filter_map(type_filter_to_core_type)
        .collect();
    if core_types.is_empty() {
        return None;
    }
    let mut ctx = ParseContext::default();
    let (remainder, additional_filter) = parse_revealed_card_gate_suffix(remainder, &mut ctx);
    Some((
        AbilityCondition::RevealedHasCardType {
            card_types: core_types,
            additional_filter,
            subtype_filter: None,
        },
        remainder_after_optional_comma(remainder).to_string(),
    ))
}

/// CR 202.3 + CR 205.3m: Optional property suffix after a revealed-card type gate
/// (`" with mana value N or less"`, `" of the chosen type"`). Shared by the
/// exiled-card demonstrative gate and the `"it's a/an … card"` gate body.
fn parse_revealed_card_gate_suffix<'a>(
    after_type: &'a str,
    ctx: &mut ParseContext,
) -> (&'a str, Option<FilterProp>) {
    if let Ok((rest_after_chosen, _)) =
        tag::<_, _, OracleError<'_>>(" of the chosen type").parse(after_type)
    {
        return (rest_after_chosen, Some(FilterProp::IsChosenCreatureType));
    }
    if let Some((prop, consumed)) =
        crate::parser::oracle_target::parse_mana_value_suffix(after_type.trim_start(), ctx)
    {
        let leading_ws = after_type.len() - after_type.trim_start().len();
        return (&after_type[leading_ws + consumed..], Some(prop));
    }
    (after_type, None)
}

/// CR 608.2c: Shared body for `"[non]<type> card[...suffix]"` gates after the
/// `"it's a/an "` or `"if it's a/an "` prefix. Returns the condition and the
/// unconsumed remainder (effect body for leading-if forms).
fn parse_its_a_card_type_gate_body<'a>(
    rest: &'a str,
    negated: bool,
    ctx: &mut ParseContext,
) -> Option<(AbilityCondition, &'a str)> {
    // CR 608.2c: "card of the chosen type" (Gathering Stone) — the chosen
    // creature type can match any card whose type line includes it.
    if let Ok((after_chosen, _)) =
        tag::<_, _, OracleError<'_>>("card of the chosen type").parse(rest)
    {
        return Some((
            maybe_negate(
                AbilityCondition::RevealedHasCardType {
                    card_types: vec![],
                    additional_filter: Some(FilterProp::IsChosenCreatureType),
                    subtype_filter: None,
                },
                negated,
            ),
            after_chosen,
        ));
    }
    // CR 205.3m: Multi-subtype creature gates ("Kraken, Leviathan, Octopus,
    // or Serpent creature card") must not collapse to bare CoreType::Creature.
    if let Some((subtype_filter, after_type)) = parse_creature_subtype_card_tail(rest) {
        return Some((
            maybe_negate(
                AbilityCondition::RevealedHasCardType {
                    card_types: vec![CoreType::Creature],
                    additional_filter: None,
                    subtype_filter: Some(Box::new(subtype_filter)),
                },
                negated,
            ),
            after_type,
        ));
    }
    let (after_type, type_str) = alt((
        terminated(take_until(" card"), tag::<_, _, OracleError<'_>>(" card")),
        terminated(take_until(", "), peek(tag(", "))),
    ))
    .parse(rest)
    .ok()?;
    let type_word = type_str.rsplit(' ').next().unwrap_or(type_str);
    let capitalized = format!("{}{}", &type_word[..1].to_uppercase(), &type_word[1..]);
    // CR 608.2c: "permanent" is not a CoreType (it spans CR 110.1's permanent card
    // types). Build the condition via the existing parse_type_phrase building block —
    // "permanent card" → TargetFilter::Typed(TypeFilter::Permanent) — and gate on it
    // with TargetMatchesFilter (the same condition variant the sibling MV arms use).
    if type_word == "permanent" {
        let (mut filter, leftover) =
            crate::parser::oracle_target::parse_type_phrase("permanent card");
        if !matches!(filter, TargetFilter::Any) && leftover.trim().is_empty() {
            let (after_type, chosen_type) = if let Ok((rest_after_chosen, _)) =
                tag::<_, _, OracleError<'_>>(" of the chosen type").parse(after_type)
            {
                (rest_after_chosen, true)
            } else {
                (after_type, false)
            };
            if chosen_type {
                let TargetFilter::Typed(typed) = &mut filter else {
                    return None;
                };
                typed.properties.push(FilterProp::IsChosenCreatureType);
            }
            return Some((
                maybe_negate(
                    AbilityCondition::TargetMatchesFilter {
                        filter,
                        use_lki: false,
                        subject_slot: None,
                    },
                    negated,
                ),
                after_type,
            ));
        }
    }
    let card_type = CoreType::from_str(&capitalized).ok()?;
    let (after_type, additional_filter) = parse_revealed_card_gate_suffix(after_type, ctx);
    Some((
        maybe_negate(
            AbilityCondition::RevealedHasCardType {
                card_types: vec![card_type],
                additional_filter,
                subtype_filter: None,
            },
            negated,
        ),
        after_type,
    ))
}

/// CR 608.2c + CR 301.5b + CR 701.3a: "If it's a[n] &lt;subtype&gt;, [you may] attach
/// it to ~" follow-up on a just-put permanent (The Invincible Iron Man: "put an
/// artifact card from your hand onto the battlefield. If it's an Equipment,
/// attach it to ~"). The bare-"it" anaphor (CR 608.2c) binds to the moved card,
/// and "~" (CR 201.5) is the ability's own source. Recognized as a UNIT so the
/// generic "it's a [subtype]" condition arm can't mis-key the type gate to the
/// ability's injected parent target (the source, not the moved card) or emit a
/// self-attach `ParentTarget`/`ParentTarget` (an Equipment can't equip itself,
/// CR 301.5c).
///
/// Emits the supported `ZoneChangedThisWay` + `Attach` forward_result idiom: the
/// condition gates on the moved card's type (read from `last_zone_changed_ids`,
/// independent of the target slot); the attach makes the moved card (`SelfRef`,
/// rebound to the forwarded object at resolution) the attachment and the source
/// (`ParentTarget`, the injected original source) the host. Only the
/// source-as-host recipient ("~") is in scope; typed hosts remain an honest gap.
/// Returns `(condition, attach_effect, attach_is_optional)`.
pub(super) fn try_parse_moved_card_subtype_attach_followup(
    text: &str,
) -> Option<(AbilityCondition, Effect, bool)> {
    let lower = text.to_lowercase();
    let trimmed = lower.trim();
    // Leading anaphoric gate: "if it's a "/"if it's an ".
    let (after_gate, _) = alt((
        tag::<_, _, OracleError<'_>>("if it's a "),
        tag("if it's an "),
    ))
    .parse(trimmed)
    .ok()?;
    // Split the subtype phrase from the "attach ..." body on the first comma.
    let (_, (subtype_phrase, body)) = nom_primitives::split_once_on(after_gate, ", ").ok()?;
    // Require a pure subtype gate (Equipment/Aura/Fortification, ...): the type
    // phrase must consume fully and reference at least one CR 205.3 subtype.
    let (filter, leftover) = parse_type_phrase(subtype_phrase);
    if !leftover.trim().is_empty() {
        return None;
    }
    let TargetFilter::Typed(typed) = &filter else {
        return None;
    };
    if !typed
        .type_filters
        .iter()
        .any(type_filter_references_subtype)
    {
        return None;
    }
    // Body: "[you may ]attach it to ~[.]" — only the source host ("~") is in scope.
    let (after_may, is_optional) = opt(tag::<_, _, OracleError<'_>>("you may "))
        .parse(body)
        .map(|(rest, matched)| (rest, matched.is_some()))
        .unwrap_or((body, false));
    let (after_attach, _) = tag::<_, _, OracleError<'_>>("attach it to ")
        .parse(after_may)
        .ok()?;
    let (after_source, _) = tag::<_, _, OracleError<'_>>("~").parse(after_attach).ok()?;
    let (after_dot, _) = opt(tag::<_, _, OracleError<'_>>("."))
        .parse(after_source)
        .ok()?;
    if !after_dot.trim().is_empty() {
        return None;
    }
    let condition = AbilityCondition::ZoneChangedThisWay { filter };
    let attach = Effect::Attach {
        attachment: TargetFilter::SelfRef,
        target: TargetFilter::ParentTarget,
    };
    Some((condition, attach, is_optional))
}

/// CR 705.2 + CR 608.2c: Strip a resolution-scoped "if you {win|lose} the flip,"
/// gate that precedes a "repeat this process" directive, mapping it to
/// `AbilityCondition::CoinFlipOutcome`. Scoped to the repeat-directive context
/// (called first inside `try_parse_repeat_process_directive`) so inline
/// Krark-style "if you win the flip, <effect>" branches — whose body is NOT
/// "repeat this process" — still fall through to the coin-flip fold. Returns the
/// original text unchanged when no flip gate is present.
pub(super) fn strip_coin_flip_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    match nom_on_lower(text, &lower, |i| {
        let (i, _) = tag::<_, _, OracleError<'_>>("if you ").parse(i)?;
        let (i, result) = alt((
            value(CoinFlipResult::Won, tag("win the flip")),
            value(CoinFlipResult::Lost, tag("lose the flip")),
        ))
        .parse(i)?;
        let (i, _) = tag(", ").parse(i)?;
        Ok((i, AbilityCondition::CoinFlipOutcome { result }))
    }) {
        Some((condition, remainder)) => (Some(condition), remainder.to_string()),
        None => (None, text.to_string()),
    }
}

pub(super) fn strip_card_type_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    if let Some((condition, remainder)) = parse_if_revealed_card_type_conditional(text) {
        return (Some(condition), remainder);
    }
    if let Some((condition, remainder)) = parse_if_exiled_card_type_conditional(text) {
        return (Some(condition), remainder);
    }
    let lower = text.to_lowercase();
    let rest = alt((
        tag::<_, _, OracleError<'_>>("if it's a "),
        tag("if it's an "),
    ))
    .parse(lower.as_str())
    .ok()
    .map(|(rest, _)| rest);
    let Some(rest) = rest else {
        return (None, text.to_string());
    };
    let (rest, negated) = opt(tag::<_, _, OracleError<'_>>("non"))
        .parse(rest)
        .map(|(rest, matched)| (rest, matched.is_some()))
        .unwrap_or((rest, false));
    let mut ctx = ParseContext::default();
    let Some((condition, after_type)) = parse_its_a_card_type_gate_body(rest, negated, &mut ctx)
    else {
        return (None, text.to_string());
    };
    let remainder = remainder_after_optional_comma(after_type);
    let offset = text.len() - remainder.len();
    (Some(condition), text[offset..].to_string())
}

fn parse_its_a_type_condition(
    condition_text: &str,
    ctx: &mut ParseContext,
) -> Option<AbilityCondition> {
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("it's a "), tag("it's an ")))
        .parse(condition_text)
        .ok()?;
    let (rest, negated) = opt(tag::<_, _, OracleError<'_>>("non"))
        .parse(rest)
        .map(|(rest, matched)| (rest, matched.is_some()))
        .unwrap_or((rest, false));
    let (condition, remainder) = parse_its_a_card_type_gate_body(rest, negated, ctx)?;
    if remainder.trim().trim_end_matches('.').is_empty() {
        Some(condition)
    } else {
        None
    }
}

/// CR 614.1a + CR 608.2c: Parse a target-anaphoric color check used as the
/// gating condition of an "instead" override. Composes three orthogonal axes:
///
///   - subject: `it`, `that creature`, `that permanent`, `that card`
///   - tense: present (`is`/`'s`) → current state, past (`was`) → LKI
///   - polarity: positive (`is`/`was`) vs. negative (`isn't`/`wasn't`/`is not`/`was not`)
///
/// Past-tense forms set `use_lki: true` per CR 400.7 so the runtime evaluates
/// the LKI snapshot rather than the current object state (matters when the
/// parent sub_ability already moved the target before the check runs).
fn parse_target_color_condition(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, AbilityCondition> {
    let (rest, _) = parse_target_anaphoric_subject(input)?;
    let (rest, (negated, use_lki)) = parse_target_anaphoric_tense_polarity(rest)?;
    let (rest, first_color) = nom_primitives::parse_color(rest)?;
    // CR 105.2: Disjunctive color condition — "white or blue" etc.
    let (rest, second_color) =
        opt(preceded(tag(" or "), nom_primitives::parse_color)).parse(rest)?;
    let mut filters: Vec<_> = std::iter::once(first_color)
        .chain(second_color)
        .map(|color| {
            TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::HasColor { color }]),
            )
        })
        .collect();
    let filter = if filters.len() > 1 {
        TargetFilter::Or { filters }
    } else {
        filters.pop().unwrap()
    };
    Ok((
        rest,
        maybe_negate(
            AbilityCondition::TargetMatchesFilter {
                filter,
                use_lki,
                subject_slot: None,
            },
            negated,
        ),
    ))
}

/// Demonstrative target-anaphoric subject — "that creature" / "that permanent" /
/// "that card". Deliberately EXCLUDES the bare "it" pronoun: the "it's a [type]
/// card" / "it's a [subtype] creature card" reveal-conditional forms are already
/// owned by `RevealedHasCardType` (via `parse_if_revealed_card_type_conditional`
/// and `strip_card_type_conditional`). Accepting "it" here would preempt those
/// paths and reclassify reveal gates as `TargetMatchesFilter` (Goblin Guide,
/// Kenessos, chosen-type forms). The demonstrative subjects are unambiguous —
/// they always denote a previously-targeted object.
fn parse_target_demonstrative_subject(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>("that creature"),
            tag("that permanent"),
            tag("that card"),
        )),
    )
    .parse(input)
}

/// CR 608.2c + CR 205.3m: target-anaphoric card-type / subtype-membership gate —
/// "that creature is a Mutant, Ninja, or Turtle" (Turtle Van), "that permanent
/// is an artifact", "that creature was a Zombie". Composes three orthogonal axes:
///
///   - subject: `that creature`, `that permanent`, `that card` (NOT "it" — see
///     `parse_target_demonstrative_subject` for why)
///   - tense: present (`is`/`'s`) → current state, past (`was`) → LKI (CR 400.7)
///   - polarity: positive (`is`/`was`) vs. negative (`isn't`/`wasn't`/…)
///
/// The predicate tail is parsed by the shared `parse_type_phrase` building block,
/// so the full comma + "or" subtype-disjunction grammar (CR 205.3m) is covered:
/// "a Mutant, Ninja, or Turtle" lowers to `Or[Subtype(Mutant), Subtype(Ninja),
/// Subtype(Turtle)]`. Emits `TargetMatchesFilter` (wrapped in `Not` when negated)
/// resolving against the ability's first object target.
fn parse_target_type_membership_condition(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, AbilityCondition> {
    let (rest, _) = parse_target_demonstrative_subject(input)?;
    let (rest, (negated, use_lki)) = parse_target_anaphoric_tense_polarity(rest)?;
    // CR 205.3: an optional "a"/"an" article precedes a single type/subtype word
    // ("is a Goblin"); a leading core type with no article ("is artifact") is not
    // real Oracle wording, so the article guard stays inside the combinator.
    let (rest, _) = opt(nom_primitives::parse_article).parse(rest)?;
    let (filter, remainder) = crate::parser::oracle_target::parse_type_phrase(rest);
    // Reject when no type word was consumed (parse_type_phrase echoes its input
    // unchanged on failure) so the alt backtracks to the color / quantity arms.
    if remainder.len() == rest.len() || matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        remainder,
        maybe_negate(
            AbilityCondition::TargetMatchesFilter {
                filter,
                use_lki,
                subject_slot: None,
            },
            negated,
        ),
    ))
}

fn parse_target_type_membership_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.trim().trim_end_matches('.').to_ascii_lowercase();
    let parsed = all_consuming(parse_target_type_membership_condition)
        .parse(lower.as_str())
        .ok()
        .map(|(_, condition)| condition);
    parsed
}

/// CR 608.2c + CR 701.20a: In a top-card predicate guessing sequence, "they
/// guessed right" means the revealed card matches the most recent opponent
/// guess. The guess itself is lowered as `ChoiceType::CardPredicateGuess`, so
/// this can reuse the same transient predicate filter used by "of the chosen
/// kind" effects.
fn parse_guessed_right_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.trim().trim_end_matches('.').to_ascii_lowercase();
    let (_, result) = all_consuming(parse_card_predicate_guess_result)
        .parse(lower.as_str())
        .ok()?;
    match (result.subject, result.outcome) {
        (_, CardPredicateGuessOutcome::Right) => Some(AbilityCondition::RevealedHasCardType {
            card_types: vec![],
            additional_filter: Some(FilterProp::MatchesLastChosenCardPredicate),
            subtype_filter: None,
        }),
    }
}

pub(super) fn strip_card_predicate_guess_result_conditional(
    text: &str,
) -> Option<(AbilityCondition, String)> {
    let (condition_fragment, body) = split_leading_conditional(text)?;
    let cond_text = strip_guess_result_condition_prefix(&condition_fragment);

    let condition = parse_guessed_right_condition_text(cond_text)?;
    Some((condition, body))
}

pub(super) fn is_card_predicate_guess_result_conditional(text: &str) -> bool {
    let Some((condition_fragment, _body)) = split_leading_conditional(text) else {
        return false;
    };
    let parsed = all_consuming(parse_any_card_predicate_guess_result)
        .parse(strip_guess_result_condition_prefix(&condition_fragment))
        .is_ok();
    parsed
}

fn strip_guess_result_condition_prefix(condition_fragment: &str) -> &str {
    let condition_lower = condition_fragment.to_lowercase();
    nom_on_lower(condition_fragment, &condition_lower, |i| {
        value(
            (),
            alt((
                tag::<_, _, OracleError<'_>>("then, if "),
                tag("then if "),
                tag("if "),
            )),
        )
        .parse(i)
    })
    .map(|((), rest)| rest)
    .unwrap_or(condition_fragment)
    .trim()
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct CardPredicateGuessResult {
    subject: CardPredicateGuessSubject,
    outcome: CardPredicateGuessOutcome,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum CardPredicateGuessSubject {
    They,
    ThatPlayer,
    ThatOpponent,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum CardPredicateGuessOutcome {
    Right,
}

fn parse_card_predicate_guess_result(input: &str) -> OracleResult<'_, CardPredicateGuessResult> {
    let (input, subject) = parse_card_predicate_guess_subject(input)?;
    let (input, _) = tag(" guessed ").parse(input)?;
    let (input, outcome) = parse_card_predicate_guess_outcome(input)?;
    Ok((input, CardPredicateGuessResult { subject, outcome }))
}

fn parse_any_card_predicate_guess_result(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = parse_any_card_predicate_guess_subject(input)?;
    let (input, _) =
        alt((tag::<_, _, OracleError<'_>>(" guessed "), tag(" guesses "))).parse(input)?;
    let (input, _) = parse_any_card_predicate_guess_outcome(input)?;
    Ok((input, ()))
}

fn parse_card_predicate_guess_subject(input: &str) -> OracleResult<'_, CardPredicateGuessSubject> {
    alt((
        value(CardPredicateGuessSubject::They, tag("they")),
        value(CardPredicateGuessSubject::ThatPlayer, tag("that player")),
        value(
            CardPredicateGuessSubject::ThatOpponent,
            tag("that opponent"),
        ),
    ))
    .parse(input)
}

fn parse_any_card_predicate_guess_subject(input: &str) -> OracleResult<'_, ()> {
    alt((
        map(parse_card_predicate_guess_subject, |_| ()),
        value((), tag("target opponent")),
        value((), tag("the player")),
        value((), tag("your opponent")),
        value((), tag("an opponent")),
    ))
    .parse(input)
}

fn parse_card_predicate_guess_outcome(input: &str) -> OracleResult<'_, CardPredicateGuessOutcome> {
    value(CardPredicateGuessOutcome::Right, tag("right")).parse(input)
}

fn parse_any_card_predicate_guess_outcome(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>("right"),
            tag("wrong"),
            tag("correctly"),
            tag("incorrectly"),
        )),
    )
    .parse(input)
}

/// Consume a target-anaphoric noun phrase used as the subject of an "instead"
/// gating condition. `it` is a special pronoun case (the only one that
/// contracts to `it's`); the noun-phrase forms always take a space before
/// their verb. Returns `()` because the subject identity is preserved by
/// `TargetMatchesFilter` resolving against the parent target.
fn parse_target_anaphoric_subject(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>("it"),
            tag("that creature"),
            tag("that permanent"),
            tag("that card"),
        )),
    )
    .parse(input)
}

/// Consume the verb portion (tense + polarity) following a target-anaphoric
/// subject. Returns `(negated, use_lki)`:
///
///   - `is` / `'s` → present, positive
///   - `is not` / `'s not` / `isn't` → present, negated
///   - `was` → past, positive
///   - `was not` / `wasn't` → past, negated
///
/// Past-tense forms (CR 400.7) require LKI evaluation. Listed longest-first
/// so `is not` wins over `is`.
fn parse_target_anaphoric_tense_polarity(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, (bool, bool)> {
    alt((
        // Negated past — must precede positive past
        value((true, true), alt((tag(" wasn't "), tag(" was not ")))),
        // Positive past
        value((false, true), tag(" was ")),
        // CR 400.7: past-tense possession ("it had mana value 3 or less", "that
        // creature had power 2 or less") → LKI snapshot. None of the recognized
        // reflexive predicates use a negated " hadn't "/" didn't have " form, so
        // only the positive arm is supplied.
        value((false, true), tag(" had ")),
        // Negated present — must precede positive present
        value(
            (true, false),
            alt((tag(" isn't "), tag("'s not "), tag(" is not "))),
        ),
        // Positive present
        value((false, false), alt((tag("'s "), tag(" is ")))),
    ))
    .parse(input)
}

fn parse_target_color_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.trim().trim_end_matches('.').to_ascii_lowercase();
    let parsed = all_consuming(parse_target_color_condition)
        .parse(lower.as_str())
        .ok()
        .map(|(_, condition)| condition);
    parsed
}

/// CR 508.1a + CR 603.4 + CR 603.7: target-anaphoric combat-history gate used
/// as a trailing "if" condition — "it attacked this turn" / "it didn't attack
/// this turn" (Aggression, Berserk, Norritt, Nettling Imp). Composes the shared
/// target-anaphoric subject parser with a verb-tense polarity axis; emits
/// `TargetMatchesFilter { creature + AttackedThisTurn }`, wrapped in `Not` for
/// the negated form, resolved against the ability's first object target (or the
/// triggering source) at runtime. `use_lki: false` — the creature is still on
/// the battlefield when the destroy condition evaluates (CR 400.7), so current
/// object state is authoritative, not an LKI snapshot.
fn parse_target_attacked_this_turn_condition(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, AbilityCondition> {
    let (rest, _) = parse_target_anaphoric_subject(input)?;
    let (rest, negated) = parse_anaphoric_attacked_tense_polarity(rest)?;
    let (rest, _) = tag(" this turn").parse(rest)?;
    Ok((
        rest,
        maybe_negate(
            AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(
                    TypedFilter::creature()
                        .properties(vec![FilterProp::AttackedThisTurn { defender: None }]),
                ),
                use_lki: false,
                subject_slot: None,
            },
            negated,
        ),
    ))
}

/// Verb tense+polarity after a target-anaphoric subject for the
/// attacked-this-turn gate. Returns `negated`. Negated/longest forms listed
/// first so "didn't attack" wins over the positive "attacked". The positive
/// form is bare " attacked" — "did attack" is not real Oracle wording.
fn parse_anaphoric_attacked_tense_polarity(input: &str) -> OracleResult<'_, bool> {
    alt((
        value(true, alt((tag(" didn't attack"), tag(" did not attack")))),
        value(false, tag(" attacked")),
    ))
    .parse(input)
}

fn parse_target_attacked_this_turn_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.trim().trim_end_matches('.').to_ascii_lowercase();
    let parsed = all_consuming(parse_target_attacked_this_turn_condition)
        .parse(lower.as_str())
        .ok()
        .map(|(_, c)| c);
    parsed
}

/// Subject of a per-target "entered this turn" gate. Two forms:
///   (i) named filter — "the creature you control" → the parsed `Typed` filter,
///       carrying its type + controller restriction (do NOT hardcode `creature()`;
///       "the artifact you control entered this turn" must keep its type).
///   (ii) anaphor — "it" / "that creature" → `creature()` (the sibling
///       attacked-this-turn form's default subject).
/// The returned filter has NO `EnteredThisTurn` property yet — the caller
/// appends it after matching the verb. `parse_type_phrase` returns a suffix
/// slice of `input`, so its remainder is a valid nom continuation.
fn parse_entered_this_turn_subject(input: &str) -> OracleResult<'_, TargetFilter> {
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("the ").parse(input) {
        let (filter, remainder) = parse_type_phrase(rest);
        if matches!(filter, TargetFilter::Typed(_)) && remainder.len() < rest.len() {
            return Ok((remainder, filter));
        }
    }
    let (rest, _) = parse_target_anaphoric_subject(input)?;
    Ok((rest, TargetFilter::Typed(TypedFilter::creature())))
}

/// CR 400.7 + CR 608.2c: per-target "if <subject> entered this turn" gate — the
/// anaphoric or filter-named subject is a permanent that entered the battlefield
/// this turn (Malamet Battle Glyph: "If the creature you control entered this
/// turn, put a +1/+1 counter on it"). Direct sibling of
/// `parse_target_attacked_this_turn_condition`, swapping the combat verb for the
/// ETB verb and the `AttackedThisTurn` property for `EnteredThisTurn`
/// (evaluated per-object at `filter.rs`: `entered_battlefield_turn ==
/// Some(turn_number)`). `use_lki: false` — the creature is on the battlefield
/// when the counter condition evaluates, so its live ETB turn is authoritative.
/// `subject_slot: None` here; the two-target counter-chain rewrite in
/// `lower_effect_chain_ir` re-keys it to `Some(0)` so the condition tests the
/// first-declared fighter under most-recent-only chain propagation.
fn parse_target_entered_this_turn_condition(input: &str) -> OracleResult<'_, AbilityCondition> {
    let (rest, mut filter) = parse_entered_this_turn_subject(input)?;
    // "entered" (past, CR 400.7) or "enters" (present) — both denote the
    // this-turn ETB predicate on the subject.
    let (rest, _) = alt((tag(" entered"), tag(" enters"))).parse(rest)?;
    let (rest, _) = tag(" this turn").parse(rest)?;
    if let TargetFilter::Typed(ref mut tf) = filter {
        if !tf.properties.contains(&FilterProp::EnteredThisTurn) {
            tf.properties.push(FilterProp::EnteredThisTurn);
        }
    }
    Ok((
        rest,
        AbilityCondition::TargetMatchesFilter {
            filter,
            use_lki: false,
            subject_slot: None,
        },
    ))
}

fn parse_target_entered_this_turn_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.trim().trim_end_matches('.').to_ascii_lowercase();
    let parsed = all_consuming(parse_target_entered_this_turn_condition)
        .parse(lower.as_str())
        .ok()
        .map(|(_, c)| c);
    parsed
}

/// CR 202.3 + CR 208.1: Parse a trailing "N or less" / "N or greater"
/// comparison threshold shared by the mana-value and power/toughness reflexive
/// predicates. Returns the typed `Comparator` and the fixed threshold. Composed
/// from `nom_primitives::parse_number` and a comparator `alt` — one combinator,
/// not a flat product of full-string tags.
fn parse_or_threshold(input: &str) -> OracleResult<'_, (Comparator, i32)> {
    let (rest, n) = nom_primitives::parse_number(input)?;
    let (rest, _) = tag(" or ").parse(rest)?;
    let (rest, comparator) = alt((
        value(Comparator::LE, alt((tag("less"), tag("fewer")))),
        value(Comparator::GE, alt((tag("greater"), tag("more")))),
    ))
    .parse(rest)?;
    Ok((rest, (comparator, n as i32)))
}

/// CR 208.1: consume a P/T-stat keyword (with its trailing space) for the
/// reflexive power/toughness predicate. Power and toughness are a leaf-level
/// parameterization of the same CR 208 characteristic pair (`PtStat`).
fn parse_reflexive_pt_stat(input: &str) -> OracleResult<'_, PtStat> {
    alt((
        value(PtStat::Power, tag("power ")),
        value(PtStat::Toughness, tag("toughness ")),
    ))
    .parse(input)
}

/// CR 608.2c: the reflexive predicate following a target-anaphoric subject +
/// tense — one branch per object characteristic. This `alt` IS the parameterized
/// predicate axis: a single combinator per characteristic, composed, not a flat
/// product of full-sentence tags. Each branch yields the `FilterProp` the
/// condition tests against the prior clause's first object target.
fn parse_reflexive_object_property(input: &str) -> OracleResult<'_, FilterProp> {
    alt((
        // CR 120.6 + CR 120.9: historical "was dealt damage this turn" (Sold Out).
        value(
            FilterProp::WasDealtDamageThisTurn,
            tag("dealt damage this turn"),
        ),
        // CR 110.5: tap status is a permanent's status (Brackish Blunder "if it
        // was tapped"). The past-tense ("was tapped") form sets use_lki, reading
        // the LKI snapshot's captured exit-time tap state — the antecedent (a
        // bounced/destroyed permanent) has left the battlefield before the rider
        // resolves (CR 110.5d: cards not on the battlefield are neither tapped nor
        // untapped, so the live object cannot answer). The present-tense ("is
        // tapped") form reads live state. Must precede `attacking`/`blocking`:
        // distinct word, no shared prefix, but kept adjacent to the status leaves.
        value(FilterProp::Tapped, tag("tapped")),
        // CR 110.5: untapped is the symmetric tap-status sibling ("if it was
        // untapped"). "untapped" is a distinct lexeme, not "not tapped" — it
        // shares no prefix with "tapped" (begins with "un"), so leaf order vs
        // `tapped` is immaterial. Tense/polarity compose for free: past-tense
        // "was untapped" sets use_lki (reads the snapshot), present-tense "is
        // untapped" reads live state, and "isn't untapped" wraps in `Not`.
        value(FilterProp::Untapped, tag("untapped")),
        // CR 508.1b: combat status (Wisecrack "is attacking").
        value(FilterProp::Attacking { defender: None }, tag("attacking")),
        // CR 509.1a: combat status (blocking sibling).
        value(FilterProp::Blocking, tag("blocking")),
        // CR 202.3: "mana value N or less/greater" (Consuming Ashes).
        map(
            preceded(tag("mana value "), parse_or_threshold),
            |(comparator, value)| FilterProp::Cmc {
                comparator,
                value: QuantityExpr::Fixed { value },
            },
        ),
        // CR 208.1: "power/toughness N or less/greater" (Driftgloom Coyote).
        map(
            (parse_reflexive_pt_stat, parse_or_threshold),
            |(stat, (comparator, value))| FilterProp::PtComparison {
                stat,
                scope: PtValueScope::Current,
                comparator,
                value: QuantityExpr::Fixed { value },
            },
        ),
    ))
    .parse(input)
}

/// CR 608.2c + CR 400.7: target-anaphoric reflexive object-property gate — the
/// rider condition for the "removal/bounce/damage, then conditional bonus"
/// class ("it was dealt damage this turn", "it had mana value 3 or less", "that
/// creature had power 2 or less", "that creature is attacking"). Composes three
/// orthogonal axes:
///
///   - subject: `it` / `that creature` / `that permanent` / `that card`
///     (`parse_target_anaphoric_subject`)
///   - tense + polarity: `parse_target_anaphoric_tense_polarity` →
///     `(negated, use_lki)` — past tense (`was`/`had`) reads LKI per CR 400.7 /
///     CR 608.2h; present tense (`is`) reads live state.
///   - predicate: a parameterized `FilterProp` axis
///     (`parse_reflexive_object_property`).
///
/// Emits `TargetMatchesFilter` (wrapped in `Not` when negated) resolving against
/// the resolving ability's first object target.
fn parse_target_reflexive_property_condition(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, AbilityCondition> {
    let (rest, _) = parse_target_anaphoric_subject(input)?;
    let (rest, (negated, use_lki)) = parse_target_anaphoric_tense_polarity(rest)?;
    let (rest, prop) = parse_reflexive_object_property(rest)?;
    Ok((
        rest,
        maybe_negate(
            AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![prop])),
                use_lki,
                subject_slot: None,
            },
            negated,
        ),
    ))
}

fn parse_target_reflexive_property_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.trim().trim_end_matches('.').to_ascii_lowercase();
    let parsed = all_consuming(parse_target_reflexive_property_condition)
        .parse(lower.as_str())
        .ok()
        .map(|(_, c)| c);
    parsed
}

/// CR 208.1: threshold grammar — `exactly N` → `EQ`, else `N or less` /
/// `N or greater` via the shared `parse_or_threshold` building block. The
/// equality leaf mirrors the proven `exactly N` → `EQ` form in oracle_nom's
/// `parse_hand_size_predicate`; `parse_or_threshold` has no such leaf, so it is
/// added here as a parameterizing prefix rather than a new variant.
fn parse_threshold_with_exactly(input: &str) -> OracleResult<'_, (Comparator, i32)> {
    if let Ok((rest, n)) = preceded(
        tag::<_, _, OracleError<'_>>("exactly "),
        nom_primitives::parse_number,
    )
    .parse(input)
    {
        return Ok((rest, (Comparator::EQ, n as i32)));
    }
    parse_or_threshold(input)
}

/// CR 115.1 + CR 208.1 + CR 608.2c: target-anaphoric possessive power/toughness
/// comparison — "that creature's power is 2 or less" / "that permanent's
/// toughness is exactly N" (Depressurize, Gore Vassal, Reptilian Recruiter's
/// first disjunct). The possessive "'s <stat> is N" form is NOT reached by
/// `parse_target_reflexive_property_condition` (its predicate parser rejects the
/// leading "is"), and the generic `parse_cda_quantity` fallback mis-scopes it to
/// `Power { CostPaidObject }`. CR 115.1: "that creature" is the ability's first
/// target, so this binds Target scope via `TargetMatchesFilter`, which resolves
/// `ability.targets[0]` and — for subject-based triggers with no chosen target —
/// falls back to the triggering source (see effects/mod.rs). Composes:
///   - subject: `parse_target_demonstrative_subject` (that creature/permanent/card)
///   - possessive `'s ` + stat (`parse_reflexive_pt_stat`) + linking `is `
///   - threshold: `parse_threshold_with_exactly`.
fn parse_target_possessive_pt_comparison(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, AbilityCondition> {
    let (rest, _) = parse_target_demonstrative_subject(input)?;
    let (rest, _) = tag("'s ").parse(rest)?;
    let (rest, stat) = parse_reflexive_pt_stat(rest)?;
    let (rest, _) = tag("is ").parse(rest)?;
    let (rest, (comparator, value)) = parse_threshold_with_exactly(rest)?;
    Ok((
        rest,
        AbilityCondition::TargetMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                FilterProp::PtComparison {
                    stat,
                    scope: PtValueScope::Current,
                    comparator,
                    value: QuantityExpr::Fixed { value },
                },
            ])),
            use_lki: false,
            subject_slot: None,
        },
    ))
}

fn parse_target_possessive_pt_comparison_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.trim().trim_end_matches('.').to_ascii_lowercase();
    let parsed = all_consuming(parse_target_possessive_pt_comparison)
        .parse(lower.as_str())
        .ok()
        .map(|(_, c)| c);
    parsed
}

/// CR 201.5 + CR 208.1 + CR 608.2c: source-referential "if its/her/his power or
/// toughness is exactly N" — the possessive subject names the ability's own
/// source (Amalia Benavides Aguirre: "destroy all other creatures if its power is
/// exactly 20", CR 201.5). `strip_property_conditional` already owns the
/// "N or less" / "N or greater" thresholds (`CostPaidObject` scope) and returns
/// None only for the equality form, so this fires SOLELY on the `exactly N`
/// equality — the `EQ` guard makes that invariant explicit and keeps it from ever
/// re-scoping a threshold condition a sibling stripper already owns.
fn parse_source_pt_comparison_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.trim().trim_end_matches('.').to_ascii_lowercase();
    let (_, sc) =
        all_consuming(crate::parser::oracle_nom::condition::parse_source_power_toughness_condition)
            .parse(lower.as_str())
            .ok()?;
    match &sc {
        StaticCondition::QuantityComparison {
            comparator: Comparator::EQ,
            ..
        } => static_condition_to_ability_condition(&sc, &mut ParseContext::default()),
        _ => None,
    }
}

pub(super) fn try_parse_type_setting(text: &str) -> Option<AbilityDefinition> {
    let lower = text.to_lowercase();
    let lower = lower.trim_end_matches('.');

    let (type_name, _) = alt((tag::<_, _, OracleError<'_>>("it's a "), tag("it's an ")))
        .parse(lower)
        .ok()?;

    let type_name = type_name.trim();
    let capitalized = format!("{}{}", &type_name[..1].to_uppercase(), &type_name[1..]);
    CoreType::from_str(&capitalized).ok()?;

    let mut remove_types = Vec::new();
    if capitalized != "Creature" {
        remove_types.push("Creature".to_string());
    }

    let effect = Effect::Animate {
        power: None,
        toughness: None,
        types: vec![capitalized],
        remove_types,
        target: TargetFilter::None,
        keywords: vec![],
    };

    let mut def = AbilityDefinition::new(AbilityKind::Spell, effect);
    def = def.duration(Duration::Permanent);
    Some(def)
}

pub(super) fn strip_turn_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    if let Some((negated, rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            value(false, tag("if it's your turn, ")),
            value(true, tag("if it's not your turn, ")),
            value(true, tag("if it isn't your turn, ")),
        ))
        .parse(input)
    }) {
        return (
            Some(maybe_negate(AbilityCondition::IsYourTurn, negated)),
            rest.to_string(),
        );
    }
    (None, text.to_string())
}

pub(super) fn strip_property_conditional(
    text: &str,
    ctx: &ParseContext,
) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // CR 608.2c + CR 608.2k + CR 208.1: "its power" binds the ability SOURCE for
    // a player/phase-subject trigger (Amalia, Lily Bowen), or the clause-local
    // object (CostPaidObject) for a target/entering-referent clause (Tribute,
    // Ent's Fury). A player/phase subject ("you", "a player", any) has no
    // clause-local object for "its" to bind, so the anaphor resolves to the
    // ability source (CR 113.7a LKI); a target/typed-object subject supplies the
    // referent the untargeted "its" reads (CR 608.2k).
    let scope = match ctx.subject {
        Some(TargetFilter::Controller) | Some(TargetFilter::Player) | Some(TargetFilter::Any) => {
            ObjectScope::Source
        }
        _ => ObjectScope::CostPaidObject,
    };

    for (property, qty_ref) in &[
        ("power", QuantityRef::Power { scope }),
        ("toughness", QuantityRef::Toughness { scope }),
    ] {
        let pattern = format!(" if its {property} is ");
        if let Some((before, after)) = tp.rsplit_around(&pattern) {
            let after = after.lower.trim_end_matches('.');

            if let Some((comparator, value)) = parse_comparison_suffix(after) {
                return (
                    Some(AbilityCondition::QuantityCheck {
                        lhs: QuantityExpr::Ref {
                            qty: qty_ref.clone(),
                        },
                        comparator,
                        rhs: QuantityExpr::Fixed { value },
                    }),
                    before.original.to_string(),
                );
            }
        }
    }

    for (pattern, use_lki) in &[
        (" if that creature was a ", true),
        (" if that creature was an ", true),
        (" if that creature is a ", false),
        (" if that creature is an ", false),
    ] {
        if let Some((before, after)) = tp.rsplit_around(pattern) {
            let type_text = after.lower.trim_end_matches('.').trim();
            let (filter, leftover) = parse_type_phrase(type_text);
            if !matches!(filter, TargetFilter::Any) && leftover.trim().is_empty() {
                return (
                    Some(AbilityCondition::TargetMatchesFilter {
                        filter,
                        use_lki: *use_lki,
                        subject_slot: None,
                    }),
                    before.original.to_string(),
                );
            }
        }
    }

    (None, text.to_string())
}

/// Parser-internal selector for which player-property a superlative-comparison
/// condition reads. Selects which `QuantityRef` to build — not stored in the
/// AST. Single arm today; future player-properties add `alt` arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlayerProperty {
    /// CR 702.179f: a player's speed.
    Speed,
}

/// CR 702.179f: parse "speed" → `PlayerProperty::Speed`.
fn parse_player_property_keyword(input: &str) -> OracleResult<'_, PlayerProperty> {
    value(PlayerProperty::Speed, tag("speed")).parse(input)
}

/// Build the `QuantityRef` for a player-property of the given player scope.
fn player_property_quantity(property: PlayerProperty, player: PlayerScope) -> QuantityRef {
    match property {
        PlayerProperty::Speed => QuantityRef::Speed { player },
    }
}

/// CR 608.2c: Strip a player-property superlative-comparison conditional that
/// gates a chained sub-ability — e.g. Spikeshell Harrier's
/// "if that opponent's speed is greater than each other player's speed, ...".
///
/// Mirrors the #333 `parse_subject_property_superlative_comparison` grammar but
/// emits an `AbilityCondition::QuantityCheck` for a chained sub-ability rather
/// than a `StaticCondition`. The LHS reads the parent object target's
/// controller's property; the RHS aggregates that property over every OTHER
/// player (CR 102.1 + CR 608.2c).
pub(super) fn strip_player_property_superlative_conditional(
    text: &str,
) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();

    // Leading "if that opponent's " / "if that player's " — connective +
    // player anaphor. The clause text arrives with the conditional as a
    // sentence prefix (CR 608.2c: conditional second effect).
    let Ok((rest, _)) = preceded(
        tag::<_, _, OracleError<'_>>("if that "),
        alt((tag("opponent's "), tag("player's "))),
    )
    .parse(lower.as_str()) else {
        return (None, text.to_string());
    };

    // LHS: "<property> is <comparator phrase>each other player's <property>, "
    let Ok((rest, lhs_property)) = parse_player_property_keyword(rest) else {
        return (None, text.to_string());
    };
    let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" is ").parse(rest) else {
        return (None, text.to_string());
    };
    let Ok((rest, (comparator, aggregate))) =
        crate::parser::oracle_nom::condition::parse_superlative_comparator_phrase(rest)
    else {
        return (None, text.to_string());
    };
    let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("each other "),
        tag("every other "),
    ))
    .parse(rest) else {
        return (None, text.to_string());
    };
    let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("player's ").parse(rest) else {
        return (None, text.to_string());
    };
    let Ok((rest, rhs_property)) = parse_player_property_keyword(rest) else {
        return (None, text.to_string());
    };
    // RHS-property guard: the compared properties must match (mirrors the
    // #333 inequality form's RHS guard).
    if lhs_property != rhs_property {
        return (None, text.to_string());
    }
    // Trailing connective ", " separates the condition from the gated effect.
    let Ok((effect_text, _)) =
        preceded(opt(char(',')), tag::<_, _, OracleError<'_>>(" ")).parse(rest)
    else {
        return (None, text.to_string());
    };

    // CR 109.4 + CR 608.2c: LHS = the bounced object's controller's property;
    // RHS = the same property aggregated over every OTHER player.
    let lhs = QuantityExpr::Ref {
        qty: player_property_quantity(lhs_property, PlayerScope::ParentObjectTargetController),
    };
    let rhs = QuantityExpr::Ref {
        qty: player_property_quantity(
            lhs_property,
            PlayerScope::AllPlayers {
                aggregate,
                exclude: Some(Box::new(PlayerScope::ParentObjectTargetController)),
            },
        ),
    };

    // The residual body is the gated effect, in original casing.
    let effect_original = text[text.len() - effect_text.len()..].to_string();
    (
        Some(AbilityCondition::QuantityCheck {
            lhs,
            comparator,
            rhs,
        }),
        effect_original,
    )
}

/// CR 608.2c + CR 122.1b: Parse "if that creature has <keyword>[ and ~ doesn't], <effect>".
///
/// The optional " and ~ doesn't" conjunct (Super-Adaptoid: "If that creature
/// has haste and Super-Adaptoid doesn't, put a haste counter on
/// Super-Adaptoid") yields a compound `And([TargetHasKeywordInstead,
/// SourceLacksKeyword])` so the keyword counter is only placed when the target
/// HAS the keyword AND the source LACKS it. Without the conjunct (Toxic riders:
/// "if that creature has toxic, draw a card") it stays a bare
/// `TargetHasKeywordInstead`. The keyword is captured up to the next " and " /
/// "," / "; " boundary and lowered through `Keyword::from_str`, so both
/// evergreen keywords and parameterized-but-bare keywords (Toxic) are
/// recognized — and the prose "and ~ doesn't" tail is never folded into the
/// keyword name (a bare `split_once(", ")` captured "haste and ~ doesn't" as
/// one `Unknown` keyword). Unrecognized phrases stay `Keyword::Unknown` (see
/// the inline note at the `from_str` call), preserving the pre-existing inert
/// rider for target-gated counter/P-T "instead" cards that have no keyword to
/// mirror. Building block for the whole "if that creature has KW [and ~
/// doesn't], …" class, not a single card.
pub(super) fn strip_target_keyword_instead(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    type E<'a> = OracleError<'a>;
    let parsed = nom_on_lower(text, &lower, |i| {
        let (i, _) = alt((
            tag::<_, _, E>("if that creature has "),
            tag("if that permanent has "),
        ))
        .parse(i)?;
        // The keyword name runs up to the first of " and " (the lack conjunct),
        // "," / "; " (the effect connective). `alt` of three terminators keeps
        // the captured span to the keyword word(s) only.
        let (i, keyword_str) = alt((
            terminated(take_until(" and "), peek(tag::<_, _, E>(" and "))),
            terminated(take_until(", "), peek(tag(", "))),
            terminated(take_until("; "), peek(tag("; "))),
        ))
        .parse(i)?;
        // CR 122.1b: `Keyword::from_str` is infallible — an unrecognized phrase
        // (a counter or power/toughness gate such as "a +1/+1 counter on it" or
        // "power 4 or greater") becomes `Keyword::Unknown`, leaving a
        // semantically-inert `TargetHasKeywordInstead { Unknown }` rider. That
        // mirrors pre-existing engine behavior for the target-gated "instead"
        // cards in that class (Bring Low, Strider, Urdnan), which have no real
        // keyword to mirror. Rejecting `Unknown` here would silently un-support
        // that unrelated class — out of scope for the Super-Adaptoid keyword
        // mirror, which only needs real keywords ("haste", captured before
        // " and ~ doesn't"). Honest counter/P-T gate support is deferred to its
        // own change.
        let keyword = Keyword::from_str(keyword_str).unwrap();
        // Optional " and ~ doesn't" lack conjunct (Super-Adaptoid class). `~` is
        // the normalized card name; the bare anaphora forms cover un-normalized
        // text.
        let (i, source_lacks) = opt((
            tag(" and "),
            alt((
                tag::<_, _, E>("~"),
                tag("this creature"),
                tag("this permanent"),
                tag("it"),
            )),
            tag(" doesn't"),
        ))
        .parse(i)
        .map(|(rest, matched)| (rest, matched.is_some()))?;
        // Connective ", " (optionally "; ") separates condition from the effect.
        let (i, _) = alt((tag(", "), tag("; "))).parse(i)?;
        let condition = if source_lacks {
            AbilityCondition::And {
                conditions: vec![
                    AbilityCondition::TargetHasKeywordInstead {
                        keyword: keyword.clone(),
                    },
                    AbilityCondition::SourceLacksKeyword { keyword },
                ],
            }
        } else {
            AbilityCondition::TargetHasKeywordInstead { keyword }
        };
        Ok((i, condition))
    });
    let Some((condition, body)) = parsed else {
        return (None, text.to_string());
    };
    let body = body.trim();
    // Structural cleanup of the already-extracted effect body (drop the
    // trailing "instead" override marker and the leading "it " pronoun), not
    // parsing dispatch — the condition has already been parsed above.
    let body = body.strip_suffix(" instead.").unwrap_or(body); // allow-noncombinator: effect-body suffix cleanup
    let body = body.strip_suffix(" instead").unwrap_or(body); // allow-noncombinator: effect-body suffix cleanup
    let body = body.strip_prefix("it ").unwrap_or(body); // allow-noncombinator: effect-body pronoun cleanup
    (Some(condition), body.to_string())
}

fn parse_counter_threshold(text: &str) -> Option<(Comparator, i32, CounterType, usize)> {
    let original_len = text.len();

    fn parse_counter_on_suffix(after_type: &str) -> Option<&str> {
        let (after_counter, _) = alt((tag::<_, _, OracleError<'_>>("counters"), tag("counter")))
            .parse(after_type)
            .ok()?;
        let (after_on, _) = alt((tag::<_, _, OracleError<'_>>("on it"), tag("on this")))
            .parse(after_counter.trim_start())
            .ok()?;
        Some(after_on)
    }

    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("no ").parse(text) {
        // CR 122.1 + CR 122.1b: shared counter-type combinator handles
        // multi-word keyword counter names.
        let (after_type, counter_type) = nom_primitives::parse_counter_type_typed(rest).ok()?;
        let after_type = after_type.trim_start();
        let after_on = parse_counter_on_suffix(after_type)?;
        let consumed = original_len - after_on.len();
        return Some((Comparator::EQ, 0, counter_type, consumed));
    }

    // CR 122.1 + CR 122.1a: an indefinite "a [type] counter" means one or more (>= 1).
    // Placed before the numeric branch, whose `parse_number` fails fast on the
    // article "a"/"an" (Oblivion's Hunger: "a +1/+1 counter on it").
    if let Ok((rest, _)) = nom_primitives::parse_article(text) {
        if let Ok((after_type, counter_type)) = nom_primitives::parse_counter_type_typed(rest) {
            let after_type = after_type.trim_start();
            if let Some(after_on) = parse_counter_on_suffix(after_type) {
                let consumed = original_len - after_on.len();
                return Some((Comparator::GE, 1, counter_type, consumed));
            }
        }
    }

    let (rest, threshold) = nom_primitives::parse_number.parse(text).ok()?;
    let rest = rest.trim_start();
    type E<'a> = OracleError<'a>;
    let (rest, comparator) = alt((
        value(Comparator::GE, tag::<_, _, E>("or more ")),
        value(Comparator::LE, tag("or fewer ")),
    ))
    .parse(rest)
    .ok()?;

    // CR 122.1 + CR 122.1b: shared counter-type combinator handles multi-word
    // keyword counter names (e.g. "double strike").
    let (after_type, counter_type) = nom_primitives::parse_counter_type_typed(rest).ok()?;
    let after_type = after_type.trim_start();
    let after_on = parse_counter_on_suffix(after_type)?;
    let consumed = original_len - after_on.len();
    Some((comparator, threshold as i32, counter_type, consumed))
}

fn build_counter_condition(
    comparator: Comparator,
    threshold: i32,
    counter_type: CounterType,
    scope: ObjectScope,
) -> AbilityCondition {
    AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::CountersOn {
                scope,
                counter_type: Some(counter_type),
            },
        },
        comparator,
        rhs: QuantityExpr::Fixed { value: threshold },
    }
}

pub(super) fn strip_counter_conditional(
    text: &str,
    in_trigger: bool,
) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Leading form: "if it has [N] [type] counter[s] on it, [effect]". The
    // subject here is ALWAYS the source ("it"); the demonstrative "that
    // creature"/"that permanent"/"that card" is deliberately NOT offered in the
    // leading position. A sentence-initial "If that creature has … counter …,
    // [source] deals N … instead" belongs to the target-gated *replacement*
    // class (Bring Low, Strider, Urdnan), whose honest counter/P-T gating is
    // deferred to the inert `strip_target_keyword_instead` rider (see the CR
    // 122.1b note there). Offering the demonstrative here would GATE OUT that
    // deferral and over-accept those cards into a false-green additive
    // `DealDamage` sibling (the "instead" is a replacement, not additive).
    // CR 115.1's demonstrative-as-target is honored only in the trailing form
    // below, where the leading-space needle can't collide with that class.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("if it has ").parse(lower.as_str()) {
        if let Some((comparator, threshold, counter_type, consumed)) = parse_counter_threshold(rest)
        {
            let after = rest[consumed..].trim_start();
            // allow-noncombinator: comma cleanup on the already-parsed remainder (the condition is parsed; not dispatch)
            let after = after.strip_prefix(',').unwrap_or(after).trim_start();
            let offset = text.len() - after.len();
            return (
                Some(build_counter_condition(
                    comparator,
                    threshold,
                    counter_type,
                    ObjectScope::Source,
                )),
                text[offset..].to_string(),
            );
        }
    }

    // Trailing form: "[effect] if {subject} has [N] [type] counter[s] on it".
    // "it" is always offered; the demonstrative "that creature"/"that permanent"/
    // "that card" only in non-trigger context (CR 115.1: the spell's target).
    let mut subjects: Vec<(&str, ObjectScope)> = vec![(" if it has ", ObjectScope::Source)];
    if !in_trigger {
        subjects.push((" if that creature has ", ObjectScope::Target));
        subjects.push((" if that permanent has ", ObjectScope::Target));
        subjects.push((" if that card has ", ObjectScope::Target));
    }
    for (needle, scope) in subjects {
        if let Some((before, after)) = tp.rsplit_around(needle) {
            if let Some((comparator, threshold, counter_type, consumed)) =
                parse_counter_threshold(after.lower)
            {
                let remaining = after.lower[consumed..].trim();
                if remaining.is_empty() || remaining == "." {
                    return (
                        Some(build_counter_condition(
                            comparator,
                            threshold,
                            counter_type,
                            scope,
                        )),
                        before.original.trim_end_matches('.').trim().to_string(),
                    );
                }
            }
        }
    }

    (None, text.to_string())
}

/// CR 202.3 + CR 608.2c: Strip trailing "if it has mana value N or less/greater" from
/// effect text. Returns a `TargetMatchesFilter` condition with `CmcLE`/`CmcGE` property.
/// Handles the class of cards that conditionally apply effects based on target mana value
/// (Fatal Push, Anoint with Affliction, Angrath, Cosmic Rebirth, etc.).
pub(super) fn strip_mana_value_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Leading position, past tense: "If its mana value was N or less/greater, [effect]."
    // CR 400.7: past-tense check → use_lki: true (LKI snapshot).
    if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("if its mana value was ").parse(lower.as_str())
    {
        if let Some((condition, body)) = parse_leading_mana_value_condition_body(text, rest, true) {
            return (Some(condition), body);
        }
    }

    // Leading position, present tense: "If it has mana value N or less/greater, [effect]."
    // CR 400.7: present-tense check → use_lki: false (current state). Covers Cosmic Rebirth.
    if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("if it has mana value ").parse(lower.as_str())
    {
        if let Some((condition, body)) = parse_leading_mana_value_condition_body(text, rest, false)
        {
            return (Some(condition), body);
        }
    }

    // Suffix position: "[effect] if its mana value was N or less/greater."
    if let Some((before, after)) = tp.rsplit_around(" if its mana value was ") {
        if let Some((comparator, threshold)) = parse_mana_value_threshold(after.lower) {
            let condition = AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::Cmc {
                        comparator,
                        value: QuantityExpr::Fixed { value: threshold },
                    },
                ])),
                use_lki: true,
                subject_slot: None,
            };
            return (
                Some(condition),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }

    // Suffix position: "[effect] if its mana value is less than or equal to [quantity]."
    if let Some((before, after)) = tp.rsplit_around(" if its mana value is ") {
        if let Some((comparator, value)) = parse_dynamic_mana_value_threshold(after.lower) {
            let condition = AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(
                    TypedFilter::default().properties(vec![FilterProp::Cmc { comparator, value }]),
                ),
                use_lki: false,
                subject_slot: None,
            };
            return (
                Some(condition),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }

    // Suffix position: "[effect] if it has mana value N or less/greater."
    if let Some((before, after)) = tp.rsplit_around(" if it has mana value ") {
        if let Some((comparator, threshold)) = parse_mana_value_threshold(after.lower) {
            let prop = match comparator {
                Comparator::LE => FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: threshold },
                },
                Comparator::GE => FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: threshold },
                },
                _ => return (None, text.to_string()),
            };
            let condition = AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![prop])),
                use_lki: false,
                subject_slot: None,
            };
            return (
                Some(condition),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }

    (None, text.to_string())
}

/// CR 608.2c: Strip trailing "if it has the [least|greatest] <property> among
/// <filter>" from a targeted spell effect (Wretched Banquet class).
pub(super) fn strip_superlative_target_conditional(
    text: &str,
) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let suffix_sep = " if ";
    let mut best: Option<(usize, AbilityCondition)> = None;
    for (idx, _) in lower.match_indices(suffix_sep) {
        let suffix_orig = &text[idx + suffix_sep.len()..];
        let suffix_lower = &lower[idx + suffix_sep.len()..];
        if let Some((condition, rest)) = nom_on_lower(suffix_orig, suffix_lower, |input| {
            terminated(
                parse_spell_target_superlative_suffix,
                (opt(tag(".")), multispace0),
            )
            .parse(input)
        }) {
            if rest.is_empty() {
                best = Some((idx, condition));
            }
        }
    }
    if let Some((split_at, condition)) = best {
        return (
            Some(condition),
            text[..split_at].trim_end_matches('.').trim().to_string(),
        );
    }
    (None, text.to_string())
}

/// Parse the body of a leading mana-value conditional — "`<N>` or less/greater, [effect]" —
/// and compute the body offset into `original`. `use_lki` is threaded into the constructed
/// `TargetMatchesFilter` condition: past-tense ("was") callers pass `true` (CR 400.7 — LKI
/// snapshot), present-tense ("has") callers pass `false` (current state).
fn parse_leading_mana_value_condition_body(
    original: &str,
    condition_and_body: &str,
    use_lki: bool,
) -> Option<(AbilityCondition, String)> {
    let (rest, threshold) = nom_primitives::parse_number(condition_and_body).ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" or ").parse(rest).ok()?;
    let (rest, comparator) = alt((
        value(Comparator::LE, tag::<_, _, OracleError<'_>>("less")),
        value(Comparator::GE, tag("greater")),
    ))
    .parse(rest)
    .ok()?;
    let rest = rest.trim_start();
    let (rest, _) = tag::<_, _, OracleError<'_>>(",").parse(rest).ok()?;
    let rest = rest.trim_start();
    let body_start = original.len() - rest.len();
    let condition = AbilityCondition::TargetMatchesFilter {
        filter: TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Cmc {
            comparator,
            value: QuantityExpr::Fixed {
                value: threshold as i32,
            },
        }])),
        use_lki,
        subject_slot: None,
    };
    Some((condition, original[body_start..].to_string()))
}

fn parse_dynamic_mana_value_threshold(text: &str) -> Option<(Comparator, QuantityExpr)> {
    let text = text.trim().trim_end_matches('.');
    let (rest, comparator) = alt((
        value(
            Comparator::LE,
            tag::<_, _, OracleError<'_>>("less than or equal to "),
        ),
        value(Comparator::LT, tag("less than ")),
        value(Comparator::GE, tag("greater than or equal to ")),
        value(Comparator::GT, tag("greater than ")),
        value(Comparator::EQ, tag("equal to ")),
    ))
    .parse(text)
    .ok()?;
    let (rest, qty) = nom_quantity::parse_quantity_ref.parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some((
        comparator,
        QuantityExpr::Ref {
            qty: canonicalize_quantity_ref(qty),
        },
    ))
}

pub(super) fn strip_target_supertype_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("if that land was nonbasic, ").parse(lower.as_str())
    {
        let body_start = text.len() - rest.len();
        return (
            Some(nonbasic_land_lki_condition()),
            text[body_start..].to_string(),
        );
    }

    if let Some((before, after)) = tp.rsplit_around(" if that land was ") {
        if all_consuming(alt((
            tag::<_, _, OracleError<'_>>("nonbasic."),
            tag("nonbasic"),
        )))
        .parse(after.lower.trim())
        .is_ok()
        {
            return (
                Some(nonbasic_land_lki_condition()),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }

    for (pattern, negated) in &[
        (" if it's ", false),
        (" if it is ", false),
        (" if it isn't ", true),
        (" if it's not ", true),
        (" if it is not ", true),
    ] {
        if let Some((before, after)) = tp.rsplit_around(pattern) {
            let supertype_text = after.lower.trim_end_matches('.').trim();
            let parsed = parse_supertype_word(supertype_text);
            let Ok((rest, supertype)) = parsed else {
                continue;
            };
            if !rest.trim().is_empty() {
                continue;
            }

            let condition = AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(
                    TypedFilter::default()
                        .properties(vec![FilterProp::HasSupertype { value: supertype }]),
                ),
                use_lki: false,
                subject_slot: None,
            };
            return (
                Some(maybe_negate(condition, *negated)),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }

    (None, text.to_string())
}

fn nonbasic_land_lki_condition() -> AbilityCondition {
    AbilityCondition::TargetMatchesFilter {
        filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
            FilterProp::NotSupertype {
                value: Supertype::Basic,
            },
        ])),
        use_lki: true,
        subject_slot: None,
    }
}

/// Parse "N or less" / "N or greater" from mana value threshold text.
/// Uses nom combinators to extract the numeric threshold and comparison direction.
fn parse_mana_value_threshold(text: &str) -> Option<(Comparator, i32)> {
    let text = text.trim().trim_end_matches('.');
    // Parse: number + " or " + "less"/"greater"
    let (rest, n) = nom_primitives::parse_number(text).ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" or ").parse(rest).ok()?;
    let (_, comparator) = alt((
        value(Comparator::LE, tag::<_, _, OracleError<'_>>("less")),
        value(Comparator::GE, tag("greater")),
    ))
    .parse(rest)
    .ok()?;
    Some((comparator, n as i32))
}

fn find_last_top_level_if(text: &str) -> Option<usize> {
    let mut last_pos = None;
    let mut paren_depth = 0u32;
    let mut in_quotes = false;

    for (index, ch) in text.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => paren_depth += 1,
            ')' if !in_quotes => paren_depth = paren_depth.saturating_sub(1),
            _ if !in_quotes && paren_depth == 0 && text[index..].starts_with(" if ") => {
                last_pos = Some(index);
            }
            _ => {}
        }
    }
    last_pos
}

/// CR 603.4 + CR 608.2h: Condition-text prefixes that cannot be re-homed onto
/// a clause-level `AbilityCondition` (`execute.condition`). Source-referential
/// conditions ("its power is", "it has", ...) and reflexive cost/choice
/// predicates ("able", "you do", "possible") have no `AbilityCondition` form;
/// such a post-effect `if` must hoist to `TriggerDefinition.condition` instead.
const NON_REHOMEABLE_CONDITION_PREFIXES: &[&str] = &[
    "able",
    "you do",
    "they do",
    "a player does",
    "no one does",
    "no player does",
    "possible",
    "it has ",
    "its power is ",
    "its toughness is ",
    "that creature has ",
    "that permanent has ",
    "you cast it from",
];

/// Single authority for the hoist-vs-rehome decision (CR 603.4). `true` if the
/// (lowercased, trimmed) condition fragment after `" if "` can be re-homed by
/// `strip_suffix_conditional` as a clause-level `AbilityCondition`.
pub(crate) fn condition_text_is_rehomeable(condition_text: &str) -> bool {
    // structural: not dispatch — membership test against a fixed exclusion
    // list (the verbatim pre-existing `excluded_prefixes` set), not parser
    // dispatch. The actual condition parsing is done downstream by
    // `parse_inner_condition` / `parse_condition_text`.
    //
    // Each prefix must match on a word boundary: a bare `starts_with` lets the
    // reflexive predicate `"you do"` swallow the control-presence condition
    // `"you don't control a Snail"` (Wick, the Whorled Mind) — the latter is
    // re-homeable as `Not(IsPresent)`, the former is the no-`AbilityCondition`
    // optional-effect signal. The check only applies to prefixes whose last
    // character is alphanumeric (e.g. "you do"): there the *following* character
    // in the text must not be alphanumeric, so "you do**n't**" (continues with
    // 'n') is rejected. Prefixes already ending in a space ("its power is ",
    // "it has ") carry their own boundary, so they exclude on a plain prefix
    // match and must NOT impose an extra boundary on the alphanumeric residual.
    !NON_REHOMEABLE_CONDITION_PREFIXES.iter().any(|prefix| {
        let prefix_ends_alnum = prefix
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric());
        // allow-noncombinator: word-boundary membership test against a fixed exclusion list, not parsing dispatch (parsing happens downstream in parse_inner_condition)
        condition_text.strip_prefix(prefix).is_some_and(|rest| {
            !prefix_ends_alnum || !rest.chars().next().is_some_and(|c| c.is_alphanumeric())
        })
    })
}

/// CR 707.10c: When a suffix condition is immediately followed by a copy-retarget
/// rider (", and you may choose new targets for the copy"), peel the rider off so
/// the condition parser sees only the predicate (Shiko and Narset, Unified).
fn peel_copy_retarget_tail_from_condition_text(condition_text: &str) -> (&str, Option<&str>) {
    let Ok((tail, prefix)) =
        terminated(take_until(", and "), tag::<_, _, OracleError<'_>>(", and "))
            .parse(condition_text)
    else {
        return (condition_text, None);
    };
    if super::sequence::recognize_copy_retarget_clause(tail) {
        (prefix.trim(), Some(tail.trim()))
    } else {
        (condition_text, None)
    }
}

fn parse_triggering_spell_targets_filter_ability_condition(text: &str) -> Option<AbilityCondition> {
    match crate::parser::oracle_condition::parse_spell_targets_filter(text)? {
        ParsedCondition::SpellTargetsFilter { filter } => {
            Some(AbilityCondition::TriggeringSpellTargetsFilter { filter })
        }
        _ => None,
    }
}

/// CR 608.2d + CR 107.4 + CR 202.1: Recognize "it has N or more colored mana
/// symbols in its mana cost" (Omnath, Locus of All — "You may reveal that card
/// if it has three or more colored mana symbols in its mana cost") into a
/// quantity-comparison condition on the target object. `color: None` counts each
/// colored mana symbol once regardless of color (CR 107.4a/107.4e/107.4f). The
/// comparator is a typed axis (GE here); "or fewer"/"exactly" variants are future
/// arms and must not be baked into the condition variant.
fn parse_colored_mana_symbol_count_target_condition(text: &str) -> Option<AbilityCondition> {
    let mut parser = all_consuming(preceded(
        tag::<_, _, OracleError<'_>>("it has "),
        terminated(
            nom_primitives::parse_number,
            tag(" or more colored mana symbols in its mana cost"),
        ),
    ));
    let (_, n) = parser.parse(text).ok()?;
    Some(AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ManaSymbolsInManaCost {
                scope: ObjectScope::Target,
                color: None,
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: n as i32 },
    })
}

pub(super) fn strip_suffix_conditional(
    text: &str,
    ctx: &mut ParseContext,
) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let Some(if_pos) = find_last_top_level_if(&lower) else {
        return (None, text.to_string());
    };

    let condition_text = lower[if_pos + " if ".len()..].trim_end_matches('.').trim();
    // CR 608.2d: "it has " is in NON_REHOMEABLE_CONDITION_PREFIXES, so this
    // source-referential mana-symbol eligibility check must be recognized BEFORE
    // the rehomeable bail or it would never run. effect_prefix/effect_text are
    // not computed yet, so return the stripped effect text directly.
    if let Some(cond) = parse_colored_mana_symbol_count_target_condition(condition_text) {
        return (Some(cond), text[..if_pos].trim().to_string());
    }
    // CR 201.5 + CR 208.1: source-referential "if its power is exactly N" (Amalia
    // Benavides Aguirre). "its power is " / "its toughness is " are in
    // NON_REHOMEABLE_CONDITION_PREFIXES, so — like the colored-mana check above —
    // this equality-only source P/T gate must run BEFORE the rehomeable bail or it
    // would never reach the condition parser. Fires solely on the "exactly N" form
    // (threshold forms are owned upstream by strip_property_conditional).
    if let Some(cond) = parse_source_pt_comparison_condition_text(condition_text) {
        return (Some(cond), text[..if_pos].trim().to_string());
    }
    if !condition_text_is_rehomeable(condition_text) {
        return (None, text.to_string());
    }

    let (condition_core, copy_retarget_tail) =
        peel_copy_retarget_tail_from_condition_text(condition_text);
    let effect_prefix = text[..if_pos].trim();
    let effect_prefix_lower = lower[..if_pos].trim();
    // CR 601.2f + CR 602.2b: do NOT peel the trailing "if [condition]" off a self
    // cost-reduction sentence; the whole sentence must reach try_parse_cost_reduction
    // (via strip_cost_reduction_node), whose own "if" arm re-homes the condition into
    // CostReduction.condition (a ParsedCondition) and applies the coverage-honesty
    // gate (unmodeled conditions stay a loud gap). #3223.
    if crate::parser::oracle_cost::is_self_cost_reduction_prefix(effect_prefix_lower) {
        return (None, text.to_string());
    }
    let effect_text = if let Some(tail) = copy_retarget_tail {
        format!("{effect_prefix}, and {tail}")
    } else {
        effect_prefix.to_string()
    };

    if let Some(cond) = parse_its_a_type_condition(condition_core, ctx) {
        return (Some(cond), effect_text);
    }

    if let Some(cond) = parse_no_mana_spent_to_cast_target_condition_text(condition_core) {
        return (Some(cond), effect_text);
    }

    if let Some(cond) = parse_additional_cost_paid_gate_condition_text(condition_core) {
        return (Some(cond), effect_text);
    }

    if let Some(cond) = parse_cast_using_teamwork_condition_text(condition_core) {
        return (Some(cond), effect_text);
    }

    if let Some(cond) = parse_mana_spent_vs_mana_value_target_condition_text(condition_core) {
        return (Some(cond), effect_text);
    }

    if let Some(condition) = parse_triggering_spell_targets_filter_ability_condition(condition_core)
        .or_else(|| try_nom_condition_as_ability_condition(condition_core, ctx))
        .or_else(|| parse_condition_text(condition_core))
        .or_else(|| parse_control_count_as_ability_condition(condition_core))
    {
        return (Some(condition), effect_text);
    }

    (None, text.to_string())
}

pub(super) fn parse_quantity_comparison(text: &str) -> Option<(Comparator, QuantityExpr)> {
    type E<'a> = OracleError<'a>;
    let mut comparator_prefixes = alt((
        value(Comparator::GE, tag::<_, _, E>("greater than or equal to ")),
        value(Comparator::LE, tag("less than or equal to ")),
        value(Comparator::GT, tag("greater than ")),
        value(Comparator::LT, tag("less than ")),
        value(Comparator::EQ, tag("equal to ")),
    ));

    if let Ok((rhs_text, comparator)) = comparator_prefixes.parse(text) {
        if let Some(rhs) = parse_cda_quantity(rhs_text) {
            return Some((comparator, rhs));
        }
    }
    if let Some((comparator, value)) = parse_comparison_suffix(text) {
        return Some((comparator, QuantityExpr::Fixed { value }));
    }
    None
}

/// CR 601.2h + CR 608.2c: "if no mana was spent to cast it/that spell" on a
/// targeted spell effect — the "it" anaphors to the ability's object target
/// (Nix, Defabricate-class riders).
fn parse_no_mana_spent_to_cast_target_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.to_ascii_lowercase();
    nom_parse_lower(&lower, |input| {
        all_consuming(parse_no_mana_spent_to_cast_target_condition).parse(input)
    })
}

fn parse_no_mana_spent_to_cast_target_condition(input: &str) -> OracleResult<'_, AbilityCondition> {
    let (rest, _) = (
        tag("no mana was spent to cast "),
        alt((tag("it"), tag("that spell"), tag("this spell"), tag("them"))),
    )
        .parse(input)?;
    Ok((
        rest,
        AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::AbilityTarget,
                    metric: CastManaSpentMetric::Total,
                },
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        },
    ))
}

/// CR 702.33d + CR 608.2c: "if it/that spell was kicked" suffix on a targeted
/// spell effect (Ertai's Trickery).
fn parse_additional_cost_paid_gate_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.to_ascii_lowercase();
    nom_parse_lower(&lower, |input| {
        all_consuming(parse_additional_cost_paid_gate_condition).parse(input)
    })
}

/// CR 601.2b + CR 702.33b (kicker) + CR 702.174b (gift): A plain conditional
/// gate ("[effect] if <additional cost was paid>") — as opposed to the
/// replacement "instead" fold — that all resolve to the shared
/// `additional_cost_paid_any()` runtime check. Covers kicked, the gift-promised
/// phrasing (Longstalk Brawl / Coiling Rebirth), and the generic
/// "additional cost was paid" wording (used inside `And` gates).
fn parse_additional_cost_paid_gate_condition(input: &str) -> OracleResult<'_, AbilityCondition> {
    let (rest, _) = alt((
        value(
            (),
            (
                alt((tag("it"), tag("that spell"), tag("this spell"))),
                tag(" was kicked"),
            ),
        ),
        value((), tag("the gift was promised")),
        value((), tag("this spell's additional cost was paid")),
        value((), tag("its additional cost was paid")),
    ))
    .parse(input)?;
    Ok((rest, AbilityCondition::additional_cost_paid_any()))
}

/// CR 601.2f + CR 608.2c: "<effect> if (this spell was | it was | it's) cast
/// using teamwork" trailing rider — gates the preceding effect specifically on
/// the Teamwork additional-cost payment (origin Teamwork), so a different
/// optional/imposed additional cost on the same spell does not satisfy it.
/// Mirrors `parse_was_kicked_condition_text`; reuses the shared phrase
/// combinator so the subject/tense axes live in one place.
fn parse_cast_using_teamwork_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.to_ascii_lowercase();
    nom_parse_lower(&lower, |input| {
        all_consuming(parse_cast_using_teamwork_condition).parse(input)
    })
}

fn parse_cast_using_teamwork_condition(input: &str) -> OracleResult<'_, AbilityCondition> {
    let (rest, ()) = parse_cast_using_teamwork_phrase(input)?;
    Ok((
        rest,
        AbilityCondition::additional_cost_paid_origin(AdditionalCostOrigin::Teamwork),
    ))
}

/// CR 601.2h + CR 608.2c: "if the amount of mana spent to cast it/that spell
/// was less than its mana value" on a targeted spell effect — the spell
/// anaphors to the ability's object target (Unravel-class riders).
fn parse_mana_spent_vs_mana_value_target_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.to_ascii_lowercase();
    nom_parse_lower(&lower, |input| {
        all_consuming(parse_mana_spent_vs_mana_value_target_condition).parse(input)
    })
}

fn parse_mana_spent_vs_mana_value_target_condition(
    input: &str,
) -> OracleResult<'_, AbilityCondition> {
    let (rest, (_, _, _, comparator, _)) = (
        tag("the amount of mana spent to cast "),
        alt((tag("it"), tag("that spell"), tag("this spell"))),
        alt((tag(" was "), tag(" is "))),
        alt((
            value(Comparator::LT, tag("less than")),
            value(Comparator::GT, tag("greater than")),
        )),
        tag(" its mana value"),
    )
        .parse(input)?;
    Ok((
        rest,
        AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::AbilityTarget,
                    metric: CastManaSpentMetric::Total,
                },
            },
            comparator,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Target,
                },
            },
        },
    ))
}

/// CR 122.1f + CR 109.4 + CR 608.2c: a poison predicate on the controller of the
/// ability's first object target — "if its controller is poisoned" (Corrupted
/// Resolve) and "if its controller has three or more poison counters" (the
/// Corrupted ability word: Bring the Ending, Anoint with Affliction). Both read
/// the SAME player counter through the SAME `QuantityRef` and the SAME `GE`
/// comparator; only the threshold differs, so the predicate is parameterized on
/// its threshold rather than split into sibling combinators. Mirrors
/// `parse_no_mana_spent_to_cast_target_condition_text`: reuses `QuantityCheck`
/// over an existing `QuantityRef` building block rather than a bespoke variant.
fn parse_target_controller_poisoned_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.to_ascii_lowercase();
    nom_parse_lower(&lower, |input| {
        all_consuming(parse_target_controller_poisoned_condition).parse(input)
    })
}

fn parse_target_controller_poisoned_condition(input: &str) -> OracleResult<'_, AbilityCondition> {
    let (rest, _) = (
        // Possessive subject anaphoring the object target: "its" (Corrupted
        // Resolve) and the demonstrative "that/this/the spell's" variants.
        alt((
            tag("its "),
            tag("that spell's "),
            tag("this spell's "),
            tag("the spell's "),
        )),
        tag("controller "),
    )
        .parse(input)?;
    let (rest, threshold) = parse_target_controller_poison_threshold(rest)?;
    Ok((
        rest,
        AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::TargetControllerCounter {
                    kind: crate::types::player::PlayerCounterKind::Poison,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: threshold },
        },
    ))
}

/// CR 122.1f: the poison threshold the predicate compares against. "poisoned" is
/// DEFINED as one or more poison counters, so it is exactly the `>= 1` case of
/// the same `<N> or more poison counters` shape the Corrupted ability word spells
/// out — one axis, two printings, not two combinators.
fn parse_target_controller_poison_threshold(input: &str) -> OracleResult<'_, i32> {
    alt((
        value(1, tag("is poisoned")),
        map(
            preceded(
                tag("has "),
                terminated(
                    nom_primitives::parse_number,
                    tag(" or more poison counters"),
                ),
            ),
            |count| count as i32,
        ),
    ))
    .parse(input)
}

pub(super) fn parse_condition_text(text: &str) -> Option<AbilityCondition> {
    let text = text.trim().trim_end_matches('.');

    if let Some(condition) = parse_no_mana_spent_to_cast_target_condition_text(text) {
        return Some(condition);
    }

    if let Some(condition) = parse_additional_cost_paid_gate_condition_text(text) {
        return Some(condition);
    }

    if let Some(condition) = parse_mana_spent_vs_mana_value_target_condition_text(text) {
        return Some(condition);
    }

    if let Some(condition) = parse_target_controller_poisoned_condition_text(text) {
        return Some(condition);
    }

    if let Some(condition) = parse_you_control_urza_land_types_condition_text(text) {
        return Some(condition);
    }

    if let Some(condition) = parse_you_control_of_each_condition_text(text) {
        return Some(condition);
    }

    if let Some(condition) = parse_controller_controlled_as_cast_condition_text(text) {
        return Some(condition);
    }

    if let Some(condition) = parse_cast_during_phase_condition_text(text) {
        return Some(condition);
    }

    if let Some(condition) = parse_mana_color_spent_condition_text(text) {
        return Some(condition);
    }

    if let Some(condition) = parse_paid_x_condition_text(text) {
        return Some(condition);
    }

    if let Some(condition) = parse_target_color_condition_text(text) {
        return Some(condition);
    }

    let lower = text.to_ascii_lowercase();
    if let Some(condition) = parse_cost_paid_object_matches_filter_condition(lower.as_str()) {
        return Some(condition);
    }

    let (lhs_text, comparator_rhs) = text.split_once(" is ")?;
    let lhs = parse_cda_quantity(lhs_text)?;
    let (comparator, rhs) = parse_quantity_comparison(comparator_rhs)?;
    Some(AbilityCondition::QuantityCheck {
        lhs,
        comparator,
        rhs,
    })
}

fn parse_you_control_urza_land_types_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.to_ascii_lowercase();
    let subtypes = nom_parse_lower(&lower, |i| {
        all_consuming(parse_you_control_urza_land_types).parse(i)
    })?;
    let conditions = subtypes
        .into_iter()
        .map(|subtype| AbilityCondition::ControllerControlsMatching {
            filter: TargetFilter::Typed(
                TypedFilter::land()
                    .subtype(subtype)
                    .controller(ControllerRef::You),
            ),
        })
        .collect();
    Some(AbilityCondition::And { conditions })
}

fn parse_you_control_urza_land_types(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, Vec<String>> {
    let (mut input, _) = tag::<_, _, OracleError<'_>>("you control ").parse(input)?;
    let (rest, first) = parse_urza_land_type(input)?;
    input = rest;
    let mut subtypes = vec![first];
    while let Ok((rest, subtype)) =
        preceded(tag::<_, _, OracleError<'_>>(" and "), parse_urza_land_type).parse(input)
    {
        subtypes.push(subtype);
        input = rest;
    }
    let (input, _) = opt(char('.')).parse(input)?;
    Ok((input, subtypes))
}

fn parse_urza_land_type(input: &str) -> super::super::oracle_nom::error::OracleResult<'_, String> {
    let (input, _) = alt((
        tag::<_, _, OracleError<'_>>("an "),
        tag::<_, _, OracleError<'_>>("a "),
    ))
    .parse(input)?;
    let (input, _) = tag("urza's ").parse(input)?;
    alt((
        value("Mine".to_string(), tag("mine")),
        value("Power-Plant".to_string(), tag("power-plant")),
        value("Tower".to_string(), tag("tower")),
    ))
    .parse(input)
}

/// CR 305.6: The five basic land types, in canonical order. A parser-local copy
/// (the deck-validation const is not `pub`) — do not cross-module-couple; both
/// enumerate the same fixed CR 305.6 set.
const BASIC_LAND_TYPES: [&str; 5] = ["Plains", "Island", "Swamp", "Mountain", "Forest"];

/// The noun a `"you control a <noun> of each <dimension>"` clause quantifies
/// over. Typed rather than stringly so the noun→`TypedFilter` mapping is an
/// exhaustive `match`, not a string comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OfEachNoun {
    Land,
    Creature,
    Permanent,
}

impl OfEachNoun {
    /// The base `TypedFilter` (type + `You` controller) this noun contributes,
    /// before the per-member dimension constraint is layered on. Exhaustive
    /// `match` so a new noun forces a decision here.
    fn base_filter(self) -> TypedFilter {
        let typed = match self {
            OfEachNoun::Land => TypedFilter::land(),
            OfEachNoun::Creature => TypedFilter::creature(),
            OfEachNoun::Permanent => TypedFilter::permanent(),
        };
        typed.controller(ControllerRef::You)
    }
}

/// CR 305.6: one member filter per basic land type — "a land of each basic land
/// type" requires a controlled land of that specific subtype.
fn basic_land_member_filter(noun: OfEachNoun, subtype: &str) -> TargetFilter {
    TargetFilter::Typed(noun.base_filter().subtype(subtype.to_string()))
}

/// CR 105.1: one member filter per color — "a <noun> of each color" requires a
/// controlled object of that color. `FilterProp::HasColor` is
/// `obj.color.contains(color)`, so a multicolored object counts for each of its
/// colors (a Gruul creature satisfies both the red and green members).
fn color_member_filter(noun: OfEachNoun, color: ManaColor) -> TargetFilter {
    TargetFilter::Typed(
        noun.base_filter()
            .properties(vec![FilterProp::HasColor { color }]),
    )
}

/// CR 104.2b + CR 608.2c: Recognize `"you control <clause> [and <clause>]…"`
/// where each clause is `"a <noun> of each <dimension>"`, and expand every clause
/// into `ControllerControlsMatching` members under one flat `AbilityCondition::And`.
/// Two dimensions are supported:
///   - `"a land of each basic land type"` → one member per CR 305.6 basic land
///     type (Plains/Island/Swamp/Mountain/Forest).
///   - `"a {creature|permanent} of each color"` → one member per CR 105.1 color
///     (WUBRG).
///
/// The leading `"you control"` possessive scopes ALL clauses (CR 608.2c: read the
/// whole sentence), so — unlike Urzatron's single-clause `" and "` list — the
/// conjunction is bound INSIDE this recognizer rather than by the generic
/// top-level splitter, which would strip the shared subject off the second
/// conjunct (`"a creature of each color"` has no `"you control"` of its own).
/// Motivating card: Coalition Victory ("You win the game if you control a land of
/// each basic land type and a creature of each color"). `evaluate_condition`
/// resolves the flat `And` identically to the nested form (`.all()`).
fn parse_you_control_of_each_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.to_ascii_lowercase();
    let member_filters = nom_parse_lower(&lower, |i| {
        all_consuming(parse_you_control_of_each_condition).parse(i)
    })?;
    let conditions = member_filters
        .into_iter()
        .map(|filter| AbilityCondition::ControllerControlsMatching { filter })
        .collect();
    Some(AbilityCondition::And { conditions })
}

/// Parses `"you control <clause> [and <clause>]…"` into the flat list of per-member
/// `TargetFilter`s across every clause. `all_consuming` at the call site forbids a
/// partial match (e.g. a trailing unparsed clause).
fn parse_you_control_of_each_condition(input: &str) -> OracleResult<'_, Vec<TargetFilter>> {
    let (mut input, _) = tag("you control ").parse(input)?;
    let (rest, mut filters) = parse_of_each_clause(input)?;
    input = rest;
    // The shared `"you control"` subject distributes over each `" and "`-joined
    // clause (CR 608.2c).
    while let Ok((rest, more)) =
        preceded(tag::<_, _, OracleError<'_>>(" and "), parse_of_each_clause).parse(input)
    {
        filters.extend(more);
        input = rest;
    }
    Ok((input, filters))
}

/// Parses one `"a <noun> of each <dimension>"` clause into its per-member
/// `TargetFilter`s (5 for either dimension), controller-scoped to `You`.
fn parse_of_each_clause(input: &str) -> OracleResult<'_, Vec<TargetFilter>> {
    let (input, noun) = alt((
        value(OfEachNoun::Land, tag::<_, _, OracleError<'_>>("a land")),
        value(OfEachNoun::Creature, tag("a creature")),
        value(OfEachNoun::Permanent, tag("a permanent")),
    ))
    .parse(input)?;
    // CR 305.6 / CR 105.1: the dimension determines the fixed member set. Land +
    // "basic land type" enumerates CR 305.6; any noun + "color" enumerates the
    // CR 105.1 colors.
    alt((
        map(tag(" of each basic land type"), move |_| {
            BASIC_LAND_TYPES
                .iter()
                .map(|subtype| basic_land_member_filter(noun, subtype))
                .collect()
        }),
        map(tag(" of each color"), move |_| {
            ManaColor::ALL
                .iter()
                .map(|&color| color_member_filter(noun, color))
                .collect()
        }),
    ))
    .parse(input)
}

fn parse_controller_controlled_as_cast_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.to_ascii_lowercase();
    nom_parse_lower(&lower, |input| {
        all_consuming(parse_controller_controlled_as_cast_condition).parse(input)
    })
}

fn parse_controller_controlled_as_cast_condition(
    input: &str,
) -> OracleResult<'_, AbilityCondition> {
    let (rest, _) = tag("you controlled ").parse(input)?;
    let (filter, remainder) = parse_type_phrase(rest);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let (rest, _) = tag("as you cast this spell").parse(remainder.trim_start())?;
    Ok((
        rest,
        AbilityCondition::ControllerControlledMatchingAsCast {
            filter: inject_controller_you(filter),
        },
    ))
}

fn parse_cast_during_phase_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.to_ascii_lowercase();
    nom_parse_lower(&lower, parse_cast_during_phase_condition)
        .map(|phases| AbilityCondition::CastDuringPhase { phases })
}

fn parse_cast_during_phase_condition(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, Vec<Phase>> {
    all_consuming(preceded(
        tag::<_, _, OracleError<'_>>("you cast this spell during your "),
        parse_phase_name_set,
    ))
    .parse(input)
}

/// CR 500.1 + CR 505.1 + CR 505.1a: Map a phase/step *name phrase* to the
/// concrete `Phase` set it denotes. Shared by the casting-time
/// `parse_cast_during_phase_condition` and the resolution-time
/// `parse_current_phase_condition`. NON-`all_consuming` by design — callers
/// wrap with `all_consuming` (or `preceded`) and own any trailing text. The
/// `alt` ordering is load-bearing: the grouped "main phase" (both main phases,
/// CR 505.1/505.1a) is tried before the "precombat"/"postcombat" refinements,
/// and "end of combat step" before "end step", so the longest/grouped phrase
/// wins.
fn parse_phase_name_set(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, Vec<Phase>> {
    alt((
        value(
            vec![Phase::PreCombatMain, Phase::PostCombatMain],
            tag::<_, _, OracleError<'_>>("main phase"),
        ),
        value(vec![Phase::PreCombatMain], tag("precombat main phase")),
        value(vec![Phase::PostCombatMain], tag("postcombat main phase")),
        value(vec![Phase::Upkeep], tag("upkeep")),
        value(vec![Phase::Draw], tag("draw step")),
        value(vec![Phase::BeginCombat], tag("beginning of combat step")),
        value(vec![Phase::DeclareAttackers], tag("declare attackers step")),
        value(vec![Phase::DeclareBlockers], tag("declare blockers step")),
        value(vec![Phase::CombatDamage], tag("combat damage step")),
        value(vec![Phase::EndCombat], tag("end of combat step")),
        value(vec![Phase::End], tag("end step")),
        value(vec![Phase::Cleanup], tag("cleanup step")),
    ))
    .parse(input)
}

/// CR 505.1 + CR 102.1 + CR 608.2c: "it is[n't] your [phase/step]" — the
/// resolution-time current-phase gate (CR 608.2c: read the whole text when the
/// ability resolves). The "your [phase]" possessive decomposes into two
/// orthogonal checks: `CurrentPhaseIs { phases }` (the live phase, via
/// `parse_phase_name_set`) AND `IsYourTurn` (CR 102.1: the active player is the
/// controller — "your" phase means a phase of your turn). The polarity prefix
/// selects negation, wrapping the conjunction in `Not`. NON-`all_consuming`:
/// the dispatcher wraps with `all_consuming` so the whole clause must be
/// consumed, which (together with the expletive "it") rules out any anaphoric
/// mis-binding.
fn parse_current_phase_condition(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, AbilityCondition> {
    let (rest, negated) = alt((
        value(
            true,
            alt((
                tag::<_, _, OracleError<'_>>("it isn't your "),
                tag("it is not your "),
                tag("it's not your "),
            )),
        ),
        value(false, alt((tag("it is your "), tag("it's your ")))),
    ))
    .parse(input)?;
    let (rest, phases) = parse_phase_name_set(rest)?;
    let condition = AbilityCondition::And {
        conditions: vec![
            AbilityCondition::CurrentPhaseIs { phases },
            AbilityCondition::IsYourTurn,
        ],
    };
    Ok((rest, maybe_negate(condition, negated)))
}

fn parse_mana_color_spent_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.to_ascii_lowercase();
    nom_parse_lower(&lower, parse_mana_color_spent_condition)
}

fn parse_mana_color_spent_condition(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, AbilityCondition> {
    all_consuming(alt((
        parse_symbolic_mana_color_spent_condition,
        parse_word_mana_color_spent_condition,
    )))
    .parse(input)
}

fn parse_symbolic_mana_color_spent_condition(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, AbilityCondition> {
    let (mut rest, first_color) = parse_basic_colored_mana_symbol(input)?;
    let mut counts = vec![(first_color, 1_u32)];
    while let Ok((after_symbol, color)) = parse_basic_colored_mana_symbol(rest) {
        if let Some((_, count)) = counts.iter_mut().find(|(seen, _)| *seen == color) {
            *count += 1;
        } else {
            counts.push((color, 1));
        }
        rest = after_symbol;
    }
    let (rest, _) = parse_spent_to_cast_tail(rest)?;
    let condition = if counts.len() == 1 {
        let (color, minimum) = counts[0];
        AbilityCondition::ManaColorSpent { color, minimum }
    } else {
        AbilityCondition::And {
            conditions: counts
                .into_iter()
                .map(|(color, minimum)| AbilityCondition::ManaColorSpent { color, minimum })
                .collect(),
        }
    };
    Ok((rest, condition))
}

fn parse_word_mana_color_spent_condition(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, AbilityCondition> {
    let (rest, _) = tag("at least ").parse(input)?;
    let (rest, minimum) = nom_primitives::parse_number(rest)?;
    let (rest, _) = tag(" ").parse(rest)?;
    let (rest, color) = nom_primitives::parse_color(rest)?;
    let (rest, _) = tag(" mana").parse(rest)?;
    let (rest, _) = parse_spent_to_cast_tail(rest)?;
    Ok((rest, AbilityCondition::ManaColorSpent { color, minimum }))
}

fn parse_spent_to_cast_tail(input: &str) -> super::super::oracle_nom::error::OracleResult<'_, ()> {
    value(
        (),
        preceded(
            tag(" was spent to cast "),
            alt((tag("this spell"), tag("it"), tag("them"), tag("~"))),
        ),
    )
    .parse(input)
}

fn parse_basic_colored_mana_symbol(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, ManaColor> {
    alt((
        value(ManaColor::White, tag("{w}")),
        value(ManaColor::Blue, tag("{u}")),
        value(ManaColor::Black, tag("{b}")),
        value(ManaColor::Red, tag("{r}")),
        value(ManaColor::Green, tag("{g}")),
    ))
    .parse(input)
}

fn parse_paid_x_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.to_ascii_lowercase();
    let (comparator, amount) = nom_parse_lower(&lower, |input| {
        all_consuming(|input| {
            let (rest, _) = tag::<_, _, OracleError<'_>>("x is ").parse(input)?;
            let (rest, amount) = nom_primitives::parse_number(rest)?;
            let (rest, comparator) = alt((
                value(Comparator::GE, tag::<_, _, OracleError<'_>>(" or more")),
                value(Comparator::LE, tag(" or less")),
                value(Comparator::GE, tag(" or greater")),
                value(Comparator::LE, tag(" or fewer")),
            ))
            .parse(rest)?;
            let (rest, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(rest)?;
            Ok((rest, (comparator, amount)))
        })
        .parse(input)
    })?;

    Some(AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::CostXPaid,
        },
        comparator,
        rhs: QuantityExpr::Fixed {
            value: amount as i32,
        },
    })
}

/// CR 614.1a + CR 614.6: the outcome of lowering an "instead" override clause.
///
/// The three-way split is load-bearing, and the middle arm is why this is not an
/// `Option`. CR 614.6 ("If an event is replaced, it never happens") makes an
/// "instead" override a **branch**, never a sequel: the engine does the override
/// or the printed effect, never both. When the override grammar matches but its
/// condition has no typed representation, returning a bare `None` let the caller
/// fall through and re-emit the override BODY as an ordinary unconditional chain
/// clause — so the engine ran the replaced effect AND its replacement, with the
/// condition dropped (Anax, Hardened in the Forge: a 2/2 dying produced three
/// Satyrs instead of one). `ConditionUnlowerable` forces the caller to fail
/// honestly instead.
pub(super) enum InsteadLowering {
    /// The override lowered to a conditional branch (`AbilityCondition::ConditionInstead`).
    Branch(Box<AbilityDefinition>),
    /// The text IS an "instead" override, but its condition has no typed variant
    /// yet. The caller MUST emit `Effect::unimplemented` — never the body.
    ConditionUnlowerable,
    /// Not the generic "instead" grammar, or deliberately owned by a more
    /// specific lowering (see the additional-cost deferral in `build_instead_def`).
    NotOwned,
}

pub(super) fn try_parse_generic_instead_clause(
    text: &str,
    kind: AbilityKind,
    ctx: &mut ParseContext,
) -> InsteadLowering {
    // Forward form: "If <cond>, [body] instead." — split on the leading "If, "
    // and strip a trailing/leading "instead" from the body.
    if let Some((cond_text, effect_text)) = split_forward_instead_clause(text) {
        return build_instead_def(cond_text, effect_text, kind, ctx);
    }

    // CR 614.1a + CR 608.2c: Inverted form — "[body] instead if <cond>." (e.g.
    // Scepter of Empires). Same semantic as the forward form but with the
    // condition trailing the override body. The chunk-level mid-text
    // `" instead if "` boundary mirrors the line-level `strip_instead_clause`
    // in `oracle.rs` but operates on a single chunk inside the chain loop.
    if let Some((cond_text, effect_text)) = split_inverted_instead_clause(text) {
        return build_instead_def(cond_text, effect_text, kind, ctx);
    }

    InsteadLowering::NotOwned
}

/// Forward instead form: "If <cond>, [body] instead." Returns the trimmed
/// `(condition_text, effect_text)` if the leading-conditional + trailing-or-
/// leading "instead" structure matches. Returns None otherwise.
fn split_forward_instead_clause(text: &str) -> Option<(String, String)> {
    let (condition_fragment, raw_body) = split_leading_conditional(text)?;
    let condition_lower = condition_fragment.to_lowercase();
    let cond_text = nom_on_lower(&condition_fragment, &condition_lower, |i| {
        value((), tag("if ")).parse(i)
    })
    .map(|((), rest)| rest)
    .unwrap_or(&condition_fragment)
    .trim()
    .to_string();

    let trimmed_body = raw_body.trim_end_matches('.').trim();
    let trimmed_lower = trimmed_body.to_lowercase();
    let effect_text = if let Some(stripped) = trimmed_body.strip_suffix(" instead") {
        stripped.trim().to_string()
    } else if let Some((_, rest)) = nom_on_lower(trimmed_body, &trimmed_lower, |i| {
        value((), tag("instead ")).parse(i)
    }) {
        rest.trim().to_string()
    } else {
        return None;
    };

    Some((cond_text, effect_text))
}

/// Inverted instead form: "[body] instead if <cond>." Returns the trimmed
/// `(condition_text, effect_text)` if the chunk contains the mid-text
/// `" instead if "` boundary. Returns None otherwise. The body must be
/// non-empty after stripping "instead"; the condition is the suffix.
fn split_inverted_instead_clause(text: &str) -> Option<(String, String)> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let (before, after) = tp.rsplit_around(" instead if ")?;
    let effect_text = before.original.trim().trim_end_matches('.').trim();
    let cond_text = after.original.trim().trim_end_matches('.').trim();
    if effect_text.is_empty() || cond_text.is_empty() {
        return None;
    }
    Some((cond_text.to_string(), effect_text.to_string()))
}

/// CR 614.1: replacement effects "watch for a particular event that **would**
/// happen and completely or partially replace that event." The modal "would" is
/// the Comprehensive Rules' own marker for the EVENT reading of an "instead"
/// clause, and it splits the grammar in two:
///
/// - **CR 614.1a EVENT replacement** — "If that spell *would* be put into your
///   graveyard, exile it instead" (Torrential Gearhulk, Goblin Dark-Dwellers,
///   Mission Briefing; ~68 faces). The clause names an event, not a game state.
///   These are owned elsewhere — by a `ReplacementDefinition`, by the line-level
///   replacement parser, or by the structural cast-then-exile rider chain that
///   `swallow_check::any_ability_has_exile_parent_rider` recognizes as the "exile
///   it instead" encoding. An unlowerable EVENT condition must therefore fall
///   through UNCHANGED: reporting it as `ConditionUnlowerable` would make the
///   caller replace a *working* rider encoding with `Effect::unimplemented`.
///
/// - **CR 608.2c STATE override** — "If the creature had power 4 or greater,
///   create two of those tokens instead" (Anax, Hardened in the Forge). The
///   clause is a game-state predicate evaluated as the ability resolves. These,
///   and only these, are `ConditionUnlowerable` when they fail to lower.
///
/// The scan is word-boundary anchored (`scan_contains`), so "would" is matched
/// as a word and never as a fragment of a longer token.
fn condition_names_an_event(cond_text: &str) -> bool {
    nom_primitives::scan_contains(&cond_text.to_lowercase(), "would")
}

/// CR 608.2c + CR 614.15: the SINGLE AUTHORITY for lowering the condition of an
/// "instead" self-replacement override — wherever that override is printed.
///
/// A self-replacement override can be printed inside the clause chain it replaces
/// ("… create two of those tokens instead", handled by `build_instead_def`) or as
/// a SEPARATE ability on its own line, "particularly when preceded by an ability
/// word" (CR 614.15 — "Corrupted — Counter that spell instead if …", handled by
/// the cross-line binder in `oracle.rs`). Both callers MUST lower the condition
/// through here.
///
/// They previously did not: the cross-line path ran only the nom `StaticCondition`
/// grammar, which cannot express a target-relative predicate, so a condition this
/// ladder lowers fine ("its controller has three or more poison counters") was
/// dropped there. The override then reached the emitter with `condition: None`
/// and was published as an UNCONDITIONAL sibling ability — the engine ran the base
/// effect AND the replacement, every time (CR 614.6). One authority, one
/// vocabulary, so the two paths cannot drift apart again.
pub(crate) fn lower_instead_condition(
    cond_text: &str,
    ctx: &mut ParseContext,
) -> Option<AbilityCondition> {
    try_nom_condition_as_ability_condition(cond_text, ctx)
        .or_else(|| parse_condition_text(cond_text))
        .or_else(|| parse_control_count_as_ability_condition(cond_text))
}

/// Shared assembly: build an `AbilityDefinition` for an instead override.
/// Lowers the condition through `lower_instead_condition`; bails if it does not
/// lower (so the chunk can fall through to other dispatch paths). Wraps the result
/// in `ConditionInstead` per CR 608.2c and rewrites cost-paid-object quantity
/// references when needed.
fn build_instead_def(
    cond_text: String,
    effect_text: String,
    kind: AbilityKind,
    ctx: &mut ParseContext,
) -> InsteadLowering {
    // CR 608.2c: An additional-cost-paid "instead" fold ("if it/this spell was
    // kicked, ... instead") is owned by `strip_additional_cost_conditional`,
    // which folds it to the dedicated `AdditionalCostPaidInstead`. Defer here so
    // the generic `parse_condition_text` recognizer (which now classifies "was
    // kicked" as the bare `AdditionalCostPaid`) does not pre-empt that fold by
    // producing a `ConditionInstead { inner: AdditionalCostPaid }` wrapper.
    // This deferral is `NotOwned`, NOT `ConditionUnlowerable`: the clause has a
    // typed home, just not this one, so the caller must fall through to it.
    if parse_additional_cost_instead_condition_fragment(&cond_text).is_some() {
        return InsteadLowering::NotOwned;
    }

    // CR 614.6: a replaced event never happens. If the override's condition has
    // no typed representation, the branch cannot be built — and emitting the
    // override body anyway would run BOTH the replaced effect and its
    // replacement. Everything that lowers today still lowers; only the failure
    // path is split, by `condition_names_an_event`, into "defer" vs "fail
    // honestly".
    let Some(condition) = lower_instead_condition(&cond_text, ctx) else {
        return if condition_names_an_event(&cond_text) {
            InsteadLowering::NotOwned
        } else {
            InsteadLowering::ConditionUnlowerable
        };
    };

    let instead_def = parse_effect_chain(&effect_text, kind);
    let mut result = instead_def;
    result.condition = Some(AbilityCondition::ConditionInstead {
        inner: Box::new(condition),
    });
    if result
        .condition
        .as_ref()
        .is_some_and(super::condition_refs_cost_paid_object)
    {
        super::rewrite_cost_paid_object_quantities_in_definition(&mut result);
    }
    InsteadLowering::Branch(Box::new(result))
}

/// CR 614.15: an "instead" marker replaces "part or all" of the ability's effect,
/// and the printed grammar says WHICH. A bare trailing "instead" replaces the whole
/// clause ("create two of those tokens instead"). A trailing "instead of <N>"
/// replaces only a PART — the antecedent's quantity — and names the OLD value it is
/// displacing ("put up to two creature cards … into your hand instead of one",
/// "search your library for up to three basic Forest cards instead of two").
///
/// That trailing "of <N>" is redundant with the antecedent, which still holds the old
/// value, so it carries nothing the branch needs: it is a marker, not an operand.
/// This recognizes both printings as the same replacement marker so the alternative
/// grammar is not blocked by the one that spells out what it replaces.
///
/// Anything else after "instead" ("instead of putting it into your hand") names a
/// non-quantity part and is NOT accepted here — that is a different replacement axis,
/// and silently treating it as a whole-clause marker would drop the part it names.
fn is_replacement_marker_tail(after_instead_lower: &str) -> bool {
    let tail = after_instead_lower.trim().trim_end_matches('.').trim();
    if tail.is_empty() {
        return true;
    }
    nom_parse_lower(tail, |input| {
        all_consuming(preceded(tag("of "), nom_primitives::parse_number)).parse(input)
    })
    .is_some()
}

/// CR 608.2c: "If <cond>, you may instead <reveal-N-from-among-body>" — conditional
/// alternative selection for a preceding `Effect::Dig`. The "instead" body re-uses
/// the preceding Dig's source (top N cards) but swaps keep_count/up_to/filter/destination.
///
/// Handles patterns like Follow the Lumarets:
///   "Look at the top four cards of your library. You may reveal a creature or land
///    card from among them and put it into your hand. If you gained life this turn,
///    you may instead reveal two creature and/or land cards from among them and put
///    them into your hand."
///
/// Returns a new AbilityDefinition carrying the alternative Dig plus condition; the
/// caller wraps the preceding Dig as `else_ability`. Class coverage: any card of form
/// "look at top N / reveal a <filter> card from among them ... if <cond>, you may
/// instead reveal M <filter'> cards from among them" (CR 608.2c replacement effect).
pub(crate) fn try_parse_dig_instead_alternative(
    text: &str,
    previous: Option<&AbilityDefinition>,
    kind: AbilityKind,
    ctx: &mut ParseContext,
) -> Option<AbilityDefinition> {
    // Gate: previous effect must be a Dig that the alternative can piggy-back on.
    let prev = previous?;
    let Effect::Dig {
        player: prev_player,
        count: prev_count,
        rest_destination: prev_rest,
        reveal: prev_reveal,
        ..
    } = &*prev.effect
    else {
        return None;
    };

    let (cond_text, body_rest, has_instead_marker) =
        if let Some((condition_fragment, raw_body)) = split_leading_conditional(text) {
            let condition_lower = condition_fragment.to_lowercase();
            let cond_text = nom_on_lower(&condition_fragment, &condition_lower, |i| {
                value((), tag("if ")).parse(i)
            })
            .map(|((), rest)| rest)
            .unwrap_or(&condition_fragment)
            .trim()
            .to_string();

            // Strip "you may instead " / "instead " / "you may " from the body to
            // get the bare reveal-from-among clause. Composed with nom combinators;
            // the "you may instead" arm is first so it wins over "you may ". Some
            // cards print the replacement marker at the end instead ("put two ...
            // instead"), so accept a trailing marker as the same alternative grammar.
            let trimmed_body = raw_body.trim_end_matches('.').trim();
            let body_lower = trimmed_body.to_lowercase();
            let (prefix_had_instead, body_rest) = nom_on_lower(trimmed_body, &body_lower, |i| {
                alt((
                    value(true, tag::<_, _, OracleError<'_>>("you may instead ")),
                    value(true, tag("instead ")),
                    value(false, tag("you may ")),
                ))
                .parse(i)
            })
            .unwrap_or((false, trimmed_body));

            let body_rest_lower = body_rest.to_lowercase();
            let body_rest_pair = TextPair::new(body_rest, &body_rest_lower);
            let (body_rest, suffix_had_instead) =
                if let Some((before, after)) = body_rest_pair.split_around(" instead") {
                    if is_replacement_marker_tail(after.lower) {
                        (before.original.trim(), true)
                    } else {
                        (body_rest, false)
                    }
                } else {
                    (body_rest, false)
                };
            (
                cond_text,
                body_rest.to_string(),
                prefix_had_instead || suffix_had_instead,
            )
        } else if let Some((cond_text, effect_text)) = split_inverted_instead_clause(text) {
            let trimmed_body = effect_text.trim_end_matches('.').trim();
            let body_lower = trimmed_body.to_lowercase();
            let body_rest = nom_on_lower(trimmed_body, &body_lower, |i| {
                value((), tag::<_, _, OracleError<'_>>("you may ")).parse(i)
            })
            .map(|((), rest)| rest)
            .unwrap_or(trimmed_body)
            .trim()
            .to_string();
            (cond_text, body_rest, true)
        } else {
            return None;
        };
    if !has_instead_marker {
        return None;
    }

    let body_rest_lower = body_rest.to_lowercase();
    let alt_continuation = parse_dig_from_among(&body_rest_lower, &body_rest)?;
    let ContinuationAst::DigFromAmong {
        quantity: alt_quantity,
        filter: alt_filter,
        destination: alt_destination,
        rest_destination: alt_rest,
        enter_tapped: alt_enter_tapped,
        ..
    } = alt_continuation
    else {
        return None;
    };
    // CR 701.20e: Map the typed `PutCount` onto the Dig's keep_count/up_to.
    // `u32::MAX` is an unbounded parser sentinel; the Dig resolver clamps it
    // to the number of seen cards.
    let (alt_keep_count, alt_keep_count_expr, alt_up_to) = alt_quantity.to_dig_keep();

    // CR 601.2f + CR 608.2c: a teamwork-gated "put ... from among them ...
    // instead" alternative reuses the preceding Dig's source; the base
    // selection runs from else_ability when Teamwork wasn't paid. Appended
    // last — the teamwork phrase is disjoint from all four arms above, so the
    // ordering is purely defensive.
    let condition = parse_additional_cost_instead_condition_fragment(&cond_text)
        .or_else(|| try_nom_condition_as_ability_condition(&cond_text, ctx))
        .or_else(|| parse_condition_text(&cond_text))
        .or_else(|| parse_control_count_as_ability_condition(&cond_text))
        .or_else(|| parse_cast_using_teamwork_condition_text(&cond_text))?;

    // Clone the preceding Dig's source (top N) and reveal-mode, apply alternative
    // selection parameters. `rest_destination` prefers the alternative's inline value
    // (same-clause "and the rest on the bottom..."); otherwise falls back to the
    // preceding Dig's (already-patched or None — a trailing PutRest continuation
    // patches both branches by rewriting into the chain).
    let alt_effect = Effect::Dig {
        player: prev_player.clone(),
        count: prev_count.clone(),
        destination: alt_destination,
        keep_count: alt_keep_count,
        keep_count_expr: alt_keep_count_expr,
        up_to: alt_up_to,
        filter: alt_filter,
        rest_destination: alt_rest.or(*prev_rest),
        reveal: *prev_reveal,
        enter_tapped: alt_enter_tapped,
        source: DigSource::Library,
    };

    let mut result = AbilityDefinition::new(kind, alt_effect);
    result.condition = Some(condition);
    Some(result)
}

pub(crate) fn parse_additional_cost_instead_condition_fragment(
    text: &str,
) -> Option<AbilityCondition> {
    let lower = text.trim().to_lowercase();
    let parsed = all_consuming(alt((
        tag::<_, _, OracleError<'_>>("this spell was kicked"),
        tag("it was kicked"),
        tag("this spell was bargained"),
        tag("it was bargained"),
        tag("this spell was beheld"),
        tag("it was beheld"),
        tag("this spell's additional cost was paid"),
        tag("its additional cost was paid"),
        tag("evidence was collected"),
        tag("the gift was promised"),
    )))
    .parse(lower.as_str())
    .is_ok();
    parsed.then_some(AbilityCondition::AdditionalCostPaidInstead)
}

fn parse_control_count_as_ability_condition(text: &str) -> Option<AbilityCondition> {
    let text = text.trim();
    let (rest, _) = tag::<_, _, OracleError<'_>>("you control ")
        .parse(text)
        .ok()?;

    let (type_rest, _) = tag::<_, _, OracleError<'_>>("fewer ").parse(rest).ok()?;
    let pos = type_rest.find(" than ")?;
    let type_text = &type_rest[..pos];
    let (mut filter, leftover) = parse_type_phrase(type_text);
    if filter == TargetFilter::Any || !leftover.trim().is_empty() {
        return None;
    }
    if let TargetFilter::Typed(ref mut typed) = filter {
        typed.controller = Some(ControllerRef::You);
    }
    let mut opponent_filter = filter.clone();
    if let TargetFilter::Typed(ref mut typed) = opponent_filter {
        typed.controller = Some(ControllerRef::Opponent);
    }
    Some(AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        },
        comparator: Comparator::LT,
        rhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: opponent_filter,
            },
        },
    })
}

/// CR 122.1 + CR 608.2c: Build an `AbilityCondition` from a counter-threshold
/// `(minimum, maximum)` pair against a counter-quantity expression. Shared by
/// the typed (`CountersOnSelf`) and any-type (`AnyCountersOnSelf`) arms of
/// `static_condition_to_ability_condition` so both round-trip identically.
fn counter_threshold_to_condition(
    qty: QuantityExpr,
    minimum: u32,
    maximum: Option<u32>,
) -> AbilityCondition {
    match (minimum, maximum) {
        // "no counters on ~" — exactly zero.
        (0, Some(0)) => AbilityCondition::QuantityCheck {
            lhs: qty,
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        },
        // "exactly N counters on ~"
        (n, Some(m)) if n == m => AbilityCondition::QuantityCheck {
            lhs: qty,
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
        // "N or fewer counters on ~"
        (0, Some(n)) => AbilityCondition::QuantityCheck {
            lhs: qty,
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
        // "N or more counters on ~" / "a counter on ~" (1+)
        (n, None) => AbilityCondition::QuantityCheck {
            lhs: qty,
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
        // Bounded range "between N and M counters" — express as compound
        // via `And` so each side stays a single QuantityCheck.
        (n, Some(m)) => AbilityCondition::And {
            conditions: vec![
                AbilityCondition::QuantityCheck {
                    lhs: qty.clone(),
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: n as i32 },
                },
                AbilityCondition::QuantityCheck {
                    lhs: qty,
                    comparator: Comparator::LE,
                    rhs: QuantityExpr::Fixed { value: m as i32 },
                },
            ],
        },
    }
}

/// CR 608.2c: Compose a `QuantityExpr::Difference` from a two-operand quantity
/// comparison condition — the unsigned magnitude gap between the operands, as
/// referenced by "a number of times equal to the difference" repeat suffixes.
/// "The difference" is an anaphoric back-reference to the operands the earlier
/// condition text established (read the whole text, apply the rules of English).
///
/// Class-general: any `AbilityCondition::QuantityCheck` yields the difference
/// of its operands. Both `lhs` and `rhs` are already `QuantityExpr`, so they
/// are cloned directly — no fresh `Ref`/`Fixed` reconstruction. `Difference`
/// resolves via `.abs()` (`fold_compose`), so operand order and comparator
/// direction are irrelevant. Returns `None` for non-comparison conditions.
pub(super) fn difference_expr(cond: &AbilityCondition) -> Option<QuantityExpr> {
    match cond {
        AbilityCondition::QuantityCheck { lhs, rhs, .. } => Some(QuantityExpr::Difference {
            left: Box::new(lhs.clone()),
            right: Box::new(rhs.clone()),
        }),
        _ => None,
    }
}

/// CR 122.1f: Bridge `StaticCondition::OpponentPoisonAtLeast` to an existential
/// `QuantityCheck` over opponents whose poison total meets the threshold
/// ("an opponent has N or more poison counters" unless gates).
fn opponent_poison_at_least_as_quantity_check(count: u32) -> AbilityCondition {
    use crate::types::ability::{PlayerFilter, PlayerRelation, QuantityRef};
    use crate::types::player::PlayerCounterKind;

    AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::PlayerAttribute {
                    relation: PlayerRelation::Opponent,
                    attr: Box::new(QuantityRef::PlayerCounter {
                        kind: PlayerCounterKind::Poison,
                        scope: CountScope::ScopedPlayer,
                    }),
                    comparator: Comparator::GE,
                    value: Box::new(QuantityExpr::Fixed {
                        value: i32::try_from(count).unwrap_or(i32::MAX),
                    }),
                },
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    }
}

/// Bridge a `StaticCondition` (from the nom condition parser) to an
/// `AbilityCondition`. Returns `None` for variants that have no
/// effect-resolution equivalent — the caller falls through to the next strategy.
///
/// Exhaustive on purpose — when you add a `StaticCondition` variant, decide
/// here whether it bridges (CLAUDE.md: bridges must be kept exhaustive).
pub(crate) fn static_condition_to_ability_condition(
    sc: &StaticCondition,
    ctx: &mut ParseContext,
) -> Option<AbilityCondition> {
    match sc {
        StaticCondition::DuringYourTurn => Some(AbilityCondition::IsYourTurn),
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => Some(AbilityCondition::QuantityCheck {
            lhs: lhs.clone(),
            comparator: *comparator,
            rhs: rhs.clone(),
        }),
        StaticCondition::HasMaxSpeed => Some(AbilityCondition::HasMaxSpeed),
        // CR 103.1: Starting-player status — 1:1 bridge (same `controller` field).
        StaticCondition::WasStartingPlayer { controller } => {
            Some(AbilityCondition::WasStartingPlayer {
                controller: controller.clone(),
            })
        }
        // CR 702.185c: "a spell was warped this turn" — 1:1 bridge (same `variant`).
        StaticCondition::SpellCastWithVariantThisTurn { variant } => {
            Some(AbilityCondition::SpellCastWithVariantThisTurn {
                variant: *variant,
            })
        }
        StaticCondition::IsMonarch => Some(AbilityCondition::IsMonarch),
        StaticCondition::IsInitiative => Some(AbilityCondition::IsInitiative),
        StaticCondition::HasCityBlessing => Some(AbilityCondition::HasCityBlessing),
        StaticCondition::IsRingBearer => Some(AbilityCondition::IsRingBearer),
        StaticCondition::OpponentPoisonAtLeast { count } => {
            Some(opponent_poison_at_least_as_quantity_check(*count))
        }
        StaticCondition::DayNightIs { state } => {
            Some(AbilityCondition::DayNightIs { state: *state })
        }
        StaticCondition::SharesColorWithMostCommonColorAmongPermanents => None,
        StaticCondition::SourceEnteredThisTurn => None,
        StaticCondition::WasCast { .. } => None,
        StaticCondition::IsPresent { filter } => {
            let filter = match filter {
                Some(f) => f.clone(),
                None => {
                    ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
                        context: "IsPresent condition has no filter".into(),
                        text: String::new(),
                        line_index: 0,
                    });
                    TargetFilter::Any
                }
            };
            Some(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { filter },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
        }
        StaticCondition::Not { condition } => match condition.as_ref() {
            StaticCondition::DuringYourTurn => Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::IsYourTurn),
            }),
            StaticCondition::SourceEnteredThisTurn => Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::SourceEnteredThisTurn),
            }),
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => Some(AbilityCondition::QuantityCheck {
                lhs: lhs.clone(),
                comparator: comparator.negate(),
                rhs: rhs.clone(),
            }),
            StaticCondition::IsPresent { filter } => {
                let filter = match filter {
                    Some(f) => f.clone(),
                    None => {
                        ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
                            context: "NegatedIsPresent has no filter".into(),
                            text: String::new(),
                            line_index: 0,
                        });
                        TargetFilter::Any
                    }
                };
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    },
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Fixed { value: 0 },
                })
            }
            // CR 611.2b: Not(SourceIsTapped) → source is untapped.
            StaticCondition::SourceIsTapped => Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::SourceIsTapped),
            }),
            StaticCondition::SourceMatchesFilter { filter } => Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::SourceMatchesFilter {
                    filter: filter.clone(),
                }),
            }),
            // CR 702.171b + CR 601.2b: "~ isn't saddled" → the source does NOT
            // match the saddled-designation filter. Mirrors the affirmative
            // `SourceIsSaddled` bridge (the source's saddled status is a runtime
            // `FilterProp::IsSaddled` property, so the negation composes as
            // `Not { SourceMatchesFilter { Typed([IsSaddled]) } }`). Drives
            // Caustic Bronco's "you lose life … if ~ isn't saddled" attack trigger.
            StaticCondition::SourceIsSaddled => Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::SourceMatchesFilter {
                    filter: source_saddled_filter(),
                }),
            }),
            StaticCondition::DayNightIs { state } => Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::DayNightIs { state: *state }),
            }),
            // CR 103.1: "you weren't the starting player" → Not(WasStartingPlayer).
            StaticCondition::WasStartingPlayer { controller } => Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::WasStartingPlayer {
                    controller: controller.clone(),
                }),
            }),
            // CR 702.185c: "no spell was warped this turn" → Not(SpellCastWithVariantThisTurn).
            StaticCondition::SpellCastWithVariantThisTurn { variant } => {
                Some(AbilityCondition::Not {
                    condition: Box::new(AbilityCondition::SpellCastWithVariantThisTurn {
                        variant: *variant,
                    }),
                })
            }
            other => static_condition_to_ability_condition(other, ctx).map(|inner| {
                AbilityCondition::Not {
                    condition: Box::new(inner),
                }
            }),
        },
        StaticCondition::SourceMatchesFilter { filter } => {
            Some(AbilityCondition::SourceMatchesFilter {
                filter: filter.clone(),
            })
        }
        StaticCondition::SourceIsTapped => Some(AbilityCondition::SourceIsTapped),
        // CR 702.171b + CR 601.2b: Bridge the source's saddled designation to the
        // effect-resolution seam. The static-layer predicate (`SourceIsSaddled`)
        // has no dedicated `AbilityCondition` variant, but the designation is a
        // permanent property the runtime filter already evaluates
        // (`FilterProp::IsSaddled` → `obj.is_saddled`), so the present-tense gate
        // composes as `SourceMatchesFilter { Typed([IsSaddled]) }` against the
        // ability's source. Drives Caustic Bronco's "if ~ isn't saddled" attack
        // trigger (via the `Not` sub-match below).
        StaticCondition::SourceIsSaddled => Some(AbilityCondition::SourceMatchesFilter {
            filter: source_saddled_filter(),
        }),
        // CR 301.5 + CR 303.4: Bridge the source-attached predicate to the
        // effect-resolution seam. Used by bestow triggers whose optional
        // payment / copy-token branch must only fire when the Aura is
        // attached, while the surrounding trigger (and its fallback
        // continuation) still resolves when unattached — Springheart Nantuko's
        // landfall ability.
        StaticCondition::SourceAttachedToCreature => {
            Some(AbilityCondition::SourceAttachedToCreature)
        }
        // CR 608.2c: Compound static predicates map recursively to ability
        // conditions. If any child is unmappable, reject the whole compound so
        // the parser does not silently drop part of the condition.
        StaticCondition::And { conditions } => {
            let mapped: Option<Vec<_>> = conditions
                .iter()
                .map(|c| static_condition_to_ability_condition(c, ctx))
                .collect();
            Some(AbilityCondition::And {
                conditions: mapped?,
            })
        }
        StaticCondition::Or { conditions } => {
            let mapped: Option<Vec<_>> = conditions
                .iter()
                .map(|c| static_condition_to_ability_condition(c, ctx))
                .collect();
            Some(AbilityCondition::Or {
                conditions: mapped?,
            })
        }
        // CR 122.1 + CR 608.2c: Counter-threshold gate on the source object.
        // Maps to `QuantityCheck { CountersOn(Self|AnyCountersOnSelf), Comparator, Fixed }`
        // so the existing sub-ability condition evaluator handles it without
        // new runtime support. `CounterMatch::OfType(ct)` reads a single typed
        // counter via `CountersOnSelf`; `CounterMatch::Any` ("no counters on
        // it" / "a counter on it") sums every type via `AnyCountersOnSelf`.
        StaticCondition::HasCounters {
            counters,
            minimum,
            maximum,
        } => {
            let qty = QuantityExpr::Ref {
                qty: match counters {
                    CounterMatch::OfType(ct) => QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: Some(ct.clone()),
                    },
                    CounterMatch::Any => QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: None,
                    },
                },
            };
            Some(counter_threshold_to_condition(qty, *minimum, *maximum))
        }
        // CR 508.1k + CR 608.2c: source-anaphoric mid-effect "if he's/she's/they're
        // attacking" rider. `SourceIsAttacking` has no dedicated `AbilityCondition`
        // variant, but "attacking" is a runtime `FilterProp` the resolver already
        // evaluates against the ability source, so the gate composes as
        // `SourceMatchesFilter` against the source — mirroring the `SourceIsSaddled`
        // bridge above. Drives The Incredible Hulk's Enrage ("untap him and there is
        // an additional combat phase") gate.
        StaticCondition::SourceIsAttacking => Some(AbilityCondition::SourceMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter {
                properties: vec![FilterProp::Attacking { defender: None }],
                ..Default::default()
            }),
        }),
        // CR 509.1g + CR 608.2c: same bridge for "he's/she's/they're blocking".
        StaticCondition::SourceIsBlocking => Some(AbilityCondition::SourceMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter {
                properties: vec![FilterProp::Blocking],
                ..Default::default()
            }),
        }),
        // CR 506.5 + CR 608.2c: same bridge for "it's attacking alone".
        StaticCondition::SourceAttackingAlone => Some(AbilityCondition::SourceMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter {
                properties: vec![FilterProp::AttackingAlone],
                ..Default::default()
            }),
        }),
        // CR 309.7 ("A player completes a dungeon as that dungeon card is removed from the
        // game") + CR 608.2c: "if you've completed a dungeon" is a plain game-state
        // predicate about the CONTROLLER, evaluated as the ability resolves — it is not
        // source-bound, layer-bound, or cost-bound like the unbridgeable statics below.
        // `AbilityCondition::CompletedDungeon { specific }` is its exact effect-resolution
        // equivalent and has existed all along; listing `CompletedADungeon` among the
        // "no equivalent -> None" arms was a vocabulary asymmetry, not a real gap.
        //
        // It went unnoticed while an unlowerable condition merely fell through silently.
        // Once the CR 614 branch guard started reporting such conditions honestly, the
        // asymmetry surfaced as a regression: Tomb of Horrors Adventurer's "If you've
        // completed a dungeon, copy that spell twice instead" lost its branch to
        // `Unimplemented`. The `specific: None` form matches the bare "a dungeon" reading;
        // a specific-dungeon static has no `StaticCondition` spelling today, so there is
        // nothing else to map here.
        StaticCondition::CompletedADungeon => {
            Some(AbilityCondition::CompletedDungeon { specific: None })
        }
        StaticCondition::DevotionGE { .. }
        // CR 702.176a + CR 611.3a: Persistent alternative-cost markers are
        // source-bound static predicates with no effect-resolution
        // `AbilityCondition` equivalent.
        | StaticCondition::CastVariantPaid { .. }
        | StaticCondition::ChosenColorIs { .. }
        // CR 614.12c + CR 607.2d: Anchor-word linked statics are evaluated
        // by `layers::evaluate_condition_with_context`; no effect-resolution
        // `AbilityCondition` equivalent (the gate only makes sense for a
        // static ability bound to the persisted source).
        | StaticCondition::ChosenLabelIs { .. }
        | StaticCondition::SpeedGE { .. }
        | StaticCondition::ClassLevelGE { .. }
        | StaticCondition::RecipientHasCounters { .. }
        | StaticCondition::RecipientMatchesFilter { .. }
        // CR 509.1b: recipient-scoped block-evasion gate; no effect-resolution
        // (`AbilityCondition`) equivalent — lowering returns `None`.
        | StaticCondition::RecipientAttackingOwnerTarget { .. }
        | StaticCondition::SourceInZone { .. }
        | StaticCondition::DefendingPlayerControls { .. }
        | StaticCondition::SourceIsBlocked
        | StaticCondition::SourceIsEquipped
        | StaticCondition::SourceIsEnchanted
        | StaticCondition::SourceIsPaired
        | StaticCondition::SourceIsMonstrous
        // CR 701.64b: the harnessed designation gates triggered/static abilities
        // (via `TriggerCondition::SourceIsHarnessed` / Layer 6), never an
        // effect-resolution-time `AbilityCondition` — mirror `SourceIsMonstrous`.
        | StaticCondition::SourceIsHarnessed
        | StaticCondition::UnlessPay { .. }
        | StaticCondition::Unrecognized { .. }
        | StaticCondition::RingLevelAtLeast { .. }
        | StaticCondition::ControlsCommander { .. }
        | StaticCondition::EnchantedIsFaceDown
        // CR 311.2 / CR 901.7: plane face-up status is a duration-only continuous-
        // effect condition (evaluated in the layer system), never an
        // effect-resolution-time `AbilityCondition` — lowering returns `None`.
        | StaticCondition::SourceIsFaceUp
        | StaticCondition::SourceControllerEquals { .. }
        // CR 702.166a: Bargain payment is a cost-determination predicate with no
        // effect-resolution (`AbilityCondition`) equivalent.
        | StaticCondition::AdditionalCostPaid
        // CR 725.1: "there is no monarch" is a trigger-only intervening-if; no
        // `AbilityCondition` monarch variant beyond `IsMonarch` exists.
        | StaticCondition::NoMonarch
        // CR 702.11b + CR 120.3: "has dealt damage since entering" is a static-only
        // Layer-6 gate (the conditional hexproof grant) with no effect-resolution
        // (`AbilityCondition`) equivalent. `Not(SourceHasDealtDamage)` is handled by
        // the inner Not sub-match's `_ => None` arm above.
        | StaticCondition::SourceHasDealtDamage
        // CR 110.5b + CR 611.2b: `IsTapped { scope }` is a duration-only
        // target-relative tap condition (Zygon Infiltrator's copy duration), not
        // an effect-resolution gate — no `AbilityCondition` equivalent.
        | StaticCondition::IsTapped { .. }
        | StaticCondition::CastingAsVariant { .. }
        // CR 401.1 + CR 401.5: the top-of-library gate is a continuous-static-only
        // predicate (Layer 6 / duration `ForAsLongAs`); no effect-resolution
        // (`AbilityCondition`) equivalent — lowering returns `None`.
        | StaticCondition::TopOfLibraryMatches { .. }
        | StaticCondition::None => None,
    }
}

/// Partial inverse of [`static_condition_to_ability_condition`].
///
/// CR 603.4 + CR 608.2h: When an in-effect `if <condition>` on a continuous
/// keyword-grant clause must be gated per-`StaticDefinition` (Odric, Lunarch
/// Marshal — each granted keyword has its own presence gate), lowering needs
/// the condition back as a `StaticCondition` so it can ride on each
/// `StaticDefinition` rather than gating the whole `AbilityDefinition`.
///
/// Only the variants that `strip_suffix_conditional` can emit for such a
/// clause are inverted; anything else returns `None`, leaving the condition on
/// `AbilityDefinition.condition` as before. The `QuantityCheck { ObjectCount,
/// GE, 1 }` shape — the bridge target of `IsPresent` — is restored to
/// `IsPresent` so the keyword-swap path (`rewrite_condition_keyword`) handles
/// it uniformly.
///
/// Exhaustive on purpose — when you add an `AbilityCondition` variant, decide
/// here whether it bridges (CLAUDE.md: bridges must be kept exhaustive).
pub(crate) fn ability_condition_to_static_condition(
    ac: &AbilityCondition,
) -> Option<StaticCondition> {
    match ac {
        AbilityCondition::IsYourTurn => Some(StaticCondition::DuringYourTurn),
        // CR 309.7: round-trips the bridge in `static_condition_to_ability_condition`.
        // Only the "any dungeon" reading round-trips — `StaticCondition` has no
        // specific-dungeon spelling, so a specific one has no static form to bridge to.
        AbilityCondition::CompletedDungeon { specific: None } => {
            Some(StaticCondition::CompletedADungeon)
        }
        AbilityCondition::CompletedDungeon { specific: Some(_) } => None,
        // CR 301.5 + CR 303.4: round-trips the bidirectional bridge in
        // `static_condition_to_ability_condition` (a continuous "attached to a
        // creature" gate can ride per-`StaticDefinition`).
        AbilityCondition::SourceAttachedToCreature => {
            Some(StaticCondition::SourceAttachedToCreature)
        }
        AbilityCondition::QuantityCheck {
            lhs,
            comparator,
            rhs,
        } => {
            // `IsPresent`'s bridge target: ObjectCount(filter) >= 1.
            if let (
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { filter },
                },
                Comparator::GE,
                QuantityExpr::Fixed { value: 1 },
            ) = (lhs, comparator, rhs)
            {
                return Some(StaticCondition::IsPresent {
                    filter: Some(filter.clone()),
                });
            }
            Some(StaticCondition::QuantityComparison {
                lhs: lhs.clone(),
                comparator: *comparator,
                rhs: rhs.clone(),
            })
        }
        AbilityCondition::Not { condition } => Some(StaticCondition::Not {
            condition: Box::new(ability_condition_to_static_condition(condition)?),
        }),

        // Casting-context conditions — read `SpellContext` / cast history at
        // resolution time; no continuous-evaluation (`StaticCondition`)
        // equivalent.
        AbilityCondition::AdditionalCostPaid { .. }
        | AbilityCondition::AdditionalCostPaidInstead
        | AbilityCondition::AlternativeManaCostPaid
        | AbilityCondition::WasCast { .. }
        | AbilityCondition::CastDuringPhase { .. }
        | AbilityCondition::CastTimingPermission { .. }
        | AbilityCondition::ManaColorSpent { .. }
        | AbilityCondition::ControllerControlledMatchingAsCast { .. }
        | AbilityCondition::CastVariantPaid { .. }
        | AbilityCondition::CastVariantPaidInstead { .. } => None,

        // Resolution-flow conditions — read in-resolution signals (effect
        // outcomes, reveals, resolved targets, zone-change events, player-scope
        // iteration); only meaningful inside `resolve_ability_chain`, never as
        // a continuous-effect gate.
        AbilityCondition::EffectOutcome { .. }
        | AbilityCondition::EventOutcomeWon
        | AbilityCondition::CoinFlipOutcome { .. }
        | AbilityCondition::WhenYouDo
        | AbilityCondition::RevealedHasCardType { .. }
        | AbilityCondition::ObjectsShareQuality { .. }
        | AbilityCondition::TargetSharesNameWithOtherExiledThisWay { .. }
        | AbilityCondition::PreviousEffectAmount { .. }
        | AbilityCondition::TargetHasKeywordInstead { .. }
        | AbilityCondition::TargetMatchesFilter { .. }
        // CR 601.2c + CR 115.1: reads the resolved ability's declared targets;
        // a resolution-flow guard with no continuous-effect (StaticCondition) form.
        | AbilityCondition::HasObjectTarget
        | AbilityCondition::TriggeringSpellTargetsFilter { .. }
        | AbilityCondition::ZoneChangeObjectMatchesFilter { .. }
        | AbilityCondition::ZoneChangedThisWay { .. }
        | AbilityCondition::CostPaidObjectMatchesFilter { .. }
        | AbilityCondition::ConditionInstead { .. }
        | AbilityCondition::NthResolutionThisTurn { .. }
        | AbilityCondition::ScopedPlayerMatches { .. } => None,

        // No `StaticCondition` counterpart exists for these game-state
        // predicates.
        AbilityCondition::FirstCombatPhaseOfTurn
        | AbilityCondition::FirstEndStepOfTurn
        | AbilityCondition::CurrentPhaseIs { .. }
        | AbilityCondition::DayNightIsNeither
        | AbilityCondition::SourceLacksKeyword { .. } => None,

        // A `StaticCondition` counterpart exists, but `strip_suffix_conditional`
        // never emits these shapes for per-`StaticDefinition` keyword-grant
        // gates, so the condition stays on `AbilityDefinition.condition` as
        // before. Invert here if the lowering path ever needs them.
        AbilityCondition::SourceEnteredThisTurn
        | AbilityCondition::HasMaxSpeed
        | AbilityCondition::IsMonarch
        | AbilityCondition::IsInitiative
        | AbilityCondition::HasCityBlessing
        | AbilityCondition::IsRingBearer
        | AbilityCondition::WasStartingPlayer { .. }
        | AbilityCondition::SpellCastWithVariantThisTurn { .. }
        | AbilityCondition::SourceIsTapped
        | AbilityCondition::SourceMatchesFilter { .. }
        | AbilityCondition::PostReplacementDamageSourceMatchesFilter { .. }
        | AbilityCondition::DayNightIs { .. }
        | AbilityCondition::ControllerControlsMatching { .. }
        | AbilityCondition::And { .. }
        | AbilityCondition::Or { .. } => None,
    }
}

/// CR 508.1a + CR 608.2c: "you attacked with <filter> [this turn]" — a filtered
/// attack-history gate. Recognizes the count form ("N or more creatures",
/// filter `None`), the token / commander / self forms, and a trailing type or
/// subtype phrase, producing a `QuantityCheck` against the (optionally filtered)
/// `AttackedThisTurn` count. Covers Neyali ("a token"), Neriv ("a commander"),
/// Boros Strike-Captain ("three or more creatures"), Goblin Researcher ("~").
fn parse_attacked_with_filter_condition(text: &str) -> Option<AbilityCondition> {
    let trimmed = text.trim().trim_end_matches('.').trim();
    let lower = trimmed.to_lowercase();
    // Strip "you['ve] attacked with ".
    let ((), after_verb) = nom_on_lower(trimmed, &lower, |i| {
        value(
            (),
            preceded(
                alt((tag::<_, _, OracleError<'_>>("you've "), tag("you "))),
                tag("attacked with "),
            ),
        )
        .parse(i)
    })?;
    // Strip an optional trailing " this turn".
    let after_verb = after_verb.trim();
    let after_lower = after_verb.to_lowercase();
    // CR 508.6: drop a trailing " this turn" if present. The closure yields `()`
    // (the `take_until` prefix borrows the lowercase local and must not escape);
    // the body is sliced from the ORIGINAL text using the mapped-back remainder.
    let body = match nom_on_lower(after_verb, &after_lower, |i| {
        value(
            (),
            terminated(
                take_until::<_, _, OracleError<'_>>(" this turn"),
                tag(" this turn"),
            ),
        )
        .parse(i)
    }) {
        Some(((), remainder)) => {
            &after_verb[..after_verb.len() - remainder.len() - " this turn".len()]
        }
        None => after_verb,
    }
    .trim();
    let body_lower = body.to_lowercase();

    let make = |filter: Option<TargetFilter>, count: i32| {
        Some(AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::AttackedThisTurn {
                    scope: CountScope::Controller,
                    filter,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: count },
        })
    };

    // Count form: "<N> [or more] creature(s)" — unfiltered attacker count.
    if let Ok((rest, n)) = nom_primitives::parse_number(body_lower.as_str()) {
        let rest = rest.trim_start();
        let (rest, _) = opt(tag::<_, _, OracleError<'_>>("or more "))
            .parse(rest)
            .ok()?;
        if matches!(rest.trim(), "creatures" | "creature") {
            return make(None, n as i32);
        }
    }

    // Self-reference (Goblin Researcher "attacked with ~").
    if matches!(body_lower.as_str(), "~" | "this creature" | "it") {
        return make(Some(TargetFilter::SelfRef), 1);
    }

    // Drop a leading article, then recognize token / commander / a type or
    // subtype phrase. A bare "a creature" is the unfiltered count of 1.
    let noun = nom_on_lower(body, &body_lower, |i| {
        value((), alt((tag::<_, _, OracleError<'_>>("a "), tag("an ")))).parse(i)
    })
    .map(|((), rest)| rest)
    .unwrap_or(body)
    .trim();
    let noun_lower = noun.to_lowercase();
    if noun_lower == "creature" {
        return make(None, 1);
    }
    if noun_lower == "token" {
        return make(
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Token]),
            )),
            1,
        );
    }
    if noun_lower == "commander" {
        return make(
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::IsCommander]),
            )),
            1,
        );
    }
    // Type / subtype phrase ("a Wolf or Werewolf", etc.).
    let (filter, rest) = parse_type_phrase(noun);
    if rest.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
        return make(Some(filter), 1);
    }
    None
}

/// Subject of an anaphoric status predicate ("it's tapped" / "~ is suspected").
/// Distinguishes the two condition seams the status maps to: the chosen target
/// (`Anaphoric` → `TargetMatchesFilter`, evaluated against the ability's first
/// object target / triggering subject) versus the ability's own permanent
/// (`SelfSource` → `SourceMatchesFilter`, evaluated against `ability.source_id`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusSubject {
    /// "it" / "that creature" — the trigger/ability's chosen object target.
    Anaphoric,
    /// "~" / "this creature" — the ability's own source permanent.
    SelfSource,
}

/// Map a self-subject permanent-status `FilterProp` to its dedicated precise
/// `AbilityCondition` variant, if one exists. Returns `None` for predicates that
/// the engine only models as a runtime filter property (no precise variant), so
/// the caller falls back to `SourceMatchesFilter`.
///
/// CLAUDE.md prefers the precise typed variant over a generic filter. The
/// anaphoric self-subject status arm (Repeat Offender's "~ is suspected", etc.)
/// must therefore NOT shadow the established `parse_inner_condition` →
/// `static_condition_to_ability_condition` mapping that produces these precise
/// variants for the same printed text. "tapped" has `SourceIsTapped`
/// (CR 110.5: tapped/untapped is a permanent status); "suspected" has no
/// `SourceIsSuspected` (CR 701.60b), so it returns `None`.
fn precise_source_condition_for_prop(prop: &FilterProp) -> Option<AbilityCondition> {
    match prop {
        // CR 110.5: "~ is tapped" reads the source's tapped status → the
        // dedicated tapped predicate.
        FilterProp::Tapped => Some(AbilityCondition::SourceIsTapped),
        _ => None,
    }
}

/// CR 608.2c + CR 601.2b: Categorical anaphoric/self status recognizer for the
/// "if <subject> is[n't] <status>" intervening clause that gates an `else`
/// branch. Composes three orthogonal axes — none enumerated as full-string
/// `tag()`s:
///   - subject: "it" / "that creature" (the chosen target, longest-first to
///     absorb the "it's" / "it isn't" contractions) vs. "~" / "this creature"
///     (the source). The subject choice selects which condition seam is used.
///   - copula/polarity: affirmative ("is " / "'s ") vs. negated ("isn't " /
///     "is not ").
///   - predicate: a permanent-status `FilterProp` the runtime filter already
///     evaluates (CR 701.60b suspected, CR 110.5 tapped).
///
/// Returns the typed `(StatusSubject, negated, FilterProp)` triple; the caller
/// builds the `Typed([prop])` filter, picks `TargetMatchesFilter` (Anaphoric) or
/// `SourceMatchesFilter` (SelfSource), and wraps in `Not` when negated. Covers
/// Shackle Slinger ("it's tapped"), Agrus Kos ("it's suspected"), and Repeat
/// Offender ("~ is suspected" on a self-targeting activated ability).
fn parse_anaphoric_status_predicate(
    input: &str,
) -> OracleResult<'_, (StatusSubject, bool, FilterProp)> {
    let (rest, subject) = alt((
        // The "it's"/"it isn't" contractions share the "it" stem; consume the
        // bare subject token here (no trailing space) and let the copula axis
        // below absorb the separator uniformly via `opt(char(' '))`.
        value(
            StatusSubject::Anaphoric,
            alt((tag("that creature"), tag("it"))),
        ),
        value(
            StatusSubject::SelfSource,
            alt((tag("this creature"), tag("~"))),
        ),
    ))
    .parse(input)?;
    // Single optional separator between subject and copula — handles both the
    // contracted "it's" (no space) and the spaced "it is" / "~ is" forms.
    let (rest, _) = opt(char(' ')).parse(rest)?;
    let (rest, negated) = alt((
        value(true, alt((tag("isn't "), tag("is not ")))),
        value(false, alt((tag("'s "), tag("is ")))),
    ))
    .parse(rest)?;
    let (rest, prop) = alt((
        // CR 701.60b: suspected designation. CR 110.5: tapped status.
        value(FilterProp::Suspected, tag("suspected")),
        value(FilterProp::Tapped, tag("tapped")),
    ))
    .parse(rest)?;
    Ok((rest, (subject, negated, prop)))
}

/// CR 702.119a-c + CR 702.187b: Recognize "[possessive subject] <emerge|mayhem>
/// cost was paid" as an `AbilityCondition::CastVariantPaid { variant }`. Only
/// Emerge (Adipose Offspring) and Mayhem (Sandman's Quicksand) route through the
/// generic trailing-"instead" path (`ConditionInstead`, via `build_instead_def`);
/// the pre-existing sneak / ninjutsu / surge / spectacle / prowl "instead" cards
/// keep their established `strip_additional_cost_conditional` (Route-2 →
/// `CastVariantPaidInstead`) path, so the membership filter prevents any
/// regression. Dispatch is nom `tag()`, never contains/find. `lower` is the
/// already-lowercased condition fragment with any leading "if " stripped.
fn parse_cast_variant_cost_paid_condition(lower: &str) -> Option<AbilityCondition> {
    use crate::parser::oracle_trigger::CAST_VARIANT_COST_PAID_PHRASES;
    // Optional possessive subject ("this creature's" / "this spell's" /
    // "this permanent's" / "its" / "~'s") — normalization, not dispatch.
    let (input, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("this creature's "),
        tag("this spell's "),
        tag("this permanent's "),
        tag("its "),
        tag("~'s "),
        tag("~’s "),
    )))
    .parse(lower)
    .ok()?;
    CAST_VARIANT_COST_PAID_PHRASES
        .iter()
        .filter(|&&(_, variant)| {
            matches!(variant, CastVariantPaid::Emerge | CastVariantPaid::Mayhem)
        })
        .find_map(|&(phrase, variant)| {
            let (rest, _) = tag::<_, _, OracleError<'_>>(phrase).parse(input).ok()?;
            // CR 603.4: allow an optional "this turn" tail, then require the
            // fragment to be fully consumed so partial matches don't leak.
            let (rest, _) = opt(tag::<_, _, OracleError<'_>>(" this turn"))
                .parse(rest)
                .ok()?;
            rest.trim()
                .is_empty()
                .then_some(AbilityCondition::CastVariantPaid {
                    variant,
                    subject: ObjectScope::Source,
                })
        })
}

/// CR 702.185a: keyword tokens recognized inside "was cast for its <variant>
/// cost". Table-driven `value(variant, tag(phrase))` dispatch so the class
/// extends by adding a row — any future `CastVariantPaid` member whose cards
/// use the "was cast for its X cost" phrasing slots in here. Today only Warp
/// (Full Bore) uses this target-scoped phrasing.
const CAST_FOR_VARIANT_PHRASES: &[(&str, CastVariantPaid)] = &[("warp", CastVariantPaid::Warp)];

/// Consume a `<variant>` keyword from `CAST_FOR_VARIANT_PHRASES`, returning the
/// typed `CastVariantPaid`. Pure nom `value(variant, tag(phrase))` over the
/// table — never `contains`/`find`/`split`.
fn parse_cast_for_variant(input: &str) -> OracleResult<'_, CastVariantPaid> {
    for &(phrase, variant) in CAST_FOR_VARIANT_PHRASES {
        if let Ok((rest, v)) = value(variant, tag::<_, _, OracleError<'_>>(phrase)).parse(input) {
            return Ok((rest, v));
        }
    }
    Err(nom::Err::Error(OracleError::new(
        input,
        nom::error::ErrorKind::Tag,
    )))
}

/// CR 115.1 + CR 608.2c + CR 702.185a: target-scoped "<that creature> was cast
/// for its <variant> cost [this turn]" rider — Full Bore's "if that creature was
/// cast for its warp cost, it also gains trample and haste". "that creature"
/// anaphors to the +3/+2 target permanent (CR 115.1), so the produced condition
/// is `subject: ObjectScope::Target` — distinct from the source-scoped "if its
/// warp cost was paid" form (CR 113.7). Pure nom: `parse_target_anaphoric_subject`
/// (which cannot match "a spell", so there is no overlap with the turn-wide
/// `SpellCastWithVariantThisTurn` path) + the " was cast for its " / " cost"
/// anchors + the table-driven variant dispatch. The caller wraps this in
/// `all_consuming`, so the " cost" anchor plus full consumption forbid any
/// partial / mis-bound match.
fn parse_target_cast_variant_paid_condition(input: &str) -> OracleResult<'_, AbilityCondition> {
    let (rest, _) = parse_target_anaphoric_subject(input)?;
    let (rest, _) = tag(" was cast for its ").parse(rest)?;
    let (rest, variant) = parse_cast_for_variant(rest)?;
    let (rest, _) = tag(" cost").parse(rest)?;
    let (rest, _) = opt(tag(" this turn")).parse(rest)?;
    Ok((
        rest,
        AbilityCondition::CastVariantPaid {
            variant,
            subject: ObjectScope::Target,
        },
    ))
}

/// Text wrapper for `parse_target_cast_variant_paid_condition`: lowercases,
/// trims a trailing period, and requires full consumption (`all_consuming`).
fn parse_target_cast_variant_paid_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.trim().trim_end_matches('.').to_ascii_lowercase();
    let parsed = all_consuming(parse_target_cast_variant_paid_condition)
        .parse(lower.as_str())
        .ok()
        .map(|(_, c)| c);
    parsed
}

/// CR 608.2c: Parse an " or if "-connected disjunction of condition clauses into
/// `AbilityCondition::Or`. Each disjunct is a full condition (the connective
/// re-introduces "if"), so the clause is split on every " or if " boundary and
/// each side is recursed back through `try_nom_condition_as_ability_condition`.
///
/// The binding is all-or-nothing: if ANY disjunct fails to parse, the whole
/// function returns None so the caller leaves the gate unrepresented (honest
/// `Condition_If` fallthrough) rather than firing the effect on a partial
/// condition. The disjuncts themselves carry no " or if ", so the recursion
/// short-circuits on the guard below — no unbounded recursion.
fn parse_or_if_disjunction(text: &str, ctx: &mut ParseContext) -> Option<AbilityCondition> {
    let lower = text.to_lowercase();
    fn split_on_or_if(input: &str) -> Option<(&str, &str)> {
        terminated(
            take_until::<_, _, OracleError<'_>>(" or if "),
            tag::<_, _, OracleError<'_>>(" or if "),
        )
        .parse(input)
        .ok()
        .map(|(rest, disjunct)| (disjunct, rest))
    }
    // The first split doubles as the guard: no " or if " connective means this is
    // not a disjunction, so the single-arm dispatchers should handle the clause.
    let (first_disjunct, mut remaining) = split_on_or_if(lower.as_str())?;
    let mut conditions = vec![try_nom_condition_as_ability_condition(
        first_disjunct.trim(),
        ctx,
    )?];
    loop {
        let (disjunct, rest) = match split_on_or_if(remaining) {
            Some((disjunct, rest)) => (disjunct, Some(rest)),
            None => (remaining, None),
        };
        conditions.push(try_nom_condition_as_ability_condition(
            disjunct.trim(),
            ctx,
        )?);
        match rest {
            Some(r) => remaining = r,
            None => break,
        }
    }
    Some(AbilityCondition::Or { conditions })
}

pub(super) fn try_nom_condition_as_ability_condition(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<AbilityCondition> {
    use crate::parser::oracle_nom::condition::parse_inner_condition;

    let lower = text.to_lowercase();

    // CR 508.4 + CR 608.2c + CR 701.42: attacking meld-pair conditions are
    // resolution-time leading conditions. Keep them in the shared condition
    // dispatcher so trigger, activated-ability, and ordinary effect chains all
    // obtain the same typed source/partner predicates.
    if let Some((condition, ..)) = super::meld::parse_live_pair_ability_condition(text) {
        return Some(condition);
    }

    // CR 608.2c: "<condition A> or if <condition B>" disjunction (Reptilian
    // Recruiter: "If that creature's power is 2 or less or if you control another
    // Lizard, ..."). Each disjunct is a complete condition clause (the second
    // re-introduces "if"), so peel on " or if " and recurse every disjunct through
    // this dispatcher; bind `Or` only when EVERY disjunct parses. Tried first so a
    // disjunction wins over any single-arm match on its leading disjunct.
    if let Some(condition) = parse_or_if_disjunction(text, ctx) {
        return Some(condition);
    }

    // CR 505.1 + CR 102.1 + CR 608.2c: resolution-time "it is[n't] your [phase]"
    // current-phase gate (Dose of Dawnglow: "if it isn't your main phase").
    // Placed before every anaphoric arm: the subject "it" here is an expletive
    // (it never anaphors to a target), and the `all_consuming` wrap over the
    // unique "it … your <phase>" shape forbids any partial / mis-bound match.
    if let Ok((_, condition)) = all_consuming(parse_current_phase_condition).parse(lower.as_str()) {
        return Some(condition);
    }

    // CR 508.1a: "you attacked with <filter> [this turn]" filtered attack-history gate.
    if let Some(condition) = parse_attacked_with_filter_condition(lower.as_str()) {
        return Some(condition);
    }

    // CR 508.1a + CR 603.4: target-anaphoric "it [didn't] attack this turn"
    // combat-history gate (Aggression end-step trigger, Berserk delayed trigger).
    // Tried after the controller-scoped "you attacked with" form above, whose
    // subject parser cannot match the anaphoric "it"/"that creature".
    if let Some(condition) = parse_target_attacked_this_turn_condition_text(lower.as_str()) {
        return Some(condition);
    }

    // CR 400.7 + CR 608.2c: per-target "if <subject> entered this turn" ETB gate
    // (Malamet Battle Glyph). Registered right after the attacked-this-turn
    // sibling and BEFORE the control-count / existential paths: its
    // `all_consuming` wrap and anaphor/"the <type>"-only subject parser reject
    // control-count ("if you control a creature that entered this turn"),
    // existential ("if a creature entered the battlefield this turn"), and the
    // source-referential "~ entered this turn" (SourceEnteredThisTurn) forms, so
    // those still reach their own recognizers.
    if let Some(condition) = parse_target_entered_this_turn_condition_text(lower.as_str()) {
        return Some(condition);
    }

    // CR 608.2c + CR 205.3m: target-anaphoric type / subtype-membership gate
    // ("that creature is a Mutant, Ninja, or Turtle" — Turtle Van). Tried after
    // the more specific anaphoric combat-history form above; its predicate tail is
    // a type/subtype phrase, so it does not collide with the bare-color form
    // (which `parse_condition_text` handles separately).
    if let Some(condition) = parse_target_type_membership_condition_text(lower.as_str()) {
        return Some(condition);
    }

    // CR 608.2c + CR 400.7: target-anaphoric reflexive object-property gate —
    // "it was dealt damage this turn" / "it had mana value 3 or less" / "that
    // creature had power 2 or less" / "that creature is attacking". The rider
    // condition for the "<removal/bounce/damage>, then conditional bonus" class
    // (Consuming Ashes, Sold Out, Driftgloom Coyote, Wisecrack). Placed after the
    // more specific combat-history and type-membership forms above and before the
    // bare-color / parse_inner_condition fallbacks: the predicate is a
    // parameterized object characteristic (dealt-damage, combat status, mana
    // value, P/T), so it must not preempt the type/color recognizers.
    if let Some(condition) = parse_target_reflexive_property_condition_text(lower.as_str()) {
        return Some(condition);
    }

    // CR 115.1 + CR 208.1 + CR 608.2c: target-anaphoric possessive P/T comparison —
    // "that creature's power is 2 or less" / "that permanent's toughness is
    // exactly N" (Depressurize, Gore Vassal, Reptilian disjunct A). Placed right
    // after the reflexive arm (which does not match the possessive "'s ... is N"
    // form) so it wins over the generic `parse_cda_quantity` path downstream that
    // would otherwise mis-scope the subject to `Power { CostPaidObject }`.
    if let Some(condition) = parse_target_possessive_pt_comparison_text(lower.as_str()) {
        return Some(condition);
    }

    if let Some(condition) = parse_you_controlled_parent_target_condition(lower.as_str()) {
        return Some(condition);
    }

    // CR 109.4 + CR 608.2c: "an opponent controls that creature" — first object
    // target is opponent-controlled (Fear of Immobility stun gate).
    if let Some(condition) = parse_opponent_controls_target_condition(lower.as_str()) {
        return Some(condition);
    }

    // CR 111.1 + CR 608.2c: "the token is a/an <type>" — the just-created token
    // matches a type (Yenna, Redtooth Regent untap/scry gate).
    if let Some(condition) = parse_the_token_is_type_condition(lower.as_str()) {
        return Some(condition);
    }

    if let Some(condition) = parse_entered_or_cast_from_zone_ability_condition(lower.as_str()) {
        return Some(condition);
    }

    // CR 702.119a-c: "[possessive] emerge cost was paid" → CastVariantPaid { Emerge },
    // routing Adipose Offspring's "instead create X of those tokens" body through
    // the ConditionInstead token-reproduction path.
    if let Some(condition) = parse_cast_variant_cost_paid_condition(lower.as_str()) {
        return Some(condition);
    }

    // CR 115.1 + CR 608.2c + CR 702.185a: target-scoped "that creature was cast
    // for its warp cost" (Full Bore) → CastVariantPaid { subject: Target }.
    // Tried after the source-scoped "[possessive] X cost was paid" form above:
    // the subject here is a target anaphor ("that creature"/"it"), not a
    // possessive cost reference, so the two are lexically disjoint.
    if let Some(condition) = parse_target_cast_variant_paid_condition_text(lower.as_str()) {
        return Some(condition);
    }

    if let Some(condition) = parse_zone_change_object_matches_filter_condition(lower.as_str()) {
        return Some(condition);
    }

    // CR 608.2c: trailing "this way" outcome gate — "[effect] if at least one
    // <filter> was <verb> this way". CR 608.2c explicitly defines that later
    // text on a card may reference an earlier instruction in the same effect
    // ("if that spell is countered this way") — here "this way" names the set
    // of objects affected by the immediately-preceding instruction. The suffix
    // lands here after
    // `strip_suffix_conditional` peels the effect, so the residual condition
    // text is the bare existential "<quantifier> <filter> (was|is) <verb> this
    // way". Delegate to the shared `parse_zone_changed_this_way_clause`
    // combinator — the same authority the leading-form
    // `strip_if_you_do_conditional` uses — so prefix ("if a creature card is
    // exiled this way, …") and suffix ("… if at least one creature card was
    // exiled this way") forms produce the identical
    // `AbilityCondition::ZoneChangedThisWay { filter }` representation.
    if let Some(condition) = parse_outcome_this_way_condition(lower.as_str()) {
        return Some(condition);
    }

    if let Some(condition) = parse_target_supertype_condition_text(lower.as_str()) {
        return Some(condition);
    }

    if let Some(condition) = parse_cost_paid_object_matches_filter_condition(lower.as_str()) {
        return Some(condition);
    }

    if let Some(condition) = parse_previous_effect_excess_damage_condition(lower.as_str()) {
        return Some(condition);
    }

    if let Some(condition) = parse_die_result_condition(lower.as_str()) {
        return Some(condition);
    }

    if let Some(condition) = parse_objects_share_quality_condition(text, ctx) {
        return Some(condition);
    }

    // CR 702.62a: "it doesn't have [keyword]" / "it does not have [keyword]" — pronoun
    // subject lacks-keyword check (e.g., "If it doesn't have suspend, it gains suspend").
    // Mirrors the "~ doesn't have" / "this creature doesn't have" handler in oracle_condition.rs.
    if let Ok((keyword_text, _)) = alt((
        tag::<_, _, OracleError<'_>>("it doesn't have "),
        tag("it does not have "),
    ))
    .parse(lower.as_str())
    {
        let keyword: Keyword = keyword_text
            .trim()
            .parse()
            .unwrap_or(Keyword::Unknown(String::new()));
        if !matches!(keyword, Keyword::Unknown(_)) {
            return Some(AbilityCondition::SourceLacksKeyword { keyword });
        }
    }

    // CR 730.2a: "it's neither day nor night" — Daybound/Nightbound ETB initialization.
    if tag::<_, _, OracleError<'_>>("it's neither day nor night")
        .parse(lower.as_str())
        .is_ok()
    {
        return Some(AbilityCondition::DayNightIsNeither);
    }

    if tag::<_, _, OracleError<'_>>("it's the first combat phase of the turn")
        .parse(lower.as_str())
        .is_ok()
    {
        return Some(AbilityCondition::FirstCombatPhaseOfTurn);
    }

    // CR 500.8 + CR 513.1: "it's the first end step of the turn" — end-step
    // sibling of the combat-phase gate above (Y'shtola Rhul's loop guard).
    if tag::<_, _, OracleError<'_>>("it's the first end step of the turn")
        .parse(lower.as_str())
        .is_ok()
    {
        return Some(AbilityCondition::FirstEndStepOfTurn);
    }

    // CR 603.4: "if this is the [Nth] time this ability has resolved this turn"
    // and the abbreviated continuation form "if it's the [Nth] time" used by
    // Omnath's later sentences (the "this ability has resolved this turn" tail
    // is anaphoric to the prior sentence and is dropped). Composes:
    //   subject: "this is" | "it's" | "it is"
    //   ordinal: "first" | "second" | ...
    //   tail:    optional " this ability has resolved this turn"
    if let Some(n) = parse_nth_resolution_condition(lower.as_str()) {
        return Some(AbilityCondition::NthResolutionThisTurn { n });
    }

    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you do or if ").parse(lower.as_str()) {
        let (rest, condition) = parse_inner_condition(rest).ok()?;
        if rest.trim().is_empty() {
            return Some(AbilityCondition::Or {
                conditions: vec![
                    AbilityCondition::effect_performed(),
                    static_condition_to_ability_condition(&condition, ctx)?,
                ],
            });
        }
    }

    if tag::<_, _, OracleError<'_>>("you win the clash")
        .parse(lower.as_str())
        .is_ok()
        || tag::<_, _, OracleError<'_>>("you won the clash")
            .parse(lower.as_str())
            .is_ok()
        || tag::<_, _, OracleError<'_>>("you win")
            .parse(lower.as_str())
            .is_ok()
        || tag::<_, _, OracleError<'_>>("you won")
            .parse(lower.as_str())
            .is_ok()
    {
        return Some(AbilityCondition::EventOutcomeWon);
    }

    // CR 603.12 + CR 608.2c: reflexive "you don't" / "you do not" / "you didn't"
    // / "you did not" — the verb anaphors back to the immediately preceding
    // optional action ("you didn't put a card into your hand this way"), so the
    // fragment is the optional-effect signal, NOT a game-state condition. But a
    // trailing game-state predicate ("you don't *control a Snail*", Wick) is a
    // genuine control-presence gate. Disambiguate by deferring to the typed
    // condition parser first: when `parse_inner_condition` consumes the whole
    // fragment, it is a real game-state condition (re-homed below); only when it
    // cannot is this the reflexive optional-effect signal.
    if alt((
        tag::<_, _, OracleError<'_>>("you don't"),
        tag("you do not"),
        tag("you didn't"),
        tag("you did not"),
    ))
    .parse(lower.as_str())
    .is_ok()
        && !parse_inner_condition(&lower).is_ok_and(|(rest, _)| rest.trim().is_empty())
    {
        return Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::effect_performed()),
        });
    }

    // CR 608.2c: "if you can't, [effect]" — the preceding mandatory instruction
    // could not be performed (no object changed zone this way). This is
    // prior-instruction-referential (it reports whether the preceding chained
    // instruction succeeded), so — like the "you don't" / "this spell was cast
    // from" arms above — it legitimately lives outside `parse_inner_condition`,
    // which only yields game-state-fact `StaticCondition`s. `last_zone_changed_ids`
    // is repopulated per-effect, so after the preceding effect resolves it holds
    // exactly that effect's zone changes; `Not { ZoneChangedThisWay { Any } }` is
    // true iff that effect moved nothing — i.e. "you can't".
    if alt((tag::<_, _, OracleError<'_>>("you can't"), tag("you cannot")))
        .parse(lower.as_str())
        .is_ok()
    {
        return Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::ZoneChangedThisWay {
                filter: TargetFilter::Any,
            }),
        });
    }

    if let Ok((after_prefix, _)) =
        tag::<_, _, OracleError<'_>>("this spell was cast from ").parse(lower.as_str())
    {
        // CR 601.2a: Match the printed source zone, then either accept the
        // bare condition (empty / period remainder) or chain a recognised
        // " and …" tail via `AbilityCondition::And`.
        let zone_match: Result<(&str, Zone), nom::Err<OracleError<'_>>> = alt((
            value(Zone::Hand, tag::<_, _, OracleError<'_>>("your hand")),
            value(Zone::Hand, tag("hand")),
            value(Zone::Graveyard, tag("your graveyard")),
            value(Zone::Graveyard, tag("a graveyard")),
            value(Zone::Exile, tag("exile")),
        ))
        .parse(after_prefix);
        if let Ok((after_zone, zone)) = zone_match {
            let trimmed = after_zone.trim_start();
            if trimmed.is_empty() {
                return Some(AbilityCondition::WasCast { zone: Some(zone) });
            }
            // CR 117.1 + CR 201.2 + CR 608.2c: "and [second condition]" suffix
            // for compound intervening-ifs like Approach of the Second Sun's
            // "this spell was cast from your hand and you've cast another
            // spell named {LITERAL} this game". Both halves must parse for the
            // compound to bind — if `and ` is consumed but the second
            // condition does not parse, we deliberately fall through (return
            // None) rather than binding bare CastFromZone, since running with
            // only half the printed gate is unsafe.
            if let Ok((second_text, _)) = tag::<_, _, OracleError<'_>>("and ").parse(trimmed) {
                if let Some(second) =
                    parse_youve_cast_another_named_this_game_condition(second_text)
                {
                    return Some(AbilityCondition::And {
                        conditions: vec![AbilityCondition::WasCast { zone: Some(zone) }, second],
                    });
                }
            }
            // Fallback: zone alone is still a valid condition if the suffix
            // is unrecognised but starts with a period / punctuation.
            // This remains a nom guard even though it is only distinguishing
            // "clause boundary follows" from "more content follows we
            // couldn't recognise".
            if alt((
                tag::<_, _, OracleError<'_>>("."),
                tag::<_, _, OracleError<'_>>(","),
            ))
            .parse(trimmed)
            .is_ok()
            {
                return Some(AbilityCondition::WasCast { zone: Some(zone) });
            }
        }
    }

    if tag::<_, _, OracleError<'_>>("this spell was foretold")
        .parse(lower.as_str())
        .is_ok()
    {
        return Some(AbilityCondition::CastVariantPaid {
            variant: CastVariantPaid::Foretell,
            subject: ObjectScope::Source,
        });
    }

    // CR 400.7 + CR 608.2c: "a[n] [type] was [verb]'d this way" — references the
    // LKI of the parent target (the object acted on by the preceding effect).
    // Shredder's Technique: "If an enchantment was destroyed this way, you lose 2 life."
    // "this way" here is scoped to the single parent target of the preceding
    // imperative (Destroy target creature or enchantment). Type-resolution via
    // LKI mirrors the "it was a [type] card" branch below.
    if let Some((type_filter, negated)) = parse_a_type_was_verbed_this_way(&lower) {
        return Some(maybe_negate(
            AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(TypedFilter::new(type_filter)),
                use_lki: true,
                subject_slot: None,
            },
            negated,
        ));
    }

    // CR 400.7 + CR 608.2c: Past-tense "it was a [type] card" — the card has already
    // moved zones; check its last-known information via TargetMatchesFilter { use_lki }.
    // Distinct from present-tense "it's a [type]" which uses RevealedHasCardType.
    {
        let mut lki_prefix = alt((
            value(true, tag::<_, _, OracleError<'_>>("it was not a ")),
            value(true, tag("it wasn't a ")),
            value(false, tag("it was a ")),
            value(false, tag("it was an ")),
        ));
        if let Ok((rest, negated_lki)) = lki_prefix.parse(lower.as_str()) {
            // Strip trailing " card" / " card." before delegating to parse_type_phrase.
            let type_text = rest
                .trim_end_matches('.')
                .trim()
                .trim_end_matches(" card")
                .trim();
            let (filter, leftover) = crate::parser::oracle_target::parse_type_phrase(type_text);
            if !matches!(filter, TargetFilter::Any) && leftover.trim().is_empty() {
                return Some(maybe_negate(
                    AbilityCondition::TargetMatchesFilter {
                        filter,
                        use_lki: true,
                        subject_slot: None,
                    },
                    negated_lki,
                ));
            }
        }
    }

    // CR 508.1 + CR 509.1 + CR 400.7: "it was/wasn't attacking/blocking" — past-tense
    // combat-status check on the trigger subject via LKI. Used by dies-triggers
    // that condition on the creature's combat state before it left the battlefield
    // (e.g., Garna, Bloodfist of Keld: "draw a card if it was attacking").
    {
        let mut parse_status = (
            alt((
                value(
                    true,
                    alt((
                        tag::<_, _, OracleError<'_>>("it wasn't "),
                        tag("it was not "),
                    )),
                ),
                value(false, tag("it was ")),
            )),
            alt((
                value(FilterProp::Attacking { defender: None }, tag("attacking")),
                value(FilterProp::Blocking, tag("blocking")),
            )),
        );
        if let Ok((rest, (negated, prop))) = parse_status.parse(lower.as_str()) {
            if rest.trim().is_empty() {
                let cond = AbilityCondition::TargetMatchesFilter {
                    filter: TargetFilter::Typed(TypedFilter {
                        properties: vec![prop],
                        ..Default::default()
                    }),
                    use_lki: true,
                    subject_slot: None,
                };
                return Some(maybe_negate(cond, negated));
            }
        }
    }

    // CR 608.2c + CR 205.3a: Article choice must not affect anaphoric subtype gates.
    let (negated, rest_after_prefix) = if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("it's not a "),
        tag("it's not an "),
    ))
    .parse(lower.as_str())
    {
        (true, Some(rest))
    } else if let Ok((rest, _)) =
        alt((tag::<_, _, OracleError<'_>>("it's a "), tag("it's an "))).parse(lower.as_str())
    {
        (false, Some(rest))
    } else if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("that card is a "),
        tag("that card is an "),
    ))
    .parse(lower.as_str())
    {
        (false, Some(rest))
    } else if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("it isn't a "),
        tag("it isn't an "),
    ))
    .parse(lower.as_str())
    {
        (true, Some(rest))
    } else {
        (false, None)
    };

    if let Some(rest) = rest_after_prefix {
        let rest = rest.trim_end_matches(" card").trim();
        // CR 608.2c: "permanent" is not a CoreType — gate on it via the existing
        // parse_type_phrase building block + TargetMatchesFilter, keeping this handler
        // in lockstep with strip_card_type_conditional's "permanent" arm.
        if rest == "permanent" {
            let (filter, leftover) =
                crate::parser::oracle_target::parse_type_phrase("permanent card");
            if !matches!(filter, TargetFilter::Any) && leftover.trim().is_empty() {
                return Some(maybe_negate(
                    AbilityCondition::TargetMatchesFilter {
                        filter,
                        use_lki: false,
                        subject_slot: None,
                    },
                    negated,
                ));
            }
        }
        let card_type = match rest {
            "creature" => Some(CoreType::Creature),
            "land" => Some(CoreType::Land),
            "nonland" => {
                return Some(maybe_negate(
                    AbilityCondition::RevealedHasCardType {
                        card_types: vec![CoreType::Land],
                        additional_filter: None,
                        subtype_filter: None,
                    },
                    !negated,
                ));
            }
            "instant" => Some(CoreType::Instant),
            "sorcery" => Some(CoreType::Sorcery),
            "artifact" => Some(CoreType::Artifact),
            "enchantment" => Some(CoreType::Enchantment),
            "planeswalker" => Some(CoreType::Planeswalker),
            _ => None,
        };
        if let Some(card_type) = card_type {
            return Some(maybe_negate(
                AbilityCondition::RevealedHasCardType {
                    card_types: vec![card_type],
                    additional_filter: None,
                    subtype_filter: None,
                },
                negated,
            ));
        }
        // CR 205.3m: Multi-subtype creature gates on a revealed/peeked card.
        if let Some(subtype_filter) = parse_creature_subtype_type_tail(rest) {
            return Some(maybe_negate(
                AbilityCondition::RevealedHasCardType {
                    card_types: vec![CoreType::Creature],
                    additional_filter: None,
                    subtype_filter: Some(Box::new(subtype_filter)),
                },
                negated,
            ));
        }
        // CR 608.2c + CR 205.3a: "it's a [subtype]" (Goblin, Aura, Equipment, ...).
        // The CoreType match above only covers card types; subtypes route through
        // the parse_type_phrase building block + TargetMatchesFilter, exactly like
        // the "permanent" arm. Subtype disjunctions ("Goblin or Orc") are handled by
        // parse_type_phrase's Or support; an unparseable remainder leaves non-empty
        // leftover and falls through to parse_inner_condition.
        //
        // The `references_subtype` gate is load-bearing: parse_type_phrase also
        // consumes CoreType phrases the explicit `match` above already owns
        // (e.g. "creature card of the chosen type" → Creature + IsChosenCreatureType
        // with empty leftover). Those belong to RevealedHasCardType, not the
        // present-target TargetMatchesFilter, so this arm fires only when the parsed
        // filter genuinely references a CR 205.3 subtype.
        let (filter, leftover) = crate::parser::oracle_target::parse_type_phrase(rest);
        if let TargetFilter::Typed(typed) = &filter {
            if leftover.trim().is_empty()
                && typed
                    .type_filters
                    .iter()
                    .any(type_filter_references_subtype)
            {
                return Some(maybe_negate(
                    AbilityCondition::TargetMatchesFilter {
                        filter: filter.clone(),
                        use_lki: false,
                        subject_slot: None,
                    },
                    negated,
                ));
            }
        }
    }

    // CR 608.2c + CR 702.1: "it has [keyword]" — affirmative pronoun keyword check
    // (e.g. "If it has flying, ..."). Routed through TargetMatchesFilter +
    // FilterProp::WithKeyword, the same abstraction the "it's a [type]" arm uses
    // (no SourceHasKeyword sibling to SourceLacksKeyword). Disjoint prefix from the
    // "it doesn't have" arm above, so ordering is irrelevant.
    if let Ok((keyword_text, _)) = tag::<_, _, OracleError<'_>>("it has ").parse(lower.as_str()) {
        let keyword: Keyword = keyword_text
            .trim()
            .parse()
            .unwrap_or(Keyword::Unknown(String::new()));
        if !matches!(keyword, Keyword::Unknown(_)) {
            return Some(AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(TypedFilter {
                    properties: vec![FilterProp::WithKeyword { value: keyword }],
                    ..Default::default()
                }),
                use_lki: false,
                subject_slot: None,
            });
        }
    }

    // CR 608.2c + CR 601.2b: anaphoric/self permanent-status gate — "if it's
    // tapped, …" / "if it's suspected, …" / "if ~ is suspected, …". Tried just
    // before the generic `parse_inner_condition` fall-through and after the
    // "it's a [type]" arm so type phrases keep their dedicated routing. The
    // subject axis selects the condition seam: a chosen-target anaphor binds the
    // status to `TargetMatchesFilter` (the ability's first object target / the
    // triggering subject), while a self-reference binds it to
    // `SourceMatchesFilter` (the ability's own permanent) — Repeat Offender's
    // activated ability carries no chosen target, so its "~ is suspected" gate
    // must read the source, not an absent target.
    if let Ok((_, (subject, negated, prop))) =
        all_consuming(parse_anaphoric_status_predicate).parse(lower.as_str())
    {
        let cond =
            match subject {
                StatusSubject::Anaphoric => AbilityCondition::TargetMatchesFilter {
                    filter: TargetFilter::Typed(TypedFilter {
                        properties: vec![prop],
                        ..Default::default()
                    }),
                    use_lki: false,
                    subject_slot: None,
                },
                // CR 611.2b: a self-subject predicate with a dedicated precise
                // `AbilityCondition` variant (e.g. "tapped" → `SourceIsTapped`) must
                // route to that variant rather than the generic
                // `SourceMatchesFilter`, per the codebase preference for the typed
                // variant over a filter. Only predicates with no precise variant
                // (e.g. "suspected" — there is no `SourceIsSuspected`) fall back to
                // `SourceMatchesFilter { Typed([prop]) }`.
                StatusSubject::SelfSource => precise_source_condition_for_prop(&prop)
                    .unwrap_or_else(|| AbilityCondition::SourceMatchesFilter {
                        filter: TargetFilter::Typed(TypedFilter {
                            properties: vec![prop],
                            ..Default::default()
                        }),
                    }),
            };
        return Some(maybe_negate(cond, negated));
    }

    let (rest, condition) = parse_inner_condition(&lower).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    static_condition_to_ability_condition(&condition, ctx)
}

/// CR 109.4 + CR 109.5 + CR 608.2c: Consume the anaphoric control predicate
/// "you control[led] [it | that <type>]" / "you (don't|did not) control[led] …"
/// and return the typed `TargetFilter` for the controlled-by-you subject plus
/// `(use_lki, negated)` flags.
///
/// Composes three orthogonal axes — none enumerated as full-string `tag()`s:
///   - polarity: positive ("you control[led]") vs. negative ("you don't /
///     do not / didn't / did not control[led]"). Negative → `negated = true`;
///     "didn't" / "did not" also imply past-tense LKI.
///   - tense: present ("control") → current state (`use_lki = false`,
///     CR 109.4 evaluated now); past ("controlled") → LKI snapshot
///     (`use_lki = true`, CR 400.7) — matters when an earlier chain link has
///     already moved the subject before the check runs.
///   - subject: pronoun "it" (controller-only typed filter) vs. "that <type>".
///
/// The subject identity is preserved by the consumer wrapping the filter in
/// `AbilityCondition::TargetMatchesFilter`, which evaluates `ability.targets[0]`
/// (the trigger subject / parent target).
fn parse_anaphoric_control_predicate(input: &str) -> OracleResult<'_, (TargetFilter, bool, bool)> {
    type E<'a> = OracleError<'a>;

    let controller_only =
        TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You));
    let permanent = TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You));
    let nonland_permanent = TargetFilter::Typed(
        TypedFilter::permanent()
            .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
            .controller(ControllerRef::You),
    );
    let card = TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::You));
    let artifact =
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You));
    let creature = TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You));
    let enchantment = TargetFilter::Typed(
        TypedFilter::new(TypeFilter::Enchantment).controller(ControllerRef::You),
    );
    let land = TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));
    let planeswalker = TargetFilter::Typed(
        TypedFilter::new(TypeFilter::Planeswalker).controller(ControllerRef::You),
    );
    let battle =
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Battle).controller(ControllerRef::You));

    // Axis 1 — polarity. "you " then optional negator, then the control verb.
    let (rest, _) = tag::<_, _, E>("you ").parse(input)?;
    let (rest, negator_lki) = opt(alt((
        value(false, tag::<_, _, E>("don't ")),
        value(false, tag("do not ")),
        value(true, tag("didn't ")),
        value(true, tag("did not ")),
    )))
    .parse(rest)?;
    let negated = negator_lki.is_some();
    // Axis 2 — tense. "controlled" → past (LKI); "control" → present.
    let (rest, verb_lki) = alt((
        value(true, tag::<_, _, E>("controlled ")),
        value(false, tag("control ")),
    ))
    .parse(rest)?;
    let use_lki = negator_lki.unwrap_or(verb_lki) || verb_lki;
    // Axis 3 — subject.
    let (rest, filter) = alt((
        value(controller_only, tag::<_, _, E>("it")),
        preceded(
            tag::<_, _, E>("that "),
            alt((
                value(nonland_permanent, tag::<_, _, E>("nonland permanent")),
                value(permanent, tag("permanent")),
                value(artifact, tag("artifact")),
                value(creature, tag("creature")),
                value(enchantment, tag("enchantment")),
                value(land, tag("land")),
                value(planeswalker, tag("planeswalker")),
                value(battle, tag("battle")),
                value(card, tag("card")),
            )),
        ),
    ))
    .parse(rest)?;

    Ok((rest, (filter, use_lki, negated)))
}

fn parse_you_controlled_parent_target_condition(lower: &str) -> Option<AbilityCondition> {
    let (_, (filter, use_lki, negated)) = all_consuming(parse_anaphoric_control_predicate)
        .parse(lower)
        .ok()?;
    Some(maybe_negate(
        AbilityCondition::TargetMatchesFilter {
            filter,
            use_lki,
            subject_slot: None,
        },
        negated,
    ))
}

/// CR 109.4 + CR 608.2c: "an opponent controls that creature / that permanent /
/// it" — the ability's first object target is controlled by an opponent (Fear of
/// Immobility: "If an opponent controls that creature, put a stun counter on
/// it"). Emits `TargetMatchesFilter` with an `Opponent` controller restriction,
/// resolved against `targets[0]`. `use_lki: false` — the tapped target is still
/// on the battlefield when the rider resolves, so its live controller is
/// authoritative (CR 109.4).
fn parse_opponent_controls_target(input: &str) -> OracleResult<'_, AbilityCondition> {
    let (rest, _) = tag("an opponent controls ").parse(input)?;
    let (rest, _) = alt((tag("that creature"), tag("that permanent"), tag("it"))).parse(rest)?;
    Ok((
        rest,
        AbilityCondition::TargetMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            use_lki: false,
            subject_slot: None,
        },
    ))
}

fn parse_opponent_controls_target_condition(lower: &str) -> Option<AbilityCondition> {
    all_consuming(parse_opponent_controls_target)
        .parse(lower)
        .ok()
        .map(|(_, c)| c)
}

/// CR 111.1 + CR 608.2c: "the token is a/an <type>" — the token created by the
/// immediately-preceding `Effect::Token`/`CopyTokenOf` matches `<type>` (Yenna,
/// Redtooth Regent: "Create a token that's a copy of it ... If the token is an
/// Aura, untap ~, then scry 2"). Represented as an existential over
/// `And([LastCreated, <type>])`: `LastCreated` reads `state.last_created_token_ids`,
/// so the count is 1 exactly when the just-created token matches the type. This is
/// the field-expressive form — a bare `TargetMatchesFilter{LastCreated}` would test
/// the ability's chosen target (the copied enchantment), not the created token.
fn parse_the_token_is_type_condition(lower: &str) -> Option<AbilityCondition> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("the token is ")
        .parse(lower)
        .ok()?;
    let (filter, remainder) = parse_type_phrase(rest);
    if !matches!(filter, TargetFilter::Typed(_)) || !remainder.trim().is_empty() {
        return None;
    }
    Some(AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::And {
                    filters: vec![TargetFilter::LastCreated, filter],
                },
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    })
}

/// CR 109.4 + CR 608.2c: Recognize a leading **inverse** anaphoric-control
/// "otherwise" connector — `"if you don't control [it | that <type>], <body>"`
/// (and the `do not` / `didn't` / `did not` spellings). Returns the residual
/// body when matched.
///
/// This is the else-branch sibling of the positive control suffix consumed by
/// `parse_anaphoric_control_predicate`: a card that reads "draw a card if you
/// control that creature. If you don't control it, its controller loses 1 life."
/// (Auntie Ool, Cursewretch class) expresses a true CR 608.2c if/else over the
/// SAME subject. The negated second sentence is the `else_ability` of the
/// positively-gated first sentence — not an independent sibling instruction.
///
/// Only the **negated** polarity is treated as an otherwise-connector; the
/// positive form ("if you control it, …") is an ordinary leading conditional
/// that gates its own clause. The caller (the Otherwise dispatch in the chunk
/// loop) additionally requires a prior clause to carry a control condition, so
/// this never fires without a positive antecedent to attach to.
pub(super) fn strip_inverse_control_otherwise_connector(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    nom_on_lower(text, &lower, |input| {
        let (input, _) = tag::<_, _, OracleError<'_>>("if ").parse(input)?;
        let (input, (_filter, _use_lki, negated)) = parse_anaphoric_control_predicate(input)?;
        // Only the negated "you don't control …" form is an else-connector.
        if !negated {
            return Err(nom::Err::Error(OracleError::new(
                input,
                nom::error::ErrorKind::Fail,
            )));
        }
        let (input, _) = tag::<_, _, OracleError<'_>>(", ").parse(input)?;
        Ok((input, ()))
    })
    .map(|((), rest)| rest.to_string())
}

fn parse_cost_paid_object_matches_filter_condition(lower: &str) -> Option<AbilityCondition> {
    if let Some(condition) = parse_cost_paid_object_subject_verb_form(lower) {
        return Some(condition);
    }
    if let Some(condition) = parse_cost_paid_object_possessive_pt_comparison(lower) {
        return Some(condition);
    }
    parse_cost_paid_object_definite_noun_form(lower)
}

/// Subject-verb form: "you sacrificed/exiled/discarded a [type] this way".
/// Only checks the type of the cost-paid object (no property predicate).
fn parse_cost_paid_object_subject_verb_form(lower: &str) -> Option<AbilityCondition> {
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("you sacrificed "),
        tag("you exiled "),
        tag("you discarded "),
    ))
    .parse(lower)
    .ok()?;
    let (rest, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("a "),
        tag("an "),
        tag("the "),
    )))
    .parse(rest)
    .ok()?;
    let (rest, type_text) = take_until::<_, _, OracleError<'_>>(" this way")
        .parse(rest)
        .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" this way").parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    let type_filter = parse_cost_paid_object_type_filter(type_text.trim())?;

    Some(AbilityCondition::CostPaidObjectMatchesFilter {
        filter: TargetFilter::Typed(TypedFilter::new(type_filter)),
    })
}

/// CR 117.1 + CR 400.7j + CR 608.2k: Definite-noun form — "the [verb]ed
/// [noun] was [property]". Used by override-instead conditions that check a
/// property of the object paid as cost (not just its type). The `was`/`is`
/// tense agrees with the cost-paid-object snapshot's LKI.
///
/// Examples (Stormscale Anarch class / Surtland Flinger):
///   "the discarded card was multicolored"
///   "the sacrificed creature was a Giant"
///   "the exiled creature was a Spirit"
///
/// Composes three orthogonal axes:
///   - verb participle: `discarded` / `sacrificed` / `exiled`
///   - noun: `card` / `creature` / `permanent` / etc. — driven by
///     [`parse_cost_paid_object_noun_prefix`] and added to `type_filters` so
///     the runtime check matches both the noun and the property.
///   - property predicate: a color-set property (multicolored/monocolored/
///     colorless/named color) OR a type-or-subtype match ("a Giant",
///     "a creature", "an artifact"). Color predicates land in `properties`;
///     type/subtype predicates extend `type_filters`.
fn parse_cost_paid_object_definite_noun_form(lower: &str) -> Option<AbilityCondition> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("the ").parse(lower).ok()?;
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("discarded "),
        tag("sacrificed "),
        tag("exiled "),
    ))
    .parse(rest)
    .ok()?;
    let (rest, noun_filter) = parse_cost_paid_object_noun_prefix(rest)?;
    // CR 608.2c: The `was`/`is` copula may be negated ("the discarded card
    // wasn't a land card" — Grab the Prize). Track the polarity so the resulting
    // `CostPaidObjectMatchesFilter` is wrapped in `Not` for the negated voice.
    let (rest, negated) = alt((
        value(
            true,
            alt((
                tag::<_, _, OracleError<'_>>("wasn't "),
                tag("isn't "),
                tag("weren't "),
                tag("was not "),
                tag("is not "),
            )),
        ),
        value(false, alt((tag("was "), tag("is ")))),
    ))
    .parse(rest)
    .ok()?;
    let predicate = parse_cost_paid_object_predicate(rest)?;

    let mut typed = TypedFilter::new(noun_filter);
    match predicate {
        CostPaidPredicate::Property(prop) => {
            typed = typed.properties(vec![prop]);
        }
        CostPaidPredicate::TypeMatch(tf) => {
            typed = typed.with_type(tf);
        }
    }
    let condition = AbilityCondition::CostPaidObjectMatchesFilter {
        filter: TargetFilter::Typed(typed),
    };
    Some(if negated {
        AbilityCondition::Not {
            condition: Box::new(condition),
        }
    } else {
        condition
    })
}

fn parse_cost_paid_object_possessive_pt_comparison(lower: &str) -> Option<AbilityCondition> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("the ").parse(lower).ok()?;
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("discarded "),
        tag("sacrificed "),
        tag("exiled "),
    ))
    .parse(rest)
    .ok()?;
    let (rest, noun_filter) = parse_cost_paid_object_possessive_noun_prefix(rest)?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("'s ").parse(rest).ok()?;
    let (rest, stat) = parse_reflexive_pt_stat(rest).ok()?;
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("was "), tag("is ")))
        .parse(rest)
        .ok()?;
    let (rest, (comparator, value)) = parse_threshold_with_exactly(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    Some(AbilityCondition::CostPaidObjectMatchesFilter {
        filter: TargetFilter::Typed(TypedFilter::new(noun_filter).properties(vec![
            FilterProp::PtComparison {
                stat,
                scope: PtValueScope::Current,
                comparator,
                value: QuantityExpr::Fixed { value },
            },
        ])),
    })
}

/// Predicate result for a definite-noun form's property clause. Property
/// predicates (color-set, status such as suspected) land on
/// `TypedFilter::properties`; type-or-subtype predicates land on
/// `TypedFilter::type_filters` so the conjunction reflects both the noun
/// ("creature") and the typed match ("a Giant"). See
/// [`parse_cost_paid_object_definite_noun_form`].
enum CostPaidPredicate {
    /// Any orthogonal `FilterProp` applied via `TypedFilter::properties`
    /// (color-set count, named color, or a status like `Suspected`).
    Property(FilterProp),
    TypeMatch(TypeFilter),
}

/// Non-consuming variant of [`parse_cost_paid_object_type_filter`]: matches a
/// leading noun word (with the trailing space) and returns `(rest, TypeFilter)`.
/// Used by the definite-noun form to bind the noun into the resulting filter.
///
/// Subtypes are intentionally excluded here — the noun position takes a
/// permanent/card category word; subtype matching belongs in the predicate
/// position ("the sacrificed creature was a Giant", not "the sacrificed Giant
/// was …").
fn parse_cost_paid_object_noun_prefix(input: &str) -> Option<(&str, TypeFilter)> {
    alt((
        value(
            TypeFilter::Creature,
            tag::<_, _, OracleError<'_>>("creature "),
        ),
        value(TypeFilter::Artifact, tag("artifact ")),
        value(TypeFilter::Enchantment, tag("enchantment ")),
        value(TypeFilter::Land, tag("land ")),
        value(TypeFilter::Planeswalker, tag("planeswalker ")),
        value(TypeFilter::Permanent, tag("permanent ")),
        value(TypeFilter::Card, tag("card ")),
    ))
    .parse(input)
    .ok()
}

/// Possessive sibling of [`parse_cost_paid_object_noun_prefix`]: matches the
/// noun immediately before `"'s "` in forms like "the sacrificed creature's
/// toughness was 4 or greater".
fn parse_cost_paid_object_possessive_noun_prefix(input: &str) -> Option<(&str, TypeFilter)> {
    alt((
        value(
            TypeFilter::Creature,
            terminated(tag::<_, _, OracleError<'_>>("creature"), peek(tag("'s "))),
        ),
        value(
            TypeFilter::Artifact,
            terminated(tag("artifact"), peek(tag("'s "))),
        ),
        value(
            TypeFilter::Enchantment,
            terminated(tag("enchantment"), peek(tag("'s "))),
        ),
        value(TypeFilter::Land, terminated(tag("land"), peek(tag("'s ")))),
        value(
            TypeFilter::Planeswalker,
            terminated(tag("planeswalker"), peek(tag("'s "))),
        ),
        value(
            TypeFilter::Permanent,
            terminated(tag("permanent"), peek(tag("'s "))),
        ),
        value(TypeFilter::Card, terminated(tag("card"), peek(tag("'s ")))),
    ))
    .parse(input)
    .ok()
}

/// Parse a definite-noun-form predicate after the `was/is` connector. Tries
/// the color-set predicate first (orthogonal property axis) and falls back to
/// a type/subtype match introduced by the article `a`/`an` (CR 205).
fn parse_cost_paid_object_predicate(rest: &str) -> Option<CostPaidPredicate> {
    if let Some(prop) = parse_color_property_predicate(rest) {
        return Some(CostPaidPredicate::Property(prop));
    }
    if let Some(prop) = parse_status_property_predicate(rest) {
        return Some(CostPaidPredicate::Property(prop));
    }
    parse_article_type_predicate(rest).map(CostPaidPredicate::TypeMatch)
}

/// CR 701.60b: Parse a status-property predicate as a `FilterProp`. Currently
/// covers "suspected" (Agency Coroner: "the sacrificed creature was
/// suspected"). Sits beside [`parse_color_property_predicate`] on the same
/// orthogonal `FilterProp` axis; add further status words here as they appear.
fn parse_status_property_predicate(input: &str) -> Option<FilterProp> {
    let trimmed = input.trim().trim_end_matches('.').trim();
    let (rest, prop) = value(
        FilterProp::Suspected,
        tag::<_, _, OracleError<'_>>("suspected"),
    )
    .parse(trimmed)
    .ok()?;
    rest.trim().is_empty().then_some(prop)
}

/// Parse an `a [type]` / `an [type]` predicate where `[type]` is any noun the
/// cost-paid-object machinery understands (creature, artifact, planeswalker,
/// …) or a subtype (Giant, Spirit, Goblin, …). Returns the matched
/// `TypeFilter` if the entire predicate is consumed.
fn parse_article_type_predicate(rest: &str) -> Option<TypeFilter> {
    let trimmed = rest.trim().trim_end_matches('.').trim();
    let (after_article, _) = alt((
        tag::<_, _, OracleError<'_>>("a "),
        tag::<_, _, OracleError<'_>>("an "),
    ))
    .parse(trimmed)
    .ok()?;
    // CR 205 + CR 108.1: A card-noun predicate ("a land card", "a creature
    // card") carries a trailing " card" grammatical class word after the type;
    // strip it so the type word alone drives `parse_cost_paid_object_type_filter`
    // (which is `all_consuming`). "card" on its own stays `TypeFilter::Card`.
    let type_text = after_article.strip_suffix(" card").unwrap_or(after_article); // allow-noncombinator: structural class-word strip on a pre-isolated type phrase
    parse_cost_paid_object_type_filter(type_text)
}

/// CR 105.2: Parse a color-set property predicate as a `FilterProp`. Covers
/// "multicolored" (>= 2 colors), "monocolored" (exactly 1), "colorless"
/// (zero), and named colors (`white`/`blue`/`black`/`red`/`green`).
fn parse_color_property_predicate(input: &str) -> Option<FilterProp> {
    let trimmed = input.trim().trim_end_matches('.').trim();
    if let Ok((rest, prop)) = alt((
        value(
            FilterProp::ColorCount {
                comparator: Comparator::GE,
                count: 2,
            },
            tag::<_, _, OracleError<'_>>("multicolored"),
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
                comparator: Comparator::EQ,
                count: 0,
            },
            tag("colorless"),
        ),
    ))
    .parse(trimmed)
    {
        if rest.trim().is_empty() {
            return Some(prop);
        }
    }
    if let Ok((rest, color)) = nom_primitives::parse_color.parse(trimmed) {
        if rest.trim().is_empty() {
            return Some(FilterProp::HasColor { color });
        }
    }
    None
}

fn parse_cost_paid_object_type_filter(text: &str) -> Option<TypeFilter> {
    all_consuming(alt((
        value(
            TypeFilter::Creature,
            tag::<_, _, OracleError<'_>>("creature"),
        ),
        value(TypeFilter::Artifact, tag("artifact")),
        value(TypeFilter::Enchantment, tag("enchantment")),
        value(TypeFilter::Land, tag("land")),
        value(TypeFilter::Planeswalker, tag("planeswalker")),
        value(TypeFilter::Permanent, tag("permanent")),
        value(TypeFilter::Card, tag("card")),
    )))
    .parse(text)
    .ok()
    .map(|(_, filter)| filter)
    .or_else(|| parse_subtype(text).map(|(subtype, _)| TypeFilter::Subtype(subtype)))
}

/// Shared damage-recipient phrase for both excess-damage condition voices
/// ("the creature the opponent controls" / "that creature" / "that permanent"
/// / "a creature" / "a permanent"). Used by the active voice ("[subject] is
/// dealt excess damage this way", the Fight class) and the passive voice
/// ("excess damage was dealt [to subject] this way", the DealDamage class).
fn parse_excess_damage_subject(input: &str) -> OracleResult<'_, &str> {
    alt((
        tag("the creature the opponent controls"),
        tag("that creature"),
        tag("that permanent"),
        tag("a creature"),
        tag("a permanent"),
    ))
    .parse(input)
}

/// CR 120.10: "[subject] is dealt excess damage this way" (active, Fight class —
/// e.g. The Last Agni Kai) or "excess damage was dealt [to subject] this way"
/// (passive, DealDamage class — e.g. Torch the Witness, Orbital Plunge). Both
/// voices gate on the resolution-local excess channel, so both map to
/// `PreviousEffectAmount { GT 0, channel: Excess }`.
///
/// Each arm is `all_consuming` and carries the `excess` keyword, so this parser
/// returns `None` for any plain "damage … this way" anaphor — it cannot shadow
/// or partially consume the non-excess `PreviousEffectAmount` / `this way`
/// parses tried elsewhere in the dispatcher.
fn parse_previous_effect_excess_damage_condition(lower: &str) -> Option<AbilityCondition> {
    let active = all_consuming((
        parse_excess_damage_subject,
        tag::<_, _, OracleError<'_>>(" is dealt excess damage this way"),
    ));
    let passive = all_consuming((
        tag::<_, _, OracleError<'_>>("excess damage was dealt"),
        opt(preceded(tag(" to "), parse_excess_damage_subject)),
        tag(" this way"),
    ));
    alt((map(active, |_| ()), map(passive, |_| ())))
        .parse(lower)
        .ok()?;
    Some(AbilityCondition::PreviousEffectAmount {
        comparator: Comparator::GT,
        rhs: QuantityExpr::Fixed { value: 0 },
        channel: DamageChannel::Excess,
    })
}

/// CR 120.3 + CR 608.2c: "a player is dealt damage this way" gates a rider
/// on both the recipient and the damage event emitted by the preceding
/// instruction. Deal-damage targets are either players or permanents, so a
/// player target is represented by the negated permanent match; the total
/// channel prevents the rider from firing when all damage was prevented.
fn parse_previous_effect_player_damage_condition(lower: &str) -> Option<AbilityCondition> {
    all_consuming(tag::<_, _, OracleError<'_>>(
        "a player is dealt damage this way",
    ))
    .parse(lower)
    .ok()?;
    Some(AbilityCondition::And {
        conditions: vec![
            AbilityCondition::PreviousEffectAmount {
                comparator: Comparator::GT,
                rhs: QuantityExpr::Fixed { value: 0 },
                channel: DamageChannel::Total,
            },
            AbilityCondition::Not {
                condition: Box::new(AbilityCondition::TargetMatchesFilter {
                    filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Permanent)),
                    use_lki: true,
                    subject_slot: None,
                }),
            },
        ],
    })
}

/// CR 701.8a + CR 608.2c: Vohar's drain checks the card discarded by the
/// preceding effect, not an object paid as an additional cost. The discard
/// publishes its hand-to-graveyard move in the resolution-local zone-change
/// ledger, which preserves the discarded card's types for this rider.
fn parse_effect_discard_instant_or_sorcery_condition(lower: &str) -> Option<AbilityCondition> {
    all_consuming(tag::<_, _, OracleError<'_>>(
        "you discarded an instant or sorcery card this way",
    ))
    .parse(lower)
    .ok()?;
    Some(AbilityCondition::ZoneChangedThisWay {
        filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::AnyOf(vec![
            TypeFilter::Instant,
            TypeFilter::Sorcery,
        ]))),
    })
}

/// CR 706.2 + CR 608.2c: "If the result is N or less / N or more / N or
/// greater / less than N / greater than N / equal to N / N" — a comparator on
/// the *actual* result of the most recent die roll in this resolution. Maps
/// to `AbilityCondition::PreviousEffectAmount`, which reads
/// `state.last_effect_amount` (populated for `Effect::RollDie` by
/// `previous_effect_amount_from_events`). Covers Deck of Many Things' "If the
/// result is 0 or less, discard your hand" and the analogous "is N or
/// less/more" phrasings used by every dice-table rider in the corpus.
/// CR 608.2c + CR 201.2: "if it shares a [quality] with [reference]" — compare
/// whether two anaphoric object references share at least one value of the named
/// quality at resolution time (Amareth: "If it shares a card type with that
/// permanent, you may reveal that card and put it into your hand").
fn parse_objects_share_quality_condition(
    text: &str,
    ctx: &ParseContext,
) -> Option<AbilityCondition> {
    let lower = text.to_lowercase();
    let (rest, subject) = if let Ok((rest, _)) =
        crate::parser::oracle_target::parse_word_bounded(lower.as_str(), "it")
    {
        (rest, TargetFilter::LastRevealed)
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that card").parse(lower.as_str()) {
        (rest, TargetFilter::LastRevealed)
    } else {
        return None;
    };
    let (rest, _) = tag::<_, _, OracleError<'_>>(" shares a ")
        .parse(rest)
        .ok()?;
    let (rest, quality) = crate::parser::oracle_target::parse_shared_quality(rest).ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" with ").parse(rest).ok()?;
    let offset = text.len() - rest.len();
    let (reference, remainder) = parse_objects_share_quality_reference(&text[offset..], ctx)?;
    if !remainder.trim().is_empty() {
        return None;
    }
    Some(AbilityCondition::ObjectsShareQuality {
        subject,
        reference,
        quality,
    })
}

/// Reference side of `parse_objects_share_quality_condition` — event-context
/// anaphors ("that permanent"), cost-paid objects, and typed target phrases.
fn parse_objects_share_quality_reference<'a>(
    text: &'a str,
    ctx: &ParseContext,
) -> Option<(TargetFilter, &'a str)> {
    if let Some((filter, rest)) = crate::parser::oracle_target::parse_event_context_ref(text) {
        return Some((filter, rest));
    }
    let lower = text.to_lowercase();
    if let Ok((rest, filter)) = value(
        TargetFilter::TriggeringSource,
        tag::<_, _, OracleError<'_>>("one of the discarded cards"),
    )
    .parse(lower.as_str())
    {
        let offset = text.len() - rest.len();
        return Some((filter, &text[offset..]));
    }
    if let Ok((rest, filter)) = value(
        TargetFilter::ParentTarget,
        tag::<_, _, OracleError<'_>>("the discarded card"),
    )
    .parse(lower.as_str())
    {
        let offset = text.len() - rest.len();
        return Some((filter, &text[offset..]));
    }
    if let Ok((rest, ())) = crate::parser::oracle_target::parse_word_bounded(&lower, "it") {
        let offset = text.len() - rest.len();
        let mut ctx_mut = ctx.clone();
        return Some((
            crate::parser::oracle_target::resolve_pronoun_target(&mut ctx_mut, "it"),
            &text[offset..],
        ));
    }
    let (filter, rest) = crate::parser::oracle_target::parse_target(text);
    if matches!(filter, TargetFilter::Any) {
        return None;
    }
    Some((filter, rest))
}

fn parse_die_result_condition(lower: &str) -> Option<AbilityCondition> {
    let rest = tag::<_, _, OracleError<'_>>("the result is ")
        .parse(lower)
        .ok()
        .map(|(rest, _)| rest)?;
    let (comparator, value) = parse_comparison_suffix(rest)?;
    Some(AbilityCondition::PreviousEffectAmount {
        comparator,
        rhs: QuantityExpr::Fixed { value },
        // CR 120.6: die-result comparison reads the total channel.
        channel: DamageChannel::Total,
    })
}

/// CR 603.4 + CR 601.2a + CR 603.6c: Origin-zone phrase for "entered from
/// <zone>" / "was cast from <zone>" ability gates. Zone tokens are delegated to
/// the canonical zone-word parser so the accepted zone vocabulary stays
/// centralized.
fn parse_entered_or_cast_origin_zone_phrase(
    input: &str,
) -> nom::IResult<&str, Zone, OracleError<'_>> {
    type E<'a> = OracleError<'a>;
    let (input, _) = opt(alt((
        tag::<_, _, E>("an opponent's "),
        tag("each opponent's "),
        tag("your "),
        tag("their "),
        tag("a "),
        tag("the "),
    )))
    .parse(input)?;
    parse_zone_word(input)
}

fn entered_or_cast_from_zone_condition(zone: Zone) -> AbilityCondition {
    // CR 603.4 + CR 601.2a + CR 603.6c: Model the source-origin gate as the
    // disjunction of entering the battlefield from that zone or being cast
    // from that zone.
    AbilityCondition::Or {
        conditions: vec![
            AbilityCondition::ZoneChangeObjectMatchesFilter {
                origin: Some(zone),
                destination: Zone::Battlefield,
                filter: TargetFilter::Any,
            },
            AbilityCondition::WasCast { zone: Some(zone) },
        ],
    }
}

/// CR 603.4 + CR 601.2 + CR 603.6c: "if it entered from your library or was
/// cast from your library" and the compact "if it entered or was cast from a
/// graveyard" class — ability-level gates for ETB draw-rider "instead" clauses
/// (Fblthp, the Lost). Composes zone-change origin with cast-origin checks.
fn parse_entered_or_cast_from_zone_ability_condition(lower: &str) -> Option<AbilityCondition> {
    let (rest, plural) = alt((
        value(false, tag::<_, _, OracleError<'_>>("it ")),
        value(true, tag::<_, _, OracleError<'_>>("they ")),
    ))
    .parse(lower)
    .ok()?;

    // Form B: "entered from <zone> or was/were cast from <zone>"
    if let Ok((rest, zone1)) = preceded(
        tag::<_, _, OracleError<'_>>("entered from "),
        parse_entered_or_cast_origin_zone_phrase,
    )
    .parse(rest)
    {
        let (rest, _) = tag::<_, _, OracleError<'_>>(" or ").parse(rest).ok()?;
        let zone2 = if plural {
            preceded(
                tag::<_, _, OracleError<'_>>("were cast from "),
                parse_entered_or_cast_origin_zone_phrase,
            )
            .parse(rest)
            .ok()
        } else {
            preceded(
                tag::<_, _, OracleError<'_>>("was cast from "),
                parse_entered_or_cast_origin_zone_phrase,
            )
            .parse(rest)
            .ok()
        };
        if let Some((rest, zone2)) = zone2 {
            if zone1 == zone2 && rest.trim().is_empty() {
                return Some(entered_or_cast_from_zone_condition(zone1));
            }
        }
    }

    // Form A: "entered or was/were cast from <zone>"
    let (rest, _) = tag::<_, _, OracleError<'_>>("entered or ")
        .parse(rest)
        .ok()?;
    let (rest, zone) = if plural {
        preceded(
            tag::<_, _, OracleError<'_>>("were cast from "),
            parse_entered_or_cast_origin_zone_phrase,
        )
        .parse(rest)
        .ok()?
    } else {
        preceded(
            tag::<_, _, OracleError<'_>>("was cast from "),
            parse_entered_or_cast_origin_zone_phrase,
        )
        .parse(rest)
        .ok()?
    };
    if !rest.trim().is_empty() {
        return None;
    }
    Some(entered_or_cast_from_zone_condition(zone))
}

fn parse_zone_change_object_matches_filter_condition(lower: &str) -> Option<AbilityCondition> {
    let (type_text, negated) = parse_zone_change_object_type_text(lower).ok()?.1;
    let (filter, leftover) = parse_type_phrase(type_text);
    if matches!(filter, TargetFilter::Any) || !leftover.trim().is_empty() {
        return None;
    }

    Some(maybe_negate(
        AbilityCondition::ZoneChangeObjectMatchesFilter {
            origin: None,
            destination: Zone::Battlefield,
            filter,
        },
        negated,
    ))
}

/// CR 608.2c + CR 400.7j: "[effect] if at least one <filter> was <verb> this
/// way" — the trailing (suffix) form of the prior-effect outcome gate. Later
/// text may reference an earlier instruction in the same effect, and objects
/// moved to public zones remain findable. Here "this way" refers to the set of
/// objects affected by the immediately-preceding instruction, and the condition
/// fires when that set contains at least one object matching `<filter>`. Kaya,
/// Orzhov Usurper's
/// +1 ("Exile up to two target
/// cards from a single graveyard. You gain 2 life if at least one creature
/// card was exiled this way.") is the motivating case.
///
/// The whole condition fragment must be consumed: `parse_zone_changed_this_way_clause`
/// already covers the existential quantifiers ("at least one"/"one or more"/
/// article), the type/subtype filter, both tenses, the verb set, and the
/// negation flag (`wasn't`/`isn't`). Requiring an empty remainder keeps this
/// matcher from firing on partial overlaps with longer condition phrases.
///
/// Both grammatical voices of the same event are recognized here so the leading
/// (`if …, [effect]`) and suffix (`[effect] if …`) forms produce identical
/// `ZoneChangedThisWay` conditions:
///   - passive: `a Cave (is|was) put onto the battlefield this way` —
///     `parse_zone_changed_this_way_clause`
///   - active: `you put a Cave onto the battlefield this way` —
///     `parse_you_put_onto_battlefield_this_way_clause` (Spelunking's ETB
///     rider "If you put a Cave onto the battlefield this way, you gain 4 life";
///     unlocks the whole "if you put a [type] onto the battlefield this way,
///     [bonus]" fetch/ramp payoff class).
fn parse_outcome_this_way_condition(lower: &str) -> Option<AbilityCondition> {
    let (rest, (filter, negated)) = parse_zone_changed_this_way_clause(lower)
        .or_else(|_| parse_you_put_onto_battlefield_this_way_clause(lower))
        .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(maybe_negate(
        AbilityCondition::ZoneChangedThisWay { filter },
        negated,
    ))
}

fn parse_zone_change_object_type_text(
    input: &str,
) -> nom::IResult<&str, (&str, bool), OracleError<'_>> {
    let (input, _) = tag("that ").parse(input)?;
    let (input, _) = alt((
        tag("permanent"),
        tag("enchantment"),
        tag("artifact"),
        tag("creature"),
        tag("equipment"),
        tag("aura"),
        tag("land"),
        tag("token"),
        tag("card"),
    ))
    .parse(input)?;
    let (input, negated) = alt((
        value(
            true,
            alt((
                tag(" is not an "),
                tag(" is not a "),
                tag(" is not "),
                tag(" isn't an "),
                tag(" isn't a "),
                tag(" isn't "),
            )),
        ),
        value(false, alt((tag(" is an "), tag(" is a "), tag(" is ")))),
    ))
    .parse(input)?;
    Ok(("", (input, negated)))
}

fn parse_target_supertype_condition_text(lower: &str) -> Option<AbilityCondition> {
    // CR 205.4: Subject anaphor + copula, tracking negation. "it" carries the
    // `'s` contraction; the explicit "that creature/permanent/card" anaphors
    // (Coiling Rebirth: "that creature isn't legendary") use the full copula.
    let (rest, negated) = alt((
        value(
            true,
            alt((
                tag::<_, _, OracleError<'_>>("it is not "),
                tag("it's not "),
                tag("it isn't "),
            )),
        ),
        value(false, alt((tag("it is "), tag("it's ")))),
        value(
            true,
            (
                alt((
                    tag("that creature"),
                    tag("that permanent"),
                    tag("that card"),
                )),
                alt((tag(" is not "), tag(" isn't "))),
            ),
        ),
        value(
            false,
            (
                alt((
                    tag("that creature"),
                    tag("that permanent"),
                    tag("that card"),
                )),
                tag(" is "),
            ),
        ),
    ))
    .parse(lower)
    .ok()?;
    let (rest, supertype) = parse_supertype_word(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    Some(maybe_negate(
        AbilityCondition::TargetMatchesFilter {
            filter: TargetFilter::Typed(
                TypedFilter::default()
                    .properties(vec![FilterProp::HasSupertype { value: supertype }]),
            ),
            use_lki: false,
            subject_slot: None,
        },
        negated,
    ))
}

fn parse_supertype_word(input: &str) -> nom::IResult<&str, Supertype, OracleError<'_>> {
    alt((
        value(
            Supertype::Legendary,
            tag::<_, _, OracleError<'_>>("legendary"),
        ),
        value(Supertype::Basic, tag("basic")),
        value(Supertype::Snow, tag("snow")),
    ))
    .parse(input)
}

/// CR 400.7 + CR 608.2c: Parse "a[n] [type] was [verb]'d this way".
///
/// Recognized verbs: `destroyed`, `exiled`, `sacrificed`, `returned`, `discarded`,
/// `milled`, `countered` — the set of imperative verbs that populate a tracked
/// set from their parent effect. Returns the matched type filter plus a
/// negation flag for `wasn't`/`was not`.
///
/// Used by Shredder's Technique ("if an enchantment was destroyed this way")
/// and parallel patterns where a conditional in the same clause tests the type
/// of the single parent target after the preceding effect resolved.
///
/// CR 303.4f / CR 301.5b: Also handles the present-tense "is [verb]ed this
/// way" form and the multi-word "put onto the battlefield" verb so that
/// future LKI-style cards using these tenses (e.g. "if a creature is
/// destroyed this way") parse without code change. The Aura/Equipment ETB
/// continuations (Armored Skyhunter, Vault 101: Birthday Party, Quest for
/// the Holy Relic, Stonehewer Giant) take the dedicated `ZoneChangedThisWay`
/// path in `strip_if_you_do_conditional` because the runtime semantic for
/// "the just-moved card" requires `state.last_zone_changed_ids`, not LKI of
/// the parent target — but extending this function keeps the parser
/// permissive for the LKI-semantic patterns and keeps the two combinators in
/// lockstep on tense + verb coverage.
/// CR 117.1 + CR 201.2: Parse "you've cast another spell named {LITERAL} this
/// game" / "you cast another spell named {LITERAL} this game" — the
/// game-scope cousin of the `this turn` family. Approach of the Second Sun
/// is the canonical user; the printed name is baked into a
/// `FilterProp::Named { name }` and counted against
/// `QuantityRef::SpellsCastThisGame { filter }`.
///
/// The comparator is `>= 2`, mirroring `parse_another_spell_cast_this_turn`'s
/// `minimum: 2` convention: at resolution time the currently-resolving spell
/// is already recorded in the cast history, so "another" means total count
/// must be at least 2 (this spell plus at least one prior printing of the
/// same name).
///
/// Matching is greedy on the name up to the trailing " this game" anchor.
/// The remainder (if any) is ignored — callers wrap this in a larger
/// condition (e.g. `AbilityCondition::And`) so the surrounding context can
/// own whatever follows.
fn parse_youve_cast_another_named_this_game_condition(lower: &str) -> Option<AbilityCondition> {
    let (rest, _): (&str, &str) = alt((
        tag::<_, _, OracleError<'_>>("you've cast another spell named "),
        tag("you cast another spell named "),
    ))
    .parse(lower)
    .ok()?;
    // CR 201.2: Consume the name up to the trailing " this game" anchor, then
    // drop the anchor itself. `terminated` returns the value of its first
    // parser (the name slice) and discards the anchor's output.
    let (_, name_text) = terminated(
        take_until::<_, _, OracleError<'_>>(" this game"),
        tag(" this game"),
    )
    .parse(rest)
    .ok()?;
    let name = name_text.trim();
    if name.is_empty() {
        return None;
    }
    let filter = TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Named {
        name: name.to_string(),
    }]));
    Some(AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisGame {
                scope: CountScope::Controller,
                filter: Some(filter),
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 2 },
    })
}

fn parse_a_type_was_verbed_this_way(lower: &str) -> Option<(TypeFilter, bool)> {
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("an "),
        tag::<_, _, OracleError<'_>>("a "),
    ))
    .parse(lower)
    .ok()?;

    let (rest, type_filter) = alt((
        value(
            TypeFilter::Creature,
            tag::<_, _, OracleError<'_>>("creature"),
        ),
        value(TypeFilter::Land, tag("land")),
        value(TypeFilter::Artifact, tag("artifact")),
        value(TypeFilter::Enchantment, tag("enchantment")),
        value(TypeFilter::Planeswalker, tag("planeswalker")),
        value(TypeFilter::Instant, tag("instant")),
        value(TypeFilter::Sorcery, tag("sorcery")),
    ))
    .parse(rest)
    .ok()?;

    // CR 400.7 + CR 608.2c: Tense + verb are orthogonal axes. Compose with
    // independent `alt` chains so adding a new verb (or tense) is a single
    // tag arm, not an N×M permutation expansion.
    let (rest, negated) = alt((
        value(true, tag::<_, _, OracleError<'_>>(" wasn't ")),
        value(true, tag(" isn't ")),
        value(true, tag(" was not ")),
        value(true, tag(" is not ")),
        value(false, tag(" was ")),
        value(false, tag(" is ")),
    ))
    .parse(rest)
    .ok()?;

    let (rest, _) = alt((
        // Multi-word verb listed first: longest-match-wins keeps the
        // single-word `tag("put")` (no such tag here, but defensive against
        // future additions) from short-circuiting the multi-word phrase.
        tag::<_, _, OracleError<'_>>("put onto the battlefield"),
        tag("destroyed"),
        tag("exiled"),
        tag("sacrificed"),
        tag("returned"),
        tag("discarded"),
        tag("milled"),
        tag("countered"),
    ))
    .parse(rest)
    .ok()?;

    let (rest, _) = tag::<_, _, OracleError<'_>>(" this way").parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some((type_filter, negated))
}

/// CR 603.4: Parse "[subject] the [Nth] time[ this ability has resolved this turn]".
///
/// Subject is one of `"this is"`, `"it's"`, `"it is"` — the second/third forms are
/// anaphoric continuations whose "this ability has resolved this turn" tail was
/// printed in a prior sentence. Ordinals span first–tenth (Omnath/Ashling print
/// up to third; the broader ceiling is conservative).
fn parse_nth_resolution_condition(lower: &str) -> Option<u32> {
    type E<'a> = OracleError<'a>;
    let (rest, _) = alt((
        tag::<_, _, E>("this is the "),
        tag("it's the "),
        tag("it is the "),
    ))
    .parse(lower)
    .ok()?;
    let (rest, n) = alt((
        value(1u32, tag::<_, _, E>("first")),
        value(2u32, tag("second")),
        value(3u32, tag("third")),
        value(4u32, tag("fourth")),
        value(5u32, tag("fifth")),
        value(6u32, tag("sixth")),
        value(7u32, tag("seventh")),
        value(8u32, tag("eighth")),
        value(9u32, tag("ninth")),
        value(10u32, tag("tenth")),
    ))
    .parse(rest)
    .ok()?;
    let (rest, _) = tag::<_, _, E>(" time").parse(rest).ok()?;
    let rest = rest.trim_end_matches('.').trim();
    // Tail is optional — anaphoric forms ("if it's the second time") drop it
    // because the prior sentence already established "this ability has resolved
    // this turn" as the subject.
    if rest.is_empty() || rest == "this ability has resolved this turn" {
        Some(n)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_nom::condition::parse_inner_condition;
    use crate::parser::parse_oracle_text;
    use crate::types::counter::{CounterMatch, CounterType};

    /// CR 400.7 + CR 608.2c: S07 Batch 1 — the leading-"if" active-voice
    /// this-way gates must resolve to their typed conditions (previously they
    /// reached only the "when"-guarded combinators and dropped to `None`).
    /// Building-block coverage for the hoisted + new combinators; the runtime
    /// gating is in `crates/engine/tests/s07_this_way_conditions.rs`.
    #[test]
    fn s07_if_put_onto_battlefield_this_way_hoisted() {
        // Oviya / Spelunking: "if you put a[n] <type> onto the battlefield this way".
        let (cond, body) = strip_if_you_do_conditional(
            "if you put an artifact onto the battlefield this way, you gain 4 life",
        );
        assert_eq!(body, "you gain 4 life");
        let Some(AbilityCondition::ZoneChangedThisWay {
            filter: TargetFilter::Typed(TypedFilter { type_filters, .. }),
        }) = cond
        else {
            panic!("expected ZoneChangedThisWay Artifact, got {cond:?}");
        };
        assert!(type_filters
            .iter()
            .any(|f| matches!(f, TypeFilter::Artifact)));
    }

    #[test]
    fn s07_if_put_into_hand_this_way() {
        // Town Greeter: "if you put a <subtype> card into your hand this way".
        let (cond, body) = strip_if_you_do_conditional(
            "if you put a town card into your hand this way, you gain 2 life",
        );
        assert_eq!(body, "you gain 2 life");
        let Some(AbilityCondition::ZoneChangedThisWay {
            filter: TargetFilter::Typed(TypedFilter { type_filters, .. }),
        }) = cond
        else {
            panic!("expected ZoneChangedThisWay Town, got {cond:?}");
        };
        assert!(type_filters
            .iter()
            .any(|f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Town"))));
    }

    #[test]
    fn s07_if_put_counters_on_type_this_way() {
        // Call the Spirit Dragons: "if you put +1/+1 counters on five Dragons this way".
        let (cond, body) = strip_if_you_do_conditional(
            "if you put +1/+1 counters on five Dragons this way, you win the game",
        );
        assert_eq!(body, "you win the game");
        assert!(
            matches!(
                cond,
                Some(AbilityCondition::QuantityCheck {
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 5 },
                    ..
                })
            ),
            "got {cond:?}"
        );
    }

    #[test]
    fn s07_if_put_no_cards_into_hand_this_way_is_count_zero() {
        // Nashi: "if you put no cards into your hand this way" → TrackedSetSize == 0.
        let (cond, body) = strip_if_you_do_conditional(
            "if you put no cards into your hand this way, put a +1/+1 counter on nashi",
        );
        assert_eq!(body, "put a +1/+1 counter on nashi");
        assert!(
            matches!(
                cond,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::TrackedSetSize
                    },
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Fixed { value: 0 },
                })
            ),
            "got {cond:?}"
        );
    }

    #[test]
    fn s07_if_another_was_returned_this_way_excludes_source() {
        // Arid Archway: "if another <subtype> was returned this way".
        let (cond, body) =
            strip_if_you_do_conditional("if another desert was returned this way, surveil 1");
        assert_eq!(body, "surveil 1");
        let Some(AbilityCondition::ZoneChangedThisWay {
            filter: TargetFilter::Typed(TypedFilter { properties, .. }),
        }) = cond
        else {
            panic!("expected ZoneChangedThisWay, got {cond:?}");
        };
        assert!(
            properties.contains(&FilterProp::Another),
            "'another' must add FilterProp::Another (exclude source), got {properties:?}"
        );
    }

    #[test]
    fn s07_if_disjunctive_subject_destroyed_this_way() {
        // Break the Spell: "if a permanent you controlled or a token was destroyed
        // this way" → ZoneChangedThisWay { Or([Permanent+You, Token]) }.
        let (cond, _) = strip_if_you_do_conditional(
            "if a permanent you controlled or a token was destroyed this way, draw a card",
        );
        let Some(AbilityCondition::ZoneChangedThisWay {
            filter: TargetFilter::Or { filters },
        }) = cond
        else {
            panic!("expected ZoneChangedThisWay Or, got {cond:?}");
        };
        assert_eq!(filters.len(), 2, "two disjuncts");
    }

    #[test]
    fn s07_if_control_or_returned_this_way_is_condition_or() {
        // Cache Grab: mixed disjunction of a control-presence gate and a
        // bounce-this-way gate → AbilityCondition::Or.
        let (cond, body) = strip_if_you_do_conditional(
            "if you control a squirrel or returned a squirrel card to your hand this way, create a food token",
        );
        assert_eq!(body, "create a food token");
        let Some(AbilityCondition::Or { conditions }) = cond else {
            panic!("expected AbilityCondition::Or, got {cond:?}");
        };
        assert!(matches!(
            conditions.as_slice(),
            [
                AbilityCondition::ControllerControlsMatching { .. },
                AbilityCondition::ZoneChangedThisWay { .. }
            ]
        ));
    }

    #[test]
    fn s07_if_draw_this_way_is_previous_effect_amount() {
        // Transcendent Archaic: "if you draw one or more cards this way".
        let (cond, body) = strip_if_you_do_conditional(
            "if you draw one or more cards this way, discard two cards",
        );
        assert_eq!(body, "discard two cards");
        assert!(
            matches!(
                cond,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::PreviousEffectAmount { .. }
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                })
            ),
            "got {cond:?}"
        );
    }

    #[test]
    fn strip_target_keyword_instead_parses_toxic_as_typed_keyword() {
        // "if that creature has toxic, ..." must lower to a real Toxic keyword,
        // not Unknown("toxic"); runtime has_keyword matches by discriminant, so an
        // Unknown variant would make the rider silently dead (Hexgold Slash,
        // Compleat Devotion, Porcelain Zealot). Building-block test, not card-name
        // hardcoded.
        let (cond, body) = strip_target_keyword_instead("If that creature has toxic, draw a card.");
        assert!(matches!(
            cond,
            Some(AbilityCondition::TargetHasKeywordInstead {
                keyword: Keyword::Toxic(_)
            })
        ));
        assert_eq!(body, "draw a card.");
    }

    /// CR 505.1 + CR 102.1: resolution-time "it is[n't] your [phase]" gate routes
    /// through the production dispatcher `try_nom_condition_as_ability_condition`
    /// (the function the new arm lives in). "your [phase]" decomposes into
    /// `And([CurrentPhaseIs{phases}, IsYourTurn])`; "isn't" wraps in `Not`. Reverting
    /// the parser arm makes every assertion here flip to `None`.
    #[test]
    fn current_phase_condition_dispatches_through_production_entry() {
        let parse =
            |t: &str| try_nom_condition_as_ability_condition(t, &mut ParseContext::default());

        let your_phase = |phases: Vec<Phase>| AbilityCondition::And {
            conditions: vec![
                AbilityCondition::CurrentPhaseIs { phases },
                AbilityCondition::IsYourTurn,
            ],
        };
        let main = || your_phase(vec![Phase::PreCombatMain, Phase::PostCombatMain]);

        // Negated polarity — Dose of Dawnglow's exact gate. Revert-failing assertion.
        assert_eq!(
            parse("it isn't your main phase"),
            Some(AbilityCondition::Not {
                condition: Box::new(main()),
            }),
        );
        // Positive polarity + contraction variant both reach the same shape.
        assert_eq!(parse("it's your main phase"), Some(main()));
        assert_eq!(parse("it is your main phase"), Some(main()));

        // Class generality: the shared `parse_phase_name_set` covers every named
        // phase/step, not just the main phase.
        assert_eq!(
            parse("it isn't your upkeep"),
            Some(AbilityCondition::Not {
                condition: Box::new(your_phase(vec![Phase::Upkeep])),
            }),
        );
        assert_eq!(
            parse("it's your end step"),
            Some(your_phase(vec![Phase::End])),
        );

        // Negative: an unrelated condition must NOT be captured by this arm.
        assert_ne!(
            parse("it is your turn"),
            Some(main()),
            "bare 'your turn' (no phase name) must not match the current-phase arm",
        );
    }

    /// CR 120.10: both voices of the excess-damage condition route through the
    /// production dispatcher and bind the typed excess channel. The passive voice
    /// ("excess damage was dealt [to <subject>] this way") is the DealDamage class
    /// (Torch the Witness, Orbital Plunge); the active voice ("<subject> is dealt
    /// excess damage this way") is the Fight class (The Last Agni Kai). Reverting
    /// the passive arm flips the passive cases to `None`; reverting the
    /// `DamageChannel::Excess` binding to `Total` flips the channel and breaks
    /// runtime gating (the resolver test below proves the channel is load-bearing).
    #[test]
    fn excess_damage_condition_dispatches_to_excess_channel() {
        let parse =
            |t: &str| try_nom_condition_as_ability_condition(t, &mut ParseContext::default());
        let excess = || {
            Some(AbilityCondition::PreviousEffectAmount {
                comparator: Comparator::GT,
                rhs: QuantityExpr::Fixed { value: 0 },
                channel: DamageChannel::Excess,
            })
        };

        // Passive voice — DealDamage class. Subject optional and varied.
        assert_eq!(parse("excess damage was dealt this way"), excess());
        assert_eq!(
            parse("excess damage was dealt to that creature this way"),
            excess()
        );
        assert_eq!(
            parse("excess damage was dealt to a permanent this way"),
            excess()
        );
        // Active voice — Fight class — also binds the excess channel (decoupled).
        assert_eq!(
            parse("that creature is dealt excess damage this way"),
            excess()
        );
        assert_eq!(
            parse("the creature the opponent controls is dealt excess damage this way"),
            excess()
        );

        // Revert-discriminating negatives:
        // - turn-scoped excess is the DamageDealtThisTurn class, NOT this one.
        assert_eq!(parse("excess damage was dealt this turn"), None);
        // - a non-excess "this way" anaphor must NOT be captured by the excess arm
        //   (proves the all_consuming `excess` arms don't shadow the plain parse).
        assert_ne!(parse("a creature is dealt damage this way"), excess());
        // - the excess arm binds Excess, never the Total default.
        assert_ne!(
            parse("excess damage was dealt this way"),
            Some(AbilityCondition::PreviousEffectAmount {
                comparator: Comparator::GT,
                rhs: QuantityExpr::Fixed { value: 0 },
                channel: DamageChannel::Total,
            })
        );
    }

    /// CR 115.1 + CR 208.1: target-anaphoric possessive P/T comparison — "that
    /// creature's power is 2 or less" / "that permanent's toughness is exactly 3"
    /// → `TargetMatchesFilter{PtComparison{.., Current, .., Fixed}}` (Target scope,
    /// NOT `Power{CostPaidObject}`). The "exactly" leaf composes with the
    /// "or less"/"or greater" thresholds.
    #[test]
    fn target_possessive_pt_comparison_binds_target_scope() {
        let pt = |c: &AbilityCondition| -> (PtStat, Comparator, i32) {
            match c {
                AbilityCondition::TargetMatchesFilter {
                    filter: TargetFilter::Typed(tf),
                    use_lki: false,
                    ..
                } => match tf.properties.as_slice() {
                    [FilterProp::PtComparison {
                        stat,
                        scope: PtValueScope::Current,
                        comparator,
                        value: QuantityExpr::Fixed { value },
                    }] => (*stat, *comparator, *value),
                    other => panic!("expected single PtComparison, got {other:?}"),
                },
                other => panic!("expected Target-scoped TargetMatchesFilter, got {other:?}"),
            }
        };
        assert_eq!(
            pt(
                &parse_target_possessive_pt_comparison_text("that creature's power is 2 or less")
                    .unwrap()
            ),
            (PtStat::Power, Comparator::LE, 2)
        );
        assert_eq!(
            pt(&parse_target_possessive_pt_comparison_text(
                "that permanent's toughness is exactly 3"
            )
            .unwrap()),
            (PtStat::Toughness, Comparator::EQ, 3)
        );
        // The reflexive past-tense "had power" form is owned by the reflexive arm,
        // not this possessive-present recognizer.
        assert!(
            parse_target_possessive_pt_comparison_text("that creature had power 2 or less")
                .is_none()
        );
    }

    /// CR 201.5: the source equality wrapper fires ONLY on "exactly N" — the
    /// threshold forms are owned upstream by `strip_property_conditional`, so the
    /// EQ guard rejects them (returns None) to avoid re-scoping them.
    #[test]
    fn source_pt_comparison_text_is_exactly_only() {
        assert_eq!(
            parse_source_pt_comparison_condition_text("its power is exactly 20"),
            Some(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Source,
                    },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 20 },
            })
        );
        assert!(parse_source_pt_comparison_condition_text("its power is 2 or less").is_none());
        assert!(parse_source_pt_comparison_condition_text("its power is 2 or greater").is_none());
    }

    /// CR 608.2c: the " or if " disjunction (Reptilian Recruiter) lowers to
    /// `Or[ TargetMatchesFilter{PtComparison{Power,Current,LE,2}}, QuantityCheck{
    /// ObjectCount GE 1} ]`. All-or-nothing: a partial disjunct returns None.
    #[test]
    fn or_if_disjunction_binds_reptilian_gate() {
        let mut ctx = ParseContext::default();
        let cond = try_nom_condition_as_ability_condition(
            "that creature's power is 2 or less or if you control another lizard",
            &mut ctx,
        );
        let conds = match cond {
            Some(AbilityCondition::Or { conditions }) => conditions,
            other => panic!("expected Or disjunction, got {other:?}"),
        };
        assert_eq!(conds.len(), 2);
        assert!(matches!(
            &conds[0],
            AbilityCondition::TargetMatchesFilter { use_lki: false, .. }
        ));
        assert!(matches!(
            &conds[1],
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. }
                },
                comparator: Comparator::GE,
                ..
            }
        ));
        // Honest fallthrough: when a disjunct does not parse, no partial Or binds.
        let mut ctx2 = ParseContext::default();
        assert!(try_nom_condition_as_ability_condition(
            "the sky is blue or if you control another lizard",
            &mut ctx2,
        )
        .is_none());
    }

    /// CR 702.119a-c + CR 702.187b: "[possessive] <emerge|mayhem> cost was paid"
    /// lowers to `CastVariantPaid { variant }` across the possessive-subject
    /// variants, and the membership filter rejects the other cast-variant phrases
    /// so their established Route-2 (`CastVariantPaidInstead`) handling is
    /// untouched.
    #[test]
    fn parse_cast_variant_cost_paid_condition_recognizes_emerge_and_mayhem() {
        for (subject, expected) in [
            ("emerge cost was paid", CastVariantPaid::Emerge),
            (
                "this creature's emerge cost was paid",
                CastVariantPaid::Emerge,
            ),
            ("its emerge cost was paid", CastVariantPaid::Emerge),
            ("this spell's emerge cost was paid", CastVariantPaid::Emerge),
            (
                "this creature's emerge cost was paid this turn",
                CastVariantPaid::Emerge,
            ),
            // Mayhem joins the generic-instead route (Sandman's Quicksand).
            ("mayhem cost was paid", CastVariantPaid::Mayhem),
            ("this spell's mayhem cost was paid", CastVariantPaid::Mayhem),
            ("its mayhem cost was paid", CastVariantPaid::Mayhem),
            (
                "this spell's mayhem cost was paid this turn",
                CastVariantPaid::Mayhem,
            ),
        ] {
            assert_eq!(
                parse_cast_variant_cost_paid_condition(subject),
                Some(AbilityCondition::CastVariantPaid {
                    variant: expected,
                    subject: ObjectScope::Source,
                }),
                "{subject:?} must lower to CastVariantPaid {{ {expected:?} }}"
            );
        }
        // Membership filter: the other cast-variant phrases keep their Route-2
        // (`strip_additional_cost_conditional` → `CastVariantPaidInstead`) path,
        // and "reduced" (not "paid") stays a clean gap.
        for other in [
            "this creature's spectacle cost was paid",
            "its surge cost was paid",
            "prowl cost was paid",
            "this creature's emerge cost was reduced",
        ] {
            assert_eq!(
                parse_cast_variant_cost_paid_condition(other),
                None,
                "{other:?} must not be claimed by the emerge/mayhem instead recognizer"
            );
        }
    }

    /// CR 115.1 + CR 608.2c + CR 702.185a: Full Bore's target-scoped "that
    /// creature was cast for its warp cost" routes through the production
    /// dispatcher `try_nom_condition_as_ability_condition` to `CastVariantPaid {
    /// variant: Warp, subject: Target }` — distinct from the source-scoped "if its
    /// warp cost was paid" form. Reverting the new parser arm flips the positive
    /// assertions to `None`. The negative arm proves the turn-wide "a spell was
    /// warped this turn" still lowers to `SpellCastWithVariantThisTurn` (NOT
    /// target-scoped), so the two phrasings stay disjoint.
    #[test]
    fn target_cast_for_warp_cost_dispatches_target_scoped() {
        let parse =
            |t: &str| try_nom_condition_as_ability_condition(t, &mut ParseContext::default());

        let target_warp = Some(AbilityCondition::CastVariantPaid {
            variant: CastVariantPaid::Warp,
            subject: ObjectScope::Target,
        });

        // Every target-anaphoric subject form reaches the same Target-scoped shape.
        assert_eq!(
            parse("that creature was cast for its warp cost"),
            target_warp
        );
        assert_eq!(parse("it was cast for its warp cost"), target_warp);
        assert_eq!(
            parse("that permanent was cast for its warp cost"),
            target_warp
        );
        // Optional " this turn" tail is consumed (CR 603.4).
        assert_eq!(
            parse("that creature was cast for its warp cost this turn"),
            target_warp
        );

        // Negative: the turn-wide flag stays `SpellCastWithVariantThisTurn`, never
        // the new Target-scoped arm.
        assert_eq!(
            parse("a spell was warped this turn"),
            Some(AbilityCondition::SpellCastWithVariantThisTurn {
                variant: crate::types::game_state::CastingVariant::Warp,
            })
        );
    }

    #[test]
    fn parse_no_mana_spent_to_cast_target_condition_reads_ability_target_mana() {
        let cond =
            parse_no_mana_spent_to_cast_target_condition_text("no mana was spent to cast it")
                .expect("should parse target no-mana-spent condition");
        assert!(matches!(
            cond,
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ManaSpentToCast {
                        scope: CastManaObjectScope::AbilityTarget,
                        metric: CastManaSpentMetric::Total,
                    },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            }
        ));

        let (cond, text) = strip_suffix_conditional(
            "Counter target spell if no mana was spent to cast it",
            &mut ParseContext::default(),
        );
        assert_eq!(text, "Counter target spell");
        assert!(matches!(cond, Some(AbilityCondition::QuantityCheck { .. })));
    }

    /// CR 122.1f + CR 109.4 + CR 608.2c: Corrupted Resolve — "counter target
    /// spell if its controller is poisoned" lowers the trailing condition to a
    /// `QuantityCheck` over the target controller's poison counters (>= 1 ==
    /// "poisoned"), and the counter body is stripped.
    #[test]
    fn parse_controller_is_poisoned_target_condition_reads_target_controller_poison() {
        use crate::types::player::PlayerCounterKind;

        let expected = AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::TargetControllerCounter {
                    kind: PlayerCounterKind::Poison,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        };

        assert_eq!(
            parse_target_controller_poisoned_condition_text("its controller is poisoned"),
            Some(expected.clone())
        );
        // CR 608.2c demonstrative-subject variants reach the same shape.
        assert_eq!(
            parse_target_controller_poisoned_condition_text("that spell's controller is poisoned"),
            Some(expected.clone())
        );

        // Full clause: condition peeled, the counter body remains for the
        // downstream `Effect::Counter` parse (mirrors the Nix suffix path).
        let (cond, text) = strip_suffix_conditional(
            "Counter target spell if its controller is poisoned",
            &mut ParseContext::default(),
        );
        assert_eq!(text, "Counter target spell");
        assert_eq!(cond, Some(expected));

        // Negative: an unrelated "its controller ..." predicate must not match.
        let unrelated =
            parse_target_controller_poisoned_condition_text("its controller controls a Swamp");
        assert!(unrelated.is_none());
    }

    /// CR 608.2d + CR 107.4 + CR 202.1: Omnath, Locus of All — "you may reveal
    /// that card if it has three or more colored mana symbols in its mana cost"
    /// re-homes the eligibility check as a `QuantityCheck` (GE) over the target's
    /// colored-mana-symbol count (`color: None`), and the optional-reveal effect
    /// text is stripped. The recognizer must run BEFORE the rehomeable bail since
    /// "it has " is in NON_REHOMEABLE_CONDITION_PREFIXES.
    #[test]
    fn parse_colored_mana_symbol_count_condition_rehomes_eligibility() {
        let cond = parse_colored_mana_symbol_count_target_condition(
            "it has three or more colored mana symbols in its mana cost",
        )
        .expect("should parse the colored-mana-symbol eligibility condition");
        assert_eq!(
            cond,
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ManaSymbolsInManaCost {
                        scope: ObjectScope::Target,
                        color: None,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            }
        );

        let (cond, text) = strip_suffix_conditional(
            "You may reveal that card if it has three or more colored mana symbols in its mana cost",
            &mut ParseContext::default(),
        );
        assert_eq!(text, "You may reveal that card");
        assert!(matches!(
            cond,
            Some(AbilityCondition::QuantityCheck {
                comparator: Comparator::GE,
                ..
            })
        ));

        // Negative: a different number still parses (typed comparator/count axis),
        // and an unrelated "it has" predicate does not falsely match.
        assert!(parse_colored_mana_symbol_count_target_condition("it has flying").is_none());
    }

    #[test]
    fn parse_was_kicked_suffix_condition_on_counter() {
        let (cond, text) = strip_suffix_conditional(
            "Counter target spell if it was kicked",
            &mut ParseContext::default(),
        );
        assert_eq!(text, "Counter target spell");
        assert_eq!(cond, Some(AbilityCondition::additional_cost_paid_any()));
    }

    #[test]
    fn parse_mana_spent_vs_mana_value_target_condition_reads_ability_target_mana() {
        let cond = parse_mana_spent_vs_mana_value_target_condition_text(
            "the amount of mana spent to cast that spell was less than its mana value",
        )
        .expect("should parse target mana-spent comparison");
        assert!(matches!(
            cond,
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ManaSpentToCast {
                        scope: CastManaObjectScope::AbilityTarget,
                        metric: CastManaSpentMetric::Total,
                    },
                },
                comparator: Comparator::LT,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Target,
                    },
                },
            }
        ));

        let (cond, text) = strip_leading_general_conditional(
            "If the amount of mana spent to cast that spell was less than its mana value, you draw a card.",
            &mut ParseContext::default(),
        );
        assert_eq!(text, "you draw a card.");
        assert!(matches!(cond, Some(AbilityCondition::QuantityCheck { .. })));
    }

    /// CR 508.1a: filtered attack-history condition — "you attacked with <X>"
    /// resolves to a QuantityCheck over the (optionally filtered) AttackedThisTurn
    /// count. Covers the count, commander, and self-reference forms.
    #[test]
    fn attacked_with_filter_condition_forms() {
        // Count form: "three or more creatures" → unfiltered, GE 3.
        assert_eq!(
            parse_attacked_with_filter_condition("you attacked with three or more creatures"),
            Some(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::AttackedThisTurn {
                        scope: CountScope::Controller,
                        filter: None,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            })
        );
        // Commander form → IsCommander filter, GE 1.
        let cmdr = parse_attacked_with_filter_condition("you attacked with a commander");
        assert!(matches!(
            cmdr,
            Some(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::AttackedThisTurn {
                        scope: CountScope::Controller,
                        filter: Some(TargetFilter::Typed(ref tf)),
                    },
                },
                ..
            }) if tf.properties.contains(&FilterProp::IsCommander)
        ));
        // Self-reference (Goblin Researcher) → SelfRef filter.
        assert!(matches!(
            parse_attacked_with_filter_condition("you attacked with ~"),
            Some(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::AttackedThisTurn {
                        scope: CountScope::Controller,
                        filter: Some(TargetFilter::SelfRef),
                    },
                },
                ..
            })
        ));
    }

    /// Assert a condition is a (possibly `Not`-wrapped) `TargetMatchesFilter`
    /// over a creature `TypedFilter` carrying `FilterProp::AttackedThisTurn`
    /// with `use_lki: false`. Returns whether the condition was negated, so the
    /// caller distinguishes the Aggression (negated) and Berserk (positive)
    /// shapes. Mirrors the structural assertions in
    /// `attacked_with_filter_condition_forms`.
    fn assert_attacked_this_turn_target_match(cond: &AbilityCondition) -> bool {
        let (inner, negated) = match cond {
            AbilityCondition::Not { condition } => (condition.as_ref(), true),
            other => (other, false),
        };
        match inner {
            AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(tf),
                use_lki,
                ..
            } => {
                assert!(
                    !use_lki,
                    "attacked-this-turn gate must use current state, not LKI (CR 400.7)"
                );
                assert!(
                    tf.type_filters.contains(&TypeFilter::Creature),
                    "filter must be a creature TypedFilter, got {tf:?}"
                );
                assert!(
                    tf.properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::AttackedThisTurn { .. })),
                    "filter must carry AttackedThisTurn, got {tf:?}"
                );
            }
            other => panic!("expected TargetMatchesFilter, got {other:?}"),
        }
        negated
    }

    /// CR 508.1a + CR 603.4: Aggression's end-step trigger — "destroy that
    /// creature if it didn't attack this turn" — peels the trailing-if into a
    /// `Not(TargetMatchesFilter{ creature + AttackedThisTurn })` gate, leaving
    /// the bare "destroy that creature" effect. Reverting the dispatch arm makes
    /// this `cond` `None`, failing the `expect`.
    #[test]
    fn target_attacked_this_turn_condition_aggression_negated() {
        let (cond, text) = strip_suffix_conditional(
            "destroy that creature if it didn't attack this turn",
            &mut ParseContext::default(),
        );
        assert_eq!(text, "destroy that creature");
        let cond = cond.expect("Aggression trailing-if must extract a combat-history gate");
        assert!(
            assert_attacked_this_turn_target_match(&cond),
            "Aggression's 'didn't attack' form must be Not-wrapped"
        );
    }

    /// CR 508.1a + CR 603.7: Berserk's delayed end-step trigger — "destroy that
    /// creature if it attacked this turn" — must produce the positive (NOT
    /// `Not`-wrapped) `TargetMatchesFilter{ creature + AttackedThisTurn }` so the
    /// drawback only fires when the creature actually attacked.
    #[test]
    fn target_attacked_this_turn_condition_berserk_positive() {
        let (cond, text) = strip_suffix_conditional(
            "destroy that creature if it attacked this turn",
            &mut ParseContext::default(),
        );
        assert_eq!(text, "destroy that creature");
        let cond = cond.expect("Berserk trailing-if must extract a combat-history gate");
        assert!(
            !assert_attacked_this_turn_target_match(&cond),
            "Berserk's 'attacked' form must NOT be Not-wrapped"
        );
    }

    /// No-regression: the controller-scoped "you attacked this turn" form must
    /// stay on the controller path and never reach the anaphoric arm — the
    /// anaphoric subject parser cannot match "you".
    #[test]
    fn target_attacked_this_turn_does_not_swallow_controller_form() {
        assert!(
            parse_target_attacked_this_turn_condition_text("you attacked this turn").is_none(),
            "controller-scoped 'you attacked this turn' must not match the anaphoric gate"
        );
    }

    /// CR 608.2c + CR 201.2: Amareth pattern is now a typed
    /// `ObjectsShareQuality` condition — the structural fallback must NOT strip
    /// the head. Issue #2921.
    #[test]
    fn objects_share_quality_condition_parses_amareth_pattern() {
        let input = "it shares a card type with that permanent";
        let condition = try_nom_condition_as_ability_condition(input, &mut ParseContext::default());
        assert_eq!(
            condition,
            Some(AbilityCondition::ObjectsShareQuality {
                subject: TargetFilter::LastRevealed,
                reference: TargetFilter::TriggeringSource,
                quality: crate::types::ability::SharedQuality::CardType,
            })
        );
    }

    /// CR 608.2c + CR 608.2d: When the leading `If <X>, ` has no typed
    /// recognizer AND the body begins with `"you may "`, the structural
    /// fallback strips the head so the inner optional choice can be peeled
    /// downstream. Issue #2277 — Tithe pattern (still unrepresented).
    #[test]
    fn strip_unrecognized_conditional_head_fires_on_optional_body() {
        let input = "If target opponent controls more lands than you, you may search \
                     your library for an additional Plains card";
        let stripped = strip_unrecognized_conditional_head_when_body_optional(input);
        assert_eq!(
            stripped,
            "you may search your library for an additional Plains card"
        );
    }

    /// CR 608.2c: Mandatory-body guard — when the body does NOT begin with
    /// `"you may "`, the function MUST be a no-op so a mandatory effect is
    /// never silently un-conditioned. Issue #2277 regression.
    #[test]
    fn strip_unrecognized_conditional_head_noop_on_mandatory_body() {
        let input = "If you control a creature, draw a card";
        let stripped = strip_unrecognized_conditional_head_when_body_optional(input);
        assert_eq!(stripped, input);
    }

    /// No-If-head guard — when there is no leading conditional at all, the
    /// function MUST return the text unchanged.
    #[test]
    fn strip_unrecognized_conditional_head_noop_on_no_if_head() {
        let input = "You may search your library for a card";
        let stripped = strip_unrecognized_conditional_head_when_body_optional(input);
        assert_eq!(stripped, input);
    }

    /// CR 603.12: After refactoring `strip_if_you_do_conditional` to delegate to
    /// the shared `parse_reflexive_conditional_connector` combinator, all eight
    /// reflexive connectors must still strip to the same `(condition, rest)`.
    #[test]
    fn strip_if_you_do_conditional_reflexive_connectors_unchanged() {
        let effect = AbilityCondition::effect_performed();
        let not_effect = AbilityCondition::Not {
            condition: Box::new(AbilityCondition::effect_performed()),
        };
        let cases: &[(&str, Option<AbilityCondition>)] = &[
            (
                "when you do, draw a card",
                Some(AbilityCondition::WhenYouDo),
            ),
            ("if a player does, draw a card", Some(effect.clone())),
            ("if they do, draw a card", Some(effect.clone())),
            ("if that player does, draw a card", Some(effect.clone())),
            ("if the player does, draw a card", Some(effect.clone())),
            (
                "if that player doesn't, draw a card",
                Some(not_effect.clone()),
            ),
            (
                "if the player doesn't, draw a card",
                Some(not_effect.clone()),
            ),
            ("if they don't, draw a card", Some(not_effect.clone())),
            ("if you do, draw a card", Some(effect.clone())),
        ];
        for (input, expected) in cases {
            let (condition, rest) = strip_if_you_do_conditional(input);
            assert_eq!(&condition, expected, "condition mismatch for {input:?}");
            assert_eq!(rest, "draw a card", "rest mismatch for {input:?}");
        }
    }

    #[test]
    fn difference_expr_composes_unsigned_gap_from_quantity_check() {
        // CR 608.2c: "the difference" back-references the two operands the earlier
        // condition established — class-general over any QuantityCheck.
        let cond = AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize,
            },
            comparator: Comparator::LT,
            rhs: QuantityExpr::Fixed { value: 2 },
        };
        assert_eq!(
            difference_expr(&cond),
            Some(QuantityExpr::Difference {
                left: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                }),
                right: Box::new(QuantityExpr::Fixed { value: 2 }),
            }),
        );
        // Non-comparison conditions yield None.
        assert_eq!(difference_expr(&AbilityCondition::IsYourTurn), None);
    }

    #[test]
    fn strip_if_you_do_conditional_renegade_reaper_at_least_one_angel() {
        // Issue #477 — Renegade Reaper: the swallowed-clause splitter must
        // recognize the quantified "if at least one Angel card is milled this
        // way" gate and emit `ZoneChangedThisWay { Angel }` on the GainLife
        // sub-ability — not drop it.
        let (condition, body) = strip_if_you_do_conditional(
            "if at least one angel card is milled this way, you gain 4 life",
        );
        assert_eq!(body, "you gain 4 life");
        let Some(AbilityCondition::ZoneChangedThisWay { filter }) = condition else {
            panic!("expected ZoneChangedThisWay condition, got {condition:?}");
        };
        match filter {
            TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                assert!(
                    type_filters.iter().any(
                        |f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Angel"))
                    ),
                    "expected Subtype Angel, got {type_filters:?}"
                );
            }
            other => panic!("expected Typed Angel filter, got {other:?}"),
        }
    }

    #[test]
    fn strip_if_you_do_conditional_hero_enters_this_way() {
        let (condition, body) = strip_if_you_do_conditional(
            "if a hero enters this way, it enters with an additional +1/+1 counter on it",
        );
        assert_eq!(body, "it enters with an additional +1/+1 counter on it");
        let Some(AbilityCondition::ZoneChangedThisWay { filter }) = condition else {
            panic!("expected ZoneChangedThisWay condition, got {condition:?}");
        };
        match filter {
            TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                assert!(type_filters.iter().any(
                    |f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Hero"))
                ));
            }
            other => panic!("expected Typed Hero filter, got {other:?}"),
        }
    }

    #[test]
    fn strip_if_you_do_conditional_gilgamesh_you_put_equipment_this_way() {
        let (condition, body) = strip_if_you_do_conditional(
            "when you put one or more equipment onto the battlefield this way, you may attach one of them to a samurai you control",
        );
        assert_eq!(body, "you may attach one of them to a samurai you control");
        let Some(AbilityCondition::ZoneChangedThisWay { filter }) = condition else {
            panic!("expected ZoneChangedThisWay condition, got {condition:?}");
        };
        match filter {
            TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                assert!(type_filters.iter().any(
                    |f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Equipment"))
                ));
            }
            other => panic!("expected Typed Equipment filter, got {other:?}"),
        }
    }

    /// CR 608.2c + CR 400.7j: Spelunking's ETB rider — "If you put a Cave onto
    /// the battlefield this way, you gain 4 life." CR 400.7j lets the rider find
    /// the Cave the preceding put-land instruction moved to the battlefield (a
    /// public zone). The leading `if`
    /// form of the active-voice put-onto-battlefield gate must lower through
    /// `strip_leading_general_conditional` to `ZoneChangedThisWay { Cave }` —
    /// not drop to `condition: null` (which fires the `Condition_If` swallow
    /// detector and marks the whole ETB chain unsupported). Unlocks the
    /// "if you put a [type] onto the battlefield this way, [bonus]" rider class.
    #[test]
    fn leading_you_put_a_cave_onto_battlefield_spelunking() {
        let (condition, body) = strip_leading_general_conditional(
            "If you put a Cave onto the battlefield this way, you gain 4 life.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "you gain 4 life.");
        let Some(AbilityCondition::ZoneChangedThisWay { filter }) = condition else {
            panic!("expected ZoneChangedThisWay condition, got {condition:?}");
        };
        match filter {
            TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                assert!(
                    type_filters.iter().any(
                        |f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Cave"))
                    ),
                    "expected Subtype Cave, got {type_filters:?}"
                );
            }
            other => panic!("expected Typed Cave filter, got {other:?}"),
        }
    }

    /// CR 608.2c: The suffix form of the same active-voice gate — "[effect] if
    /// you put a [type] onto the battlefield this way" — routes through
    /// `parse_outcome_this_way_condition` to the identical `ZoneChangedThisWay`
    /// condition, so leading and trailing phrasings stay in lockstep.
    #[test]
    fn outcome_you_put_a_creature_onto_battlefield_active_voice() {
        let condition =
            parse_outcome_this_way_condition("you put a creature onto the battlefield this way")
                .expect("active-voice put gate must lower to ZoneChangedThisWay");
        let AbilityCondition::ZoneChangedThisWay { filter } = condition else {
            panic!("expected ZoneChangedThisWay condition, got {condition:?}");
        };
        match filter {
            TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                assert!(
                    type_filters
                        .iter()
                        .any(|f| matches!(f, TypeFilter::Creature)),
                    "expected Creature type filter, got {type_filters:?}"
                );
            }
            other => panic!("expected Typed Creature filter, got {other:?}"),
        }
    }

    /// CR 603.12 + CR 701.9a: "When you discard a card this way, [body]" — the
    /// reflexive gate created by a preceding "discard a card" instruction
    /// (Talion's Messenger, The Ancient One). The bare "a card" form parses to a
    /// `TypeFilter::Card` existential filter, mirroring the active-voice
    /// put-onto-battlefield gate. Runtime semantics are covered end-to-end by
    /// `crates/engine/tests/reflexive_discard_this_way.rs`.
    #[test]
    fn strip_if_you_do_conditional_when_you_discard_a_card_this_way() {
        let (condition, body) = strip_if_you_do_conditional(
            "when you discard a card this way, target player mills cards equal to its mana value",
        );
        assert_eq!(body, "target player mills cards equal to its mana value");
        let Some(AbilityCondition::ZoneChangedThisWay { filter }) = condition else {
            panic!("expected ZoneChangedThisWay condition, got {condition:?}");
        };
        match filter {
            TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
                assert!(
                    type_filters.iter().any(|f| matches!(f, TypeFilter::Card)),
                    "bare 'a card' must yield a Card type filter, got {type_filters:?}"
                );
            }
            other => panic!("expected Typed Card filter, got {other:?}"),
        }
    }

    #[test]
    fn suffix_outcome_this_way_kaya_creature_card_exiled() {
        // Kaya, Orzhov Usurper +1 (PR #2447): "Exile up to two target cards
        // from a single graveyard. You gain 2 life if at least one creature
        // card was exiled this way." The trailing outcome gate must re-home
        // onto the GainLife clause as `ZoneChangedThisWay { creature card }`
        // — never drop to `condition: null` (which triggers the Condition_If
        // swallowed-clause warning).
        let (condition, body) = strip_suffix_conditional(
            "You gain 2 life if at least one creature card was exiled this way.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "You gain 2 life");
        let Some(AbilityCondition::ZoneChangedThisWay { filter }) = condition else {
            panic!("expected ZoneChangedThisWay condition, got {condition:?}");
        };
        let TargetFilter::Typed(TypedFilter { type_filters, .. }) = filter else {
            panic!("expected Typed creature-card filter, got {filter:?}");
        };
        assert!(
            type_filters
                .iter()
                .any(|f| matches!(f, TypeFilter::Creature)),
            "expected Creature type filter, got {type_filters:?}"
        );
    }

    #[test]
    fn parse_outcome_this_way_negated_returns_not() {
        // Build-for-the-class coverage: the suffix gate inherits the
        // combinator's negation flag, so "wasn't exiled this way" maps to
        // `Not { ZoneChangedThisWay { .. } }`.
        let cond = parse_outcome_this_way_condition("a creature card wasn't exiled this way");
        let Some(AbilityCondition::Not { condition }) = cond else {
            panic!("expected Not, got {cond:?}");
        };
        assert!(matches!(
            *condition,
            AbilityCondition::ZoneChangedThisWay { .. }
        ));
    }

    #[test]
    fn if_you_cant_parses_as_not_zone_changed_this_way() {
        // CR 608.2c: "if you can't, draw a card" — the gating condition on the
        // already-parsed `Draw` must be `Not { ZoneChangedThisWay { Any } }` so
        // the draw fires iff the preceding mandatory effect moved nothing.
        for text in ["you can't", "you cannot"] {
            let cond = try_nom_condition_as_ability_condition(text, &mut ParseContext::default());
            assert_eq!(
                cond,
                Some(AbilityCondition::Not {
                    condition: Box::new(AbilityCondition::ZoneChangedThisWay {
                        filter: TargetFilter::Any,
                    }),
                }),
                "expected Not {{ ZoneChangedThisWay {{ Any }} }} for {text:?}",
            );
        }
    }

    #[test]
    fn leading_that_enchantment_is_aura_checks_zone_change_object() {
        let (condition, body) = strip_leading_general_conditional(
            "If that enchantment is an Aura, you may attach it to the token.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "you may attach it to the token.");

        let Some(AbilityCondition::ZoneChangeObjectMatchesFilter {
            origin: None,
            destination: Zone::Battlefield,
            filter,
        }) = condition
        else {
            panic!("expected zone-change object filter condition, got {condition:?}");
        };
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected typed Aura filter");
        };
        assert!(
            filter
                .type_filters
                .iter()
                .any(|ty| matches!(ty, TypeFilter::Subtype(subtype) if subtype == "Aura")),
            "expected Aura subtype filter, got {:?}",
            filter.type_filters
        );
    }

    #[test]
    fn token_then_conditional_aura_attach_targets_created_token() {
        let def = parse_effect_chain(
            "Create a 2/2 white Cat creature token. If that enchantment is an Aura, you may attach it to the token.",
            AbilityKind::Spell,
        );
        let Effect::Token { .. } = *def.effect else {
            panic!("expected token root, got {:?}", def.effect);
        };
        let attach = def
            .sub_ability
            .expect("expected conditional attach sub-ability");
        let Effect::Attach { attachment, target } = &*attach.effect else {
            panic!("expected attach sub-ability, got {:?}", attach.effect);
        };
        assert_eq!(*attachment, TargetFilter::TriggeringSource);
        assert_eq!(*target, TargetFilter::LastCreated);
        assert!(attach.optional);
        assert!(
            matches!(
                attach.condition,
                Some(AbilityCondition::ZoneChangeObjectMatchesFilter {
                    destination: Zone::Battlefield,
                    ..
                })
            ),
            "expected zone-change object condition, got {:?}",
            attach.condition
        );
    }

    #[test]
    fn leading_its_legendary_checks_parent_target_supertype() {
        let (condition, body) = strip_leading_general_conditional(
            "If it's legendary, gain 3 life.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "gain 3 life.");
        let Some(AbilityCondition::TargetMatchesFilter {
            filter, use_lki, ..
        }) = condition
        else {
            panic!("expected TargetMatchesFilter, got {condition:?}");
        };
        assert!(!use_lki);
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected typed supertype filter");
        };
        assert!(
            filter.properties.iter().any(|prop| matches!(
                prop,
                FilterProp::HasSupertype {
                    value: Supertype::Legendary
                }
            )),
            "expected Legendary supertype filter, got {:?}",
            filter.properties
        );
    }

    #[test]
    fn leading_its_color_checks_parent_target_color() {
        let (condition, body) = strip_leading_general_conditional(
            "If it's red, you may cast it this turn.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "you may cast it this turn.");
        let Some(AbilityCondition::TargetMatchesFilter {
            filter, use_lki, ..
        }) = condition
        else {
            panic!("expected TargetMatchesFilter, got {condition:?}");
        };
        assert!(!use_lki);
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected typed color filter");
        };
        assert!(
            filter.properties.contains(&FilterProp::HasColor {
                color: ManaColor::Red
            }),
            "expected Red color filter, got {:?}",
            filter.properties
        );
    }

    #[test]
    fn suffix_its_color_checks_parent_target_color() {
        let (condition, body) = strip_suffix_conditional(
            "Counter target spell if it's blue.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "Counter target spell");
        let Some(AbilityCondition::TargetMatchesFilter {
            filter, use_lki, ..
        }) = condition
        else {
            panic!("expected TargetMatchesFilter, got {condition:?}");
        };
        assert!(!use_lki);
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected typed color filter");
        };
        assert!(
            filter.properties.contains(&FilterProp::HasColor {
                color: ManaColor::Blue
            }),
            "expected Blue color filter, got {:?}",
            filter.properties
        );
    }

    #[test]
    fn leading_this_enchantment_isnt_creature_checks_source_type() {
        let (condition, body) = strip_leading_general_conditional(
            "If this enchantment isn't a creature, it becomes a 3/3 Angel creature with flying.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "it becomes a 3/3 Angel creature with flying.");
        assert!(matches!(
            condition,
            Some(AbilityCondition::Not { condition })
                if matches!(*condition, AbilityCondition::SourceMatchesFilter { .. })
        ));
    }

    #[test]
    fn leading_you_win_maps_to_event_outcome_won() {
        let (condition, body) = strip_leading_general_conditional(
            "If you win, put a +1/+1 counter on this creature.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "put a +1/+1 counter on this creature.");
        assert_eq!(condition, Some(AbilityCondition::EventOutcomeWon));
    }

    #[test]
    fn leading_you_dont_maps_to_not_if_you_do() {
        let (condition, body) = strip_leading_general_conditional(
            "If you didn't put a card into your hand this way, draw a card.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "draw a card.");
        assert_eq!(
            condition,
            Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::effect_performed())
            })
        );
    }

    #[test]
    fn strip_superlative_target_conditional_least_power() {
        use crate::types::ability::{AggregateFunction, ObjectProperty};

        let (condition, body) = strip_superlative_target_conditional(
            "Destroy target creature if it has the least power among creatures.",
        );
        assert_eq!(body, "Destroy target creature");
        let Some(AbilityCondition::QuantityCheck {
            lhs,
            comparator,
            rhs,
        }) = condition
        else {
            panic!("expected QuantityCheck, got {condition:?}");
        };
        assert_eq!(comparator, Comparator::LE);
        assert_eq!(
            lhs,
            QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Target,
                }
            }
        );
        assert_eq!(
            rhs,
            QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Min,
                    property: ObjectProperty::Power,
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                }
            }
        );
    }

    /// CR 608.2c: Wretched Banquet's spell-target gate carries a redundant
    /// "or is tied for least power" tail before "among <filter>". In the
    /// spell-target superlative form the target is itself part of the aggregate
    /// population, so the comparison is already non-strict (LE for "least") and
    /// the tail adds nothing — it must be consumed, not swallowed. The trailing
    /// "on the battlefield" zone qualifier is folded into the aggregate filter.
    #[test]
    fn strip_superlative_target_conditional_least_power_or_tied_for_least() {
        use crate::types::ability::{AggregateFunction, ObjectProperty};

        let (condition, body) = strip_superlative_target_conditional(
            "Destroy target creature if it has the least power or is tied for least power among creatures on the battlefield.",
        );
        assert_eq!(body, "Destroy target creature");
        let Some(AbilityCondition::QuantityCheck {
            lhs,
            comparator,
            rhs,
        }) = condition
        else {
            panic!("expected QuantityCheck, got {condition:?}");
        };
        // "least ... or is tied for least" stays a non-strict comparison.
        assert_eq!(comparator, Comparator::LE);
        assert_eq!(
            lhs,
            QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Target,
                }
            }
        );
        let QuantityExpr::Ref {
            qty:
                QuantityRef::Aggregate {
                    function,
                    property,
                    filter,
                },
        } = rhs
        else {
            panic!("expected Aggregate rhs, got {rhs:?}");
        };
        assert_eq!(function, AggregateFunction::Min);
        assert_eq!(property, ObjectProperty::Power);
        // The "on the battlefield" qualifier rides the aggregate population.
        assert!(
            matches!(&filter, TargetFilter::Typed(tf)
            if tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::InZone { zone: Zone::Battlefield }
            ))),
            "aggregate filter should be creatures on the battlefield, got {filter:?}"
        );
    }

    #[test]
    fn strip_superlative_target_conditional_plural_subject() {
        use crate::types::ability::{AggregateFunction, ObjectProperty};

        let (condition, body) = strip_superlative_target_conditional(
            "Destroy those creatures if they have the greatest toughness among creatures.",
        );
        assert_eq!(body, "Destroy those creatures");
        let Some(AbilityCondition::QuantityCheck {
            comparator, rhs, ..
        }) = condition
        else {
            panic!("expected QuantityCheck, got {condition:?}");
        };
        assert_eq!(comparator, Comparator::GE);
        assert_eq!(
            rhs,
            QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Max,
                    property: ObjectProperty::Toughness,
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                }
            }
        );
    }

    #[test]
    fn dynamic_target_mana_value_suffix_uses_object_count_quantity() {
        let (condition, body) = strip_mana_value_conditional(
            "Put target creature card from an opponent's graveyard onto the battlefield under your control if its mana value is less than or equal to the number of Allies you control.",
        );
        assert_eq!(
            body,
            "Put target creature card from an opponent's graveyard onto the battlefield under your control"
        );
        let Some(AbilityCondition::TargetMatchesFilter {
            filter, use_lki, ..
        }) = condition
        else {
            panic!("expected TargetMatchesFilter, got {condition:?}");
        };
        assert!(!use_lki);
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected typed filter");
        };
        let [FilterProp::Cmc { comparator, value }] = filter.properties.as_slice() else {
            panic!("expected Cmc property, got {:?}", filter.properties);
        };
        assert_eq!(*comparator, Comparator::LE);
        let QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } = value
        else {
            panic!("expected ObjectCount quantity, got {value:?}");
        };
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected typed object-count filter");
        };
        assert_eq!(filter.controller, Some(ControllerRef::You));
    }

    #[test]
    fn suffix_symbolic_mana_spent_condition_parses_single_color() {
        let (condition, body) = strip_suffix_conditional(
            "Each player loses 1 life for each attacking creature they control if {B} was spent to cast this spell.",
            &mut ParseContext::default(),
        );
        assert_eq!(
            body,
            "Each player loses 1 life for each attacking creature they control"
        );
        assert_eq!(
            condition,
            Some(AbilityCondition::ManaColorSpent {
                color: ManaColor::Black,
                minimum: 1,
            })
        );
    }

    #[test]
    fn suffix_symbolic_mana_spent_condition_parses_mixed_colors() {
        let condition = parse_condition_text("{W}{B} was spent to cast this spell")
            .expect("mixed color spend condition should parse");
        let AbilityCondition::And { conditions } = condition else {
            panic!("expected And condition");
        };
        assert!(conditions.contains(&AbilityCondition::ManaColorSpent {
            color: ManaColor::White,
            minimum: 1,
        }));
        assert!(conditions.contains(&AbilityCondition::ManaColorSpent {
            color: ManaColor::Black,
            minimum: 1,
        }));
    }

    #[test]
    fn suffix_another_filtered_spell_condition_uses_spell_history_quantity() {
        let (condition, body) = strip_suffix_conditional(
            "Target creature you control gets +1/+0 until end of turn if you've cast another instant or sorcery spell this turn.",
            &mut ParseContext::default(),
        );
        assert_eq!(
            body,
            "Target creature you control gets +1/+0 until end of turn"
        );
        let Some(AbilityCondition::QuantityCheck {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::SpellsCastThisTurn {
                            scope: crate::types::ability::CountScope::Controller,
                            filter: Some(TargetFilter::Or { filters }),
                        },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 2 },
        }) = condition
        else {
            panic!("expected filtered spell-history quantity condition, got {condition:?}");
        };
        assert!(
            filters.iter().any(|filter| matches!(
                filter,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                    if type_filters == &vec![TypeFilter::Instant]
            )) && filters.iter().any(|filter| matches!(
                filter,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                    if type_filters == &vec![TypeFilter::Sorcery]
            ))
        );
    }

    #[test]
    fn suffix_night_condition_uses_day_night_designation() {
        let (condition, body) = strip_suffix_conditional(
            "Target creature you control gets +2/+0 until end of turn if it's night.",
            &mut ParseContext::default(),
        );
        assert_eq!(
            body,
            "Target creature you control gets +2/+0 until end of turn"
        );
        assert_eq!(
            condition,
            Some(AbilityCondition::DayNightIs {
                state: crate::types::game_state::DayNight::Night
            })
        );
    }

    #[test]
    fn leading_word_mana_spent_condition_parses_adamant() {
        let (condition, body) = strip_leading_general_conditional(
            "If at least three red mana was spent to cast this spell, it deals 4 damage instead.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "it deals 4 damage instead.");
        assert_eq!(
            condition,
            Some(AbilityCondition::ManaColorSpent {
                color: ManaColor::Red,
                minimum: 3,
            })
        );
    }

    /// CR 122.1 + CR 608.2c: "there are no counters on ~" round-trips through
    /// the bridge to a `QuantityCheck` against `AnyCountersOnSelf`. Previously
    /// the bridge returned `None` for `CounterMatch::Any`, which silently
    /// dropped the gate and caused effects (Gemstone Mine, depletion lands)
    /// to fire unconditionally.
    #[test]
    fn bridge_has_counters_any_no_counters_yields_any_counters_on_self_eq_zero() {
        let (rest, sc) = parse_inner_condition("there are no counters on ~").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            sc,
            StaticCondition::HasCounters {
                counters: CounterMatch::Any,
                minimum: 0,
                maximum: Some(0),
            }
        );
        let bridged = static_condition_to_ability_condition(&sc, &mut ParseContext::default())
            .expect(
                "CounterMatch::Any must round-trip — None here is the silent-failure regression",
            );
        match bridged {
            AbilityCondition::QuantityCheck {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::CountersOn {
                                scope: ObjectScope::Source,
                                counter_type: None,
                            },
                    },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            } => {}
            other => panic!("unexpected bridged condition: {other:?}"),
        }
    }

    /// `"~ has a counter on it"` (Demon Wall): minimum=1, maximum=None →
    /// `AnyCountersOnSelf >= 1`.
    #[test]
    fn bridge_has_counters_any_at_least_one_yields_any_counters_on_self_ge_one() {
        let (rest, sc) = parse_inner_condition("~ has a counter on it").unwrap();
        assert_eq!(rest, "");
        let bridged = static_condition_to_ability_condition(&sc, &mut ParseContext::default())
            .expect("must bridge");
        match bridged {
            AbilityCondition::QuantityCheck {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::CountersOn {
                                scope: ObjectScope::Source,
                                counter_type: None,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("unexpected bridged condition: {other:?}"),
        }
    }

    /// Typed-counter case still routes to `CountersOnSelf { counter_type }` —
    /// confirms the shared `counter_threshold_to_condition` helper preserves the
    /// existing behavior for the OfType branch.
    #[test]
    fn bridge_has_counters_typed_yields_counters_on_self() {
        let sc = StaticCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Plus1Plus1),
            minimum: 2,
            maximum: None,
        };
        let bridged = static_condition_to_ability_condition(&sc, &mut ParseContext::default())
            .expect("must bridge");
        match bridged {
            AbilityCondition::QuantityCheck {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::CountersOn {
                                scope: ObjectScope::Source,
                                counter_type: Some(counter_type),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            } => assert_eq!(counter_type, CounterType::Plus1Plus1),
            other => panic!("unexpected bridged condition: {other:?}"),
        }
    }

    /// CR 702.33d + CR 608.2c: Plain "if it was kicked, …" emits the
    /// default-shape `AdditionalCostPaid` (variant=None, min_count=1) so the
    /// existing single-bool semantics survive. Regression guard for Gift /
    /// Buyback / Bargain / Evidence and Archangel of Wrath's first trigger.
    #[test]
    fn plain_kicked_emits_default_shape() {
        let (cond, body) =
            strip_additional_cost_conditional("If it was kicked, it deals 2 damage to any target.");
        assert_eq!(cond, Some(AbilityCondition::additional_cost_paid_any()));
        assert_eq!(body, "it deals 2 damage to any target.");
    }

    #[test]
    fn beheld_emits_default_additional_cost_condition() {
        let (cond, body) = strip_additional_cost_conditional("If a Dragon was beheld, surveil 2.");
        assert_eq!(cond, Some(AbilityCondition::additional_cost_paid_any()));
        assert_eq!(body, "surveil 2.");
    }

    /// CR 702.137a + CR 603.4: Rix Maadi Reveler — "If this creature's
    /// spectacle cost was paid, instead [effect]" → CastVariantPaidInstead
    /// { Spectacle }, stripping the leading "instead ".
    #[test]
    fn spectacle_instead_emits_cast_variant_paid_instead() {
        let (cond, body) = strip_additional_cost_conditional(
            "If this creature's spectacle cost was paid, instead discard your hand, then draw three cards.",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::CastVariantPaidInstead {
                variant: CastVariantPaid::Spectacle,
            })
        );
        assert_eq!(body, "discard your hand, then draw three cards.");
    }

    /// CR 702.117a + CR 603.4: surge "...instead" rider mirrors the spectacle
    /// path — building-block coverage of the parameterized condition over a
    /// second variant.
    #[test]
    fn surge_instead_emits_cast_variant_paid_instead() {
        let (cond, body) = strip_additional_cost_conditional(
            "If its surge cost was paid, instead draw two cards.",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::CastVariantPaidInstead {
                variant: CastVariantPaid::Surge,
            })
        );
        assert_eq!(body, "draw two cards.");
    }

    /// CR 702.76a: "if its prowl cost was paid, [effect]" — non-"instead"
    /// variant that gates a sub-ability on prowl payment (Latchkey Faerie).
    #[test]
    fn prowl_cost_paid_emits_cast_variant_paid() {
        let (cond, body) =
            strip_additional_cost_conditional("if its prowl cost was paid, draw a card.");
        assert_eq!(
            cond,
            Some(AbilityCondition::CastVariantPaid {
                variant: CastVariantPaid::Prowl,
                subject: ObjectScope::Source,
            })
        );
        assert_eq!(body, "draw a card.");
    }

    /// CR 702.33b + CR 603.4: "if it was kicked twice, …" → min_count = 2.
    /// Archangel of Wrath's second trigger.
    #[test]
    fn kicked_twice_emits_min_count_two() {
        let (cond, body) = strip_additional_cost_conditional(
            "If it was kicked twice, it deals 2 damage to any target.",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::additional_cost_paid_n_times(2))
        );
        assert_eq!(body, "it deals 2 damage to any target.");
    }

    /// CR 702.33b/c: "if it was kicked three times, …" → min_count = N.
    /// Exercises the `parse_number` English-word path.
    #[test]
    fn kicked_three_times_emits_min_count_three() {
        let (cond, body) =
            strip_additional_cost_conditional("If it was kicked three times, draw a card.");
        assert_eq!(
            cond,
            Some(AbilityCondition::additional_cost_paid_n_times(3))
        );
        assert_eq!(body, "draw a card.");
    }

    /// CR 702.33f: "if it was kicked with its {COST} kicker, …" records the
    /// printed mana cost so synthesis can map it to the positional kicker.
    #[test]
    fn kicked_with_specific_kicker_emits_cost_metadata() {
        let (cond, body) = strip_additional_cost_conditional(
            "If it was kicked with its {2}{U} kicker, target player discards three cards.",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::additional_cost_paid_kicker_cost(
                parse_kicker_condition_mana_cost("{2}{U}").unwrap()
            ))
        );
        assert_eq!(body, "target player discards three cards.");
    }

    /// Delver of Secrets: instant/sorcery gate on revealed card.
    #[test]
    fn issue_2367_if_instant_or_sorcery_revealed_this_way() {
        let (cond, body) = strip_card_type_conditional(
            "If an instant or sorcery card is revealed this way, transform this creature.",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::RevealedHasCardType {
                card_types: vec![CoreType::Instant, CoreType::Sorcery],
                additional_filter: None,
                subtype_filter: None,
            })
        );
        assert_eq!(body, "transform this creature.");
    }

    /// CR 205.3m + CR 608.2c: "that creature is a Mutant, Ninja, or Turtle"
    /// (Turtle Van) → `TargetMatchesFilter` over an `Or` of the three subtypes.
    /// The comma + "or" disjunction is parsed via the shared `parse_type_phrase`
    /// building block, so the condition covers the whole class of multi-subtype
    /// target-anaphoric gates, not just this card.
    #[test]
    fn target_type_membership_subtype_disjunction() {
        let cond = parse_target_type_membership_condition_text(
            "that creature is a Mutant, Ninja, or Turtle",
        );
        let Some(AbilityCondition::TargetMatchesFilter {
            filter, use_lki, ..
        }) = cond
        else {
            panic!("expected TargetMatchesFilter, got {cond:?}");
        };
        assert!(
            !use_lki,
            "present-tense 'is' must read current state, not LKI"
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or of subtypes, got {filter:?}");
        };
        let subtypes: Vec<String> = filters
            .iter()
            .filter_map(|f| match f {
                TargetFilter::Typed(tf) => tf.type_filters.iter().find_map(|t| match t {
                    TypeFilter::Subtype(s) => Some(s.clone()),
                    _ => None,
                }),
                _ => None,
            })
            .collect();
        assert_eq!(subtypes, vec!["Mutant", "Ninja", "Turtle"]);
    }

    /// CR 400.7 + CR 608.2c: past-tense "that creature was a Goblin" reads LKI.
    #[test]
    fn target_type_membership_past_tense_uses_lki() {
        let cond = parse_target_type_membership_condition_text("that creature was a Goblin");
        let Some(AbilityCondition::TargetMatchesFilter { use_lki, .. }) = cond else {
            panic!("expected TargetMatchesFilter, got {cond:?}");
        };
        assert!(use_lki, "past-tense 'was' must use LKI per CR 400.7");
    }

    /// Negated form "that creature isn't a Turtle" wraps in `Not`.
    #[test]
    fn target_type_membership_negated() {
        let cond = parse_target_type_membership_condition_text("that creature isn't a Turtle");
        let Some(AbilityCondition::Not { condition }) = cond else {
            panic!("expected Not, got {cond:?}");
        };
        assert!(matches!(
            *condition,
            AbilityCondition::TargetMatchesFilter { .. }
        ));
    }

    // ---- reflexive-if-rider recognizer (S01) ----
    //
    // Discrimination / revert-probe: reverting the `parse_reflexive_object_property`
    // arm (or the `try_nom_condition_as_ability_condition` registration) makes
    // `parse_target_reflexive_property_condition_text` return `None`, reproducing
    // the pre-fix swallow (`condition: null` on the rider sub-ability, measured in
    // card-data.json for Sold Out / Consuming Ashes / Brackish Blunder). Each
    // sibling negative proves a distinct axis (predicate / comparator / tense /
    // polarity) is load-bearing.

    fn single_prop(cond: &AbilityCondition) -> (&FilterProp, bool) {
        let AbilityCondition::TargetMatchesFilter {
            filter, use_lki, ..
        } = cond
        else {
            panic!("expected TargetMatchesFilter, got {cond:?}");
        };
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert_eq!(tf.type_filters.len(), 0, "reflexive filter carries no type");
        assert_eq!(tf.properties.len(), 1, "exactly one predicate prop");
        (&tf.properties[0], *use_lki)
    }

    /// CR 110.5 + CR 400.7: Brackish Blunder "if it was tapped" → Tapped, LKI.
    /// The LKI snapshot now carries exit-time tap state, so the past-tense rider
    /// reads it after the antecedent leaves the battlefield (no longer the honest
    /// red swallow this assertion previously guarded).
    ///
    /// Revert-probe: deleting the `value(FilterProp::Tapped, tag("tapped"))` leaf
    /// in `parse_reflexive_object_property` makes this return `None` — the leaf is
    /// load-bearing for the whole "tapped" predicate class.
    #[test]
    fn reflexive_was_tapped_uses_lki() {
        let cond = parse_target_reflexive_property_condition_text("it was tapped").unwrap();
        let (prop, use_lki) = single_prop(&cond);
        assert_eq!(*prop, FilterProp::Tapped);
        assert!(use_lki, "past-tense 'was tapped' must use LKI per CR 400.7");
    }

    /// CR 608.2c: negated past "it wasn't tapped" → Not{Tapped, LKI}. Proves the
    /// polarity axis composes for free over the new `tapped` leaf.
    #[test]
    fn reflexive_wasnt_tapped_negated_uses_lki() {
        let cond = parse_target_reflexive_property_condition_text("it wasn't tapped").unwrap();
        let AbilityCondition::Not { condition } = &cond else {
            panic!("expected Not, got {cond:?}");
        };
        let (prop, use_lki) = single_prop(condition);
        assert_eq!(*prop, FilterProp::Tapped);
        assert!(use_lki, "negated past-tense 'wasn't tapped' uses LKI");
    }

    /// CR 110.5: present-tense "that permanent isn't tapped" → Not{Tapped, LIVE}.
    /// Proves the tense axis (present ⇒ use_lki:false, live state) composes over
    /// the new `tapped` leaf without a dedicated arm.
    #[test]
    fn reflexive_isnt_tapped_negated_live() {
        let cond =
            parse_target_reflexive_property_condition_text("that permanent isn't tapped").unwrap();
        let AbilityCondition::Not { condition } = &cond else {
            panic!("expected Not, got {cond:?}");
        };
        let (prop, use_lki) = single_prop(condition);
        assert_eq!(*prop, FilterProp::Tapped);
        assert!(!use_lki, "present-tense 'isn't tapped' reads live state");
    }

    /// CR 110.5 + CR 400.7: untapped sibling of `reflexive_was_tapped_uses_lki`.
    /// "it was untapped" → Untapped, LKI — the past-tense rider reads the LKI
    /// snapshot's exit-time tap state after the antecedent leaves the battlefield.
    ///
    /// Revert-probe: deleting the `value(FilterProp::Untapped, tag("untapped"))`
    /// leaf in `parse_reflexive_object_property` makes this return `None` — the
    /// leaf is load-bearing for the whole "untapped" predicate class (the gap
    /// the maintainer flagged on PR #4559).
    #[test]
    fn reflexive_was_untapped_uses_lki() {
        let cond = parse_target_reflexive_property_condition_text("it was untapped").unwrap();
        let (prop, use_lki) = single_prop(&cond);
        assert_eq!(*prop, FilterProp::Untapped);
        assert!(
            use_lki,
            "past-tense 'was untapped' must use LKI per CR 400.7"
        );
    }

    /// CR 110.5: present-tense "that permanent isn't untapped" → Not{Untapped,
    /// LIVE}. Proves the composed axes (tense ⇒ live, polarity ⇒ Not) carry over
    /// the new `untapped` leaf for free — the same axes the `tapped` siblings
    /// exercise, confirming the sibling is a true parameterization, not a one-off.
    #[test]
    fn reflexive_isnt_untapped_negated_live() {
        let cond = parse_target_reflexive_property_condition_text("that permanent isn't untapped")
            .unwrap();
        let AbilityCondition::Not { condition } = &cond else {
            panic!("expected Not, got {cond:?}");
        };
        let (prop, use_lki) = single_prop(condition);
        assert_eq!(*prop, FilterProp::Untapped);
        assert!(!use_lki, "present-tense 'isn't untapped' reads live state");
    }

    /// Sibling: negated present "that creature isn't attacking" → Not +
    /// use_lki:false. Proves the polarity AND tense axes are load-bearing on a
    /// runtime-supported predicate.
    #[test]
    fn reflexive_isnt_attacking_emits_negated_present() {
        let cond = parse_target_reflexive_property_condition_text("that creature isn't attacking")
            .unwrap();
        let AbilityCondition::Not { condition } = &cond else {
            panic!("expected Not, got {cond:?}");
        };
        let (prop, use_lki) = single_prop(condition);
        assert_eq!(*prop, FilterProp::Attacking { defender: None });
        assert!(!use_lki, "present-tense 'isn't' must read live state");
    }

    /// CR 202.3: Consuming Ashes "it had mana value 3 or less" → Cmc LE 3, LKI.
    #[test]
    fn reflexive_had_mana_value_le3() {
        let cond =
            parse_target_reflexive_property_condition_text("it had mana value 3 or less").unwrap();
        let (prop, use_lki) = single_prop(&cond);
        assert_eq!(
            *prop,
            FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 3 },
            }
        );
        assert!(use_lki, "past-tense 'had' must use LKI per CR 400.7");
    }

    /// Sibling: comparator axis — "4 or greater" → GE 4.
    #[test]
    fn reflexive_had_mana_value_ge4() {
        let cond = parse_target_reflexive_property_condition_text("it had mana value 4 or greater")
            .unwrap();
        let (prop, _) = single_prop(&cond);
        assert_eq!(
            *prop,
            FilterProp::Cmc {
                comparator: Comparator::GE,
                value: QuantityExpr::Fixed { value: 4 },
            }
        );
    }

    /// CR 208.1: Driftgloom Coyote "that creature had power 2 or less" →
    /// PtComparison{Power, LE, 2}, LKI.
    #[test]
    fn reflexive_had_power_le2() {
        let cond =
            parse_target_reflexive_property_condition_text("that creature had power 2 or less")
                .unwrap();
        let (prop, use_lki) = single_prop(&cond);
        assert_eq!(
            *prop,
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 2 },
            }
        );
        assert!(use_lki, "past-tense 'had' must use LKI per CR 400.7");
    }

    /// Sibling: stat axis — toughness, GE.
    #[test]
    fn reflexive_had_toughness_ge3() {
        let cond = parse_target_reflexive_property_condition_text(
            "that creature had toughness 3 or greater",
        )
        .unwrap();
        let (prop, _) = single_prop(&cond);
        assert_eq!(
            *prop,
            FilterProp::PtComparison {
                stat: PtStat::Toughness,
                scope: PtValueScope::Current,
                comparator: Comparator::GE,
                value: QuantityExpr::Fixed { value: 3 },
            }
        );
    }

    /// CR 508.1b: Wisecrack "that creature is attacking" → Attacking, present
    /// tense reads live state (use_lki:false).
    #[test]
    fn reflexive_is_attacking_live() {
        let cond =
            parse_target_reflexive_property_condition_text("that creature is attacking").unwrap();
        let (prop, use_lki) = single_prop(&cond);
        assert_eq!(*prop, FilterProp::Attacking { defender: None });
        assert!(!use_lki, "present-tense 'is attacking' reads live combat");
    }

    /// Sibling: combat-status predicate axis — blocking.
    #[test]
    fn reflexive_is_blocking_live() {
        let cond =
            parse_target_reflexive_property_condition_text("that creature is blocking").unwrap();
        let (prop, _) = single_prop(&cond);
        assert_eq!(*prop, FilterProp::Blocking);
    }

    /// CR 120.6 + CR 120.9: Sold Out "it was dealt damage this turn" →
    /// WasDealtDamageThisTurn, LKI.
    #[test]
    fn reflexive_was_dealt_damage_this_turn() {
        let cond = parse_target_reflexive_property_condition_text("it was dealt damage this turn")
            .unwrap();
        let (prop, use_lki) = single_prop(&cond);
        assert_eq!(*prop, FilterProp::WasDealtDamageThisTurn);
        assert!(use_lki, "past-tense look-back must use LKI per CR 400.7");
    }

    /// Issue #4796 (CR 608.2c + CR 301.5b): "if it's a[n] <subtype>, attach it
    /// to ~" recognizer — The Invincible Iron Man's Equipment self-attach
    /// follow-up. Emits `ZoneChangedThisWay{Equipment}` + `Attach{SelfRef,
    /// ParentTarget}` (moved card is the attachment, source is the host), not a
    /// self-attach and not a wrong-subject `TargetMatchesFilter`.
    #[test]
    fn moved_card_subtype_attach_followup_equipment_to_source() {
        let (cond, effect, is_optional) =
            try_parse_moved_card_subtype_attach_followup("if it's an equipment, attach it to ~.")
                .expect("Iron Man Equipment attach follow-up must be recognized");
        assert!(!is_optional, "the attach itself is mandatory once gated");
        match cond {
            AbilityCondition::ZoneChangedThisWay { filter } => match filter {
                TargetFilter::Typed(t) => assert!(
                    t.type_filters.iter().any(|f| matches!(
                        f,
                        TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Equipment")
                    )),
                    "expected Equipment subtype gate, got {:?}",
                    t.type_filters
                ),
                other => panic!("expected Typed Equipment filter, got {other:?}"),
            },
            other => panic!("expected ZoneChangedThisWay condition, got {other:?}"),
        }
        assert!(
            matches!(
                effect,
                Effect::Attach {
                    attachment: TargetFilter::SelfRef,
                    target: TargetFilter::ParentTarget,
                }
            ),
            "moved card must be the attachment (SelfRef) and the source the host (ParentTarget), got {effect:?}"
        );
    }

    /// The recognizer is scoped to the "attach it to ~" self-host body and to
    /// CR 205.3 subtype gates: it declines non-attach bodies, core-type gates,
    /// and typed (non-source) hosts so those fall through to the generic path.
    #[test]
    fn moved_card_subtype_attach_followup_declines_out_of_scope() {
        // No "attach it to ~" body (reveal/scry-style follow-up).
        assert!(try_parse_moved_card_subtype_attach_followup(
            "if it's an equipment, you draw a card."
        )
        .is_none());
        // Core type, not a CR 205.3 subtype.
        assert!(try_parse_moved_card_subtype_attach_followup(
            "if it's a creature, attach it to ~."
        )
        .is_none());
        // Typed host (not the source "~") is an honest gap.
        assert!(try_parse_moved_card_subtype_attach_followup(
            "if it's an equipment, attach it to a creature you control."
        )
        .is_none());
    }

    /// CR 608.2c: Faller's Faithful "that creature wasn't dealt damage this turn"
    /// → Not{WasDealtDamageThisTurn, LKI}. (Part B card; recognizer-side proof.)
    #[test]
    fn reflexive_wasnt_dealt_damage_negated() {
        let cond = parse_target_reflexive_property_condition_text(
            "that creature wasn't dealt damage this turn",
        )
        .unwrap();
        let AbilityCondition::Not { condition } = &cond else {
            panic!("expected Not, got {cond:?}");
        };
        let (prop, use_lki) = single_prop(condition);
        assert_eq!(*prop, FilterProp::WasDealtDamageThisTurn);
        assert!(use_lki, "negated past-tense look-back uses LKI");
    }

    /// Coverage-regression guard (Zemo / Isochron class): a board-state leading
    /// conditional whose subject is NOT a target anaphor ("you control a
    /// creature") must NOT be folded into a `TargetMatchesFilter`. The reflexive
    /// recognizer returns `None`, and the full dispatch routes it to its existing
    /// control-presence condition.
    #[test]
    fn reflexive_recognizer_ignores_board_state_conditional() {
        assert!(
            parse_target_reflexive_property_condition_text("you control a creature").is_none(),
            "board-state 'you control a creature' must not match the reflexive recognizer"
        );
        let mut ctx = ParseContext::default();
        let routed = try_nom_condition_as_ability_condition("you control a creature", &mut ctx);
        assert!(
            !matches!(
                routed,
                Some(AbilityCondition::TargetMatchesFilter { .. })
                    | Some(AbilityCondition::Not { .. })
            ),
            "control-presence gate must not be mis-folded into TargetMatchesFilter, got {routed:?}"
        );
    }

    /// CR 608.2c: "permanent" is not a CoreType — strip_card_type_conditional must
    /// still gate on it via TargetMatchesFilter (parse_type_phrase building block).
    /// Covers Primal Surge's "If it's a permanent card, you may put it onto the
    /// battlefield."
    #[test]
    fn strip_card_type_conditional_permanent() {
        let (cond, body) = strip_card_type_conditional("If it's a permanent card, draw a card.");
        let Some(AbilityCondition::TargetMatchesFilter {
            filter, use_lki, ..
        }) = cond
        else {
            panic!("expected TargetMatchesFilter for 'permanent', got {cond:?}");
        };
        assert!(!use_lki, "present-tense 'it's a' check must not use LKI");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter for permanent");
        };
        assert!(
            tf.type_filters.contains(&TypeFilter::Permanent),
            "expected Permanent type filter, got {:?}",
            tf.type_filters
        );
        assert_eq!(body, "draw a card.");
    }

    #[test]
    fn strip_card_type_conditional_permanent_of_chosen_type() {
        let (cond, body) = strip_card_type_conditional(
            "If it's a permanent card of the chosen type, draw a card.",
        );
        let Some(AbilityCondition::TargetMatchesFilter {
            filter, use_lki, ..
        }) = cond
        else {
            panic!("expected TargetMatchesFilter for permanent chosen type, got {cond:?}");
        };
        assert!(!use_lki, "present-tense 'it's a' check must not use LKI");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter for permanent chosen type");
        };
        assert!(
            tf.type_filters.contains(&TypeFilter::Permanent),
            "expected Permanent type filter, got {:?}",
            tf.type_filters
        );
        assert!(
            tf.properties.contains(&FilterProp::IsChosenCreatureType),
            "expected chosen-type property, got {:?}",
            tf.properties
        );
        assert_eq!(body, "draw a card.");
    }

    #[test]
    fn strip_card_type_conditional_nonpermanent_negated() {
        let (cond, body) = strip_card_type_conditional("If it's a nonpermanent card, draw a card.");
        let Some(AbilityCondition::Not { condition }) = cond else {
            panic!("expected negated condition for 'nonpermanent', got {cond:?}");
        };
        let AbilityCondition::TargetMatchesFilter { filter, .. } = *condition else {
            panic!("expected inner TargetMatchesFilter");
        };
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Permanent));
        assert_eq!(body, "draw a card.");
    }

    /// CR 608.2c + CR 205.3a: "If it's a [subtype]" gates the parent target on a
    /// subtype via parse_type_phrase + TargetMatchesFilter. Pre-fix this dropped to
    /// `None` (only CoreType words matched), which silently dropped the else-branch.
    #[test]
    fn if_its_a_subtype_parses_condition() {
        let (cond, body) = strip_leading_general_conditional(
            "If it's a Goblin, destroy it.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "destroy it.");
        let Some(AbilityCondition::TargetMatchesFilter {
            filter, use_lki, ..
        }) = cond
        else {
            panic!("expected TargetMatchesFilter for 'Goblin' subtype, got {cond:?}");
        };
        assert!(!use_lki, "present-tense 'it's a' check must not use LKI");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter for subtype");
        };
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Subtype("Goblin".to_string())),
            "expected Goblin subtype filter, got {:?}",
            tf.type_filters
        );
    }

    /// CR 608.2c + CR 205.3a: Article choice must not affect anaphoric subtype
    /// gates. "If it's an Elf" is the same condition family as "If it's a Goblin".
    #[test]
    fn if_its_an_subtype_parses_condition() {
        let (cond, body) = strip_leading_general_conditional(
            "If it's an Elf, create three 1/1 green Elf Warrior creature tokens.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "create three 1/1 green Elf Warrior creature tokens.");
        let Some(AbilityCondition::TargetMatchesFilter {
            filter, use_lki, ..
        }) = cond
        else {
            panic!("expected TargetMatchesFilter for 'Elf' subtype, got {cond:?}");
        };
        assert!(!use_lki, "present-tense 'it's an' check must not use LKI");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter for subtype");
        };
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Subtype("Elf".to_string())),
            "expected Elf subtype filter, got {:?}",
            tf.type_filters
        );
    }

    /// CR 608.2c + CR 702.1: "If it has [keyword]" gates on FilterProp::WithKeyword.
    /// Pre-fix this dropped to `None` (only the negative "it doesn't have" arm
    /// existed), dropping the else-branch.
    #[test]
    fn if_it_has_keyword_parses_condition() {
        let (cond, body) = strip_leading_general_conditional(
            "If it has flying, destroy it.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "destroy it.");
        let Some(AbilityCondition::TargetMatchesFilter {
            filter, use_lki, ..
        }) = cond
        else {
            panic!("expected TargetMatchesFilter for 'flying' keyword, got {cond:?}");
        };
        assert!(!use_lki);
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter for keyword");
        };
        assert!(
            tf.properties.contains(&FilterProp::WithKeyword {
                value: Keyword::Flying
            }),
            "expected WithKeyword(Flying) property, got {:?}",
            tf.properties
        );
    }

    /// CR 608.2c + CR 205.3a: "If it's not a [subtype]" wraps the subtype filter in
    /// `Not` via the existing maybe_negate path (the "it's not a " prefix branch).
    #[test]
    fn if_its_not_a_subtype_negates() {
        let (cond, body) = strip_leading_general_conditional(
            "If it's not a Goblin, destroy it.",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "destroy it.");
        let Some(AbilityCondition::Not { condition }) = cond else {
            panic!("expected negated condition for 'not a Goblin', got {cond:?}");
        };
        let AbilityCondition::TargetMatchesFilter { filter, .. } = *condition else {
            panic!("expected inner TargetMatchesFilter");
        };
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter");
        };
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Subtype("Goblin".to_string())),
            "expected Goblin subtype filter, got {:?}",
            tf.type_filters
        );
    }

    #[test]
    fn kenessos_multi_subtype_creature_card_condition() {
        let cond = try_nom_condition_as_ability_condition(
            "it's a Kraken, Leviathan, Octopus, or Serpent creature card",
            &mut ParseContext::default(),
        );
        let Some(AbilityCondition::RevealedHasCardType {
            card_types,
            subtype_filter: Some(subtype_filter),
            ..
        }) = cond
        else {
            panic!("expected RevealedHasCardType creature subtype Or, got {cond:?}");
        };
        assert_eq!(card_types, vec![CoreType::Creature]);
        let TargetFilter::Or { filters } = *subtype_filter else {
            panic!("expected subtype Or filter, got {subtype_filter:?}");
        };
        assert_eq!(filters.len(), 4);
    }

    #[test]
    fn kenessos_split_leading_conditional_preserves_multi_subtype() {
        let (condition, rest) = split_leading_conditional(
            "If it's a Kraken, Leviathan, Octopus, or Serpent creature card, you may put it onto the battlefield.",
        )
        .expect("comma inside subtype list must not split early");
        assert_eq!(
            condition,
            "If it's a Kraken, Leviathan, Octopus, or Serpent creature card"
        );
        assert_eq!(rest, "you may put it onto the battlefield.");
        let cond = try_nom_condition_as_ability_condition(
            "it's a Kraken, Leviathan, Octopus, or Serpent creature card",
            &mut ParseContext::default(),
        );
        let Some(AbilityCondition::RevealedHasCardType {
            card_types,
            subtype_filter: Some(subtype_filter),
            ..
        }) = cond
        else {
            panic!("expected multi-subtype RevealedHasCardType, got {cond:?}");
        };
        assert_eq!(card_types, vec![CoreType::Creature]);
        let TargetFilter::Or { filters } = *subtype_filter else {
            panic!("expected subtype Or filter");
        };
        assert_eq!(filters.len(), 4);
    }

    #[test]
    fn its_a_type_condition_preserves_multi_subtype() {
        let cond = parse_its_a_type_condition(
            "it's a kraken, leviathan, octopus, or serpent creature card",
            &mut ParseContext::default(),
        );
        let Some(AbilityCondition::RevealedHasCardType {
            card_types,
            subtype_filter: Some(subtype_filter),
            ..
        }) = cond
        else {
            panic!("expected multi-subtype RevealedHasCardType, got {cond:?}");
        };
        assert_eq!(card_types, vec![CoreType::Creature]);
        let TargetFilter::Or { filters } = *subtype_filter else {
            panic!("expected subtype Or filter");
        };
        assert_eq!(filters.len(), 4);
    }

    /// Issue #1525 — Gathering Stone: "if it's a card of the chosen type".
    #[test]
    fn if_its_a_card_of_the_chosen_type_revealed_condition() {
        let (cond, _) = strip_card_type_conditional(
            "If it's a card of the chosen type, you may reveal it and put it into your hand.",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::RevealedHasCardType {
                card_types: vec![],
                additional_filter: Some(FilterProp::IsChosenCreatureType),
                subtype_filter: None,
            })
        );
    }

    /// CR 202.3 + CR 608.2c: Kellan, Daring Traveler — leading-if revealed-card
    /// gate with a mana-value bound must carry `FilterProp::Cmc` as
    /// `additional_filter`, not drop the suffix.
    #[test]
    fn strip_card_type_conditional_creature_with_mana_value_ceiling() {
        let (cond, body) = strip_card_type_conditional(
            "If it's a creature card with mana value 3 or less, put it into your hand.",
        );
        assert_eq!(body, "put it into your hand.");
        let Some(AbilityCondition::RevealedHasCardType {
            card_types,
            additional_filter,
            subtype_filter,
        }) = cond
        else {
            panic!("expected RevealedHasCardType with Cmc filter, got {cond:?}");
        };
        assert_eq!(card_types, vec![CoreType::Creature]);
        assert_eq!(
            additional_filter,
            Some(FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 3 },
            })
        );
        assert!(subtype_filter.is_none());
    }

    /// CR 608.2c: Suffix-if peel (`strip_suffix_conditional`) must stay in lockstep
    /// with the leading-if `strip_card_type_conditional` mana-value gate.
    #[test]
    fn suffix_if_creature_with_mana_value_ceiling() {
        let (cond, body) = strip_suffix_conditional(
            "put it into your hand if it's a creature card with mana value 3 or less",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "put it into your hand");
        let Some(AbilityCondition::RevealedHasCardType {
            card_types,
            additional_filter,
            ..
        }) = cond
        else {
            panic!("expected suffix-if RevealedHasCardType, got {cond:?}");
        };
        assert_eq!(card_types, vec![CoreType::Creature]);
        assert_eq!(
            additional_filter,
            Some(FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 3 },
            })
        );
    }

    /// CR 406.6 + CR 202.3: Exiled-card demonstrative gates share the same
    /// mana-value suffix surface as revealed-card gates.
    #[test]
    fn exiled_card_type_conditional_with_mana_value_ceiling() {
        let (cond, body) = parse_if_exiled_card_type_conditional(
            "If the exiled card is a creature card with mana value 2 or less, put it onto the battlefield.",
        )
        .expect("exiled-card MV gate must parse");
        assert_eq!(body, "put it onto the battlefield.");
        let AbilityCondition::RevealedHasCardType {
            card_types,
            additional_filter,
            ..
        } = cond
        else {
            panic!("expected RevealedHasCardType, got {cond:?}");
        };
        assert_eq!(card_types, vec![CoreType::Creature]);
        assert_eq!(
            additional_filter,
            Some(FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 2 },
            })
        );
    }

    /// CR 608.2c: Regression guard for the subtype fall-through. parse_type_phrase
    /// fully consumes "creature card of the chosen type" (CoreType + chosen-type
    /// property, empty leftover), but that phrase belongs to RevealedHasCardType
    /// (produced by strip_card_type_conditional in the chain path), not the
    /// present-target subtype filter. The references_subtype gate ensures the new
    /// subtype arm declines here rather than hijacking it into a TargetMatchesFilter
    /// — without the gate this regressed Herald's Horn (Dig + chosen-type reveal).
    #[test]
    fn if_its_a_creature_card_of_chosen_type_not_hijacked_by_subtype_arm() {
        let cond = try_nom_condition_as_ability_condition(
            "it's a creature card of the chosen type",
            &mut ParseContext::default(),
        );
        assert!(
            !matches!(cond, Some(AbilityCondition::TargetMatchesFilter { .. })),
            "CoreType chosen-type phrase must not be hijacked into a subtype \
             TargetMatchesFilter, got {cond:?}"
        );
    }

    /// CR 603.4 + CR 601.2 + CR 603.6c: Fblthp, the Lost (issue #2374) — library
    /// origin gate for the "draw two cards instead" rider.
    #[test]
    fn entered_from_library_or_cast_from_library_condition() {
        let cond = try_nom_condition_as_ability_condition(
            "it entered from your library or was cast from your library",
            &mut ParseContext::default(),
        );
        let Some(AbilityCondition::Or { conditions }) = cond else {
            panic!("expected Or condition, got {cond:?}");
        };
        assert_eq!(conditions.len(), 2);
        assert!(matches!(
            &conditions[0],
            AbilityCondition::ZoneChangeObjectMatchesFilter {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                ..
            }
        ));
        assert!(matches!(
            &conditions[1],
            AbilityCondition::WasCast {
                zone: Some(Zone::Library)
            }
        ));
    }

    /// CR 608.2c: Full instead-clause assembly for Fblthp's ETB draw rider.
    #[test]
    fn fblthp_library_origin_instead_clause() {
        let InsteadLowering::Branch(instead) = try_parse_generic_instead_clause(
            "If it entered from your library or was cast from your library, draw two cards instead.",
            AbilityKind::Spell,
            &mut ParseContext::default(),
        ) else {
            panic!("instead clause must lower to a conditional branch");
        };
        assert!(matches!(&*instead.effect, Effect::Draw { .. }));
        let cond = instead
            .condition
            .as_ref()
            .expect("instead must carry condition");
        let AbilityCondition::ConditionInstead { inner } = cond else {
            panic!("expected ConditionInstead wrapper, got {cond:?}");
        };
        assert!(matches!(
            inner.as_ref(),
            AbilityCondition::Or { conditions } if conditions.len() == 2
        ));
    }

    /// L02 BB-FU4 narrowing fence (CR 601.2a + CR 707.10): the copy-correct
    /// `∃cast` And-wrap is applied ONLY to the "anywhere other than X"
    /// producer. The opposite-presupposition "you didn't cast it from X" arm
    /// stays a BARE `Not(WasCast{Some(X)})` — wrapping it would regress
    /// reanimate/copy semantics (a reanimated object correctly evaluates true
    /// there). This directly exercises both arms of the exact edited function:
    /// removing the wrap from line ~1090 flips the first assertion; adding a
    /// wrap to line ~1049 flips the second.
    #[test]
    fn bbfu4_only_anywhere_other_than_gains_existential_cast_conjunct() {
        // Positive-cast presupposition → And[WasCast{None}, Not(WasCast{Some(Hand)})].
        let (anywhere, rest_a) = strip_cast_from_zone_conditional(
            "if this spell was cast from anywhere other than your hand",
        );
        assert_eq!(rest_a, "");
        assert_eq!(
            anywhere,
            Some(AbilityCondition::And {
                conditions: vec![
                    AbilityCondition::WasCast { zone: None },
                    AbilityCondition::Not {
                        condition: Box::new(AbilityCondition::WasCast {
                            zone: Some(Zone::Hand)
                        }),
                    },
                ],
            }),
            "the 'anywhere other than' arm must gain the ∃cast And-wrap"
        );

        // Opposite presupposition → BARE Not, NO And-wrap.
        let (didnt, rest_d) =
            strip_cast_from_zone_conditional("if you didn't cast it from your hand");
        assert_eq!(rest_d, "");
        assert_eq!(
            didnt,
            Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::WasCast {
                    zone: Some(Zone::Hand)
                }),
            }),
            "the 'didn't cast it from X' arm must stay a bare Not (no ∃cast conjunct)"
        );
    }

    /// CR 608.2c: ETB base draw + library-origin instead override chain.
    #[test]
    fn fblthp_etb_draw_chain_with_library_instead() {
        let def = parse_effect_chain(
            "Draw a card. If it entered from your library or was cast from your library, draw two cards instead.",
            AbilityKind::Spell,
        );
        assert!(matches!(&*def.effect, Effect::Draw { .. }));
        let sub = def
            .sub_ability
            .as_ref()
            .expect("expected instead sub_ability");
        assert!(matches!(
            &*sub.effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ));
        let cond = sub.condition.as_ref().expect("instead sub must be gated");
        assert!(matches!(
            cond,
            AbilityCondition::ConditionInstead { inner }
                if matches!(inner.as_ref(), AbilityCondition::Or { .. })
        ));
    }

    /// CR 611.2b + CR 115.1: Shackle Slinger's "it's tapped" binds the tapped
    /// status to the chosen target (CR 115.1: targets) via `TargetMatchesFilter`.
    #[test]
    fn anaphoric_status_its_tapped_targets_chosen_creature() {
        let cond =
            try_nom_condition_as_ability_condition("it's tapped", &mut ParseContext::default())
                .expect("'it's tapped' should parse");
        match cond {
            AbilityCondition::TargetMatchesFilter {
                filter, use_lki, ..
            } => {
                assert!(!use_lki, "present-tense status uses current state");
                assert_eq!(
                    filter,
                    TargetFilter::Typed(TypedFilter {
                        properties: vec![FilterProp::Tapped],
                        ..Default::default()
                    })
                );
            }
            other => panic!("expected TargetMatchesFilter, got {other:?}"),
        }
    }

    /// CR 701.60b + CR 115.1: Agrus Kos's "it's suspected" binds the suspected
    /// designation to the chosen target (CR 115.1: targets) via `TargetMatchesFilter`.
    #[test]
    fn anaphoric_status_its_suspected_targets_chosen_creature() {
        let cond =
            try_nom_condition_as_ability_condition("it's suspected", &mut ParseContext::default())
                .expect("'it's suspected' should parse");
        assert_eq!(
            cond,
            AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(TypedFilter {
                    properties: vec![FilterProp::Suspected],
                    ..Default::default()
                }),
                use_lki: false,
                subject_slot: None,
            }
        );
    }

    /// CR 701.60b + CR 601.2b: Repeat Offender's self-referential "~ is suspected"
    /// (an activated ability that carries no chosen target) binds to the SOURCE via
    /// `SourceMatchesFilter`, not `TargetMatchesFilter` — an absent target would
    /// never satisfy the gate. "this creature is suspected" resolves identically.
    #[test]
    fn anaphoric_status_self_suspected_reads_source() {
        let expected = AbilityCondition::SourceMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter {
                properties: vec![FilterProp::Suspected],
                ..Default::default()
            }),
        };
        for text in ["~ is suspected", "this creature is suspected"] {
            let cond = try_nom_condition_as_ability_condition(text, &mut ParseContext::default())
                .unwrap_or_else(|| panic!("{text} should parse"));
            assert_eq!(cond, expected, "{text}");
        }
    }

    /// The anaphoric-status arm must not poach type phrases: "it's a Goblin" still
    /// routes to the `TargetMatchesFilter` subtype arm (a `Goblin` subtype filter),
    /// not the status recognizer (which only knows tapped/suspected).
    #[test]
    fn anaphoric_status_does_not_poach_its_a_type() {
        let cond =
            try_nom_condition_as_ability_condition("it's a Goblin", &mut ParseContext::default())
                .expect("'it's a Goblin' should still parse via the type arm");
        match cond {
            AbilityCondition::TargetMatchesFilter { filter, .. } => match filter {
                TargetFilter::Typed(typed) => {
                    assert!(
                        typed
                            .type_filters
                            .iter()
                            .any(type_filter_references_subtype),
                        "expected a subtype filter, got {typed:?}"
                    );
                    assert!(
                        !typed.properties.contains(&FilterProp::Tapped)
                            && !typed.properties.contains(&FilterProp::Suspected),
                        "type arm must not carry a status prop"
                    );
                }
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("expected TargetMatchesFilter, got {other:?}"),
        }
    }

    /// CR 702.171b: Caustic Bronco's "~ isn't saddled" lowers to
    /// `Not { SourceMatchesFilter { Typed([IsSaddled]) } }` — the runtime seam the
    /// saddled designation is evaluated through (no dedicated `AbilityCondition`).
    #[test]
    fn source_isnt_saddled_lowers_to_not_source_matches_filter() {
        let cond =
            try_nom_condition_as_ability_condition("~ isn't saddled", &mut ParseContext::default())
                .expect("'~ isn't saddled' should parse");
        match cond {
            AbilityCondition::Not { condition } => match *condition {
                AbilityCondition::SourceMatchesFilter { filter } => {
                    assert_eq!(
                        filter,
                        TargetFilter::Typed(TypedFilter {
                            properties: vec![FilterProp::IsSaddled],
                            ..Default::default()
                        })
                    );
                }
                other => panic!("expected SourceMatchesFilter, got {other:?}"),
            },
            other => panic!("expected Not, got {other:?}"),
        }
    }

    /// CR 702.171b: the affirmative "~ is saddled" lowers to the bare
    /// `SourceMatchesFilter` (un-negated sibling of the test above).
    #[test]
    fn source_is_saddled_lowers_to_source_matches_filter() {
        let cond =
            try_nom_condition_as_ability_condition("~ is saddled", &mut ParseContext::default())
                .expect("'~ is saddled' should parse");
        assert_eq!(
            cond,
            AbilityCondition::SourceMatchesFilter {
                filter: TargetFilter::Typed(TypedFilter {
                    properties: vec![FilterProp::IsSaddled],
                    ..Default::default()
                }),
            }
        );
    }

    /// CR 508.1k + CR 509.1g + CR 506.5 + CR 608.2c: the source-anaphoric
    /// combat-state static conditions bridge to `SourceMatchesFilter` against
    /// the ability source with the matching runtime `FilterProp`. This is the
    /// building-block-level guard for the whole class (not one card): if any
    /// arm regresses to `None`, the in-effect `if he's attacking/blocking`
    /// rider is silently dropped and the gated sub-effects fire
    /// unconditionally (The Incredible Hulk's Enrage untap + extra combat).
    #[test]
    fn source_combat_state_conditions_bridge_to_source_matches_filter() {
        let cases = [
            (
                StaticCondition::SourceIsAttacking,
                vec![FilterProp::Attacking { defender: None }],
            ),
            (
                StaticCondition::SourceIsBlocking,
                vec![FilterProp::Blocking],
            ),
            (
                StaticCondition::SourceAttackingAlone,
                vec![FilterProp::AttackingAlone],
            ),
        ];
        for (sc, props) in cases {
            let mapped = static_condition_to_ability_condition(&sc, &mut ParseContext::default());
            assert_eq!(
                mapped,
                Some(AbilityCondition::SourceMatchesFilter {
                    filter: TargetFilter::Typed(TypedFilter {
                        properties: props,
                        ..Default::default()
                    }),
                }),
                "{sc:?} must bridge to SourceMatchesFilter, not None"
            );
        }
        // `SourceIsBlocked` has no clean 1:1 runtime FilterProp and must stay
        // unmapped (left in the `=> None` bucket) — guards against accidentally
        // moving it out alongside the bridged siblings.
        assert_eq!(
            static_condition_to_ability_condition(
                &StaticCondition::SourceIsBlocked,
                &mut ParseContext::default()
            ),
            None,
            "SourceIsBlocked must remain unmapped (no clean FilterProp::Blocked)"
        );
    }

    /// CR 400.7 + CR 608.2c: the per-target "entered this turn" detector (a)
    /// recognizes both the filter-named subject ("the creature you control") and
    /// the anaphor ("it"), carrying the subject's type + controller and appending
    /// `EnteredThisTurn`. The subject_slot is `None` at parse time — the two-target
    /// counter rewrite sets it to `Some(0)`.
    #[test]
    fn entered_this_turn_detector_named_and_anaphor_subjects() {
        // Filter-named subject keeps its controller (You).
        let cond = parse_target_entered_this_turn_condition_text(
            "the creature you control entered this turn",
        )
        .expect("named-filter subject parses");
        match cond {
            AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(tf),
                use_lki,
                subject_slot,
            } => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::EnteredThisTurn));
                assert!(
                    !use_lki,
                    "creature is on the battlefield — live state (CR 400.7)"
                );
                assert_eq!(
                    subject_slot, None,
                    "slot binding is set later by the rewrite"
                );
            }
            other => panic!("expected TargetMatchesFilter, got {other:?}"),
        }

        // Anaphor subject defaults to creature() (no controller restriction).
        let cond = parse_target_entered_this_turn_condition_text("it entered this turn")
            .expect("anaphor subject parses");
        match cond {
            AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(tf),
                ..
            } => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert!(tf.properties.contains(&FilterProp::EnteredThisTurn));
            }
            other => panic!("expected TargetMatchesFilter, got {other:?}"),
        }
    }

    /// A2 over-fire guards: `all_consuming` must reject the control-count,
    /// existential, and source-referential forms so they still reach their own
    /// recognizers (QuantityCheck / SourceEnteredThisTurn) rather than being
    /// cannibalized into a per-target `TargetMatchesFilter`.
    #[test]
    fn entered_this_turn_detector_rejects_non_per_target_forms() {
        assert!(
            parse_target_entered_this_turn_condition_text(
                "you control a creature that entered this turn"
            )
            .is_none(),
            "control-count form must not parse as the per-target detector"
        );
        assert!(
            parse_target_entered_this_turn_condition_text(
                "a creature entered the battlefield this turn"
            )
            .is_none(),
            "existential form must not parse as the per-target detector"
        );
        assert!(
            parse_target_entered_this_turn_condition_text("~ entered this turn").is_none(),
            "source-referential form stays with SourceEnteredThisTurn"
        );
    }

    /// The dispatcher routes the filter-named form to the new per-target detector
    /// (proving the registration point), and does NOT route the control-count
    /// form to it.
    #[test]
    fn dispatcher_routes_entered_this_turn_to_per_target_detector() {
        let routed = try_nom_condition_as_ability_condition(
            "the creature you control entered this turn",
            &mut ParseContext::default(),
        );
        assert!(
            matches!(
                routed,
                Some(AbilityCondition::TargetMatchesFilter {
                    subject_slot: None,
                    ..
                })
            ),
            "dispatcher routes the named-subject entered-this-turn gate to the detector, got {routed:?}"
        );
    }

    // ---- "you control a <noun> of each <dimension>" (Coalition Victory) ----

    /// CR 305.6: the single-clause recognizer expands "a land of each basic land
    /// type" into one `ControllerControlsMatching{land.subtype(x).You}` per basic
    /// land type, in canonical order.
    #[test]
    fn of_each_basic_land_type_expands_to_five_subtypes() {
        let cond =
            parse_you_control_of_each_condition_text("you control a land of each basic land type")
                .expect("clause must parse");
        let AbilityCondition::And { conditions } = cond else {
            panic!("expected And, got {cond:?}");
        };
        let subtypes: Vec<String> = conditions
            .iter()
            .map(|c| match c {
                AbilityCondition::ControllerControlsMatching {
                    filter: TargetFilter::Typed(tf),
                } => {
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                    assert!(tf
                        .type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Land)));
                    tf.get_subtype().expect("subtype").to_string()
                }
                other => panic!("expected ControllerControlsMatching land, got {other:?}"),
            })
            .collect();
        assert_eq!(
            subtypes,
            vec!["Plains", "Island", "Swamp", "Mountain", "Forest"]
        );
    }

    /// CR 105.1: "a creature of each color" expands to one
    /// `ControllerControlsMatching{creature.HasColor(c).You}` per WUBRG color.
    #[test]
    fn of_each_color_expands_to_five_colors() {
        let cond = parse_you_control_of_each_condition_text("you control a creature of each color")
            .expect("clause must parse");
        let AbilityCondition::And { conditions } = cond else {
            panic!("expected And, got {cond:?}");
        };
        let colors: Vec<ManaColor> = conditions
            .iter()
            .map(|c| match c {
                AbilityCondition::ControllerControlsMatching {
                    filter: TargetFilter::Typed(tf),
                } => {
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                    assert!(tf
                        .type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Creature)));
                    match tf.properties.as_slice() {
                        [FilterProp::HasColor { color }] => *color,
                        other => panic!("expected single HasColor prop, got {other:?}"),
                    }
                }
                other => panic!("expected ControllerControlsMatching creature, got {other:?}"),
            })
            .collect();
        assert_eq!(colors, ManaColor::ALL.to_vec());
    }

    /// CR 105.1: "a permanent of each color" (Spirit of Resistance's inner clause)
    /// parses to the same 5-color permanent expansion. This exercises the
    /// building-block for that card's dimension; note that Spirit of Resistance's
    /// static "as long as" path resolves through a SEPARATE `ParsedCondition`
    /// parser (`oracle_condition.rs`) and is unaffected by this recognizer.
    #[test]
    fn of_each_color_permanent_variant_parses() {
        let cond =
            parse_you_control_of_each_condition_text("you control a permanent of each color")
                .expect("permanent clause must parse");
        let AbilityCondition::And { conditions } = cond else {
            panic!("expected And, got {cond:?}");
        };
        assert_eq!(conditions.len(), 5);
        for c in &conditions {
            let AbilityCondition::ControllerControlsMatching {
                filter: TargetFilter::Typed(tf),
            } = c
            else {
                panic!("expected ControllerControlsMatching, got {c:?}");
            };
            assert!(tf
                .type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Permanent)));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::HasColor { .. })));
        }
    }

    /// The `"you control"` possessive scopes BOTH clauses, so the recognizer binds
    /// the conjunction itself: "you control a land of each basic land type and a
    /// creature of each color" → a flat `And` of all 10 members (5 lands + 5
    /// colors). The generic top-level splitter would strip "you control" off the
    /// second conjunct, which is why the conjunction lives inside this recognizer.
    #[test]
    fn of_each_recognizer_binds_the_shared_subject_conjunction() {
        let cond = parse_you_control_of_each_condition_text(
            "you control a land of each basic land type and a creature of each color",
        )
        .expect("full conjunction must parse");
        let AbilityCondition::And { conditions } = cond else {
            panic!("expected And, got {cond:?}");
        };
        assert_eq!(conditions.len(), 10, "5 basic land types + 5 colors");
        // A bare clause with no leading subject must NOT parse (the possessive is
        // mandatory — this is a controller-scoped condition).
        assert!(
            parse_you_control_of_each_condition_text("a creature of each color").is_none(),
            "the 'you control' subject is required"
        );
    }

    /// Isolates the trailing-if router: `strip_suffix_conditional` on Coalition
    /// Victory's whole sentence must peel the effect and return the composed
    /// `And{[…10 members]}` condition.
    #[test]
    fn strip_suffix_conditional_composes_coalition_victory() {
        let (cond, body) = strip_suffix_conditional(
            "You win the game if you control a land of each basic land type and a creature of each color",
            &mut ParseContext::default(),
        );
        assert_eq!(body, "You win the game");
        let Some(AbilityCondition::And { conditions }) = cond else {
            panic!("expected And, got {cond:?}");
        };
        assert_eq!(conditions.len(), 10);
    }

    /// CR 104.2b + CR 608.2c: Coalition Victory's whole condition attaches to the
    /// parsed ability as a flat `And{[10× ControllerControlsMatching]}` (5 basic
    /// land subtypes + 5 colors). Revert guard: on pre-fix code `condition` is
    /// `None` (the win-condition was dropped and the win fired unconditionally).
    #[test]
    fn coalition_victory_attaches_full_ten_member_condition() {
        let parsed = parse_oracle_text(
            "You win the game if you control a land of each basic land type and a creature of each color.",
            "Coalition Victory",
            &[],
            &["Sorcery".to_string()],
            &[],
        );
        let ability = parsed
            .abilities
            .iter()
            .find(|a| matches!(*a.effect, Effect::WinTheGame { .. }))
            .expect("Coalition Victory must parse a WinTheGame ability");
        let Some(AbilityCondition::And { conditions }) = ability.condition.clone() else {
            panic!("expected And condition, got {:?}", ability.condition);
        };
        assert_eq!(conditions.len(), 10, "5 basic land types + 5 colors");
        // 5 land-subtype members, controller-scoped.
        let land_subtypes: Vec<String> = conditions
            .iter()
            .filter_map(|c| match c {
                AbilityCondition::ControllerControlsMatching {
                    filter: TargetFilter::Typed(tf),
                } if tf.get_subtype().is_some() => {
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                    Some(tf.get_subtype().unwrap().to_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            land_subtypes,
            vec!["Plains", "Island", "Swamp", "Mountain", "Forest"]
        );
        // 5 color members via HasColor, controller-scoped.
        let colors: Vec<ManaColor> = conditions
            .iter()
            .filter_map(|c| match c {
                AbilityCondition::ControllerControlsMatching {
                    filter: TargetFilter::Typed(tf),
                } => tf.properties.iter().find_map(|p| match p {
                    FilterProp::HasColor { color } => {
                        assert_eq!(tf.controller, Some(ControllerRef::You));
                        Some(*color)
                    }
                    _ => None,
                }),
                _ => None,
            })
            .collect();
        assert_eq!(colors, ManaColor::ALL.to_vec());
    }
}
