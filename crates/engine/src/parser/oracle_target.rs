use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::{tag, take_till, take_till1};
use nom::character::complete::space1;
use nom::combinator::{eof, not, opt, peek, success, value};
use nom::multi::many0;
use nom::sequence::{preceded, terminated};
use nom::Parser;

use crate::types::ability::{
    AggregateFunction, AttachmentKind, ChoiceType, CombatRelation, CombatRelationSubject,
    Comparator, ControllerRef, CountScope, DamageKindFilter, FilterProp, ObjectProperty,
    ObjectScope, ParitySource, PlayerFilter, PtStat, PtValueScope, QuantityExpr, QuantityRef,
    SeatDirection, SharedQuality, SharedQualityRelation, TargetFilter, TargetSelectionMode,
    TypeFilter, TypedFilter,
};
use crate::types::card_type::Supertype;
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::identifiers::TrackedSetId;
use crate::types::keywords::{Keyword, KeywordKind};
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

use super::oracle_effect::{
    is_bare_object_pronoun, parse_multi_target_count_expr, resolve_it_pronoun,
};
use super::oracle_ir::context::ParseContext;
use super::oracle_ir::diagnostic::OracleDiagnostic;
use super::oracle_nom::error::{OracleError, OracleResult};
use super::oracle_nom::filter as nom_filter;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::quantity as nom_quantity;
use super::oracle_nom::target as nom_target;
use super::oracle_quantity::capitalize_first;
use super::oracle_util::{
    merge_or_filters, parse_subtype, strip_possessive, TextPair, SELF_REF_PARSE_ONLY_PHRASES,
    SELF_REF_TYPE_PHRASES,
};

/// CR 115.1: Whether a parsed target phrase used the "target" keyword
/// (`TargetKeyword`) or a controller-scope descriptor like "a creature you
/// control" (`Descriptor`). Used to distinguish targeted bounce effects from
/// the Whitemane Lion class at lowering time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSyntax {
    /// The phrase contained the "target" keyword.
    TargetKeyword,
    /// The phrase used a descriptor (no "target" keyword).
    Descriptor,
}

/// Run a nom combinator on lowercased text, returning the result and
/// remainder from the original (mixed-case) text.
///
/// This bridges the nom combinator world (which expects lowercase input)
/// with the oracle_target API (which preserves original casing in remainders).
fn nom_on_lower<'a, T, F>(text: &'a str, lower: &str, mut parser: F) -> Option<(T, &'a str)>
where
    F: FnMut(&str) -> super::oracle_nom::error::OracleResult<'_, T>,
{
    let (rest, result) = parser(lower).ok()?;
    let consumed = lower.len() - rest.len();
    Some((result, &text[consumed..]))
}

/// CR 608.2c + CR 608.2k: Resolve a bare object pronoun ("it", "them", "him",
/// "her") to the correct anaphor binding based on parser context.
///
/// Two anaphor classes apply to bare object pronouns:
///
/// 1. **Trigger-subject anaphor** (CR 608.2k): the pronoun refers to the
///    object matched by the triggering event ("Whenever an Elf you control
///    dies, exile it"). Activated only when `ctx.subject` is a *typed* (or
///    `AttachedTo`) filter — i.e. a non-source object the trigger condition
///    explicitly named. Routes via `resolve_it_pronoun` → `TriggeringSource`.
///    Issue #319 (Serpent's Soul-Jar): without this routing, "exile it"
///    incorrectly bound to the Jar instead of the dying Elf.
///
/// 2. **Compound-effect parent-target anaphor** (CR 608.2c): the pronoun
///    refers back to a target selected earlier in the same instruction
///    sequence ("Tap target creature. It doesn't untap"; "When ~ enters, choose
///    a target creature. Exile it"). Activated when `ctx.subject` is `None`,
///    `SelfRef`, or `Any` — these contexts do not introduce a non-source
///    triggering object, so the only valid antecedent is the parent ability's
///    selected target. Returns `ParentTarget`.
///
/// The discriminator is *whether the trigger subject introduces a non-source
/// object*, not *whether a subject exists*. Self-ETB triggers (`SelfRef`
/// subject) and player-actor triggers (`Any` subject) must keep
/// `ParentTarget` so cards like Agrus Kos ("Whenever ~ enters, choose target
/// creature. Exile it") continue to exile the chosen creature, not the source.
///
/// `pronoun` is accepted only for diagnostic clarity at call sites; the
/// resolution itself is uniform across the bare object pronoun family per
/// `is_bare_object_pronoun`.
pub(crate) fn resolve_pronoun_target(ctx: &mut ParseContext, pronoun: &str) -> TargetFilter {
    debug_assert!(
        is_bare_object_pronoun(pronoun),
        "resolve_pronoun_target called with non-pronoun token: {pronoun}"
    );
    if let Some(target) = ctx.object_pronoun_ref.clone() {
        return target;
    }
    match &ctx.subject {
        Some(subject) if !matches!(subject, TargetFilter::SelfRef | TargetFilter::Any) => {
            resolve_it_pronoun(ctx)
        }
        _ => TargetFilter::ParentTarget,
    }
}

/// CR 608.2c: Recognize a demonstrative ("that creature"), definite-article
/// ("the creature"), or bare-pronoun ("it") back-reference to a parent
/// instruction's chosen target, in contexts where `resolve_pronoun_target`'s
/// trigger-subject branch cannot apply (no live `ParseContext` at this call
/// site — only the precomputed `parent_target_available` flag). Every
/// verified card using this recognizer is a non-trigger activated ability or
/// instant, so `ParentTarget` is the uniform correct answer; a future
/// trigger-context card needing `TriggeringSource` should extend via
/// `resolve_pronoun_target`'s ctx-threading instead of this function.
/// Distinct from `parse_event_context_ref`'s "that creature" arm (which
/// resolves to `TriggeringSource` for triggered-ability event context — the
/// wrong filter for this non-trigger context).
///
/// Returns the bound filter and the remainder of the ORIGINAL-case `text`
/// following the matched anaphor phrase.
pub(crate) fn parse_anaphoric_target_ref(
    text: &str,
    parent_target_available: bool,
) -> Option<(TargetFilter, &str)> {
    if !parent_target_available {
        return None;
    }
    // CR 608.2c: the anaphor grammar itself ("that <type>" / "the <type>" →
    // `ParentTarget`, and bare word-bounded "it" → `ParentTarget` via
    // `resolve_pronoun_target`'s default-context fallthrough) is authoritatively
    // implemented by `parse_target` — reuse that single implementation rather
    // than re-deriving the same rule here. `parse_target` is the ctx-free
    // wrapper (default `ParseContext`), so no live trigger subject exists and
    // the bare-pronoun/demonstrative back-reference uniformly resolves to
    // `ParentTarget` — exactly the non-trigger semantics this call site needs.
    // Accept only when the leading phrase actually resolved to the parent
    // target; a fresh typed filter or the `Any` fallback means the text was not
    // an anaphor, and this recognizer must decline (preserving the old
    // combinator's None-on-no-match contract).
    let (filter, rest) = parse_target(text);
    matches!(filter, TargetFilter::ParentTarget).then_some((filter, rest))
}

/// Parse a word with a word boundary check: the next char after the word must be
/// non-alphanumeric (whitespace, comma, period, etc.) or end-of-input.
/// Prevents "it" from matching "item", "you" from matching "your", etc.
pub(crate) fn parse_word_bounded<'a>(
    input: &'a str,
    word: &str,
) -> super::oracle_nom::error::OracleResult<'a, ()> {
    let (rest, _) = tag::<_, _, OracleError<'_>>(word).parse(input)?;
    match rest.chars().next() {
        None | Some(' ' | ',' | '.' | ';' | ':' | ')' | '\'' | '"' | '/' | '-') => Ok((rest, ())),
        _ => Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        ))),
    }
}

fn parse_card_or_cards_word(input: &str) -> super::oracle_nom::error::OracleResult<'_, ()> {
    parse_word_bounded(input, "cards").or_else(|_| parse_word_bounded(input, "card"))
}

/// CR 608.2c + CR 406.6 + CR 607.2a + CR 707.10 + CR 702.75a: Resolve a
/// singular "the exiled card" / "copy the exiled card" anaphor to the
/// correct binding. When an earlier clause in the SAME resolution chain
/// already exiled a card (`chain_has_prior_exile_producer`), the anaphor
/// refers to that same-chain exile and keeps its pre-existing binding
/// (`same_chain_binding`, e.g. `TrackedSet{0}` or `ParentTarget`). Otherwise
/// the referenced exile happened in an EARLIER, separately-resolved ability
/// (e.g. an ETB Imprint or a synthesized Hideaway ETB) — CR 406.6 durable
/// exile-zone tracking is required, so the anaphor must bind to
/// `TargetFilter::ExiledBySource`, resolved at runtime via this source's
/// `exile_links` (CR 607.1/607.2a: linked abilities reference the same
/// object across resolutions).
pub(crate) fn resolve_singular_exiled_card_target(
    chain_has_prior_exile_producer: bool,
    same_chain_binding: TargetFilter,
) -> TargetFilter {
    if chain_has_prior_exile_producer {
        same_chain_binding
    } else {
        TargetFilter::ExiledBySource
    }
}

/// Parse an event-context possessive reference from Oracle text.
/// These resolve from the triggering event, not from player targeting.
/// Must be checked BEFORE standard `parse_target` for trigger-based effects.
/// CR 608.2k: Parse event-context references ("that player", "that permanent", etc.)
/// that refer back to objects/players mentioned in a trigger condition or cost.
/// Returns the matched filter and unconsumed remainder text.
pub fn parse_event_context_ref(text: &str) -> Option<(TargetFilter, &str)> {
    let text = text.trim();
    let lower = text.to_lowercase();

    // CR 608.2k: Event-context references resolve from the triggering event.
    // All patterns in one nom alt() for clean longest-match-first dispatch.
    nom_on_lower(text, &lower, |input| {
        nom::branch::alt((
            // Longest-match-first within shared prefixes.
            value(
                TargetFilter::TriggeringSpellController,
                tag::<_, _, OracleError<'_>>("that spell's controller"),
            ),
            value(
                TargetFilter::TriggeringSpellOwner,
                tag("that spell's owner"),
            ),
            // CR 608.2c: "its controller" / "their controller" — controller of the parent target.
            value(TargetFilter::ParentTargetController, tag("its controller")),
            value(
                TargetFilter::ParentTargetController,
                tag("their controller"),
            ),
            // CR 108.3 + CR 608.2c: "its owner" / "their owner" — owner of the parent target.
            // Used by Aura damage triggers (Enslave) and damage continuations (Bomb Squad,
            // The Beast Deathless Prince) where the anaphoric "its" refers to a permanent
            // mentioned earlier in the sentence.
            value(TargetFilter::ParentTargetOwner, tag("its owner")),
            value(TargetFilter::ParentTargetOwner, tag("their owner")),
            value(TargetFilter::TriggeringPlayer, tag("that player")),
            value(TargetFilter::TriggeringSource, tag("that source")),
            value(
                TargetFilter::TriggeringSource,
                terminated(
                    tag("that permanent"),
                    not(preceded(
                        tag(" "),
                        alt((tag("or player"), tag("or a player"))),
                    )),
                ),
            ),
            // CR 608.2k + CR 301.5a: "that creature" inside a trigger refers to the
            // triggering source object (e.g. Pip-Boy 3000's "Whenever equipped
            // creature attacks ... put a +1/+1 counter on that creature"), not to
            // a parent target. Placed after longer "that ..." phrases so
            // longest-match-first dispatch is preserved.
            value(TargetFilter::TriggeringSource, tag("that creature")),
            // CR 508.5 / CR 508.5a: "defending player" — the player (or the
            // protector of the battle / controller of the planeswalker) that the
            // attacking creature is attacking.
            value(TargetFilter::DefendingPlayer, tag("defending player")),
        ))
        .parse(input)
    })
}

/// Parse a target description from Oracle text, returning (filter, remaining_text).
/// Consumes the longest matching target phrase.
///
/// Uses first-character dispatch to group `starts_with` checks, reducing average
/// comparisons from ~12 to ~3-4 per call.
///
/// Prefer `parse_target_with_ctx` when a `ParseContext` is available — diagnostics
/// from the fallback path (TargetFallback) are accumulated there. This wrapper
/// creates a temporary context whose diagnostics are discarded.
pub fn parse_target(text: &str) -> (TargetFilter, &str) {
    parse_target_with_ctx(text, &mut ParseContext::default())
}

/// Parse a target noun phrase, additionally consuming an optional trailing
/// heterogeneous relative-clause disjunction that the base grammar cannot fold
/// into one typed filter — a card type ("that's an artifact") OR a mana-value
/// bound ("that has mana value 3 or less"). Desdemona, Freedom's Edge: "target
/// creature card in your graveyard that's an artifact or that has mana value 3
/// or less".
///
/// CR 115.1 + CR 608.2c: a card type lives in `type_filters` and a mana-value
/// bound lives in `properties` — both AND-combined within a single
/// `TypedFilter` — so an "or" *between* the two layers cannot collapse into one
/// `FilterProp::AnyOf`. Instead the disjunction distributes over the whole typed
/// filter as `TargetFilter::Or`, one leg per disjunct: each leg is the base
/// filter plus that disjunct's restriction. A lone (non-"or") relative clause
/// collapses to a single restricted `TypedFilter`.
///
/// Returns the base filter and remainder unchanged when the base is not a single
/// typed filter or no such relative clause follows — every existing call shape
/// is preserved.
pub(crate) fn parse_target_with_disjunctive_restriction(text: &str) -> (TargetFilter, &str) {
    let (base, rest) = parse_target(text);
    let TargetFilter::Typed(base_typed) = &base else {
        return (base, rest);
    };
    // The relative clause is case-insensitive; lowercasing is byte-length
    // preserving for the ASCII relative-clause grammar, so `consumed` maps
    // directly back onto `rest`.
    let rest_lower = rest.to_lowercase();
    let Some((restrictions, consumed)) = parse_disjunctive_relative_restriction(&rest_lower) else {
        return (base, rest);
    };
    let filter = if restrictions.len() == 1 {
        TargetFilter::Typed(restrictions[0].apply(base_typed))
    } else {
        TargetFilter::Or {
            filters: restrictions
                .iter()
                .map(|r| TargetFilter::Typed(r.apply(base_typed)))
                .collect(),
        }
    };
    (filter, &rest[consumed..])
}

/// One disjunct of a heterogeneous relative-clause restriction (see
/// `parse_target_with_disjunctive_restriction`).
#[derive(Debug, Clone)]
enum DisjunctRestriction {
    /// "that's an artifact" — an additional card type AND-merged into the leg's
    /// `type_filters`.
    CardType(TypeFilter),
    /// "that has mana value 3 or less" — a `FilterProp::Cmc` bound AND-merged
    /// into the leg's `properties` (CR 202.3).
    ManaValue {
        comparator: Comparator,
        value: QuantityExpr,
    },
}

impl DisjunctRestriction {
    /// Build a leg by cloning the base typed filter and applying this disjunct's
    /// restriction at its native layer (type vs property).
    fn apply(&self, base: &TypedFilter) -> TypedFilter {
        let mut leg = base.clone();
        match self {
            DisjunctRestriction::CardType(tf) => leg.type_filters.push(tf.clone()),
            DisjunctRestriction::ManaValue { comparator, value } => {
                leg.properties.push(FilterProp::Cmc {
                    comparator: *comparator,
                    value: value.clone(),
                });
            }
        }
        leg
    }
}

/// Parse `that('s|is|has|have) <disjunct> [ or that(...) <disjunct> ]*` from
/// already-lowercased text, returning the disjuncts and the bytes consumed
/// (including any leading whitespace). Returns `None` when the text does not
/// open a recognized relative-clause disjunct.
fn parse_disjunctive_relative_restriction(
    input: &str,
) -> Option<(Vec<DisjunctRestriction>, usize)> {
    let trimmed = input.trim_start();
    let leading_ws = input.len() - trimmed.len();
    let (mut remaining, first) = parse_disjunct_restriction(trimmed).ok()?;
    let mut restrictions = vec![first];
    while let Ok((after_or, _)) = tag::<_, _, OracleError<'_>>(" or ").parse(remaining) {
        match parse_disjunct_restriction(after_or) {
            Ok((rest, next)) => {
                restrictions.push(next);
                remaining = rest;
            }
            // A non-relative-clause "or" (e.g. "or a Goblin you control") ends
            // the disjunction; leave it for the caller to reject as leftover.
            Err(_) => break,
        }
    }
    Some((restrictions, leading_ws + (trimmed.len() - remaining.len())))
}

/// Parse a single "that('s|is|has|have) <card type | mana value bound>" disjunct.
fn parse_disjunct_restriction(
    input: &str,
) -> super::oracle_nom::error::OracleResult<'_, DisjunctRestriction> {
    let (after_intro, _) = alt((
        tag::<_, _, OracleError<'_>>("that's "),
        tag("that is "),
        tag("that has "),
        tag("that have "),
    ))
    .parse(input)?;
    alt((parse_disjunct_card_type, parse_disjunct_mana_value)).parse(after_intro)
}

/// "an artifact" / "a creature" → a card-type restriction.
fn parse_disjunct_card_type(
    input: &str,
) -> super::oracle_nom::error::OracleResult<'_, DisjunctRestriction> {
    let (after_article, _) = alt((tag::<_, _, OracleError<'_>>("an "), tag("a "))).parse(input)?;
    let (rest, tf) = nom_target::parse_type_filter_word(after_article)?;
    Ok((rest, DisjunctRestriction::CardType(tf)))
}

/// "mana value 3 or less" / "mana value 5 or greater" → a `Cmc` bound (CR 202.3).
fn parse_disjunct_mana_value(
    input: &str,
) -> super::oracle_nom::error::OracleResult<'_, DisjunctRestriction> {
    let (after_mv, _) = tag::<_, _, OracleError<'_>>("mana value ").parse(input)?;
    let (after_num, mv) = nom_quantity::parse_quantity_expr_number(after_mv)?;
    let after_num = after_num.trim_start();
    let (rest, comparator) = alt((
        value(Comparator::LE, tag::<_, _, OracleError<'_>>("or less")),
        value(Comparator::GE, tag("or greater")),
    ))
    .parse(after_num)?;
    Ok((
        rest,
        DisjunctRestriction::ManaValue {
            comparator,
            value: mv,
        },
    ))
}

/// Context-aware variant of `parse_target`. TargetFallback diagnostics are
/// accumulated on `ctx.diagnostics` instead of being silently lost.
///
/// Discards the `TargetSyntax` discriminator returned by
/// `parse_target_with_syntax`. Use the latter directly when distinguishing
/// `target`-keyword vs descriptor phrases matters (e.g. Bounce lowering).
pub fn parse_target_with_ctx<'a>(text: &'a str, ctx: &mut ParseContext) -> (TargetFilter, &'a str) {
    let (filter, rest, _syntax) = parse_target_with_syntax(text, ctx);
    (filter, rest)
}

/// CR 701.14a: Parse the object of a `fight` clause. The reciprocal "each other"
/// ("those creatures fight each other" — Malamet Battle Glyph, Longstalk Brawl,
/// Duel for Dominance, and 7 siblings) is NOT an independent target: both
/// fighters are the two earlier-declared chosen creatures. Emit
/// `TargetFilter::ParentTarget` so fight slot-gen (`ability_utils`) creates no
/// spurious target slot — otherwise the unrecognized "each other" falls back to
/// an empty `Typed` filter that generates an illegal all-players slot and panics
/// the cast. Every non-reciprocal object ("~ fights target creature", "it fights
/// target creature") delegates unchanged to `parse_target_with_ctx`, keeping its
/// explicit target slot. Shared by BOTH `fight ` dispatchers
/// (`parse_targeted_action_ast`, `try_parse_verb_and_target`) so neither path
/// silently no-ops.
pub fn parse_fight_target<'a>(text: &'a str, ctx: &mut ParseContext) -> (TargetFilter, &'a str) {
    let lower = text.trim().to_ascii_lowercase();
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("each other").parse(lower.as_str()) {
        // Structural trailing-period cleanup after the `each other` tag matched.
        // allow-noncombinator: punctuation strip on a matched chunk, not dispatch.
        let rest = rest.strip_prefix('.').unwrap_or(rest);
        if rest.trim().is_empty() {
            return (TargetFilter::ParentTarget, "");
        }
    }
    parse_target_with_ctx(text, ctx)
}

/// Context-aware target parser that additionally reports whether the phrase
/// used the "target" keyword (`TargetKeyword`) or a descriptor scope
/// (`Descriptor`). CR 115.1 + Whitemane Lion ruling distinguishes these for
/// `Effect::Bounce` lowering: targeted bounce uses the targeting pipeline,
/// while descriptor bounce ("return a creature you control") selects at
/// resolution via `EffectZoneChoice`.
pub fn parse_target_with_syntax<'a>(
    text: &'a str,
    ctx: &mut ParseContext,
) -> (TargetFilter, &'a str, TargetSyntax) {
    let mut syntax = TargetSyntax::Descriptor;
    let text = text.trim_start();
    let lower = text.to_lowercase();

    // CR 115.1 + CR 701.9b: Trailing " chosen at random" suffix on a noun-phrase
    // target (e.g. Zaffai, Thunder Conductor — "an opponent chosen at random").
    // This is the noun-phrase analogue of the leading "random target X"
    // pattern handled below: instead of `random target opponent`, the random
    // qualifier rides as a postnominal modifier. Strip it, mark the selection
    // mode on `ctx`, and recurse on the prefix so the underlying noun phrase
    // ("an opponent") parses through the normal arms below. Use `TextPair`
    // for the dual-string strip so the original casing is preserved.
    {
        let tp = TextPair::new(text, &lower);
        // Trim trailing punctuation (period/comma) and whitespace before
        // checking the suffix, so " chosen at random." matches.
        let trimmed = tp
            .trim_end()
            .trim_end_matches('.')
            .trim_end_matches(',')
            .trim_end();
        for suffix in [" chosen at random", " at random"] {
            // allow-noncombinator: TextPair::strip_suffix is the dual-string structural API for postnominal qualifier stripping (PATTERNS.md §9).
            if let Some(prefix) = trimmed.strip_suffix(suffix) {
                ctx.target_selection_mode = TargetSelectionMode::Random;
                let (filter, _, _) = parse_target_with_syntax(prefix.original, ctx);
                let filter = use_owner_for_random_non_battlefield_zone(filter);
                // Return empty remainder — the entire input has been consumed
                // (prefix + stripped suffix + any trailing punctuation).
                return (filter, &text[text.len()..], syntax);
            }
        }
    }
    if let Ok((_, (before_random, after_random))) =
        nom_primitives::split_once_on(lower.as_str(), " at random ")
    {
        if alt((
            tag::<_, _, OracleError<'_>>("from "),
            tag("in "),
            tag("on "),
        ))
        .parse(after_random)
        .is_ok()
        {
            ctx.target_selection_mode = TargetSelectionMode::Random;
            let before_original = &text[..before_random.len()];
            let after_original = &text[lower.len() - after_random.len()..];
            let rewritten = format!("{before_original} {after_original}");
            let (filter, _, _) = parse_target_with_syntax(&rewritten, ctx);
            let filter = use_owner_for_random_non_battlefield_zone(filter);
            return (filter, &text[text.len()..], syntax);
        }
    }

    // Strip leading article ("a "/"an ") before "target" to handle "a target creature".
    // Guard: only strip when followed by "target " (controller-choice) or
    // "random target " (random-selection, CR 115.1 + CR 701.9b) to avoid
    // over-stripping. The recursion re-enters parse_target_with_ctx where the
    // bare-"random " arm below sets the selection mode on `ctx`.
    if let Ok((after_article, _)) = alt((
        // CR 115.1: Ordinal targets — "a second", "a third", etc. — surface
        // distinctness over multi-target effects (Cone of Flame, Serpentine
        // Spike). The article is structural; the ordinal is enforced by the
        // multi-target machinery rather than the filter, so they collapse to
        // the same bare-"target" arm as "a "/"an ".
        tag::<_, _, OracleError<'_>>("a second "),
        tag("a third "),
        tag("a fourth "),
        tag("a fifth "),
        tag("a "),
        tag("an "),
    ))
    .parse(lower.as_str())
    {
        if alt((
            tag::<_, _, OracleError<'_>>("target "),
            tag("random target "),
        ))
        .parse(after_article)
        .is_ok()
        {
            let original_rest = &text[lower.len() - after_article.len()..];
            return parse_target_with_syntax(original_rest, ctx);
        }
        // CR 115.1: Bare-trailing "target" with no following type word — the
        // recipient is the multi-target chain's terminal slot ("a third
        // target", Cone of Flame). Recurse on the original-case offset so the
        // bare-target arm below resolves to `TargetFilter::Any`.
        if let Ok((rest_after_target, _)) =
            tag::<_, _, OracleError<'_>>("target").parse(after_article)
        {
            if rest_after_target.is_empty() || rest_after_target.starts_with([',', '.']) {
                let original_rest = &text[lower.len() - after_article.len()..];
                return parse_target_with_syntax(original_rest, ctx);
            }
        }
    }

    // CR 115.1 + CR 701.9b: "random target X" — the game (not the controller) selects
    // the target. Strip the "random " modifier, mark the mode on the parse context,
    // and recurse to parse the underlying target normally. The chunk loop in
    // `parse_effect_chain_ir` snapshots the mode into the produced `ClauseIr`,
    // which lowering then stamps onto the `AbilityDefinition`. The engine reads
    // this field at target-selection time to short-circuit `WaitingFor::TargetSelection`
    // and pick the target uniformly via `state.rng`.
    if let Ok((rest, _)) = (
        tag::<_, _, OracleError<'_>>("random "),
        peek(tag("target ")),
    )
        .parse(lower.as_str())
    {
        ctx.target_selection_mode = TargetSelectionMode::Random;
        let original_rest = &text[lower.len() - rest.len()..];
        return parse_target_with_syntax(original_rest, ctx);
    }

    // Quantified target phrases routed here from callers that only need the filter,
    // not the target-count metadata.
    static QUANTIFIED_PREFIXES: &[&str] = &[
        "any number of ",
        "up to x ",
        "up to one ",
        "up to two ",
        "up to three ",
        "up to four ",
        "up to five ",
        "up to six ",
        "one, two, or three ",
        "a second ",
        "one or two ",
        "one ",
        "two ",
        "three ",
        "four ",
        "five ",
        "six ",
        "x ",
    ];
    for prefix in QUANTIFIED_PREFIXES {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(*prefix).parse(lower.as_str()) {
            let trimmed_rest = rest.trim_start();
            let quantified_target = alt((
                tag::<_, _, OracleError<'_>>("target "),
                tag("other target "),
                tag("another target "),
                tag("other "),
            ))
            .parse(rest)
            .is_ok()
                || starts_with_type_word(trimmed_rest)
                || starts_with_type_phrase_lead(trimmed_rest)
                || parse_combat_status_prefix(trimmed_rest).is_some()
                // Pronoun references after quantity: "any number of them"
                || parse_word_bounded(trimmed_rest, "them").is_ok()
                || parse_word_bounded(trimmed_rest, "it").is_ok()
                || (!matches!(*prefix, "one " | "up to one ") && trimmed_rest.starts_with("of "));
            if quantified_target {
                let original_rest = &text[lower.len() - rest.len()..];
                return parse_target_with_syntax(original_rest, ctx);
            }
        }
    }

    for prefix in ["or untap ", "untap ", "or tap ", "tap "] {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(prefix).parse(lower.as_str()) {
            let original_rest = &text[lower.len() - rest.len()..];
            return parse_target_with_syntax(original_rest, ctx);
        }
    }

    // CR 115.1d: bare bounded-count target arms ("one or two targets", "one,
    // two, or three targets") share the single `BOUNDED_TARGET_CARDINALITIES`
    // authority (composing the plural noun off the stem); the unbounded forms
    // ("any number of targets", bare "targets") stay inline.
    for &(stem, _, _) in crate::parser::oracle_effect::lower::BOUNDED_TARGET_CARDINALITIES {
        if let Ok((rest, _)) =
            (tag::<_, _, OracleError<'_>>(stem), tag(" targets")).parse(lower.as_str())
        {
            return (TargetFilter::Any, &text[lower.len() - rest.len()..], syntax);
        }
    }
    for phrase in ["any number of targets", "targets"] {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(phrase).parse(lower.as_str()) {
            return (TargetFilter::Any, &text[lower.len() - rest.len()..], syntax);
        }
    }

    // CR 608.2c + CR 608.2k: Bare anaphoric object pronouns ("it", "them", "him",
    // "her") refer back to a previously-mentioned object. `resolve_pronoun_target`
    // dispatches on `ctx.subject` to pick the correct antecedent class — see its
    // doc comment for the typed-subject vs. compound-anaphor split.
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| parse_word_bounded(input, "it")) {
        return (resolve_pronoun_target(ctx, "it"), rest, syntax);
    }
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| parse_word_bounded(input, "them")) {
        return (resolve_pronoun_target(ctx, "them"), rest, syntax);
    }
    if tag::<_, _, OracleError<'_>>("one of ")
        .parse(lower.as_str())
        .is_err()
    {
        if let Some((_, rest)) =
            nom_on_lower(text, &lower, |input| parse_word_bounded(input, "one"))
        {
            // "one" is a quantity word, not an object pronoun — preserve the
            // legacy `ParentTarget` binding (multi-target chains).
            return (TargetFilter::ParentTarget, rest, syntax);
        }
    }
    // Gendered object pronouns follow the same trigger-subject vs. compound
    // anaphor dispatch as "it"/"them".
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| parse_word_bounded(input, "him")) {
        return (resolve_pronoun_target(ctx, "him"), rest, syntax);
    }
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| parse_word_bounded(input, "her")) {
        return (resolve_pronoun_target(ctx, "her"), rest, syntax);
    }
    if let Some((filter, rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            |i| parse_cost_paid_object_reference(i, ctx),
            value(
                TargetFilter::TriggeringSource,
                (
                    alt((tag("the discarded card"), tag("that discarded card"))),
                    opt(tag(" from your graveyard")),
                ),
            ),
            value(
                TargetFilter::ParentTargetController,
                tag::<_, _, OracleError<'_>>("that creature's controller"),
            ),
            value(
                TargetFilter::ParentTargetController,
                tag("that permanent's controller"),
            ),
            value(
                TargetFilter::ParentTargetController,
                tag("that land's controller"),
            ),
        ))
        .parse(input)
    }) {
        return (filter, rest, syntax);
    }
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("on ").parse(lower.as_str()) {
        let original_rest = &text[lower.len() - rest.len()..];
        if matches!(
            rest,
            "it" | "them" | "him" | "her" | "enchanted permanent" | "enchanted creature"
        ) {
            return parse_target_with_syntax(original_rest, ctx);
        }
    }
    // CR 608.2k: Bare "that spell" refers to the triggering spell object
    // (Krark, the Thumbless; Spellchain Scatter). "that card" is NOT included —
    // it stays on the ParentTarget arm below and is rewritten to TrackedSet when
    // a prior sibling publishes an affected set (Sin, Spira's Punishment). Predicate
    // continuations ("that spell is countered this way") keep ParentTarget because
    // text remains after the noun; comma continuations ("copy that spell, and …")
    // still name the triggering spell.
    if let Ok((rest_subject, _)) = tag::<_, _, OracleError<'_>>("that ").parse(lower.as_str()) {
        let original_rest = &text[lower.len() - rest_subject.len()..];
        if let Ok((after, _)) = parse_word_bounded(rest_subject, "spell") {
            if after.is_empty() || after.starts_with([',', ';']) {
                let orig_after = original_rest.get("spell".len()..).unwrap_or(original_rest);
                return (TargetFilter::TriggeringSource, orig_after, syntax);
            }
        }
        let (filter, rem) = parse_type_phrase_with_ctx(original_rest, ctx);
        if !matches!(filter, TargetFilter::Any) {
            // CR 601.2c + CR 608.2c: when the chain declared multiple target
            // slots and "that <type>" names exactly one of them (Stolen Uniform's
            // "that Equipment"), bind the precise slot instead of the ambiguous
            // whole-chain `ParentTarget`. Empty/ambiguous registry → falls through
            // to the `ParentTarget` lift below, unchanged.
            if let Some((slot_filter, slot_rest)) =
                parse_definite_parent_reference(lower.as_str(), &ctx.declared_target_slots)
            {
                return (slot_filter, &text[lower.len() - slot_rest.len()..], syntax);
            }
            return (TargetFilter::ParentTarget, rem, syntax);
        }
    }
    // "the first [type phrase]" → anaphoric reference to an object identified
    // by the triggering event. Lifeline-style delayed triggers snapshot this
    // parent target while the event context is still available.
    //
    // CR 608.2c carve-out: "the first player" / "the second player" are
    // cross-clause ordinal player anaphors with distinct semantics (chooser
    // vs. chosen target — see the longest-match anaphor block below). The
    // generic "the first [type phrase] → ParentTarget" lift would clobber
    // both bindings, so let the player-anaphor block handle them. The check
    // is intentionally narrow: "the first card", "the first creature", etc.
    // continue to flow through this generic arm.
    if let Ok((rest_subject, _)) = tag::<_, _, OracleError<'_>>("the first ").parse(lower.as_str())
    {
        let is_player_ordinal_anaphor = tag::<_, _, OracleError<'_>>("player")
            .parse(rest_subject)
            .is_ok_and(|(after, _)| after.is_empty() || after.starts_with([' ', ',', '.']));
        if !is_player_ordinal_anaphor {
            let original_rest = &text[lower.len() - rest_subject.len()..];
            let (filter, rem) = parse_type_phrase_with_ctx(original_rest, ctx);
            if !matches!(filter, TargetFilter::Any) {
                return (TargetFilter::ParentTarget, rem, syntax);
            }
        }
    }

    // CR 201.5: self-references name only the source object. Bare "it" is
    // handled by the anaphoric-pronoun block above, so this primarily covers
    // "~", "itself", and typed self-reference phrases.
    if let Some((filter, rest)) = nom_on_lower(text, &lower, nom_target::parse_self_reference) {
        return (filter, rest, syntax);
    }

    // "any other target" — matches any legal target different from previously chosen targets
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| {
        value((), tag::<_, _, OracleError<'_>>("any other target")).parse(input)
    }) {
        return (
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Another])),
            rest,
            syntax,
        );
    }

    // "any target" — matches any legal target
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| {
        value(
            TargetFilter::Any,
            tag::<_, _, OracleError<'_>>("any target"),
        )
        .parse(input)
    }) {
        return (TargetFilter::Any, rest, syntax);
    }

    // CR 610.3 / CR 406.6: linked exile and counter-marked exile phrases are
    // more specific than the generic "all <type phrase>" parser below.
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("each card exiled with ~"),
        tag("each card exiled with it"),
        tag("all cards exiled with ~"),
        tag("all cards exiled with it"),
        tag("all cards they own exiled with ~"),
        tag("all cards they own exiled with it"),
        tag("card they own exiled with ~"),
        tag("card they own exiled with it"),
        tag("cards they own exiled with ~"),
        tag("cards they own exiled with it"),
        tag("card exiled with ~"),
        tag("card exiled with it"),
        tag("cards exiled with ~"),
        tag("cards exiled with it"),
    ))
    .parse(lower.as_str())
    {
        return (
            TargetFilter::ExiledBySource,
            &text[lower.len() - rest.len()..],
            syntax,
        );
    }

    // "all " + type phrase
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("all ").parse(lower.as_str()) {
        let (filter, rest) = parse_type_phrase_with_ctx(&text[lower.len() - rest.len()..], ctx);
        return (filter, rest, syntax);
    }

    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| parse_word_bounded(input, "player"))
    {
        return (TargetFilter::Player, rest, syntax);
    }

    for zone_word in ["graveyard", "graveyards"] {
        if let Some((_, rest)) =
            nom_on_lower(text, &lower, |input| parse_word_bounded(input, zone_word))
        {
            return (
                TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                    zone: Zone::Graveyard,
                }])),
                rest,
                syntax,
            );
        }
    }

    // CR 201.5: "this creature", "this spell", etc. — self-reference
    for phrase in SELF_REF_TYPE_PHRASES
        .iter()
        .chain(SELF_REF_PARSE_ONLY_PHRASES)
    {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(*phrase).parse(lower.as_str()) {
            return (
                TargetFilter::SelfRef,
                &text[lower.len() - rest.len()..],
                syntax,
            );
        }
    }

    // CR 115.1: Bare "target" with no following type phrase — terminal usage in
    // multi-target damage chains ("3 damage to a third target", Cone of Flame /
    // Serpentine Spike). The recipient is otherwise unspecified; resolves to
    // any legal target. Boundary check ensures we don't swallow "targeted" /
    // "targets" or the leading word of "target creature".
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("target").parse(lower.as_str()) {
        if rest.is_empty() || rest.starts_with([',', '.']) {
            // CR 115.1: "target" keyword consumed — surfaced via the returned
            // `TargetSyntax` for downstream lowering (e.g. Bounce selection).
            syntax = TargetSyntax::TargetKeyword;
            return (TargetFilter::Any, &text[lower.len() - rest.len()..], syntax);
        }
    }

    // "target" group — longest-match-first within
    if let Ok((after_target, _)) = tag::<_, _, OracleError<'_>>("target ").parse(lower.as_str()) {
        // CR 115.1: "target" keyword consumed — surfaced via the returned
        // `TargetSyntax` for downstream lowering (e.g. Bounce selection).
        // Whitemane Lion's "return a creature you control" parses through
        // this path's *absence*, so the returned `Descriptor` lets the
        // lowering pipeline pick the non-targeted variant.
        syntax = TargetSyntax::TargetKeyword;
        let target_offset = lower.len() - after_target.len();
        // "target player or planeswalker"
        if let Ok((rest, _)) =
            tag::<_, _, OracleError<'_>>("player or planeswalker").parse(after_target)
        {
            return (
                TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Player,
                        typed(TypeFilter::Planeswalker, None, vec![], vec![]),
                    ],
                },
                &text[lower.len() - rest.len()..],
                syntax,
            );
        }
        // CR 115.1: "target permanent or player" — the proliferate-style
        // target pool (Skyship Plunderer, Maulfist Revolutionary).
        // Matched before the bare "permanent" type phrase (longest-match-first)
        // so the "or player" half is not dropped.
        if let Ok((rest, _)) =
            tag::<_, _, OracleError<'_>>("permanent or player").parse(after_target)
        {
            return (
                TargetFilter::Or {
                    filters: vec![
                        typed(TypeFilter::Permanent, None, vec![], vec![]),
                        TargetFilter::Player,
                    ],
                },
                &text[lower.len() - rest.len()..],
                syntax,
            );
        }
        // CR 122.1 + CR 702.62b: "target permanent or suspended card" — a
        // battlefield∪exile target pool (Clockspinning). A suspended card is in
        // exile, has suspend, and bears ≥1 time counter. Matched before the bare
        // "permanent" type phrase (longest-match-first) so the "or suspended card"
        // half is not dropped.
        if let Ok((rest, _)) =
            tag::<_, _, OracleError<'_>>("permanent or suspended card").parse(after_target)
        {
            return (
                TargetFilter::Or {
                    filters: vec![
                        // Battlefield permanent. The explicit `InZone{Battlefield}`
                        // is required so `targeting::extract_explicit_zones` unions
                        // Battlefield with Exile across this `Or` (otherwise only
                        // Exile would be searched for legal targets).
                        typed(
                            TypeFilter::Permanent,
                            None,
                            vec![FilterProp::InZone {
                                zone: Zone::Battlefield,
                            }],
                            vec![],
                        ),
                        // CR 702.62b: a suspended card.
                        TargetFilter::Typed(TypedFilter::card().properties(vec![
                            FilterProp::InZone { zone: Zone::Exile },
                            FilterProp::HasKeywordKind {
                                value: KeywordKind::Suspend,
                            },
                            FilterProp::Counters {
                                counters: CounterMatch::OfType(CounterType::Time),
                                comparator: Comparator::GE,
                                count: QuantityExpr::Fixed { value: 1 },
                            },
                        ])),
                    ],
                },
                &text[lower.len() - rest.len()..],
                syntax,
            );
        }
        // "target opponent"
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("opponent").parse(after_target) {
            return (
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                &text[lower.len() - rest.len()..],
                syntax,
            );
        }
        // "target player"
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("player").parse(after_target) {
            return (
                TargetFilter::Player,
                &text[lower.len() - rest.len()..],
                syntax,
            );
        }
        // "target" + type phrase (generic). CR 903.3 + CR 108.3: "commander[s]"
        // is recognized as a typed-phrase prefix inside `parse_type_phrase_with_ctx`
        // — it pushes `IsCommander` and composes uniformly with the existing
        // suffix machinery (ownership, control, counters, "with X", etc.).
        let (filter, rest) = parse_type_phrase_with_ctx(&text[target_offset..], ctx);
        let consumed_end = lower.len() - rest.len();
        return (
            scope_target_spell_phrase(filter, &lower[target_offset..consumed_end]),
            rest,
            syntax,
        );
    }

    // CR 608.2k + CR 509.3d: "the other creature"/"the other permanent" — the
    // single object opposite the trigger's own source in a compound
    // blocks-or-becomes-blocked pairing (Venom's "destroy the other creature",
    // Mammoth Harness). This is a per-firing anaphor resolved at runtime via
    // `blocked_attacker_from_event`, NOT the split-pile "the other" tracked-set
    // reference in TRACKED_SET_PHRASES below — so it MUST be matched first, or
    // the bare "the other" prefix would consume it and bind it to an
    // (unpopulated) tracked set.
    if let Some((filter, rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            value(
                TargetFilter::ParentTarget,
                tag::<_, _, OracleError<'_>>("the other creature"),
            ),
            value(TargetFilter::ParentTarget, tag("the other permanent")),
        ))
        .parse(input)
    }) {
        return (filter, rest, syntax);
    }

    // CR 603.7: Anaphoric tracked-set pronouns
    static TRACKED_SET_PHRASES: &[&str] = &[
        "the chosen cards",
        "the rest",
        "the other",
        "those land cards",
        "those permanent cards",
        "those creature cards",
        "those lands",
        "those tokens",
        "those auras",
        "the revealed cards",
        // CR 603.7 + CR 707.12: "those exiled cards" / "the copies" — the cards a
        // prior clause (or, for Baron Helmut Zemo's Boast, the activation COST)
        // exiled and published into the tracked set. Ordered before "those cards"
        // so the longer "those exiled cards" anaphor is not shadowed.
        "those exiled cards",
        "the copies",
        "those cards",
        "those permanents",
        "those creatures",
        "the exiled cards",
        "the exiled card",
        "the exiled permanents",
        "the exiled permanent",
        "the exiled creature",
        "both creatures",
        // CR 608.2c: "later text on the card may modify the meaning of earlier
        // text" — anaphoric back-reference to objects produced by prior sibling
        // steps in the same resolution (e.g., Sword of Hearth and Home: exiled
        // creature + searched basic land → "Put both cards onto the battlefield
        // under your control").
        "both cards",
    ];
    for phrase in TRACKED_SET_PHRASES {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(*phrase).parse(lower.as_str()) {
            return (
                TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                &text[lower.len() - rest.len()..],
                syntax,
            );
        }
    }

    if let Some(rest) = parse_selected_from_set_reference(lower.as_str()) {
        return (
            TargetFilter::ParentTarget,
            &text[lower.len() - rest.len()..],
            syntax,
        );
    }
    if let Some((filter, rest)) =
        parse_definite_parent_reference(lower.as_str(), &ctx.declared_target_slots)
    {
        return (filter, &text[lower.len() - rest.len()..], syntax);
    }

    // Singular selection from a previously-referenced set.
    static SELECTED_FROM_SET_PHRASES: &[&str] = &[
        "new targets for the copies",
        "new targets for the copy",
        "new targets for that copy",
        "new targets for target spell",
        "new targets for it",
        "a new target for it",
        "up to one of them",
        "either of them",
        "the chosen creatures",
        "the chosen creature",
        "the chosen cards",
        "the chosen card",
        "the chosen players",
        "the chosen player",
        "the chosen permanent",
        "the last chosen card",
        "the revealed card",
        "the token",
        "one of those cards",
        "one of those permanents",
        "one of those creatures",
        "one of the revealed cards",
        "one of them",
    ];
    for phrase in SELECTED_FROM_SET_PHRASES {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(*phrase).parse(lower.as_str()) {
            return (
                TargetFilter::ParentTarget,
                &text[lower.len() - rest.len()..],
                syntax,
            );
        }
    }

    // Set references that appear after an explicit quantity has already been parsed
    // upstream, e.g. "put two of them into your hand".
    static SET_REFERENCE_SUFFIXES: &[&str] = &[
        "of those cards",
        "of those permanents",
        "of those creatures",
        "of the revealed cards",
        "of them",
    ];
    for phrase in SET_REFERENCE_SUFFIXES {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(*phrase).parse(lower.as_str()) {
            return (
                TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                &text[lower.len() - rest.len()..],
                syntax,
            );
        }
    }

    // CR 608.2k / CR 603.7c: "the spell you cast" / bare "the spell" is an
    // untargeted anaphor to the triggering spell object on a cast trigger
    // (Taigam, Master Opportunist: "exile the spell you cast"). It maps to
    // TriggeringSource, mirroring the bare-"that spell" arm above. Disambiguate
    // purely by textual continuation (no ctx.subject gate): an explicit
    // "you cast" continuation, or an empty / comma / semicolon continuation,
    // names the triggering spell. A predicate continuation ("the spell is
    // countered this way") keeps ParentTarget — preserving the
    // "Counter target spell … the spell is countered this way" compound
    // carve-out — by falling through to the bare "the spell" → ParentTarget arm
    // below. Placed before the longest-match anaphor block so this earlier
    // match wins, exactly as the "that spell" arm precedes its fallbacks.
    if let Ok((rest_subject, _)) = tag::<_, _, OracleError<'_>>("the ").parse(lower.as_str()) {
        let original_rest = &text[lower.len() - rest_subject.len()..];
        if let Ok((after, _)) = parse_word_bounded(rest_subject, "spell") {
            let orig_after = original_rest.get("spell".len()..).unwrap_or(original_rest);
            if let Ok((you_cast_after, _)) = tag::<_, _, OracleError<'_>>(" you cast").parse(after)
            {
                let consumed = after.len() - you_cast_after.len();
                let orig_after = orig_after.get(consumed..).unwrap_or(orig_after);
                return (TargetFilter::TriggeringSource, orig_after, syntax);
            }
            if after.is_empty() || after.starts_with([',', ';']) {
                return (TargetFilter::TriggeringSource, orig_after, syntax);
            }
        }
    }
    // CR 608.2c: Definite anaphoric references to previously-mentioned objects/players.
    // Longest-match-first: "the creature's controller" before "the creature".
    if let Some((filter, rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            value(
                TargetFilter::ParentTargetController,
                tag::<_, _, OracleError<'_>>("the creature's controller"),
            ),
            value(
                TargetFilter::ParentTargetController,
                tag("the source's controller"),
            ),
            value(TargetFilter::ParentTargetController, tag("its controller")),
            // CR 108.3 + CR 608.2c: "its owner" / "their owner" — owner of the parent target.
            value(TargetFilter::ParentTargetOwner, tag("its owner")),
            value(TargetFilter::ParentTargetOwner, tag("their owner")),
            // CR 115.1 + CR 608.2c: "the permanent or player" — anaphoric
            // back-reference to the parent target on "any target" effects
            // (Rhystic Lightning's "deals 2 damage to the permanent or
            // player"). Longer phrase before "the player" / "the permanent"
            // for longest-match-first dispatch.
            value(TargetFilter::ParentTarget, tag("the permanent or player")),
            value(TargetFilter::ParentTarget, tag("the permanent")),
            // CR 608.2c: Cross-clause ordinal player anaphors. When a prior
            // sentence binds two distinct players via "<subject> chooses
            // target player ...", later sentences refer to them by ordinal:
            // "the first player" = the subject/chooser (the triggering
            // player for upkeep triggers), "the second player" = the chosen
            // target (the prior `TargetOnly` slot, hence ParentTargetSlot 0).
            // Used by Oath of Mages — "that player chooses target player who
            // has more life ... The first player may have this enchantment
            // deal 1 damage to the second player." Placed before the bare
            // "the player" arm so the longer phrase wins under longest-match.
            value(TargetFilter::TriggeringPlayer, tag("the first player")),
            value(
                TargetFilter::ParentTargetSlot { index: 0 },
                tag("the second player"),
            ),
            // CR 102.1 + CR 103.1: "the player to your right/left" —
            // seating-relative neighbor. Right = previous seat (clockwise turn
            // order proceeds to the left). Placed before the bare "the player"
            // arm so the longer phrase wins under longest-match-first dispatch.
            value(
                TargetFilter::Neighbor {
                    direction: SeatDirection::Right,
                },
                tag("the player to your right"),
            ),
            value(
                TargetFilter::Neighbor {
                    direction: SeatDirection::Left,
                },
                tag("the player to your left"),
            ),
            value(TargetFilter::ParentTarget, tag("the player")),
            value(TargetFilter::ParentTarget, tag("the creature")),
            value(TargetFilter::ParentTarget, tag("the spell")),
            value(TargetFilter::ParentTarget, tag("the land")),
        ))
        .parse(input)
    }) {
        return (filter, rest, syntax);
    }
    // Generic "the [noun]'s controller" — any possessive ending in "'s controller"
    // catches subtypes like "the Wall's controller" and similar.
    if let Ok((after_the, _)) = tag::<_, _, OracleError<'_>>("the ").parse(lower.as_str()) {
        if let Some(pos) = after_the.find("'s controller") {
            let consumed = "the ".len() + pos + "'s controller".len();
            return (
                TargetFilter::ParentTargetController,
                &text[consumed..],
                syntax,
            );
        }
    }
    // "the [type] card" / "the enchanted [type] card" — definite reference to a
    // previously-mentioned typed card. Must come after tracked-set phrases.
    if let Ok((after_the, _)) = tag::<_, _, OracleError<'_>>("the ").parse(lower.as_str()) {
        // "the enchanted card" / "the enchanted instant card"
        let type_start =
            if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("enchanted ").parse(after_the) {
                rest
            } else {
                after_the
            };

        // Check for [type] card pattern: the remaining must start with a type word
        // followed by " card"/"cards", or just be "card"/"cards" directly.
        let has_type_card =
            if let Ok((after_type, _)) = nom_target::parse_type_filter_word(type_start) {
                let after_type = after_type.trim_start();
                parse_card_or_cards_word(after_type).is_ok() || after_type.is_empty()
            } else {
                false
            };

        // Also check bare "card"/"cards" (e.g., "the enchanted card")
        let is_bare_card = parse_card_or_cards_word(type_start).is_ok();

        if has_type_card || is_bare_card {
            // Find end of "card"/"cards"
            let card_start = if is_bare_card {
                type_start
            } else if let Ok((after_type, _)) = nom_target::parse_type_filter_word(type_start) {
                after_type.trim_start()
            } else {
                type_start
            };
            let rest_after_card = parse_card_or_cards_word(card_start)
                .map(|(r, _)| r)
                .unwrap_or(card_start);
            let consumed = lower.len() - rest_after_card.len();
            return (TargetFilter::ParentTarget, &text[consumed..], syntax);
        }
    }
    // "himself" / "herself" — archaic self-reference (e.g., "deals damage to himself")
    if let Ok((rest, _)) =
        alt((tag::<_, _, OracleError<'_>>("himself"), tag("herself"))).parse(lower.as_str())
    {
        return (
            TargetFilter::SelfRef,
            &text[lower.len() - rest.len()..],
            syntax,
        );
    }

    // CR 108.3 + CR 404.1: an opponent's graveyard as a target resolves to a
    // card in that graveyard. This more-specific possessive phrase MUST be tried
    // before the bare opponent-player references below: the un-bounded
    // `tag("an opponent")` arm would otherwise match the "an opponent" prefix of
    // "an opponent's graveyard" and return a bare Opponent-player filter, leaving
    // "'s graveyard" as an unconsumed remainder. The no-"an" sibling
    // ("opponent's graveyard") is unaffected either way (no opponent-player tag
    // matches "opponent's"), so both possessive forms now agree.
    for phrase in ["opponent's graveyard", "an opponent's graveyard"] {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(phrase).parse(lower.as_str()) {
            return (
                TargetFilter::Typed(TypedFilter::card().properties(vec![
                    FilterProp::Owned {
                        controller: ControllerRef::Opponent,
                    },
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ])),
                &text[lower.len() - rest.len()..],
                syntax,
            );
        }
    }

    // CR 115.1 + CR 102.2: Opponent player references — "each opponent",
    // "opponents", and the bare "an opponent" form used by postnominal
    // random-selection patterns (Zaffai — "an opponent chosen at random")
    // and chooser phrases ("an opponent of your choice"). The bare "an
    // opponent" arm must appear here because the leading-article guard
    // above only strips "a "/"an " when followed by a recognized type word,
    // and "opponent" is a player reference rather than a card type.
    if let Some((filter, rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            value(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                tag::<_, _, OracleError<'_>>("each opponent"),
            ),
            value(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                tag("an opponent"),
            ),
            value(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                tag("opponents"),
            ),
        ))
        .parse(input)
    }) {
        return (filter, rest, syntax);
    }

    // CR 610.3 / CR 406.6: "each card exiled with this <type>" is a linked-
    // object reference to cards exiled by this source.
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("each card exiled with ~"),
        tag("each card exiled with it"),
        tag("all cards exiled with ~"),
        tag("all cards exiled with it"),
        tag("all cards they own exiled with ~"),
        tag("all cards they own exiled with it"),
        tag("card they own exiled with ~"),
        tag("card they own exiled with it"),
        tag("cards they own exiled with ~"),
        tag("cards they own exiled with it"),
        tag("card exiled with ~"),
        tag("card exiled with it"),
        tag("cards exiled with ~"),
        tag("cards exiled with it"),
    ))
    .parse(lower.as_str())
    {
        return (
            TargetFilter::ExiledBySource,
            &text[lower.len() - rest.len()..],
            syntax,
        );
    }
    if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("each card exiled with this ").parse(lower.as_str())
    {
        // Skip the type word after "this " to consume "each card exiled with this artifact"
        let after_type = rest.find(' ').map_or("", |i| &rest[i..]);
        return (
            TargetFilter::ExiledBySource,
            &text[text.len() - after_type.len()..],
            syntax,
        );
    }
    if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("card exiled with this ").parse(lower.as_str())
    {
        let after_type = rest.find(' ').map_or("", |i| &rest[i..]);
        return (
            TargetFilter::ExiledBySource,
            &text[text.len() - after_type.len()..],
            syntax,
        );
    }

    // CR 608.2c: "[each] <noun> chosen this way" is an anaphor over the exact set
    // of objects a prior "[each player may] choose …" step selected, NOT a fresh
    // board-wide type filter. Without this arm "destroy each permanent chosen this
    // way" (Druid of Purification) parsed the head noun as `Typed(Permanent)` and
    // dropped "chosen this way", destroying EVERY permanent instead of only the
    // chosen ones (#4780). Recognize an optional "each "/"the " determiner, a head
    // noun, then the "chosen this way" tail → the published `TrackedSet`.
    if let Ok((rest_lower, _)) = (
        opt(alt((
            tag::<_, _, OracleError<'_>>("each "),
            tag::<_, _, OracleError<'_>>("the "),
        ))),
        alt((
            tag::<_, _, OracleError<'_>>("permanents"),
            tag("permanent"),
            tag("creatures"),
            tag("creature"),
            tag("artifacts"),
            tag("artifact"),
            tag("enchantments"),
            tag("enchantment"),
            tag("cards"),
            tag("card"),
        )),
        tag::<_, _, OracleError<'_>>(" chosen this way"),
    )
        .parse(lower.as_str())
    {
        return (
            TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
            &text[lower.len() - rest_lower.len()..],
            syntax,
        );
    }

    // CR 608.2c: "each of those <type>" — anaphoric reference to objects
    // affected by a preceding instruction in the same ability (Urge to Feed:
    // vampires tapped for the optional cost; Zimone-class "revealed this way"
    // uses the bare creatures/permanents/cards arms). A typed tail ("Vampires",
    // "Zombies you control") intersects the tracked set with the type filter;
    // without this arm, "each of those Vampires" fell through to `each ` +
    // `parse_type_phrase("of those Vampires")`, producing an empty TypedFilter
    // that matched every permanent on the battlefield.
    if let Ok((rest_lower, _)) =
        tag::<_, _, OracleError<'_>>("each of those ").parse(lower.as_str())
    {
        let phrase_start = lower.len() - rest_lower.len();
        let phrase = &text[phrase_start..];
        // CR 608.2c: A trailing predicate on a bare-noun anaphor ("each of those
        // creatures that didn't attack this turn", Maddening Imp) must fold into
        // the tracked set as `TrackedSetFiltered{Not(AttackedThisTurn)}` — the
        // frozen "those creatures" population INTERSECTED with the did-not-attack
        // predicate. Parse the whole typed phrase first; if it carries any
        // predicate PROPERTY beyond the head type noun, wrap it. A bare noun
        // ("creatures"/"permanents"/"cards") with no trailing predicate yields
        // only a head `type_filter` and no properties → the plain `TrackedSet`.
        let (filter, remainder) = parse_type_phrase_with_ctx(phrase, ctx);
        if target_filter_carries_predicate_property(&filter) {
            return (
                TargetFilter::TrackedSetFiltered {
                    id: TrackedSetId(0),
                    filter: Box::new(filter),
                    // "each of those <type>" is an anaphor over the affected set
                    // with no verb-specific zone binding.
                    caused_by: None,
                },
                remainder,
                syntax,
            );
        }
        if let Ok((rest_lower, _)) = alt((
            tag::<_, _, OracleError<'_>>("creatures"),
            tag("permanents"),
            tag("cards"),
        ))
        .parse(rest_lower)
        {
            return (
                TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                &text[lower.len() - rest_lower.len()..],
                syntax,
            );
        }
        if target_filter_has_meaningful_content(&filter) {
            return (
                TargetFilter::TrackedSetFiltered {
                    id: TrackedSetId(0),
                    filter: Box::new(filter),
                    caused_by: None,
                },
                remainder,
                syntax,
            );
        }
    }

    // CR 608.2c: "each of them" is a plural-pronoun anaphor that refers back to
    // the parent ability's chosen targets. Centralising the binding here means
    // every sibling effect parser (destroy, exile, bounce, tap, etc.) benefits
    // automatically instead of each site adding its own special case. A word-
    // boundary guard via `parse_word_bounded` excludes "themselves". Resolution
    // delegates to `resolve_pronoun_target` which applies the same trigger-
    // subject vs. compound-anaphor dispatch as the bare "them" pronoun arm above.
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| {
        let (i, ()) = value((), tag::<_, _, OracleError<'_>>("each of ")).parse(input)?;
        parse_word_bounded(i, "them")
    }) {
        return (resolve_pronoun_target(ctx, "them"), rest, syntax);
    }

    // CR 601.2c: "each of <count> target <type>" is an exact-count multi-target
    // distribution (handled upstream by the counter.rs strip), NOT an all-matching
    // "each" filter. For any non-counter effect that reaches here, route the type
    // through "target" parsing rather than the bare "each " path below — which
    // would call `parse_type_phrase_with_ctx("of <count> target <type>")` and
    // degenerate to an all-matching TypedFilter.
    if let Ok((rest_lower, ())) = (|i| {
        let (i, ()) = value((), tag::<_, _, OracleError<'_>>("each of ")).parse(i)?;
        let (i, _count) = parse_multi_target_count_expr(i)?;
        let (i, ()) = value((), space1).parse(i)?;
        let (i, _) = peek(tag::<_, _, OracleError<'_>>("target")).parse(i)?;
        Ok::<_, nom::Err<OracleError<'_>>>((i, ()))
    })(lower.as_str())
    {
        let tail = &text[lower.len() - rest_lower.len()..];
        let (filter, rest) = parse_target_with_ctx(tail, ctx);
        return (filter, rest, syntax);
    }

    // "each " + type phrase
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("each ").parse(lower.as_str()) {
        let (filter, rest) = parse_type_phrase_with_ctx(&text[lower.len() - rest.len()..], ctx);
        return (filter, rest, syntax);
    }

    // "enchanted [type]" / "equipped creature"
    // First check special case: "enchanted permanent's controller" → controller ref
    if let Some((filter, rest)) = nom_on_lower(text, &lower, |input| {
        value(
            TargetFilter::ParentTargetController,
            tag::<_, _, OracleError<'_>>("enchanted permanent's controller"),
        )
        .parse(input)
    }) {
        return (filter, rest, syntax);
    }
    // "enchanted [type phrase]" → parse the type after "enchanted " and add EnchantedBy
    if let Ok((rest_lower, _)) = tag::<_, _, OracleError<'_>>("enchanted ").parse(lower.as_str()) {
        let after_enchanted = &text[lower.len() - rest_lower.len()..];
        let (filter, rest) = parse_type_phrase_with_ctx(after_enchanted, ctx);
        if target_filter_has_meaningful_content(&filter) {
            let enchanted = match filter {
                TargetFilter::Typed(mut tf) => {
                    tf.properties.push(FilterProp::EnchantedBy);
                    TargetFilter::Typed(tf)
                }
                other => other,
            };
            return (enchanted, rest, syntax);
        }
    }
    // "equipped creature" → creature with EquippedBy
    if let Some((filter, rest)) = nom_on_lower(text, &lower, |input| {
        value(
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy])),
            tag::<_, _, OracleError<'_>>("equipped creature"),
        )
        .parse(input)
    }) {
        return (filter, rest, syntax);
    }

    // "exiled cards with [counter] counters on them" — linked only by the
    // counter marker, not by source. Keep the target narrowed to exile plus
    // the counter type instead of falling back to Any.
    if let Ok((rest, counter_type)) = alt((
        (
            tag::<_, _, OracleError<'_>>("exiled cards with "),
            nom_primitives::parse_counter_type_typed,
            tag(" on them"),
        )
            .map(|(_, counter_type, _)| counter_type),
        (
            tag("exiled cards with "),
            take_till1::<_, _, OracleError<'_>>(|c: char| c.is_whitespace()),
            tag(" counters on them"),
        )
            .map(|(_, counter_name, _)| CounterType::Generic(counter_name.to_string())),
    ))
    .parse(lower.as_str())
    {
        return (
            TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::InZone { zone: Zone::Exile },
                FilterProp::Counters {
                    counters: CounterMatch::OfType(counter_type),
                    comparator: Comparator::GE,
                    count: QuantityExpr::Fixed { value: 1 },
                },
            ])),
            &text[lower.len() - rest.len()..],
            syntax,
        );
    }
    if let Ok((rest, _)) =
        tag::<_, _, OracleError<'_>>("cards exiled with this ").parse(lower.as_str())
    {
        let after_type = rest.find(' ').map_or("", |i| &rest[i..]);
        return (
            TargetFilter::ExiledBySource,
            &text[text.len() - after_type.len()..],
            syntax,
        );
    }

    // "you" — the controller (not a targeted player), with word boundary
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| parse_word_bounded(input, "you")) {
        return (TargetFilter::Controller, rest, syntax);
    }

    // "the top/bottom [N] [type] card[s] of [possessive] library/graveyard"
    // Zone position references that appear as targets of exile/mill/reveal effects.
    // Returns a filter with InZone for the referenced zone and controller.
    if let Some((filter, rest)) = parse_zone_position_ref(text, &lower) {
        return (filter, rest, syntax);
    }

    // CR 400.12: Bare possessive zone references ("their graveyard", "your library").
    // Effects targeting a zone act on all cards in that zone.
    // Skip "its owner's" — ControllerRef has no Owner variant; handle when needed.
    if let Some((poss, rest)) = strip_possessive(&lower) {
        if poss != "its owner's" {
            static ZONE_WORDS: &[(&str, Zone)] = &[
                ("graveyard", Zone::Graveyard),
                ("library", Zone::Library),
                ("hand", Zone::Hand),
            ];
            for &(zone_word, zone) in ZONE_WORDS {
                if let Ok((zone_rest, _)) = tag::<_, _, OracleError<'_>>(zone_word).parse(rest) {
                    let consumed = lower.len() - zone_rest.len();
                    // CR 110.1 + CR 108.3: a graveyard/hand/library card is not a
                    // permanent and has no controller — membership is keyed by
                    // owner. CR 109.5: "their" in an each-player iteration binds
                    // to the iterated player (ControllerRef::ScopedPlayer),
                    // distinct from "your" (the controller). Emit FilterProp::Owned,
                    // not a controller match. Other possessives keep the existing
                    // ControllerRef::You behavior (distinct referents resolved
                    // upstream via the subject/target slot).
                    let (controller, properties) = if poss == "their" {
                        (
                            None,
                            vec![
                                FilterProp::Owned {
                                    controller: ControllerRef::ScopedPlayer,
                                },
                                FilterProp::InZone { zone },
                            ],
                        )
                    } else {
                        (Some(ControllerRef::You), vec![FilterProp::InZone { zone }])
                    };
                    return (
                        TargetFilter::Typed(TypedFilter {
                            controller,
                            properties,
                            ..Default::default()
                        }),
                        &text[consumed..],
                        syntax,
                    );
                }
            }
        }
    }

    // Bare type phrase fallback: try parse_type_phrase before giving up.
    // Handles "commander[s] you own / they control" (non-possessive — the
    // possessive form is matched inside the typed-phrase grammar), bare "commander" (Witch's Clinic
    // class), and combinations like "commander creature you control"
    // (Drillworks Mole class). The commander recognition itself lives in
    // `parse_type_phrase_with_ctx` so it composes with the full suffix grammar
    // (ownership, control, counter, "with X", etc.) — CR 903.3 + CR 108.3.
    // Handles "other nonland permanents you own and control" after quantifier stripping.
    let (filter, rest) = parse_type_phrase_with_ctx(text, ctx);
    if target_filter_has_meaningful_content(&filter) {
        let consumed_end = lower.len() - rest.len();
        (
            scope_target_spell_phrase(filter, &lower[..consumed_end]),
            rest,
            syntax,
        )
    } else {
        ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
            context: "parse_target could not classify".into(),
            text: text.trim().into(),
            line_index: 0,
        });
        (TargetFilter::Any, text, syntax)
    }
}

fn use_owner_for_random_non_battlefield_zone(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed)
            if typed.controller == Some(ControllerRef::You)
                && typed.properties.iter().any(|prop| {
                    matches!(prop, FilterProp::InZone { zone } if *zone != Zone::Battlefield)
                })
                && !typed
                    .properties
                    .iter()
                    .any(|prop| matches!(prop, FilterProp::Owned { .. })) =>
        {
            typed.controller = None;
            typed.properties.push(FilterProp::Owned {
                controller: ControllerRef::You,
            });
            TargetFilter::Typed(typed)
        }
        other => other,
    }
}

fn parse_selected_from_set_reference(input: &str) -> Option<&str> {
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>("a different "))
        .parse(input)
        .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("one of those ")
        .parse(rest)
        .ok()?;
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("artifact cards"),
        tag::<_, _, OracleError<'_>>("cards"),
        tag::<_, _, OracleError<'_>>("creatures"),
        tag::<_, _, OracleError<'_>>("dragons"),
        tag::<_, _, OracleError<'_>>("lands"),
        tag::<_, _, OracleError<'_>>("permanents"),
    ))
    .parse(rest)
    .ok()?;
    let (rest, _) = opt(nom::sequence::preceded(
        tag::<_, _, OracleError<'_>>(" of "),
        alt((
            tag::<_, _, OracleError<'_>>("their choice"),
            tag::<_, _, OracleError<'_>>("his or her choice"),
            tag::<_, _, OracleError<'_>>("that player's choice"),
        )),
    ))
    .parse(rest)
    .ok()?;
    Some(rest)
}

/// CR 601.2c + CR 608.2c: Resolve a definite anaphor ("the artifact", "the
/// artifact card", "that Equipment", "the chosen creature") to the specific
/// `ParentTargetSlot { index }` it names, by matching the anaphor's noun phrase
/// (type/subtype token + optional "card" zone qualifier) against the chain's
/// declared target slots (`ctx.declared_target_slots`).
///
/// Registry-driven: Goblin Welder's two-artifact disambiguation ("the artifact"
/// = the battlefield slot, "the artifact card" = the graveyard slot) is
/// reproduced from the slot filters' own zone properties, not a hardcoded
/// artifact special case. Returns `None` — falling through to the broad
/// `ParentTarget`/set-selection behavior — when the registry is empty
/// (single-target spell) or the anaphor matches zero or ≥2 slots (ambiguous),
/// so no anaphor is ever bound to a specific slot on a guess.
///
/// `input` is lowercase; the returned remainder is a slice of `input`.
pub(super) fn parse_definite_parent_reference<'a>(
    input: &'a str,
    slots: &[TargetFilter],
) -> Option<(TargetFilter, &'a str)> {
    if slots.is_empty() {
        return None;
    }
    // A definite determiner is REQUIRED — a bare type word ("creature") is a
    // fresh target, not a back-reference. Longest-match-first: "the chosen "
    // before "the ".
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("the chosen "),
        tag("the "),
        tag("that "),
    ))
    .parse(input)
    .ok()?;
    let (after_type_word, anaphor_type) = nom_target::parse_type_filter_word(rest).ok()?;
    // Optional trailing "card"/"cards" zone qualifier (Goblin Welder's "the
    // artifact card"). When present, the anaphor names a non-battlefield
    // (card-zone) slot.
    let (rest, is_card) = match parse_card_or_cards_word(after_type_word.trim_start()) {
        Ok((r, _)) => (r, true),
        Err(_) => (after_type_word, false),
    };
    // A possessive continuation ("the creature's controller") is a distinct
    // anaphor class (controller/owner of the slot), not a bare slot reference —
    // refuse it so the possessive arms downstream keep their bindings.
    if tag::<_, _, OracleError<'_>>("'").parse(rest).is_ok() {
        return None;
    }
    // Guard against consuming the head of a COMPOUND type phrase ("the artifact
    // creature") as an anaphor: if the remainder begins with another type word,
    // this is a fresh typed filter, not a slot back-reference.
    let tail = rest.trim_start();
    if !tail.is_empty() && nom_target::parse_type_filter_word(tail).is_ok() {
        return None;
    }
    // CR 601.2c: each anaphor names exactly one earlier slot — bind only a
    // UNIQUE match; zero or ≥2 matches fall through as `None`.
    let mut matched: Option<usize> = None;
    for (index, slot) in slots.iter().enumerate() {
        if slot_matches_anaphor(&anaphor_type, is_card, slot) {
            if matched.is_some() {
                return None;
            }
            matched = Some(index);
        }
    }
    matched.map(|index| (TargetFilter::ParentTargetSlot { index }, rest))
}

/// CR 205.3 + CR 400.1: Whether a declared target slot filter matches a definite
/// anaphor's parsed `(type token, is-card)`. Type match is by core-type
/// membership or subtype equality; the card qualifier requires the slot to be
/// (`is_card`) or not be (`!is_card`) in a non-battlefield card zone.
fn slot_matches_anaphor(anaphor_type: &TypeFilter, is_card: bool, slot: &TargetFilter) -> bool {
    let TargetFilter::Typed(tf) = slot else {
        return false;
    };
    let type_ok = match anaphor_type {
        TypeFilter::Subtype(sub) => tf
            .get_subtype()
            .is_some_and(|slot_sub| slot_sub.eq_ignore_ascii_case(sub)),
        other => tf.type_filters.iter().any(|t| t == other),
    };
    if !type_ok {
        return false;
    }
    // A "card" lives in a non-battlefield zone (Goblin Welder's graveyard slot);
    // a battlefield permanent carries no such zone property.
    let slot_is_card = slot
        .extract_in_zone()
        .is_some_and(|zone| zone != Zone::Battlefield);
    slot_is_card == is_card
}

/// CR 201.2: Match a clause boundary that ends a card name in a board-filter
/// "X named <CardName> …" phrase, scanned at word boundaries (most arms begin
/// with a space; the comma arm begins with ","). A bare comma or " and " is NOT
/// a terminator on its own — card names embed both ("Bruna, the Fading Light";
/// "Gisa and Geralf") — so the name is never split on internal punctuation. The
/// name ends only at a *clause-joining* connective: the controller suffix
/// ("… you control"), a relative pronoun ("… that has flying"), the predicate
/// verb that opens the enclosing relative clause ("… draws a card", "… loses 3
/// life"), or a comma that introduces a *referential* clause about the named
/// object ("…, it gains", "…, they draw"). The comma arm is pronoun-guarded:
/// a legendary epithet after a comma is a noun phrase ("…, the Fading Light"),
/// never a bare referential pronoun, so comma-bearing names stay whole while
/// "Falkenrath Gorger, it gains" still terminates at "Falkenrath Gorger". This
/// mirrors `oracle_effect::search::parse_name_terminator` (the search-zone
/// analogue) but covers the board-filter predicate verbs rather than search
/// follow-up actions.
///
/// The verb arms are third-person singular/plural present forms because the
/// enclosing subject is a singular "permanent/creature named X" or the
/// per-player iteration of "each player who controls a permanent named X"
/// (issue #2016, Bonder's Ornament). They are kept as a single composable
/// `alt()` over the predicate lead so the boundary covers the class, not one
/// card.
fn parse_named_filter_origin_zone_terminator(
    input: &str,
) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    tag::<_, _, OracleError<'_>>(" from ").parse(input)?;
    let Some((_, _, consumed)) = parse_zone_suffix(input) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };
    Ok((&input[consumed..], ()))
}

/// CR 201.2 + CR 400.1: A locative zone constraint ("in your graveyard", "in
/// all graveyards", "on the battlefield") scopes *where the named objects are
/// counted/filtered*; it is outside the literal card name. Unlike the
/// "from <zone>" move-origin suffix (`parse_named_filter_origin_zone_terminator`,
/// which the caller consumes as a move source and leaves in the remainder), an
/// "in"/"on" locative both terminates the name and is re-attached as an
/// `InZone`/`InAnyZone` filter prop by the caller. Requires a real zone after
/// the "in"/"on" preposition so a name-internal "in"/"on" stays whole. Covers
/// the "cards named X in your graveyard / in all graveyards" count class
/// (Frantic Inventory, Accumulated Knowledge, Take Inventory, Undead Servant,
/// Goblin Gathering, Galvanic Bombardment, Ancestral Anger) and "creatures
/// named X on the battlefield" (Plague Rats).
fn parse_named_filter_locative_zone_terminator(
    input: &str,
) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    alt((tag::<_, _, OracleError<'_>>(" in "), tag(" on "))).parse(input)?;
    let Some((_, _, consumed)) = parse_zone_suffix(input) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };
    Ok((&input[consumed..], ()))
}

fn parse_named_filter_terminator(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    alt((
        // Controller-scope suffixes (CR 109.4). Longest-match-first.
        value((), tag(" you don't control")),
        value((), tag(" you control")),
        value((), tag(" you own")),
        value((), tag(" an opponent controls")),
        value((), tag(" your opponents control")),
        // Relative-pronoun clause leads (CR 201.2 descriptive clauses).
        value((), tag(" that ")),
        value((), tag(" with ")),
        value((), tag(" without ")),
        // CR 201.2 + CR 400.1: origin-zone suffixes are outside the literal
        // card name ("card named X from your graveyard"). Require a real zone
        // suffix after "from" so names like "Extract from Darkness" stay whole.
        parse_named_filter_origin_zone_terminator,
        // CR 201.2 + CR 400.1: locative "in <zone>"/"on the battlefield" count
        // scope ("cards named X in your graveyard") — reattached as InZone by
        // the caller. Requires a real zone so name-internal "in"/"on" is kept.
        parse_named_filter_locative_zone_terminator,
        // Copular / state predicates opening a relative clause.
        value((), tag(" is ")),
        value((), tag(" are ")),
        value((), tag(" has ")),
        value((), tag(" have ")),
        // Per-player / per-permanent action predicates (issue #2016 class:
        // "… draws a card", "… loses N life", "… sacrifices a permanent").
        // Excludes conjugated verbs that occur verbatim inside real card
        // names — matching them would truncate the name: "gains" (Ill-Gotten
        // Gains), "gets" (Bird Gets the Worm), "deals" (Orzhova, the Church of
        // Deals). Plural/modal board-filter predicates ("get", "can't") are
        // split upstream by the static parser before this terminator sees them.
        value(
            (),
            (
                tag(" "),
                alt((
                    tag("draws "),
                    tag("loses "),
                    tag("sacrifices "),
                    tag("discards "),
                    tag("creates "),
                    tag("mills "),
                    tag("destroys "),
                    tag("exiles "),
                    tag("puts "),
                    tag("reveals "),
                    tag("searches "),
                )),
            ),
        ),
        // CR 201.2: A comma that opens a referential clause about the named
        // object ("Falkenrath Gorger, it gains"). Pronoun-guarded so a
        // name-internal comma followed by an epithet noun phrase ("Bruna, the
        // Fading Light") is preserved — legendary epithets never begin with a
        // bare referential pronoun.
        value(
            (),
            (
                tag(", "),
                alt((
                    tag("~ "),
                    tag("it "),
                    tag("they "),
                    tag("he "),
                    tag("she "),
                    tag("you "),
                    tag("its "),
                )),
            ),
        ),
    ))
    .parse(input)
}

/// Parse a type phrase like "creature", "nonland permanent", "artifact or enchantment",
/// "creature you control", "creature an opponent controls".
///
/// Prefer `parse_type_phrase_with_ctx` when a `ParseContext` is available —
/// it enables relative-player scope resolution for "that player controls".
pub fn parse_type_phrase(text: &str) -> (TargetFilter, &str) {
    parse_type_phrase_with_ctx(text, &mut ParseContext::default())
}

/// CR 608.2c: separator byte length for a mass-target union continuation
/// ("…, all artifacts, and all enchantments"). Longest-match-first over the
/// comma / "and" / "or" connectors. Returns `None` when `lower` does not start
/// with a union separator.
fn match_mass_union_separator(lower: &str) -> Option<usize> {
    alt((
        tag::<_, _, OracleError<'_>>(", and/or "),
        tag(", and "),
        tag(", or "),
        tag(", "),
        tag(" and/or "),
        tag(" and "),
        tag(" or "),
    ))
    .parse(lower)
    .ok()
    .map(|(rest, _)| lower.len() - rest.len())
}

/// CR 205.2a + CR 205.3a + CR 608.2c: Parse a mass target as a comma/"and"-separated
/// union of "[all|each] <type-phrase>" legs — where each leg's type word spans both
/// card types (205.2a: creature/artifact/enchantment) and subtypes (205.3a) — e.g.
/// "creatures except those that share a
/// creature type with a creature that convoked this spell, all artifacts, and
/// all enchantments" (Everything Comes to Dust). Each leg is parsed by the full
/// `parse_target_with_ctx` grammar (type words, relative clauses, the
/// "except those" exclusion suffix, and spell-target stack scoping) and the legs
/// are combined with `merge_or_filters`.
///
/// A single-leg input returns exactly what `parse_target_with_ctx` returns, so
/// every existing `exile all <type>` card is unchanged — the loop only fires on a
/// repeated-`all`/`each` continuation, which the base grammar's early type-union
/// (`starts_with_or_article_type_segment` rejects a leading "all") deliberately
/// stops at.
pub(crate) fn parse_mass_type_union<'a>(
    text: &'a str,
    ctx: &mut ParseContext,
) -> (TargetFilter, &'a str) {
    let (mut acc, mut rest) = parse_target_with_ctx(text, ctx);
    loop {
        let lower = rest.to_lowercase();
        let Some(sep_len) = match_mass_union_separator(&lower) else {
            break;
        };
        let after_sep = &rest[sep_len..];
        let after_sep_lower = after_sep.to_lowercase();
        // Optional repeated "all "/"each " pluralizer the early union does not fold.
        let plural_len = alt((tag::<_, _, OracleError<'_>>("all "), tag("each ")))
            .parse(after_sep_lower.as_str())
            .map(|(r, _)| after_sep_lower.len() - r.len())
            .unwrap_or(0);
        let leg_text = &after_sep[plural_len..];
        if !starts_with_type_word(&leg_text.to_lowercase()) {
            break;
        }
        let (leg, next) = parse_target_with_ctx(leg_text, ctx);
        acc = merge_or_filters(acc, leg);
        rest = next;
    }
    (acc, rest)
}

/// Context-aware variant of `parse_type_phrase`. Enables relative-player scope
/// resolution via `ctx.relative_player_scope`.
pub fn parse_type_phrase_with_ctx<'a>(
    text: &'a str,
    ctx: &mut ParseContext,
) -> (TargetFilter, &'a str) {
    let lower = text.to_lowercase();
    let mut pos = 0;
    let mut properties = Vec::new();
    let mut property_disjunction_ranges: Vec<(usize, usize)> = Vec::new();
    let lower_trimmed = lower.trim_start();
    let offset = lower.len() - lower_trimmed.len();
    pos += offset;

    // Strip leading article ("a "/"an ") when followed by a recognized type word
    // or the "commander" class. Guard: "an opponent" → "opponent" fails type word
    // check → no stripping. CR 903.3: "commander" is recognized by the commander
    // atom below (it pushes `IsCommander`), not by `starts_with_type_phrase_lead`,
    // so the article guard must also accept it — otherwise "a commander you own"
    // (Hellkite Courser, #5256) keeps its article and never reaches the atom,
    // collapsing to a match-anything filter. "commander you own" / "target
    // commander" already work; this makes the indefinite article compose too.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("a ").parse(&lower[pos..]) {
        if starts_with_type_phrase_lead(rest) || starts_with_commander_word(rest) {
            pos += "a ".len();
        }
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("an ").parse(&lower[pos..]) {
        if starts_with_type_phrase_lead(rest) || starts_with_commander_word(rest) {
            pos += "an ".len();
        }
    }

    // CR 109.2: A description that includes a card type or subtype means
    // permanents of that type/subtype on the battlefield. A leading universal
    // quantifier — "all", "each", or "every" — ranges over every such object,
    // source included, so it is a semantic no-op on the filter and adds NO
    // FilterProp::Another (unlike "other"/"another" below, which exclude the
    // source). Strip it so a subject like "Each Vehicle you control" / "All Cats
    // you control" reaches the type word instead of leaking the quantifier into
    // the subtype string (e.g. Subtype("Each Vehicle")). Guarded on a following
    // type-phrase lead so a bare quantifier without a type word is left intact.
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("all "),
        tag("each "),
        tag("every "),
    ))
    .parse(&lower[pos..])
    {
        // Strip the quantifier when a type-phrase lead follows directly OR through
        // an "other"/"another" exclusion ("each other creature", "all other
        // nonland permanents"); in the latter case only the quantifier is consumed
        // here and the handler below adds `FilterProp::Another`.
        let after_other = alt((tag::<_, _, OracleError<'_>>("other "), tag("another ")))
            .parse(rest)
            .map(|(r, _)| r)
            .ok();
        if starts_with_type_phrase_lead(rest)
            || after_other.is_some_and(starts_with_type_phrase_lead)
        {
            pos = lower.len() - rest.len();
        }
    }

    // Handle "other"/"another" prefix: "other creatures", "another creature",
    // "other nonland permanents", "another target creature". Reads from the
    // current `pos` (not the raw trimmed head) so it composes with a universal
    // quantifier already stripped above ("all other creatures" → Another + type).
    if tag::<_, _, OracleError<'_>>("other ")
        .parse(&lower[pos..])
        .is_ok()
    {
        properties.push(FilterProp::Another);
        pos += "other ".len();
    } else if tag::<_, _, OracleError<'_>>("another ")
        .parse(&lower[pos..])
        .is_ok()
    {
        properties.push(FilterProp::Another);
        pos += "another ".len();
    }
    // "another target [type]" — strip "target " after "another " so the type is reachable.
    if properties.contains(&FilterProp::Another) {
        if let Ok((_, _)) = tag::<_, _, OracleError<'_>>("target ").parse(&lower[pos..]) {
            pos += "target ".len();
        }
    }

    // CR 509.1h: Consume combat status prefixes (unblocked, attacking, blocking).
    // Handles "or" compound as a property disjunction: "attacking or blocking
    // creature" means attacking creature OR blocking creature, not both.
    while let Some((prop, consumed)) = parse_combat_status_prefix(&lower[pos..]) {
        let disjunction_start = properties.len();
        properties.push(prop);
        pos += consumed;
        // Check for "or " followed by another combat status prefix
        if let Ok((after_or, _)) = tag::<_, _, OracleError<'_>>("or ").parse(&lower[pos..]) {
            if let Some((next_prop, next_consumed)) = parse_combat_status_prefix(after_or) {
                properties.push(next_prop);
                property_disjunction_ranges.push((disjunction_start, 2));
                pos += "or ".len() + next_consumed;
            }
        }
    }

    // CR 205.4a: Parse supertype prefix: "legendary", "basic", "snow"
    // Must come BEFORE color prefix so "legendary white creature" works:
    // supertype consumed first, then color at the new position.
    if let Ok((rest, supertype)) = nom_target::parse_supertype_prefix(&lower[pos..]) {
        properties.push(FilterProp::HasSupertype { value: supertype });
        pos += lower[pos..].len() - rest.len();
    }

    // CR 303.4 + CR 301.5: "enchanted" / "equipped" attachment adjective prefix.
    // Attach the property; runtime evaluation degrades "EnchantedBy" to
    // "has any Aura attached" when the trigger source itself is not the Aura
    // (Hateful Eidolon). Source-relative sources (Auras, Equipment) retain the
    // CR 702.5a semantics via the same FilterProp.
    if let Ok((rest, prop)) = alt((
        value(
            FilterProp::EnchantedBy,
            tag::<_, _, OracleError<'_>>("enchanted "),
        ),
        value(
            FilterProp::EquippedBy,
            tag::<_, _, OracleError<'_>>("equipped "),
        ),
    ))
    .parse(&lower[pos..])
    {
        // Only consume if a type word follows (so "enchanted forest" also works,
        // as does "enchanted creature", but bare "enchanted" alone does not).
        if starts_with_type_phrase_lead(rest) {
            properties.push(prop);
            pos += lower[pos..].len() - rest.len();
        }
    }

    // CR 700.4 + CR 700.9: "modified" adjective prefix. A permanent is modified
    // if it has counters on it, is equipped, or is enchanted by an Aura its
    // controller controls. Emits FilterProp::Modified (a first-class typed
    // predicate — see `FilterProp::Modified` in types/ability.rs). Mirrors the
    // "enchanted " / "equipped " adjective handling above: only consume when a
    // type word follows, so bare "modified" alone doesn't hijack other
    // contexts.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("modified ").parse(&lower[pos..]) {
        if starts_with_type_phrase_lead(rest) {
            properties.push(FilterProp::Modified);
            pos += lower[pos..].len() - rest.len();
        }
    }

    // CR 702.112b: "renowned" is a permanent designation used as an adjective
    // in filters like "renowned creature you control".
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("renowned ").parse(&lower[pos..]) {
        if starts_with_type_phrase_lead(rest) {
            properties.push(FilterProp::Renowned);
            pos += lower[pos..].len() - rest.len();
        }
    }

    // CR 700.6: "historic" adjective prefix. An object is historic if it has
    // the legendary supertype, the artifact card type, or the Saga subtype.
    // Emits FilterProp::Historic (a first-class typed predicate — see
    // `FilterProp::Historic` in types/ability.rs). Mirrors the "modified"
    // adjective handling above: only consume when a type word follows, so
    // bare "historic" alone doesn't hijack other contexts.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("historic ").parse(&lower[pos..]) {
        if starts_with_type_phrase_lead(rest) {
            properties.push(FilterProp::Historic);
            pos += lower[pos..].len() - rest.len();
        }
    }

    // CR 903.3 + CR 109.5: "your commander" is owner-scoped, not merely
    // controller-scoped. Consume only the possessive determiner here; the
    // commander atom below still supplies `IsCommander` and leaves suffix
    // parsing centralized for zones, counters, and control clauses.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("your ").parse(&lower[pos..]) {
        if alt((tag::<_, _, OracleError<'_>>("commanders"), tag("commander")))
            .parse(rest)
            .is_ok()
        {
            properties.push(FilterProp::Owned {
                controller: ControllerRef::You,
            });
            pos += "your ".len();
        }
    }

    // CR 903.3 + CR 108.3: "commander[s]" is a class identified by the
    // `IsCommander` flag, not by a card type or subtype. Treat the bare word
    // as a typed-phrase atom so the subsequent grammar (ownership/control
    // suffix, counter suffix, "with X", combinator separators) composes
    // uniformly. Three shapes:
    //   - bare "commander" / "commanders" (Witch's Clinic, Sanctum of Eternity)
    //   - "commander[s] <suffix>" (you own / they control / target player controls)
    //   - "commander <type-word>" (Drillworks Mole: "commander creature you control")
    // For the first two, no type word follows — the prefix sets `IsCommander`
    // and downstream suffix machinery does the rest. For the third, advance
    // past "commander " and let the normal color/subtype/core-type loop
    // consume the trailing type word.
    if let Ok((after_commander_word, _)) = alt((
        tag::<_, _, OracleError<'_>>("commanders "),
        tag("commander "),
    ))
    .parse(&lower[pos..])
    {
        properties.push(FilterProp::IsCommander);
        pos += lower[pos..].len() - after_commander_word.len();
    } else if let Ok((after_commander_word, _)) =
        alt((tag::<_, _, OracleError<'_>>("commanders"), tag("commander"))).parse(&lower[pos..])
    {
        // Bare end-of-phrase "commander" with no trailing space (e.g.,
        // "target commander." or "target commander").
        if after_commander_word.is_empty() || after_commander_word.starts_with([',', '.']) {
            properties.push(FilterProp::IsCommander);
            pos += lower[pos..].len() - after_commander_word.len();
        }
    }

    // CR 208.1 (#2912): a leading "N/M" power/toughness designation ("a 1/1
    // creature", "two 2/2 creatures") constrains the object's current power and
    // toughness — it is NOT a subtype. Emit a `PtComparison` for each side and
    // let the trailing type word ("creature") parse normally; previously the
    // whole "1/1 creature" fused into `Subtype("1/1 Creature")`, so e.g. Sword
    // of the Meek never matched 1/1 tokens.
    if let Some((power, toughness, consumed)) = parse_leading_pt_designation(&lower[pos..]) {
        properties.push(FilterProp::PtComparison {
            stat: PtStat::Power,
            scope: PtValueScope::Current,
            comparator: Comparator::EQ,
            value: QuantityExpr::Fixed { value: power },
        });
        properties.push(FilterProp::PtComparison {
            stat: PtStat::Toughness,
            scope: PtValueScope::Current,
            comparator: Comparator::EQ,
            value: QuantityExpr::Fixed { value: toughness },
        });
        pos += consumed;
    }

    // CR 105.1 + CR 105.2: Handle color adjective prefixes:
    // "white creature", "red spell", "colorless creature", "multicolored card", etc.
    let color_prop =
        parse_color_prefix(&lower[pos..]).or_else(|| parse_color_quality_prefix(&lower[pos..]));
    if let Some((ref prop, color_len)) = color_prop {
        properties.push(prop.clone());
        pos += color_len;
    }

    // CR 205.4b: Parse one or more comma-separated negation prefixes.
    // "noncreature, nonland permanent" → [Non(Creature), Non(Land)] in type_filters
    // "nonartifact, nonblack creature" → Non(Artifact) in type_filters, NotColor("Black") in properties
    //
    // parse_non_prefix uses whitespace as word boundary, but in stacked negation the
    // separator is ", " (comma-space). We must strip the trailing comma from the negated
    // word when the ", non" continuation pattern follows.
    let mut neg_type_filters: Vec<TypeFilter> = Vec::new();
    loop {
        let remaining = &lower[pos..];
        let Ok((after_non, _)) = tag::<_, _, OracleError<'_>>("non").parse(remaining) else {
            break;
        };
        // Optional hyphen: "non-" or "non"
        let after_non = match tag::<_, _, OracleError<'_>>("-").parse(after_non) {
            Ok((r, _)) => r,
            Err(_) => after_non,
        };
        let prefix_len = remaining.len() - after_non.len(); // "non" or "non-"

        // Find the negated word: ends at comma or whitespace
        let end = after_non
            .find(|c: char| c.is_whitespace() || c == ',')
            .unwrap_or(after_non.len());
        if end == 0 {
            break;
        }
        let negated = &after_non[..end];
        match classify_negation(negated) {
            NegationResult::Type(tf) => neg_type_filters.push(tf),
            NegationResult::Prop(prop) => properties.push(prop),
        }
        pos += prefix_len + end;

        // Check for ", non" continuation (stacked negation)
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(", ").parse(&lower[pos..]) {
            if tag::<_, _, OracleError<'_>>("non").parse(rest).is_ok() {
                pos += ", ".len();
                continue;
            }
        }
        // Consume trailing whitespace after the negated word
        if pos < lower.len() && lower.as_bytes()[pos] == b' ' {
            pos += 1;
        }
        break;
    }

    // CR 205.4a: A supertype adjective can also appear AFTER a `non-`
    // token-identity/type negation prefix (e.g. "nontoken legendary permanent"
    // in Cadric, Soul Kindler / issue #3677 class). The pre-negation arm above
    // only fires when the supertype word leads the phrase, so a leading
    // "nontoken " left it unparsed, dropping the legendary restriction entirely.
    // Mirrors the post-negation `historic` re-check directly below.
    if let Ok((rest, supertype)) = nom_target::parse_supertype_prefix(&lower[pos..]) {
        if !properties
            .iter()
            .any(|p| matches!(p, FilterProp::HasSupertype { .. }))
        {
            properties.push(FilterProp::HasSupertype { value: supertype });
            pos += lower[pos..].len() - rest.len();
        }
    }

    // CR 105.1 + CR 105.2: A color adjective can also appear AFTER a `non-`
    // token-identity/type negation prefix (e.g. "nontoken blue creature",
    // Flare of Denial / issue #3677). The pre-negation arm above only fires
    // when the color word leads the phrase, so a leading "nontoken " left the
    // color word — and therefore the entire creature-type filter behind it —
    // unparsed, silently degrading the cost to "sacrifice a nontoken
    // permanent" (which a land never is, so any permanent paid the alt cost).
    // Mirrors the post-negation `historic` re-check directly below.
    if color_prop.is_none() {
        if let Some((prop, color_len)) =
            parse_color_prefix(&lower[pos..]).or_else(|| parse_color_quality_prefix(&lower[pos..]))
        {
            properties.push(prop);
            pos += color_len;
        }
    }

    // CR 700.4 + CR 700.9: A "modified" adjective can also appear AFTER a
    // `non-` token-identity/type negation prefix (e.g. "nontoken modified
    // creature" in Akki Ember-Keeper / issue #3677 class). The pre-negation
    // arm above only fires when "modified" leads the phrase. Mirrors the
    // post-negation `historic` re-check directly below.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("modified ").parse(&lower[pos..]) {
        if starts_with_type_phrase_lead(rest) && !properties.contains(&FilterProp::Modified) {
            properties.push(FilterProp::Modified);
            pos += lower[pos..].len() - rest.len();
        }
    }

    let mut adjective_type_filters: Vec<TypeFilter> = Vec::new();

    // CR 700.6: "historic" adjective prefix can appear AFTER negation prefixes
    // (e.g. "nontoken historic permanent" in Arbaaz Mir). The pre-negation arm
    // above handles the bare-prefix case ("historic permanent"); this arm
    // handles the post-negation case so the adjective composes with `non`
    // negation. Mirrors the structural reasoning that produced
    // `is_adjective_prefix_prop` — the predicate is leg-local but its position
    // in surface text varies relative to negation.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("historic ").parse(&lower[pos..]) {
        if starts_with_type_phrase_lead(rest) && !properties.contains(&FilterProp::Historic) {
            properties.push(FilterProp::Historic);
            pos += lower[pos..].len() - rest.len();
        }
    }

    // CR 700.12: "outlaw creature[s]" uses the outlaw subtype disjunction as
    // an adjective before the concrete Creature type.
    if let Ok((rest, type_filter)) = nom_target::parse_type_filter_word(&lower[pos..]) {
        if matches!(type_filter, TypeFilter::AnyOf(_)) {
            let rest_trimmed = rest.trim_start();
            let ws = rest.len() - rest_trimmed.len();
            if ws > 0 && starts_with_type_phrase_lead(rest_trimmed) {
                adjective_type_filters.push(type_filter);
                pos += lower[pos..].len() - rest_trimmed.len();
            }
        }
    }

    // Parse the core type, falling back to subtype recognition
    let (card_type, subtype, type_len) = parse_core_type(&lower[pos..]);
    pos += type_len;

    // If no core type was found, try subtype recognition as fallback.
    // "Zombies you control" → subtype="Zombie", no card_type.
    let subtype = if card_type.is_none() && subtype.is_none() {
        if let Some((sub_name, sub_len)) = parse_subtype(&lower[pos..]) {
            pos += sub_len;
            Some(sub_name)
        } else {
            None
        }
    } else {
        subtype
    };

    // CR 205.3a: "[Subtype] [CoreType]" patterns like "Wizard creatures",
    // "Goblin creatures", "Elf Warriors" — when parse_core_type (via parse_type_filter_word)
    // matched a subtype word, check if a concrete core type word follows. If so, promote
    // the subtype to the subtype slot and the trailing core type to card_type.
    // Excludes Card/Spell (handled by redundant suffix stripping) and subtypes.
    let (card_type, subtype) =
        if matches!(card_type, Some(TypeFilter::Subtype(_))) && subtype.is_none() {
            let rest_after = lower[pos..].trim_start();
            let ws = lower[pos..].len() - rest_after.len();
            if let Ok((ct_rest, tf)) = nom_target::parse_type_filter_word(rest_after) {
                let is_concrete_core_type = matches!(
                    tf,
                    TypeFilter::Creature
                        | TypeFilter::Artifact
                        | TypeFilter::Enchantment
                        | TypeFilter::Instant
                        | TypeFilter::Sorcery
                        | TypeFilter::Planeswalker
                        | TypeFilter::Land
                        | TypeFilter::Battle
                        | TypeFilter::Permanent
                );
                if is_concrete_core_type {
                    let ct_len = rest_after.len() - ct_rest.len();
                    pos += ws + ct_len;
                    let sub_name = match card_type {
                        Some(TypeFilter::Subtype(s)) => s,
                        _ => unreachable!(),
                    };
                    (Some(tf), Some(sub_name))
                } else {
                    (card_type, subtype)
                }
            } else {
                (card_type, subtype)
            }
        } else {
            (card_type, subtype)
        };

    // CR 205.2a: Multi-type adjective conjunction — "artifact creature", "legendary
    // creature", "noncreature artifact", "enchantment creature", etc. The first core
    // type was consumed above; collect trailing concrete core type words as
    // additional conjunctive type filters (evaluated via AND in `filter.rs`).
    //
    // Example: "whenever you cast an artifact creature spell" → primary = Artifact,
    // conjunctive = [Creature]. A non-creature artifact spell would NOT satisfy
    // this filter, whereas the single-type parse would have incorrectly accepted it.
    //
    // Guard: only consume adjacent core-type words (no separator between them).
    // Word-boundary on the next character prevents "creature" from eating into
    // suffixes like "creatures". Stop before `Card` / `Subtype` — those are
    // informational suffixes ("creature card") or belong to the subtype slot.
    let mut extra_core_type_filters: Vec<TypeFilter> = Vec::new();
    let mut relative_core_type_filters: Vec<TypeFilter> = Vec::new();
    if matches!(
        card_type,
        Some(
            TypeFilter::Creature
                | TypeFilter::Artifact
                | TypeFilter::Enchantment
                | TypeFilter::Instant
                | TypeFilter::Sorcery
                | TypeFilter::Planeswalker
                | TypeFilter::Land
                | TypeFilter::Battle
                | TypeFilter::Permanent
        )
    ) {
        loop {
            let rest_after = lower[pos..].trim_start();
            let ws = lower[pos..].len() - rest_after.len();
            // `ws == 0` means no whitespace separator — not an adjacent adjective.
            if ws == 0 {
                break;
            }
            let Ok((ct_rest, tf)) = nom_target::parse_type_filter_word(rest_after) else {
                break;
            };
            let is_concrete_core_type = matches!(
                tf,
                TypeFilter::Creature
                    | TypeFilter::Artifact
                    | TypeFilter::Enchantment
                    | TypeFilter::Instant
                    | TypeFilter::Sorcery
                    | TypeFilter::Planeswalker
                    | TypeFilter::Land
                    | TypeFilter::Battle
            );
            if !is_concrete_core_type {
                break;
            }
            // Must not duplicate the primary or an already-accumulated filter.
            if card_type.as_ref() == Some(&tf) || extra_core_type_filters.contains(&tf) {
                break;
            }
            let ct_len = rest_after.len() - ct_rest.len();
            pos += ws + ct_len;
            extra_core_type_filters.push(tf);
        }
    }

    // Skip redundant trailing "spell"/"spells"/"card"/"cards" after a specific type like
    // "sorcery spell", "creature card". When the core type is already Instant/Sorcery/etc.,
    // the word is informational — consuming it allows suffix parsers (e.g., "that targets only")
    // and event verb parsers to see what follows.
    // Tracks whether the left leg ended in a "card"/"cards" noun — the discriminator
    // that a following article-led "or a <type> card" is a card-type disjunction
    // (Overlord of the Balemurk, #5331) rather than an elided-verb "you control X
    // or a Y" clause the condition layer folds one level up (which must stay as
    // `parse_type_phrase` remainder — `parse_type_phrase_leaves_article_led_or_rhs_as_remainder`).
    let mut left_card_suffix = false;
    if card_type.is_some() && !matches!(card_type, Some(TypeFilter::Card) | Some(TypeFilter::Any)) {
        let rest_trimmed = lower[pos..].trim_start();
        let ws_len = lower[pos..].len() - rest_trimmed.len();
        // CR 108.1: "spell" and "card" are informational suffixes after a typed qualifier.
        // Longest-match-first ordering (plurals before singular). The paired flag
        // records whether the consumed noun was "card"/"cards" (vs "spell") — a
        // card-type disjunction whose article-led RHS is a sibling type, not an
        // elided-verb clause. Data-driven so the discriminator comes from the
        // matched tag, not from string inspection of the suffix.
        static REDUNDANT_SUFFIXES: &[(&str, bool)] = &[
            ("spells ", false),
            ("spell ", false),
            ("cards ", true),
            ("card ", true),
        ];
        let mut consumed_suffix = false;
        for (suffix, is_card) in REDUNDANT_SUFFIXES {
            if let Ok((after, _)) = tag::<_, _, OracleError<'_>>(*suffix).parse(rest_trimmed) {
                let suffix_len = rest_trimmed.len() - after.len();
                pos += ws_len + suffix_len;
                consumed_suffix = true;
                left_card_suffix = *is_card;
                break;
            }
        }
        if !consumed_suffix {
            // Check end-of-input variants (no trailing space)
            for (suffix, is_card) in &[
                ("spells", false),
                ("spell", false),
                ("cards", true),
                ("card", true),
            ] {
                if rest_trimmed == *suffix {
                    pos += ws_len + suffix.len();
                    left_card_suffix = *is_card;
                    break;
                }
            }
        }
    }

    if let Some(consumed) = parse_token_suffix(&lower[pos..]) {
        properties.push(FilterProp::Token);
        pos += consumed;
    }

    if let Some((prop, consumed)) = parse_combat_relation_suffix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    // CR 205.3a: Comma-separated type lists ("artifacts, creatures, and lands") are
    // syntactic sugar for set-union, same as "and" between two types.
    let rest_lower = lower[pos..].trim_start();
    let rest_offset = lower[pos..].len() - rest_lower.len();

    // Try each type combinator separator in longest-match-first order.
    // Each separator produces an Or combination when followed by a recognized type word.
    static TYPE_SEPARATORS: &[&str] = &[
        ", and/or ",
        ", and ",
        ", or ",
        ", ",
        "or ",
        "and/or ",
        "and ",
    ];
    for separator in TYPE_SEPARATORS {
        if let Ok((after_sep, _)) = tag::<_, _, OracleError<'_>>(*separator).parse(rest_lower) {
            let after_trimmed = after_sep.trim_start();
            let can_recurse = if separator.starts_with(',') {
                starts_with_or_article_type_segment(after_trimmed)
            } else {
                // A bare "and"/"or" disjunct may lead with an article
                // ("non-Avatar creature card or *a* planeswalker card" — Overlord
                // of the Balemurk, #5331). The comma path already accepts that via
                // `starts_with_or_article_type_segment`; without it here the
                // second disjunct is silently dropped and the card can only return
                // creatures, never planeswalkers. `starts_with_type_word` still
                // covers the article-less form ("creature or planeswalker card").
                //
                // The article-led form is gated on `left_card_suffix`: only a
                // "<type> card or a <type> card" disjunction folds here. Without
                // the "card" noun, an article-led "or a <type>" is an elided-verb
                // clause ("you control an artifact creature or a Plan") that the
                // condition layer (`parse_you_control_a`) folds one level up, so it
                // must remain as remainder (asserted by
                // `parse_type_phrase_leaves_article_led_or_rhs_as_remainder`).
                // A BARE-card RHS ("or a card with disturb" — Shipwreck Sifters)
                // is a keyword-membership branch folded at the trigger layer, not a
                // type union, so it is excluded even though the left carried "card".
                starts_with_type_word(after_trimmed)
                    || (left_card_suffix
                        && !is_article_led_bare_card(after_trimmed)
                        && starts_with_or_article_type_segment(after_trimmed))
            };
            if can_recurse {
                let sep_text = &text[pos + rest_offset + separator.len()..];
                let (other_filter, final_rest) = parse_type_phrase_with_ctx(sep_text, ctx);
                // CR 205.2a: The left branch of a type disjunction must retain
                // every type word that bound to it before the connector — the
                // primary core type (`card_type`), the trailing core types from
                // adjective-conjunction ("artifact creature" → `Creature` in
                // `extra_core_type_filters`), any adjective subtype unions
                // ("outlaw" → `AnyOf(...)` in `adjective_type_filters`), and the
                // negated types collected via the `non-` scan. Dropping any of
                // these on the floor would collapse a multi-type conjunction
                // (AND of `type_filters`, per `game/filter.rs`) into a strictly
                // looser filter, e.g. parsing "artifact creature card or
                // Vehicle card" to `Or[Typed{Artifact}, Typed{Vehicle}]` —
                // which would match any artifact, not only artifact creatures
                // (#1537, Szarekh, the Silent King).
                // This branch `return`s immediately below, so the three
                // accumulators are never read again — drain them into
                // `left_extras` instead of cloning. `std::mem::take` (rather
                // than a plain move) keeps the borrow checker happy inside the
                // `for separator` loop, and `append` reuses each backing
                // allocation rather than heap-cloning every `TypeFilter`.
                let mut left_extras = std::mem::take(&mut adjective_type_filters);
                left_extras.append(&mut extra_core_type_filters);
                left_extras.append(&mut neg_type_filters);
                let left = typed(
                    card_type.unwrap_or(TypeFilter::Any),
                    subtype,
                    properties.clone(),
                    left_extras,
                );
                let combined = merge_or_filters(left, other_filter);
                // CR 105.1 + CR 205.2: an article-led disjunct ("… or *an*
                // artifact creature card") is a syntactically self-contained noun
                // phrase, so the left leg's leading adjective properties
                // (color/supertype/tapped — the leg-local set in
                // `is_adjective_prefix_prop`) bind only to the left noun and must
                // NOT distribute onto it. "red creature card or an artifact
                // creature card" (Purphoros, Bronze-Blooded) does not require the
                // artifact creature to be red — distributing `HasColor(Red)` would
                // wrongly reject a colorless artifact creature. A bare disjunct
                // ("… or creature") shares the left adjectives, unchanged.
                let right_is_article_led = alt((tag::<_, _, OracleError<'_>>("an "), tag("a ")))
                    .parse(after_trimmed)
                    .is_ok();
                let shared_props: Vec<FilterProp> = if right_is_article_led {
                    properties
                        .iter()
                        .filter(|prop| !is_adjective_prefix_prop(prop))
                        .cloned()
                        .collect()
                } else {
                    properties.clone()
                };
                let combined = distribute_shared_properties(combined, &shared_props);
                let combined = distribute_controller_to_or(combined);
                let combined = distribute_core_type_to_or(combined);
                let combined = distribute_neg_type_filters_to_or(combined);
                return (distribute_properties_to_or(combined), final_rest);
            }
        }
    }

    // CR 108.3 + CR 110.2: Ownership and control are distinct; "you own and control" satisfies both.
    let mut controller = None;
    pos +=
        parse_ownership_or_controller_suffix(&lower[pos..], &mut properties, &mut controller, ctx);

    // Grammar normalization: strip the distributive-"each" linker between a
    // collective type word and a per-object property suffix —
    // "creatures, each with power 1 or less" /
    // "creatures, each with base power or toughness 1 or less" (Angelic
    // Aberration class; #967) and the comma-less form "cards each with mana
    // value X or less" (Dance of the Manse). Consuming the `[,] each ` token
    // normalizes the remaining input to the bare suffix form ("with …") so
    // that all downstream suffix parsers (power/toughness via CR 208,
    // mana-value via CR 202.3, counters via CR 122.1, keywords via CR 702)
    // receive the same input regardless of whether the Oracle text used the
    // comma linker, the comma-less linker, or plain "with …". The trailing
    // `peek("with ")` restricts stripping to a genuine property suffix so a
    // non-distributive "each" (e.g. "each player owns") is left intact.
    {
        let after_ws = lower[pos..].trim_start();
        let ws = lower[pos..].len() - after_ws.len();
        if let Ok((rem, _)) = (
            opt((tag::<_, _, OracleError<'_>>(","), opt(space1))),
            tag::<_, _, OracleError<'_>>("each "),
            peek(tag::<_, _, OracleError<'_>>("with ")),
        )
            .parse(after_ws)
        {
            pos += ws + (after_ws.len() - rem.len());
        }
    }

    // Check "with power N or less/greater" suffix
    if let Some((prop, consumed)) = parse_mana_value_suffix(&lower[pos..], ctx) {
        properties.push(prop);
        pos += consumed;
    }

    // Check "with power N or less/greater" suffix
    if let Some((prop, consumed)) = parse_power_suffix(&lower[pos..], ctx) {
        properties.push(prop);
        pos += consumed;
    }

    // Check "with [counter] counter(s) on it/them" suffix
    if let Some((prop, consumed)) = parse_counter_suffix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    // CR 113.1 + CR 113.3: "<type> with no abilities" — an object with none of the
    // four ability categories. Narrow predicate combinator lives in oracle_nom/filter.rs;
    // this arm supplies the "with " lead + offset handling, mirroring parse_counter_suffix.
    {
        let after_ws = lower[pos..].trim_start();
        let ws = lower[pos..].len() - after_ws.len();
        if let Ok((with_rest, _)) = (tag::<_, _, OracleError<'_>>("with"), space1).parse(after_ws) {
            if let Ok((rest, prop)) = nom_filter::parse_no_abilities(with_rest) {
                properties.push(prop);
                pos += ws + (after_ws.len() - rest.len());
            }
        }
    }

    if let Some((keyword_props, consumed)) = parse_without_keyword_suffix(&lower[pos..]) {
        properties.extend(keyword_props);
        pos += consumed;
    } else if let Some((suffix, consumed)) = parse_keyword_suffix(&lower[pos..]) {
        if suffix.disjunctive && suffix.properties.len() > 1 {
            property_disjunction_ranges.push((properties.len(), suffix.properties.len()));
        }
        properties.extend(suffix.properties);
        pos += consumed;
    }

    if let Some((prop, consumed)) = parse_same_name_suffix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    if controller.is_none()
        && !properties
            .iter()
            .any(|prop| matches!(prop, FilterProp::Owned { .. }))
    {
        pos += parse_ownership_or_controller_suffix(
            &lower[pos..],
            &mut properties,
            &mut controller,
            ctx,
        );
    }

    // CR 700.9 (modified) + CR 109.4 (control): "<typed filter> other than ~"
    // excludes the ability source from the population. FilterProp::Another
    // (filter.rs:2206) matches every object except the source, so the count
    // omits the source permanent (Thundering Raiju: "modified creatures you
    // control other than this creature" — normalized to "~"). The trailing
    // self-reference is recognized via `nom_target::parse_self_reference`
    // ("~"/"it"/"this creature"/"itself"/…).
    {
        let remaining_other_than = lower[pos..].trim_start();
        let other_than_offset = lower[pos..].len() - remaining_other_than.len();
        if let Ok((rest, _)) = (
            tag::<_, _, OracleError<'_>>("other than "),
            nom_target::parse_self_reference,
        )
            .parse(remaining_other_than)
        {
            if !properties.contains(&FilterProp::Another) {
                properties.push(FilterProp::Another);
            }
            pos += other_than_offset + (remaining_other_than.len() - rest.len());
        }
    }

    // CR 205.3 + CR 205.4b: "that isn't a <Subtype>" relative-clause negation.
    // Checked before `parse_that_clause_suffix` so the subtype exclusion short-circuits
    // the generic that-clause branch (which does not recognize subtype negation).
    if let Some((neg_tfs, consumed)) = parse_that_isnt_subtype_suffix(&lower[pos..]) {
        neg_type_filters.extend(neg_tfs);
        pos += consumed;
    }

    // CR 205.3 (#2905): positive "that's a/an <Subtype> [or a/an <Subtype>]"
    // relative-clause restriction ("creature you control that's an Ape or a
    // Monkey"). Append the subtype constraint as an adjective type filter so it
    // AND-merges with the core type (Creature) rather than being dropped — the
    // clause previously fell through, leaving every creature eligible. Checked
    // before `parse_that_clause_suffix` (mirrors the `that isn't` arm); it only
    // fires for real subtypes, so color/supertype "that's" clauses are unaffected.
    if let Some((subtype_filter, consumed)) = parse_that_is_subtype_suffix(&lower[pos..]) {
        adjective_type_filters.push(subtype_filter);
        pos += consumed;
    }

    // Positive relative card-type restriction:
    // "permanent that's an artifact, creature, or enchantment" keeps the base
    // permanent/supertype restrictions and distributes the trailing card-type
    // list as OR branches.
    if let Some((core_types, consumed)) = parse_that_is_core_type_suffix(&lower[pos..]) {
        relative_core_type_filters = core_types;
        pos += consumed;
    }

    // "that share(s) a creature type" / "that has/have [keyword]" relative clause.
    if let Some((that_props, consumed)) = parse_that_clause_suffix(&lower[pos..], Some(ctx)) {
        properties.extend(that_props);
        pos += consumed;
    }

    // CR 608.2c: "<type> except those that <relative-clause>" / "other than those
    // that <relative-clause>" — an exclusion suffix. The inner relative clause is
    // parsed by the same `parse_that_clause_suffix` grammar and the leg matches the
    // *complement* of the whole clause. `parse_that_clause_suffix` returns its
    // predicates AND-combined (a conjunctive clause, e.g. "that are attacking and
    // tapped"), so the complement is the De Morgan dual
    // Not(X AND Y) = Not(X) OR Not(Y). A single predicate negates directly; a
    // multi-predicate conjunction folds to a single `AnyOf{[Not(X), Not(Y)]}`
    // (disjunction of negations) — never per-prop `Not(X) AND Not(Y)`, which would
    // exclude every object matching X *or* Y rather than only those matching both.
    // A clause whose disjunction is already a single prop (e.g. "enchanted or
    // equipped" → `HasAnyAttachmentOf`) stays one prop and its `Not` De Morgans
    // correctly at runtime. Covers "all creatures except those that share a
    // creature type with a creature that convoked this spell" (Everything Comes to
    // Dust) and the general class ("except those that attacked this turn").
    {
        let rem = lower[pos..].trim_start();
        let ws = lower[pos..].len() - rem.len();
        if let Ok((after_those, _)) = alt((
            tag::<_, _, OracleError<'_>>("except those "),
            tag("other than those "),
        ))
        .parse(rem)
        {
            let prefix_len = rem.len() - after_those.len();
            if let Some((excl_props, consumed)) = parse_that_clause_suffix(after_those, Some(ctx)) {
                let negated: Vec<FilterProp> = excl_props
                    .into_iter()
                    .map(|prop| FilterProp::Not {
                        prop: Box::new(prop),
                    })
                    .collect();
                match negated.len() {
                    0 => {}
                    1 => properties.push(
                        negated
                            .into_iter()
                            .next()
                            .expect("len checked to be exactly 1"),
                    ),
                    _ => properties.push(FilterProp::AnyOf { props: negated }),
                }
                pos += ws + prefix_len + consumed;
            }
        }
    }

    // CR 608.2c + CR 205.2b: "<type> except for <type-list>" — plain type-list
    // exclusion (Scourglass: "Destroy all permanents except for artifacts and
    // lands"; Elspeth Tirel: "except for lands and tokens"), distinct from the
    // predicate-based "except those that" clause immediately above. Tried only
    // when that block didn't match — "except those "/"other than those " vs
    // "except for " diverge at the 8th character of "except ", so the two are
    // mutually exclusive.
    {
        let rem = lower[pos..].trim_start();
        let ws = lower[pos..].len() - rem.len();
        if let Some((excl_types, excl_props, consumed)) = parse_except_for_type_list_suffix(rem) {
            neg_type_filters.extend(excl_types);
            properties.extend(excl_props);
            pos += ws + consumed;
        }
    }

    // CR 109.4: "that <player> control(s)" relative clause supplying the object
    // controller — e.g. "permanents you own that your opponents control"
    // (Zedruu). Placed after `parse_that_clause_suffix` so the quality/combat/
    // attachment "that …" clauses get first crack, and gated on
    // `controller.is_none()` so it only fills a controller not already set
    // (e.g. by an earlier "you control"/"an opponent controls" suffix). The
    // controller phrase delegates to `parse_controller_suffix`, which routes the
    // bare "your opponents control"/"an opponent controls" forms through
    // `nom_filter::parse_zone_controller`. Composes with a preceding "you own"
    // → `FilterProp::Owned{You}`, yielding the owned-but-opponent-controlled
    // population.
    if controller.is_none() {
        let remaining_that_ctrl = lower[pos..].trim_start();
        let that_ctrl_offset = lower[pos..].len() - remaining_that_ctrl.len();
        if let Ok((after_that, _)) =
            tag::<_, _, OracleError<'_>>("that ").parse(remaining_that_ctrl)
        {
            if let Some((ctrl, consumed)) = parse_controller_suffix(after_that, ctx) {
                controller = Some(ctrl);
                pos += that_ctrl_offset + "that ".len() + consumed;

                // A predicate relative clause can follow the controller clause —
                // e.g. "untapped creatures that player controls that didn't attack
                // this turn" (Angel's Trumpet). The controller clause was consumed
                // above, so re-run the generic relative-clause extractor on the
                // remainder to pick up the trailing verb/quality/attachment "that …"
                // restriction that the first call (which saw "that player controls")
                // could not match.
                if let Some((trailing_props, consumed)) =
                    parse_that_clause_suffix(&lower[pos..], Some(ctx))
                {
                    properties.extend(trailing_props);
                    pos += consumed;
                }
            }
        }
    }

    if let Some((prop, consumed)) = parse_attacking_defender_suffix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    // Check zone suffix: "card from a graveyard", "card in your graveyard", "from exile", etc.
    if let Some((zone_props, zone_ctrl, consumed)) = parse_zone_suffix(&lower[pos..]) {
        properties.extend(zone_props);
        pos += consumed;
        // Apply zone-derived controller if we don't already have one
        if controller.is_none() {
            controller = zone_ctrl;
        }
    }

    if let Some((prop, consumed)) =
        parse_zone_changed_this_turn_suffix(&lower[pos..], zone_for_scope(&properties))
    {
        properties.push(prop);
        pos += consumed;
    }

    // Check "of the chosen type" / "of that type" suffix (Cavern of Souls,
    // Metallic Mimic; Selfless Safewright). CR 205.3m + CR 608.2c: "of that
    // type" is the anaphor form of "of the chosen type" — same typed reference,
    // same runtime resolution against the source's chosen creature type — so
    // both surface forms route to one suffix arm. Mirrors the dual recognition
    // in `parse_chosen_qualifier_subject` (oracle_static/keyword_grant.rs).
    let remaining = lower[pos..].trim_start();
    let remaining_offset = lower[pos..].len() - remaining.len();
    if let Ok((_, of_chosen_len)) = alt((
        value(
            "of the chosen type".len(),
            tag::<_, _, OracleError<'_>>("of the chosen type"),
        ),
        value("of that type".len(), tag("of that type")),
    ))
    .parse(remaining)
    {
        // CR 205.2a: Disambiguate which "chosen type" axis this refers to by the
        // base type, mirroring the static cost-mod path in
        // `oracle_static/static_helpers.rs`. The default is a chosen CREATURE
        // subtype — the overwhelmingly common case ("creature ... of the chosen
        // type", Cavern of Souls; "token ... of the chosen type", tribal
        // companions) where a "choose a creature type" was made. Flip to a
        // chosen CARD type ONLY when the base is an explicit *card-type* filter
        // ("cards of the chosen type", Winding Way's "Choose creature or land";
        // "land of the chosen type"), where the chosen value is a card type.
        // Emitting `IsChosenCreatureType` for a card-typed base never matches at
        // runtime, so the filtered move would resolve to nothing.
        let is_card_typed_base = matches!(
            &card_type,
            Some(
                TypeFilter::Card
                    | TypeFilter::Land
                    | TypeFilter::Artifact
                    | TypeFilter::Enchantment
                    | TypeFilter::Instant
                    | TypeFilter::Sorcery
                    | TypeFilter::Planeswalker
                    | TypeFilter::Battle
            )
        );
        // CR 205.3m + CR 608.2c: When a preceding `Choose` committed
        // `CreatureType`, "cards of that type" still refers to the chosen
        // creature subtype (Grave Sifter), not a card type — even though the
        // head noun is `Card`. Without this override, `IsChosenCardType` reads
        // the wrong `ChosenAttribute` axis and the graveyard return finds no
        // eligible cards.
        let chosen_prop = if is_card_typed_base {
            match ctx.pending_choice_type.as_ref() {
                Some(ChoiceType::CreatureType { .. }) => FilterProp::IsChosenCreatureType,
                _ => FilterProp::IsChosenCardType,
            }
        } else {
            FilterProp::IsChosenCreatureType
        };
        properties.push(chosen_prop);
        pos += remaining_offset + of_chosen_len;
    }

    // CR 115.2: A spell or ability may target an object in a zone other than
    // the battlefield only when it specifies that zone, so the trailing zone
    // phrase must be parsed onto the target filter. Zone phrases may trail "of
    // the chosen type" ("target creature card of the chosen type from your
    // graveyard", From the Rubble). The primary `parse_zone_suffix` arm above
    // runs before this suffix.
    if let Some((zone_props, zone_ctrl, consumed)) = parse_zone_suffix(&lower[pos..]) {
        properties.extend(zone_props);
        pos += consumed;
        if controller.is_none() {
            controller = zone_ctrl;
        }
    }

    // CR 122.1 + CR 400.1: A counter-presence clause may TRAIL a zone clause
    // ("a creature card in exile with a takeover counter on it" — The Master,
    // Formed Anew). The pre-zone `parse_counter_suffix` pass above only catches
    // counters that precede the zone; this second pass catches the
    // zone-then-counter ordering so the full source-filter phrase is consumed and
    // no leftover remains (a leftover that the clone-replacement guard rejects).
    if let Some((prop, consumed)) = parse_counter_suffix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    // CR 113.6b: A "without <keyword>" clause may TRAIL a zone clause ("nonland
    // card in your hand without foretell" — Dream Devourer). The pre-zone
    // `parse_without_keyword_suffix` pass above only catches the clause when it
    // precedes the zone; this second pass catches the zone-then-without ordering
    // so the subject fully consumes (the graveyard/hand keyword-grant gate
    // requires an empty remainder).
    if let Some((keyword_props, consumed)) = parse_without_keyword_suffix(&lower[pos..]) {
        properties.extend(keyword_props);
        pos += consumed;
    }

    let mut exclude_chosen_type = false;
    let mut exclude_owned_by_controller: Option<ControllerRef> = None;
    let remaining_not_owned = lower[pos..].trim_start();
    let not_owned_offset = lower[pos..].len() - remaining_not_owned.len();
    if let Some(ref ctrl) = controller {
        for suffix in &[
            "but don't own",
            "but do not own",
            "but doesn't own",
            "but does not own",
        ] {
            if tag::<_, _, OracleError<'_>>(*suffix)
                .parse(remaining_not_owned)
                .is_ok()
            {
                exclude_owned_by_controller = Some(ctrl.clone());
                pos += not_owned_offset + suffix.len();
                break;
            }
        }
    }

    let remaining = lower[pos..].trim_start();
    let remaining_offset = lower[pos..].len() - remaining.len();
    for suffix in &[
        "that aren't of the chosen type",
        "that are not of the chosen type",
        "not of the chosen type",
    ] {
        if tag::<_, _, OracleError<'_>>(*suffix)
            .parse(remaining)
            .is_ok()
        {
            exclude_chosen_type = true;
            pos += remaining_offset + suffix.len();
            break;
        }
    }

    // CR 406.6 + CR 607.2a: "exiled with [source]" / "exiled this way" linkage
    // suffix on a typed reference. Singular targeted forms compose with the
    // typed filter via `TargetFilter::And { [Typed, ExiledBySource] }`,
    // mirroring the `exclude_chosen_type` wrapping pattern below. The plural
    // and "each card" forms are handled at the top of `parse_target` since
    // they bypass type-phrase parsing entirely.
    //
    // These grammars share the same lowering:
    //   * `exiled with this <type>` / `exiled with ~` — explicit-source linkage
    //     (CR 406.6). The trailing type word is informational and consumed as
    //     a single non-space run via `take_till1` so it doesn't leak.
    //   * `that were exiled this way` / `that was exiled this way` — relative-
    //     clause linkage (CR 607.2a). "This way" refers back to the preceding
    //     exile instruction within the same effect; the resolver maps it to
    //     the same `ExiledBySource` predicate, since the link is established
    //     by the linked-exile bookkeeping at exile time.
    //   * bare `exiled this way` — the same CR 607.2a linkage as a reduced
    //     past-participle adjective with no relative pronoun (Espers to
    //     Magicite: "choose up to one target creature card exiled this way").
    //     Without this arm the qualifier is dropped and the target degrades to
    //     a battlefield "creature card", which resolves against on-battlefield
    //     creatures instead of the cards this spell exiled.
    let mut exiled_by_source = false;
    let remaining_exiled = lower[pos..].trim_start();
    let exiled_offset = lower[pos..].len() - remaining_exiled.len();
    if let Ok((rest, _)) = (
        tag::<_, _, OracleError<'_>>("exiled with this "),
        nom::bytes::complete::take_till1::<_, _, OracleError<'_>>(|c: char| c.is_whitespace()),
    )
        .parse(remaining_exiled)
    {
        exiled_by_source = true;
        pos += exiled_offset + (remaining_exiled.len() - rest.len());
    } else if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("exiled with ~"),
        // CR 607.2 + CR 406.6: "exiled with it" — the anaphoric "it" names the
        // source object, identical linked-exile semantics to "exiled with ~"
        // (Sothera, the Supervoid: "a creature card exiled with it"). The
        // "exiled with this <type>" arm above already claimed the demonstrative
        // form, so "it" is the disjoint pronoun variant.
        tag("exiled with it"),
    ))
    .parse(remaining_exiled)
    {
        exiled_by_source = true;
        pos += exiled_offset + (remaining_exiled.len() - rest.len());
    } else if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("that were exiled this way"),
        tag::<_, _, OracleError<'_>>("that was exiled this way"),
        tag::<_, _, OracleError<'_>>("exiled this way"),
    ))
    .parse(remaining_exiled)
    {
        exiled_by_source = true;
        pos += exiled_offset + (remaining_exiled.len() - rest.len());
    }

    // CR 608.2c + CR 122.1: "that had counters put on it this way" — relative-
    // clause linkage to objects that received counters from the preceding
    // instruction in the same ability (Agitator Ant: "Goad each creature that
    // had counters put on it this way"). The resolver publishes the affected
    // set when counters are placed; `TrackedSetFiltered` intersects it with the
    // type filter.
    let mut counters_put_this_way = false;
    let remaining_counters = lower[pos..].trim_start();
    let counters_offset = lower[pos..].len() - remaining_counters.len();
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("that had counters put on it this way"),
        tag::<_, _, OracleError<'_>>("that had a counter put on it this way"),
    ))
    .parse(remaining_counters)
    {
        counters_put_this_way = true;
        pos += counters_offset + (remaining_counters.len() - rest.len());
    }

    // CR 201.2a + CR 201.4: "<type-phrase> with the chosen name" / "<type-phrase>
    // with a name chosen for ~" — restrict the object class to objects whose name
    // equals the source's ChosenAttribute::CardName (bound by a preceding
    // Effect::Choose { CardName, persist: true }, e.g. Day of the Moon's "Choose
    // a creature card name, then goad all creatures with a name chosen for this
    // enchantment"). The self-reference noun ("this enchantment"/"this permanent"
    // /...) is normalized to `~` before parsing (SELF_REF_TYPE_PHRASES in
    // oracle_util.rs), so every noun variant collapses to the single canonical
    // form "with a name chosen for ~" — matching `~` is both correct and verb-/
    // noun-agnostic. Both surface forms are CR-201.2a name-match synonyms and
    // lower identically to a HasChosenName leg. Mirrors the `exiled_by_source`
    // recognizer above: a pos-tracked boolean wrapped into TargetFilter::And at
    // end-of-function. The static-line analogue ("Spells with the chosen name
    // can't be cast") lives in oracle_static/shared.rs::parse_continuous_subject_filter.
    let mut has_chosen_name = false;
    let remaining_chosen = lower[pos..].trim_start();
    let chosen_offset = lower[pos..].len() - remaining_chosen.len();
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("with the chosen name"),
        tag::<_, _, OracleError<'_>>("with a name chosen for ~"),
    ))
    .parse(remaining_chosen)
    {
        has_chosen_name = true;
        pos += chosen_offset + (remaining_chosen.len() - rest.len());
    }

    // CR 608.2d: "of their choice" / "of his or her choice" — informational qualifier
    // on opponent-choice effects. The actual choice is handled by the WaitingFor state machine.
    let remaining_choice = lower[pos..].trim_start();
    let choice_offset = lower[pos..].len() - remaining_choice.len();
    for suffix in &["of their choice", "of his or her choice"] {
        if tag::<_, _, OracleError<'_>>(*suffix)
            .parse(remaining_choice)
            .is_ok()
        {
            pos += choice_offset + suffix.len();
            // CR 601.2c + CR 603.3d: a TARGETED "of their choice" whose target filter
            // is controlled by the phase-trigger active player ("destroy target X that
            // player controls of their choice") announces its target at stack placement —
            // the chooser is that scoped player. Distinct from CR 608.2d resolution-time
            // sacrifices (controller not ScopedPlayer → stays None).
            if controller.as_ref() == Some(&ControllerRef::ScopedPlayer) {
                ctx.target_chooser = Some(TargetFilter::ScopedPlayer);
            }
            break;
        }
    }

    // CR 201.2: "named [card name]" suffix — filter by exact card name.
    // Handles "creature named X", "cards named X", "named X" patterns.
    let remaining_named = lower[pos..].trim_start();
    let named_offset = lower[pos..].len() - remaining_named.len();
    if let Ok((name_text, _)) = tag::<_, _, OracleError<'_>>("named ").parse(remaining_named) {
        // CR 201.2: The card name runs to the earliest *clause* boundary, NOT to
        // the first comma/period. Card names legitimately contain commas and the
        // word "and" ("Bruna, the Fading Light"; "Gisa and Geralf"), so splitting
        // on bare punctuation truncates them, while scanning to end-of-string
        // over-consumes the trailing relative-clause predicate. Issue #2016:
        // "each player who controls a permanent named Bonder's Ornament draws a
        // card" produced `Named { name: "Bonder's Ornament draws a card" }` — the
        // predicate verb was swallowed into the name, so the controls-predicate
        // matched nobody and the whole "who controls …" scope was dropped, making
        // *every* player draw. Scan word boundaries (spaces, and commas for the
        // pronoun-guarded comma-clause arm) and stop at the first clause-joining
        // terminator (see `parse_named_filter_terminator`), which preserves
        // comma/and-bearing names while ending the name at the controller
        // suffix, relative pronoun, predicate verb, or referential comma clause.
        let name_end = name_text
            .char_indices()
            .filter(|&(_, c)| c == ' ' || c == ',')
            .find(|&(idx, _)| parse_named_filter_terminator(&name_text[idx..]).is_ok())
            .map_or_else(
                || name_text.find(['.', ':', ';']).unwrap_or(name_text.len()),
                |(idx, _)| idx,
            );
        let raw_name = name_text[..name_end].trim();
        if !raw_name.is_empty() {
            // Reconstruct original-case name from the same position in `text`
            let orig_offset = pos + named_offset + "named ".len();
            let orig_name = text[orig_offset..orig_offset + raw_name.len()].trim();
            properties.push(FilterProp::Named {
                name: orig_name.to_string(),
            });
            pos += named_offset + "named ".len() + name_end;

            // CR 201.2 + CR 400.1: Re-run the zone-suffix pass now that the name
            // is consumed, so a trailing locative constraint ("named X in your
            // graveyard", "named X in all graveyards", "named X on the
            // battlefield") attaches as an `InZone`/`InAnyZone` filter prop —
            // parity with the non-named "creature card in your graveyard" path.
            // The primary `parse_zone_suffix` pass above ran before the name was
            // consumed and could not see it. Scoped to "in"/"on" locatives so
            // "from <zone>" move-origins stay in the remainder for the caller
            // (CR 115.2 target zones, return-from-graveyard move sources).
            let after_named = lower[pos..].trim_start();
            let is_locative =
                alt((tag::<_, _, OracleError<'_>>("in "), tag("on "))).parse(after_named);
            if is_locative.is_ok() {
                if let Some((zone_props, zone_ctrl, consumed)) = parse_zone_suffix(&lower[pos..]) {
                    properties.extend(zone_props);
                    pos += consumed;
                    if controller.is_none() {
                        controller = zone_ctrl;
                    }
                }
            }
        }
    }

    let mut base_type_filters = [
        adjective_type_filters,
        card_type.map(|ct| vec![ct]).unwrap_or_default(),
        extra_core_type_filters,
        subtype
            .map(|s| vec![TypeFilter::Subtype(s)])
            .unwrap_or_default(),
        neg_type_filters,
    ]
    .concat();

    let type_filter_branches = if relative_core_type_filters.is_empty() {
        vec![base_type_filters]
    } else if relative_core_type_filters.len() == 1 {
        base_type_filters.push(
            relative_core_type_filters
                .pop()
                .expect("len checked to be exactly 1"),
        );
        vec![base_type_filters]
    } else {
        relative_core_type_filters
            .into_iter()
            .map(|relative_type| {
                let mut branch = base_type_filters.clone();
                branch.push(relative_type);
                branch
            })
            .collect::<Vec<_>>()
    };

    let property_branches = if property_disjunction_ranges.is_empty() {
        vec![properties]
    } else {
        let mut disjunctive_indices = vec![false; properties.len()];
        for (start, len) in &property_disjunction_ranges {
            for is_disjunctive in disjunctive_indices.iter_mut().skip(*start).take(*len) {
                *is_disjunctive = true;
            }
        }
        let common_props = properties
            .iter()
            .enumerate()
            .filter(|(idx, _)| !disjunctive_indices[*idx])
            .map(|(_, prop)| prop.clone())
            .collect::<Vec<_>>();
        let mut branch_props = vec![common_props];
        for (start, len) in property_disjunction_ranges {
            let disjunctive_props = properties[start..start + len].to_vec();
            branch_props = branch_props
                .into_iter()
                .flat_map(|common| {
                    disjunctive_props.iter().cloned().map(move |prop| {
                        let mut branch = common.clone();
                        branch.push(prop);
                        branch
                    })
                })
                .collect();
        }
        branch_props
    };

    let mut filters = Vec::new();
    for type_filters in type_filter_branches {
        for properties in &property_branches {
            filters.push(TargetFilter::Typed(TypedFilter {
                type_filters: type_filters.clone(),
                controller: controller.clone(),
                properties: properties.clone(),
            }));
        }
    }
    let filter = if filters.len() == 1 {
        filters.pop().expect("single typed filter should exist")
    } else {
        TargetFilter::Or { filters }
    };
    let filter = if exclude_chosen_type {
        TargetFilter::And {
            filters: vec![
                filter,
                TargetFilter::Not {
                    filter: Box::new(TargetFilter::Typed(
                        TypedFilter::default().properties(vec![FilterProp::IsChosenCreatureType]),
                    )),
                },
            ],
        }
    } else {
        filter
    };
    let filter = if let Some(controller) = exclude_owned_by_controller {
        TargetFilter::And {
            filters: vec![
                filter,
                TargetFilter::Not {
                    filter: Box::new(TargetFilter::Typed(
                        TypedFilter::default().properties(vec![FilterProp::Owned { controller }]),
                    )),
                },
            ],
        }
    } else {
        filter
    };

    // CR 406.6: Compose the typed filter with the exile-link constraint when
    // the singular "exiled with ~" suffix was present. Runtime evaluation of
    // `TargetFilter::And` requires every inner filter to match (game/filter.rs
    // line 347), and `extract_in_zone` surfaces `Zone::Exile` from the
    // `ExiledBySource` arm so the resolver scans the correct zone.
    let filter = if exiled_by_source {
        TargetFilter::And {
            filters: vec![filter, TargetFilter::ExiledBySource],
        }
    } else {
        filter
    };

    // CR 201.2a: Compose the typed filter with the chosen-name constraint when
    // the suffix was present. Runtime And-eval requires every inner filter to
    // match (game/filter.rs line 1464/1782); the HasChosenName arm
    // (game/filter.rs line 1604) compares the object's name to the source's
    // ChosenAttribute::CardName.
    let filter = if has_chosen_name {
        TargetFilter::And {
            filters: vec![filter, TargetFilter::HasChosenName],
        }
    } else {
        filter
    };

    let filter = if counters_put_this_way {
        TargetFilter::TrackedSetFiltered {
            id: TrackedSetId(0),
            filter: Box::new(filter),
            // "counters put this way" names objects that received counters but
            // did not change zones — a selection set with no zone binding.
            caused_by: None,
        }
    } else {
        filter
    };

    (filter, &text[pos..])
}

/// Result of classifying a negated word — routes to `type_filters` or `properties`.
enum NegationResult {
    /// Core type/subtype negation → goes into `type_filters`
    Type(TypeFilter),
    /// Color/supertype negation → stays in `properties`
    Prop(FilterProp),
}

/// CR 205.4b: Classify a negated word by semantic layer.
/// `parse_non_prefix` strips "non"/"non-" and lowercases, so `negated` is e.g. "black", "basic", "creature".
fn classify_negation(negated: &str) -> NegationResult {
    if tag::<_, _, OracleError<'_>>("token")
        .parse(negated)
        .is_ok_and(|(rest, _)| rest.is_empty())
    {
        return NegationResult::Prop(FilterProp::NonToken);
    }
    // CR 700.6: "nonhistoric" / "not historic" — historic is a card property,
    // not a subtype, so it must not fall through to `Non(Subtype("Historic"))`.
    if tag::<_, _, OracleError<'_>>("historic")
        .parse(negated)
        .is_ok_and(|(rest, _)| rest.is_empty())
    {
        return NegationResult::Prop(FilterProp::NotHistoric);
    }

    match negated {
        // Color negation — parallel to HasColor
        "white" => NegationResult::Prop(FilterProp::NotColor {
            color: ManaColor::White,
        }),
        "blue" => NegationResult::Prop(FilterProp::NotColor {
            color: ManaColor::Blue,
        }),
        "black" => NegationResult::Prop(FilterProp::NotColor {
            color: ManaColor::Black,
        }),
        "red" => NegationResult::Prop(FilterProp::NotColor {
            color: ManaColor::Red,
        }),
        "green" => NegationResult::Prop(FilterProp::NotColor {
            color: ManaColor::Green,
        }),
        // CR 205.4a: Supertype negation — parallel to HasSupertype
        "basic" => NegationResult::Prop(FilterProp::NotSupertype {
            value: Supertype::Basic,
        }),
        "legendary" => NegationResult::Prop(FilterProp::NotSupertype {
            value: Supertype::Legendary,
        }),
        "snow" => NegationResult::Prop(FilterProp::NotSupertype {
            value: Supertype::Snow,
        }),
        // CR 205.4b: Type/subtype negation → TypeFilter::Non
        _ => {
            let inner = match negated {
                "creature" => TypeFilter::Creature,
                "land" => TypeFilter::Land,
                "artifact" => TypeFilter::Artifact,
                "enchantment" => TypeFilter::Enchantment,
                "instant" => TypeFilter::Instant,
                "sorcery" => TypeFilter::Sorcery,
                "planeswalker" => TypeFilter::Planeswalker,
                other => TypeFilter::Subtype(capitalize_first(other)),
            };
            NegationResult::Type(TypeFilter::Non(Box::new(inner)))
        }
    }
}

/// CR 903.3 + CR 108.3: does `text` start with the "commander"/"commanders"
/// class word (word-bounded)? Commander is not a card type or subtype — it is a
/// per-object `IsCommander` flag recognized by the commander atom in
/// `parse_type_phrase_with_ctx` — so `starts_with_type_phrase_lead` deliberately
/// does not report it. The indefinite-article guard uses this to strip "a "/"an "
/// before a commander subject ("a commander you own", Hellkite Courser).
fn starts_with_commander_word(text: &str) -> bool {
    alt((tag::<_, _, OracleError<'_>>("commanders"), tag("commander")))
        .parse(text)
        .is_ok_and(|(after, _)| after.is_empty() || after.starts_with([' ', ',', '.', ';']))
}

/// Guard: does text start with something `parse_type_phrase` would recognize?
/// Used to prevent comma/and/or recursion on non-type text.
pub(crate) fn starts_with_type_word(text: &str) -> bool {
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
    // parse_type_phrase handles color prefixes internally, but the article guard
    // must recognize them to strip "a "/"an " correctly.
    if let Ok((rest, _)) = nom_primitives::parse_color(text) {
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
    // CR 205.4b: Negated type prefix: "noncreature spell", "nonland permanent",
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
    // CR 700.4 + CR 700.9: "modified <type>" adjective phrase leads a type
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

fn starts_with_type_phrase_lead(text: &str) -> bool {
    let text = text.trim_start();
    starts_with_type_word(text)
        || nom_target::parse_supertype_prefix(text).is_ok()
        || parse_color_prefix(text).is_some()
        || parse_color_quality_prefix(text).is_some()
        || parse_combat_status_prefix(text).is_some()
        // CR 208.1 (#2912): "1/1 creature" leads a type phrase (the P/T
        // designation is followed by a type word).
        || parse_leading_pt_designation(text).is_some()
}

/// Guard for comma/and/or type-list continuations where core-type segments may
/// carry their own article — e.g. "an artifact, a creature, or a land" (Braids,
/// Cabal Minion / issue #847).
/// Guard for a post-comma continuation of an Oxford-comma type list — the
/// segment may carry a leading list conjunction ("or " / "and " / "and/or ")
/// before its type word or article + type word (CR 205.3a: "an Aura,
/// Equipment, or Vehicle spell"). Used by payload truncation logic that must
/// distinguish a ", " continuing a type list from the ", " that begins a
/// trailing clause (issue #5324, Sram, Senior Edificer).
pub(crate) fn starts_with_type_list_continuation(text: &str) -> bool {
    let text = text.trim_start();
    let text = opt(alt((
        tag::<_, _, OracleError<'_>>("and/or "),
        tag("or "),
        tag("and "),
    )))
    .parse(text)
    .map(|(rest, _)| rest)
    .unwrap_or(text);
    starts_with_or_article_type_segment(text)
}

fn starts_with_or_article_type_segment(text: &str) -> bool {
    let text = text.trim_start();
    if let Ok((rest, _)) = alt((tag::<_, _, OracleError<'_>>("an "), tag("a "))).parse(text) {
        return starts_with_article_core_type_segment(rest);
    }
    starts_with_type_phrase_lead(text)
}

/// True when `text` is an article-led BARE-card segment: "a/an card(s) …" with
/// no type word before the "card" noun (e.g. "a card with disturb", Shipwreck
/// Sifters). Such a disjunct is a keyword/predicate-membership branch folded at
/// the trigger layer, NOT a card-*type* union like Overlord of the Balemurk's
/// "a planeswalker card" (#5331) — so the type-disjunction splitter must not
/// swallow it. `parse_card_or_cards_word` word-bounds "card"/"cards", so a typed
/// lead ("a planeswalker card") returns false here.
fn is_article_led_bare_card(text: &str) -> bool {
    alt((tag::<_, _, OracleError<'_>>("an "), tag("a ")))
        .parse(text.trim_start())
        .is_ok_and(|(rest, _)| parse_card_or_cards_word(rest).is_ok())
}

fn starts_with_article_core_type_segment(text: &str) -> bool {
    let text = text.trim_start();
    if parse_core_type(text).0.is_some() {
        return true;
    }
    if let Ok((rest, _)) = nom_primitives::parse_color(text) {
        if let Ok((after_space, _)) = tag::<_, _, OracleError<'_>>(" ").parse(rest) {
            return starts_with_article_core_type_segment(after_space);
        }
    }
    if let Some((_prop, consumed)) = parse_color_quality_prefix(text) {
        return starts_with_article_core_type_segment(&text[consumed..]);
    }
    false
}

fn target_filter_has_meaningful_content(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => !tf.type_filters.is_empty() || !tf.properties.is_empty(),
        TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. } => true,
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_has_meaningful_content)
        }
        _ => false,
    }
}

/// CR 608.2c: True when a typed filter carries a `FilterProp` PREDICATE beyond
/// the bare head type noun (e.g. `Not(AttackedThisTurn)`, `Untapped`, a
/// controller-scoping property). Used by the "each of those <noun> that
/// <predicate>" anaphor to decide whether the trailing predicate must fold into
/// a `TrackedSetFiltered` (frozen set ∩ predicate) rather than collapsing to a
/// bare `TrackedSet` that would drop the predicate.
fn target_filter_carries_predicate_property(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => !tf.properties.is_empty(),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_carries_predicate_property)
        }
        TargetFilter::Not { filter } => target_filter_carries_predicate_property(filter),
        _ => false,
    }
}

fn scope_target_spell_phrase(filter: TargetFilter, phrase: &str) -> TargetFilter {
    if !target_phrase_mentions_spell_word(phrase) {
        return filter;
    }

    scope_spell_targets_to_stack(filter, target_phrase_uses_spell_suffix(phrase))
}

fn target_phrase_mentions_spell_word(phrase: &str) -> bool {
    // CR 109.2 + CR 109.2b: the word "spell" makes a head descriptor mean a spell
    // on the stack, but "this spell" / "that spell" is an anaphoric self-reference
    // to the source object inside a relative clause — NOT the target's head
    // descriptor — so it must not trigger spell-target stack scoping (otherwise "a
    // creature that convoked this spell", Everything Comes to Dust, whose head is
    // the battlefield permanent "a creature" per CR 109.2, would be wrongly
    // rewritten to a stack spell). Any other occurrence ("instant and sorcery
    // spells", "another spell") is a real head-descriptor type noun.
    let mut previous: Option<&str> = None;
    for word in phrase
        .split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, ',' | ';' | '(' | ')'))
        .filter(|word| !word.is_empty())
    {
        if matches!(word, "spell" | "spells") && !matches!(previous, Some("this") | Some("that")) {
            return true;
        }
        previous = Some(word);
    }
    false
}

fn target_phrase_uses_spell_suffix(phrase: &str) -> bool {
    let mut previous = None;
    for word in phrase
        .split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, ',' | ';' | '(' | ')'))
        .filter(|word| !word.is_empty())
    {
        if matches!(word, "spell" | "spells") {
            return previous.is_some_and(|previous| !matches!(previous, "or" | "and/or"));
        }
        previous = Some(word);
    }
    false
}

fn scope_spell_targets_to_stack(filter: TargetFilter, scope_all_typed: bool) -> TargetFilter {
    match filter {
        TargetFilter::Typed(typed)
            if scope_all_typed
                || typed
                    .type_filters
                    .iter()
                    .any(|ty| matches!(ty, TypeFilter::Card)) =>
        {
            stack_spell_filter(typed)
        }
        TargetFilter::Typed(typed) => TargetFilter::Typed(typed),
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| scope_spell_targets_to_stack(filter, scope_all_typed))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|filter| scope_spell_targets_to_stack(filter, scope_all_typed))
                .collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(scope_spell_targets_to_stack(*filter, scope_all_typed)),
        },
        other => other,
    }
}

fn stack_spell_filter(mut typed: TypedFilter) -> TargetFilter {
    typed
        .type_filters
        .retain(|ty| !matches!(ty, TypeFilter::Card));
    typed
        .properties
        .retain(|prop| !matches!(prop, FilterProp::InZone { zone } if *zone == Zone::Stack));

    if typed.type_filters.is_empty() && typed.controller.is_none() && typed.properties.is_empty() {
        TargetFilter::StackSpell
    } else {
        TargetFilter::And {
            filters: vec![TargetFilter::StackSpell, TargetFilter::Typed(typed)],
        }
    }
}

fn distribute_shared_properties(filter: TargetFilter, shared_props: &[FilterProp]) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            for prop in shared_props {
                if !typed
                    .properties
                    .iter()
                    .any(|existing| prop.same_kind(existing))
                {
                    typed.properties.push(prop.clone());
                }
            }
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| distribute_shared_properties(filter, shared_props))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|filter| distribute_shared_properties(filter, shared_props))
                .collect(),
        },
        other => other,
    }
}

/// Returns true when the given property is leg-local (produced by an adjective
/// prefix during `parse_type_phrase` scanning, or by a type-scoped keyword
/// suffix on only the final disjunct) and must NOT distribute back across
/// earlier legs of a comma-OR list. Every other property is assumed to
/// originate from a trailing-suffix parser and is eligible for distribution —
/// e.g., "artifacts and creatures with mana value 2 or less" distributes
/// `CmcLE` back onto the artifact leg, while "Auras, Equipment, and modified
/// creatures you control" must NOT propagate `FilterProp::Modified` to the
/// Aura/Equipment legs.
///
/// CR 115.1: "artifact, enchantment, or creature with flying" binds flying
/// only to the creature disjunct. Spreading `WithKeyword(Flying)` onto the
/// artifact/enchantment legs would require those permanents to have flying and
/// would block activation when only a legal enchantment is present (#2941).
///
/// SHARED LEG-LOCALITY AUTHORITY: this predicate is the single registry of
/// inherently-leg-local `FilterProp`s for BOTH disjunctive grammars — the
/// target-phrase grammar (`parse_type_phrase`) and the search-filter
/// disjunction grammar (`oracle_effect::search::parse_search_filter_disjunction`,
/// CR 701.23a). Every `FilterProp` that an adjective prefix or a type-scoped
/// suffix binds to exactly one disjunct MUST be registered here, or it will be
/// wrongly distributed across earlier `Or` legs and silently break the affected
/// cards (e.g. #2892). When adding a new leg-local search/target prop, add it to
/// this match.
pub(crate) fn is_adjective_prefix_prop(prop: &FilterProp) -> bool {
    matches!(
        prop,
        // CR 700.4 + CR 700.9: "modified [type]" adjective prefix.
        FilterProp::Modified
            // CR 702.112b: "renowned [type]" adjective prefix.
            | FilterProp::Renowned
            // CR 700.6: "historic [type]" adjective prefix.
            | FilterProp::Historic
            | FilterProp::NotHistoric
            // CR 303.4 + CR 301.5: "enchanted [type]" / "equipped [type]".
            | FilterProp::EnchantedBy
            | FilterProp::EquippedBy
            // CR 115.10a: "another [type]" / "other [type]".
            | FilterProp::Another
            // CR 110.5: "tapped [type]" / "untapped [type]".
            | FilterProp::Tapped
            | FilterProp::Untapped
            // CR 702.171b: "saddled [type]" adjective prefix.
            | FilterProp::IsSaddled
            | FilterProp::ProtectorMatches { .. }
            // CR 509.1h: combat-status prefixes "attacking/blocking/unblocked".
            | FilterProp::Attacking { defender: None }
            | FilterProp::Blocking
            | FilterProp::Unblocked
            // CR 105.1 + CR 205.2: color / supertype adjectives.
            | FilterProp::HasColor { .. }
            | FilterProp::ColorCount { .. }
            | FilterProp::NotColor { .. }
            | FilterProp::HasSupertype { .. }
            | FilterProp::NotSupertype { .. }
            // Token qualifier ("creature tokens").
            | FilterProp::Token
            | FilterProp::NonToken
            // CR 702: "<type> with [keyword]" suffixes bind to the type
            // phrase that parsed them — never retroactively onto earlier Or
            // disjuncts ("artifact, enchantment, or creature with flying").
            | FilterProp::WithKeyword { .. }
            | FilterProp::WithoutKeyword { .. }
            | FilterProp::WithoutKeywordKind { .. }
            // CR 702.1: "<type> with [keyword kind]" (e.g. "a card with
            // augment", Clever Combo) is a keyword-membership predicate that
            // binds only to its own disjunct — distributing it onto a sibling
            // leg ("host card with augment") would empty that leg's match set.
            | FilterProp::HasKeywordKind { .. }
            // CR 201.2 / CR 201.2a: a card-name predicate binds only to its own
            // disjunct — distributing `Named` onto a sibling leg ("basic land
            // named jiang yanggu") would empty that leg's match set. Named is
            // inherently leg-local, the same class as HasKeywordKind/WithKeyword.
            // This is defense-in-depth: no current card routes a `Named` leg
            // through the Or distributor (name-disjunction cards either use bare
            // "and", which takes the dual-filter MatchEachFilter path and never
            // reaches this distributor, or carry `Named` on every leg and are
            // deduped by `same_kind`), but excluding it future-proofs the guard.
            | FilterProp::Named { .. }
    )
}

/// Distribute trailing filter properties (Cmc, PtComparison, etc.)
/// from the last `Typed` element in an `Or` filter to all preceding `Typed`
/// elements that lack a property of the same kind.
/// Handles "artifacts and creatures with mana value 2 or less" where only the
/// final type parses the "with mana value N or less/greater" suffix.
///
/// CR 700.4: Only distributes props produced by trailing-suffix parsers. Props
/// produced by adjective prefixes (e.g. FilterProp::Modified from "modified
/// creatures", FilterProp::EnchantedBy from "enchanted creature") are
/// leg-local and retained only on their originating leg. See
/// `is_adjective_prefix_prop`.
///
/// Exposed `pub(crate)` so disjunctive grammars that compose their own `Or` from
/// independently-parsed disjuncts can reuse this shared trailing-suffix
/// distribution instead of duplicating it. In particular the search-filter
/// disjunction grammar (CR 701.23a, "creature, instant, or sorcery card with
/// mana value N", #2892) parses each comma/or segment independently, so only the
/// final segment carries the "with mana value N" suffix — this distributes the
/// `Cmc` prop back onto the earlier `Typed` legs. `is_adjective_prefix_prop` is
/// the shared registry that keeps leg-local props (keyword/name/adjective) from
/// being distributed; every leg-local search prop MUST be registered there.
pub(crate) fn distribute_properties_to_or(filter: TargetFilter) -> TargetFilter {
    let TargetFilter::Or { mut filters } = filter else {
        return filter;
    };

    // Collect trailing-suffix properties from the last Typed element. Filter
    // out adjective-prefix props (CR 700.4, etc.) that are leg-local.
    let trailing_props: Vec<FilterProp> = filters
        .iter()
        .rev()
        .find_map(|f| {
            if let TargetFilter::Typed(TypedFilter { properties, .. }) = f {
                let suffix_props: Vec<FilterProp> = properties
                    .iter()
                    .filter(|p| !is_adjective_prefix_prop(p))
                    .cloned()
                    .collect();
                if suffix_props.is_empty() {
                    None
                } else {
                    Some(suffix_props)
                }
            } else {
                None
            }
        })
        .unwrap_or_default();

    if !trailing_props.is_empty() {
        for f in &mut filters {
            if let TargetFilter::Typed(ref mut typed) = f {
                for prop in &trailing_props {
                    if !typed.properties.iter().any(|p| prop.same_kind(p)) {
                        typed.properties.push(prop.clone());
                    }
                }
            }
        }
    }

    TargetFilter::Or { filters }
}

/// Distribute the controller from the last `Typed` element in an `Or` filter
/// to all preceding `Typed` elements that have `controller: None`.
/// Handles "artifacts, creatures, and lands your opponents control" where only
/// the final type parses the controller suffix.
///
/// Exposed `pub(crate)` so disjunctive grammars that compose their own `Or` from
/// independently-parsed disjuncts (e.g. the trigger-doubler source filter in
/// `oracle_static::evasion`, "a Shaman or another Wizard you control") can reuse
/// the same shared-controller-scope distribution instead of duplicating it.
pub(crate) fn distribute_controller_to_or(filter: TargetFilter) -> TargetFilter {
    let TargetFilter::Or { mut filters } = filter else {
        return filter;
    };

    // Find the controller from the last Typed element (reverse search)
    let controller = filters.iter().rev().find_map(|f| {
        if let TargetFilter::Typed(TypedFilter {
            controller: Some(ref ctrl),
            ..
        }) = f
        {
            Some(ctrl.clone())
        } else {
            None
        }
    });

    if let Some(ctrl) = controller {
        for f in &mut filters {
            if let TargetFilter::Typed(ref mut typed) = f {
                if typed.controller.is_none() {
                    typed.controller = Some(ctrl.clone());
                }
            }
        }
    }

    TargetFilter::Or { filters }
}

/// Backfill the concrete core type onto `Or` legs assembled as `[TypeFilter::Any]`
/// because the type noun appeared only after a later disjunct ("green or white
/// creature" — the "green" leg is built with `Any` before "creature" is parsed,
/// while the final "white creature" leg carries `[Creature]`). Without this, the
/// `Any` leg imposes no type restriction (type_filters are ANDed in
/// game/filter.rs), so a green noncreature would be a legal target.
///
/// CR 105.2 (color is a characteristic) + CR 109.2 (a type-word object
/// description restricts to that card type): the trailing type word binds to
/// EVERY disjunct of the color/adjective disjunction; an `Any`-only leg from a
/// deferred type noun must inherit the concrete core type of the type-bearing leg.
///
/// Source: the UNIQUE non-`[Any]` `type_filters` shared by every type-bearing
/// `Typed` leg. Backfill happens ONLY when the disjunction is unambiguous — i.e.
/// all non-`[Any]` Typed legs agree on the same `type_filters`. Guards:
/// - only an exactly-`[Any]` leg is rewritten (an `[Artifact]` leg in "artifact
///   or creature" is untouched);
/// - if NO leg has a concrete type (genuine "X or Y permanent" where every leg
///   is `[Any]`/`[Permanent]`) there is no source → no-op;
/// - if the type-bearing legs DISAGREE (a compound disjunction like "red or
///   green instant or sorcery spell", whose legs carry `[Instant]` vs
///   `[Sorcery]`), there is no single core type to project onto the bare color
///   legs, so the `[Any]` legs are left unchanged — preserving the prior, looser
///   behavior the runtime relies on (e.g. Wort, the Raidmother granting conspire
///   to a red *instant*). Over-narrowing such a leg to one branch's type
///   ("[Sorcery]") would wrongly exclude the other ("a red instant").
///
/// The common case ("green or white creature" → exactly one type leg `[Creature]`)
/// has a single distinct value and is backfilled onto the bare color legs.
pub(crate) fn distribute_core_type_to_or(filter: TargetFilter) -> TargetFilter {
    let TargetFilter::Or { mut filters } = filter else {
        return filter;
    };
    let mut distinct: Vec<Vec<TypeFilter>> = Vec::new();
    for f in &filters {
        if let TargetFilter::Typed(TypedFilter { type_filters, .. }) = f {
            if type_filters.as_slice() != [TypeFilter::Any] && !distinct.contains(type_filters) {
                distinct.push(type_filters.clone());
            }
        }
    }
    if distinct.len() == 1 {
        let types = &distinct[0];
        for f in &mut filters {
            if let TargetFilter::Typed(ref mut typed) = f {
                if typed.type_filters.as_slice() == [TypeFilter::Any] {
                    typed.type_filters = types.clone();
                }
            }
        }
    }
    TargetFilter::Or { filters }
}

/// CR 109.2 + CR 205.2a + CR 205.3: When a leading `non-` negation scopes a
/// type/subtype disjunction ("non-Lesson instant and sorcery card"), the
/// negated type must bind to every disjunct — not only the first leg parsed
/// before the `and`/`or` connector. Without this, "non-Lesson instant and
/// sorcery" would match any sorcery, including Lessons (issue #1163, Iroh,
/// Grand Lotus).
///
/// Guarded to a single shared negation: if any OTHER leg already carries its
/// own `Non(_)` type filter, the legs are independently negated ("non-Equipment
/// artifact and non-Aura enchantment" — Bello, Bard of the Brambles) and must
/// NOT be cross-contaminated with the first leg's negation. Distributing
/// unconditionally would leak the artifact leg's `Non(Equipment)` onto the
/// enchantment leg (which only wants `Non(Aura)`), silently narrowing Bello's
/// enchantment conjunct to exclude non-Aura-non-Equipment enchantments the
/// Oracle text never excludes.
pub(crate) fn distribute_neg_type_filters_to_or(filter: TargetFilter) -> TargetFilter {
    let TargetFilter::Or { mut filters } = filter else {
        return filter;
    };

    let neg_filters: Vec<TypeFilter> = filters
        .first()
        .and_then(|f| {
            if let TargetFilter::Typed(TypedFilter { type_filters, .. }) = f {
                Some(
                    type_filters
                        .iter()
                        .filter(|tf| matches!(tf, TypeFilter::Non(_)))
                        .cloned()
                        .collect(),
                )
            } else {
                None
            }
        })
        .unwrap_or_default();

    if neg_filters.is_empty() {
        return TargetFilter::Or { filters };
    }

    let other_legs_already_negated = filters.iter().skip(1).any(|f| {
        matches!(f, TargetFilter::Typed(TypedFilter { type_filters, .. })
            if type_filters.iter().any(|tf| matches!(tf, TypeFilter::Non(_))))
    });
    if other_legs_already_negated {
        return TargetFilter::Or { filters };
    }

    for f in filters.iter_mut().skip(1) {
        if let TargetFilter::Typed(ref mut typed) = f {
            for neg in &neg_filters {
                if !typed.type_filters.iter().any(|existing| existing == neg) {
                    typed.type_filters.push(neg.clone());
                }
            }
        }
    }

    TargetFilter::Or { filters }
}

fn parse_core_type(text: &str) -> (Option<TypeFilter>, Option<String>, usize) {
    // Delegate to the shared nom combinator table which handles both singular
    // and plural forms in longest-match-first order.
    if let Ok((rest, tf)) = nom_target::parse_type_filter_word(text) {
        let consumed = text.len() - rest.len();
        return (Some(tf), None, consumed);
    }

    (None, None, 0)
}

/// Parse a controller suffix like " you control", " an opponent controls", " your opponents control".
/// Returns `(ControllerRef, bytes_consumed)` where consumed includes leading whitespace.
///
/// Delegates to `nom_target::parse_controller_suffix` for the common patterns
/// ("you control", "an opponent controls", "your opponents control"), then
/// handles additional patterns not in the shared combinator.
fn parse_controller_suffix(text: &str, ctx: &ParseContext) -> Option<(ControllerRef, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();

    // CR 608.2i + CR 608.2h: Past-tense controller predicates inside look-back
    // aggregates over non-battlefield objects (Oversimplify class: "creatures
    // they controlled that were exiled this way"). These MUST be tried before
    // the present-tense delegate below because `tag("you control")` would
    // match "you controlled" as a prefix and leave "led" stranded —
    // longest-match-first ordering is load-bearing here. Adding a new
    // past-tense form means extending the `alt()`, not the function shape.
    if let Ok((rest, ctrl)) = alt((
        value(
            ControllerRef::You,
            tag::<_, _, OracleError<'_>>("you controlled"),
        ),
        value(
            ControllerRef::Opponent,
            tag::<_, _, OracleError<'_>>("an opponent controlled"),
        ),
        value(
            ControllerRef::Opponent,
            tag::<_, _, OracleError<'_>>("your opponents controlled"),
        ),
        // CR 102.1 + CR 608.2i: past-tense "the active player controlled"
        // look-back. Longest-match-first preserved (no prefix collision with
        // the arms above).
        value(
            ControllerRef::ActivePlayer,
            tag::<_, _, OracleError<'_>>("the active player controlled"),
        ),
    ))
    .parse(trimmed)
    {
        return Some((ctrl, leading_ws + trimmed.len() - rest.len()));
    }
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("they controlled").parse(trimmed) {
        // CR 608.2i + CR 109.5: "They" inside an each-player iteration body
        // binds to the iterating player. `ScopedPlayer` is the typed scope for
        // that iteration; without an explicit `relative_player_scope`, fall
        // back to `ScopedPlayer` (NOT `You`) — at runtime `ScopedPlayer`
        // gracefully degrades to the source controller when no iteration is
        // active (`scoped_player_or_controller`), giving the same behavior as
        // `You` for solo casts while staying correct for per-player loops.
        // Intentionally distinct from the present-tense "they control" arm
        // below: past-tense forms appear only inside look-back aggregates,
        // where each-player iteration is the dominant context.
        let ctrl = ctx
            .relative_player_scope
            .clone()
            .unwrap_or(ControllerRef::ScopedPlayer);
        return Some((ctrl, leading_ws + trimmed.len() - rest.len()));
    }
    // CR 608.2i + CR 109.4: Past-tense sibling of the present-tense
    // "target player controls" / "that player controls" arms below. Same
    // anaphor semantics — the chosen target player or the
    // relative-player-scope anaphor — applied to a look-back filter. Kept
    // here rather than folded into the alt() above because both arms route
    // through `ctx.relative_player_scope`, while the alt() arms emit fixed
    // ControllerRef variants.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("target player controlled").parse(trimmed) {
        return Some((
            ControllerRef::TargetPlayer,
            leading_ws + trimmed.len() - rest.len(),
        ));
    }
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that player controlled").parse(trimmed) {
        let ctrl = ctx
            .relative_player_scope
            .clone()
            .unwrap_or(ControllerRef::ScopedPlayer);
        return Some((ctrl, leading_ws + trimmed.len() - rest.len()));
    }

    // CR 508.1 + CR 608.2c: "its controller controls" / "their controller
    // controls" — the controller of the anaphoric object ("it"). In a trigger
    // subject context the anaphor is the triggering source, whose controller is
    // the triggering player (the active player who declared attackers per
    // CR 508.1, or whichever player the triggering event identifies); otherwise
    // ("it" refers to a chosen parent target) it is that target's controller.
    // The subject discriminator is a verbatim mirror of `resolve_pronoun_target`
    // / `resolve_it_pronoun`, so "its controller" binds to the SAME anaphor as a
    // sibling "shares … with it" clause. Present-tense only; a past-tense
    // look-back ("its controller controlled") would be a new alt() arm.
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("its controller controls"),
        tag("their controller controls"),
    ))
    .parse(trimmed)
    {
        let ctrl = match &ctx.subject {
            Some(subject) if !matches!(subject, TargetFilter::SelfRef | TargetFilter::Any) => {
                ControllerRef::TriggeringPlayer
            }
            _ => ControllerRef::ParentTargetController,
        };
        return Some((ctrl, leading_ws + trimmed.len() - rest.len()));
    }

    // Delegate to nom_filter::parse_zone_controller which handles common patterns,
    // then fall through to additional nom-based patterns.
    if let Ok((rest, ctrl)) = nom_filter::parse_zone_controller(trimmed) {
        return Some((ctrl, leading_ws + trimmed.len() - rest.len()));
    }

    // Additional patterns via nom tag().
    // Note: "target player controls" is handled by `parse_zone_controller` above
    // (single-authority for `ControllerRef::TargetPlayer`).
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that player controls").parse(trimmed) {
        // CR 109.4 + CR 115.1: "that player controls" is a relative reference
        // back to a player introduced earlier in the ability (e.g. the attacked
        // player in a "whenever you attack a player, ... that player controls"
        // trigger). When the surrounding parser set `ctx.relative_player_scope`,
        // emit `ControllerRef::TargetPlayer` so the runtime auto-surfaces a
        // companion `TargetFilter::Player` slot via `effect_references_target_player`
        // (game/ability_utils.rs). Without a scope, fall back to the legacy
        // `ControllerRef::You` behaviour relied on by per-player iteration
        // contexts (`resolve_quantity_scoped`).
        let ctrl = ctx
            .relative_player_scope
            .clone()
            .unwrap_or(ControllerRef::You);
        return Some((ctrl, leading_ws + trimmed.len() - rest.len()));
    }
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("controlled by that player").parse(trimmed)
    {
        let ctrl = ctx
            .relative_player_scope
            .clone()
            .unwrap_or(ControllerRef::You);
        return Some((ctrl, leading_ws + trimmed.len() - rest.len()));
    }
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("they control").parse(trimmed) {
        // "They control" is an anaphoric player reference when the surrounding
        // parser supplies a relative player scope; otherwise keep the legacy
        // ControllerRef::You fallback used by "any opponent may" accepting-
        // player resolution.
        let ctrl = ctx
            .relative_player_scope
            .clone()
            .unwrap_or(ControllerRef::You);
        return Some((ctrl, leading_ws + trimmed.len() - rest.len()));
    }
    None
}

fn parse_token_suffix(text: &str) -> Option<usize> {
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

fn parse_combat_relation_suffix(text: &str) -> Option<(FilterProp, usize)> {
    let (rest, _) = (
        tag::<_, _, OracleError<'_>>(" blocking or blocked by target "),
        tag("creature"),
    )
        .parse(text)
        .ok()?;
    Some((
        FilterProp::CombatRelation {
            relation: CombatRelation::BlockingOrBlockedBy,
            subject: CombatRelationSubject::ParentTarget,
        },
        text.len() - rest.len(),
    ))
}

/// Parse a color adjective prefix: "white ", "blue ", "black ", "red ", "green ".
/// Returns (FilterProp::HasColor, bytes consumed including trailing space).
///
/// Delegates to `nom_primitives::parse_color` for color word recognition,
/// then verifies a trailing space exists (color as adjective, not standalone).
fn parse_color_prefix(text: &str) -> Option<(FilterProp, usize)> {
    let (rest, color) = nom_primitives::parse_color(text).ok()?;
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
fn parse_color_quality_prefix(text: &str) -> Option<(FilterProp, usize)> {
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
fn parse_leading_pt_designation(input: &str) -> Option<(i32, i32, usize)> {
    let (after_power, power) = nom_primitives::parse_number(input).ok()?;
    let (after_slash, _) = tag::<_, _, OracleError<'_>>("/").parse(after_power).ok()?;
    let (after_toughness, toughness) = nom_primitives::parse_number(after_slash).ok()?;
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
/// Delegates to `nom_filter::parse_property_filter` for the common property keywords,
/// then handles "face-down " (hyphenated variant not in the nom combinator).
pub(crate) fn parse_combat_status_prefix(text: &str) -> Option<(FilterProp, usize)> {
    // Try the shared nom property filter combinator for combat/tap status keywords.
    // Filter to only the status properties relevant as type phrase prefixes.
    if let Ok((rest, prop)) = nom_filter::parse_property_filter(text) {
        if matches!(
            prop,
            FilterProp::Unblocked
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

/// CR 508.1b: Postnominal "attacking you" / "attacking your opponents" on a
/// typed phrase ("target creature attacking you"). The prefix form emits
/// `Attacking { defender: None }`; this suffix scopes the defending player.
///
/// An optional "that's "/"that is "/"that are " relative-clause intro before
/// "attacking" is consumed first (CR 608.2c). "Each creature that's attacking
/// one of your opponents" (Oviya) is the relative-clause form of the bare
/// postnominal "creature attacking your opponents"; both resolve to the same
/// `Attacking { defender }` property, so the intro is stripped here rather than
/// forking a second attacking grammar in the `that's`-clause path.
fn parse_attacking_defender_suffix(text: &str) -> Option<(FilterProp, usize)> {
    let trimmed_outer = text.trim_start();
    let trimmed = opt(alt((
        tag::<_, _, OracleError<'_>>("that's "),
        tag("that is "),
        tag("that are "),
    )))
    .parse(trimmed_outer)
    .map(|(rest, _)| rest)
    .unwrap_or(trimmed_outer);

    if let Ok((rest, prop)) = parse_attacking_alone_suffix_status(trimmed) {
        return Some((prop, text.len() - rest.len()));
    }

    for (pattern, defender) in [
        (
            "attacking you or a planeswalker you control",
            ControllerRef::You,
        ),
        (
            "attacking you and/or planeswalkers you control",
            ControllerRef::You,
        ),
        ("attacking you", ControllerRef::You),
        (
            "attacking your opponents and/or planeswalkers they control",
            ControllerRef::Opponent,
        ),
        ("attacking your opponents", ControllerRef::Opponent),
        // CR 508.1b: "attacking one of your opponents" — in a multiplayer
        // attack-multiple-players game each attacker is assigned one defending
        // player; "one of your opponents" scopes the defender to any of the
        // controller's opponents (Oviya, Automech Artisan). Same defender scope
        // as the plural "your opponents" form; the singular phrasing only
        // changes the surface text, not the `ControllerRef::Opponent` mapping.
        ("attacking one of your opponents", ControllerRef::Opponent),
    ] {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(pattern).parse(trimmed) {
            let rest_trim = rest.trim_start();
            // "...attacking you if it's controlled by..." is a target resolution
            // gate, not a defender suffix (Stalking Leonin). Accepting the bare
            // "attacking you" prefix leaves the trailing " if " unrepresented
            // and trips swallowed-clause detection.
            if alt((
                tag::<_, _, OracleError<'_>>("if "),
                tag::<_, _, OracleError<'_>>("unless "),
                tag::<_, _, OracleError<'_>>("and/or "),
                tag::<_, _, OracleError<'_>>("or "),
            ))
            .parse(rest_trim)
            .is_ok()
            {
                continue;
            }
            match rest.chars().next() {
                None | Some('.') | Some(',') | Some(' ') if rest_trim.is_empty() => {
                    return Some((
                        FilterProp::Attacking {
                            defender: Some(defender),
                        },
                        text.len() - rest.len(),
                    ));
                }
                _ => {}
            }
        }
    }
    None
}

/// CR 506.5: "attacking alone" is a combat status distinct from merely
/// attacking a particular defender; runtime evaluation already lives on
/// FilterProp::AttackingAlone.
fn parse_attacking_alone_suffix_status(input: &str) -> OracleResult<'_, FilterProp> {
    let (input, _) = (tag("attacking"), space1, tag("alone")).parse(input)?;
    let (_, _) = parse_attacking_status_clause_boundary(input)?;
    Ok((input, FilterProp::AttackingAlone))
}

fn parse_attacking_status_clause_boundary(input: &str) -> OracleResult<'_, ()> {
    let trimmed = input.trim_start();
    let (_, _) = not(alt((
        tag::<_, _, OracleError<'_>>("if "),
        tag("unless "),
        tag("and/or "),
        tag("or "),
    )))
    .parse(trimmed)?;

    alt((
        value((), eof),
        value((), peek(tag::<_, _, OracleError<'_>>("."))),
        value((), peek(tag(","))),
        value((), (space1, eof)),
    ))
    .parse(input)
}

/// Parse "with power [or toughness] N or less/greater", "with toughness N or
/// less/greater", and "with greater power" suffixes. Returns `(FilterProp,
/// bytes consumed from the original text)`. CR 208 governs P/T comparisons;
/// CR 509.1b covers the source-relative "greater power" form.
///
/// The P/T-comparison grammar (including the disjunctive "power or toughness"
/// form and the optional "base " scope marker per CR 208.4b) is delegated in
/// full to the single shared combinator `nom_filter::parse_pt_comparison`, so
/// this function holds no duplicate grammar — it only handles the source-
/// relative "greater power" leaf and adapts the combinator's `&str` remainder
/// into the byte-offset return contract this call site expects. Used by Arnyn
/// Deathbloom Botanist, Stern Scolding, Leonardo Sewer Samurai, Warping Wail,
/// etc.
fn parse_power_suffix(text: &str, ctx: &mut ParseContext) -> Option<(FilterProp, usize)> {
    let trimmed = text.trim_start();

    // CR 509.1b: "with greater power" — relative to the source object. This is
    // source-relative (not a numeric threshold) and is not part of the shared
    // P/T-comparison combinator, so it is handled here.
    if let Ok((after, _)) = tag::<_, _, OracleError<'_>>("with greater power").parse(trimmed) {
        return Some((FilterProp::PowerGTSource, text.len() - after.len()));
    }

    if let Some((prop @ FilterProp::PtComparison { .. }, consumed)) =
        parse_superlative_property_suffix(text, ctx)
    {
        return Some((prop, consumed));
    }

    // Delegate the full P/T-comparison grammar to the canonical combinator. It
    // consumes the leading "with " itself (optional prefix), so pass `trimmed`.
    // Recompute the consumed-byte offset against the original `text` from the
    // combinator's remainder (`text.len() - rest.len()`).
    let (rest, prop) = nom_filter::parse_pt_comparison(trimmed).ok()?;
    Some((prop, text.len() - rest.len()))
}

fn superlative_property_filter_prop(
    function: AggregateFunction,
    property: ObjectProperty,
    filter: TargetFilter,
) -> FilterProp {
    let value = QuantityExpr::Ref {
        qty: QuantityRef::Aggregate {
            function,
            property,
            filter,
        },
    };
    match property {
        ObjectProperty::ManaValue => FilterProp::Cmc {
            comparator: Comparator::EQ,
            value,
        },
        ObjectProperty::Power => FilterProp::PtComparison {
            stat: PtStat::Power,
            scope: PtValueScope::Current,
            comparator: Comparator::EQ,
            value,
        },
        ObjectProperty::Toughness => FilterProp::PtComparison {
            stat: PtStat::Toughness,
            scope: PtValueScope::Current,
            comparator: Comparator::EQ,
            value,
        },
        // ManaSymbolCount is a zone-aggregated chroma property (`QuantityRef::
        // Aggregate`), never a per-object superlative comparison filter.
        ObjectProperty::ManaSymbolCount(_) => unreachable!(
            "ManaSymbolCount is aggregated via QuantityRef::Aggregate, not a superlative filter"
        ),
    }
}

/// Postnominal superlative qualifier —
/// "with the greatest|highest <power|toughness|mana value> among <type-set> <controller> control(s)".
/// Encoded as a dynamic equality comparison against `QuantityRef::Aggregate`,
/// mirroring the library-search path in
/// `oracle_effect/search.rs::parse_highest_mana_value_library_suffix`.
/// The eligible set after "among " is parsed by the authoritative
/// `parse_type_phrase_with_ctx` combinator (type list + controller suffix).
/// Returns (FilterProp, bytes consumed from the original text).
fn parse_superlative_property_suffix(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<(FilterProp, usize)> {
    let trimmed = text.trim_start();
    // "with the <superlative> <property> among " — the superlative head selects
    // the aggregate direction and the property is the second axis. Factor the
    // 2×3 cross product into two alts (PATTERNS.md §8b).
    // CR 208.1 (power and toughness) + CR 202.3 (mana value): greatest/highest =
    // Max, least/lowest/smallest = Min. Both directions feed the same generic
    // superlative_property_filter_prop, so the runtime (values.min()/max() in
    // game/quantity.rs) and the Sacrifice/Destroy/return resolvers select the
    // constrained object unchanged.
    let (rest, (function, property)) = (
        tag::<_, _, OracleError<'_>>("with the "),
        alt((
            value(
                AggregateFunction::Max,
                alt((tag("greatest "), tag("highest "))),
            ),
            value(
                AggregateFunction::Min,
                alt((tag("least "), tag("lowest "), tag("smallest "))),
            ),
        )),
        alt((
            value(ObjectProperty::Power, tag("power")),
            value(ObjectProperty::Toughness, tag("toughness")),
            value(ObjectProperty::ManaValue, tag("mana value")),
        )),
        tag(" among "),
    )
        .parse(trimmed)
        .map(|(rest, (_, function, property, _))| (rest, (function, property)))
        .ok()?;
    // Delegate the "<type-set> <controller> control(s)" clause to the
    // authoritative type-phrase combinator — it parses the multi-type
    // or/and list, any leading article, and the trailing controller suffix.
    let (eligible, after) = parse_type_phrase_with_ctx(rest, ctx);
    let prop = superlative_property_filter_prop(function, property, eligible);
    Some((prop, text.len() - after.len()))
}

/// Parse "with/that have/that each have mana value N or less" / "… or greater"
/// suffixes, dynamic "with mana value less than or equal to that [type]"
/// patterns, and the superlative "with the greatest/highest mana value among
/// <set>" form.
/// Returns (FilterProp, bytes consumed from the original text).
pub(crate) fn parse_mana_value_suffix(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<(FilterProp, usize)> {
    let trimmed = text.trim_start();
    // CR 202.3: try the more specific superlative head ("with the
    // greatest/highest mana value among ...") before the comparator forms.
    if let Some((prop, consumed)) = parse_superlative_property_suffix(text, ctx) {
        return Some((prop, consumed));
    }
    if let Some((prop, after)) = parse_relative_mana_value_suffix(trimmed) {
        return Some((prop, text.len() - after.len()));
    }

    if let Ok((after, _)) = (
        alt((
            tag::<_, _, OracleError<'_>>("with "),
            tag::<_, _, OracleError<'_>>("that have "),
            tag::<_, _, OracleError<'_>>("that each have "),
        )),
        tag::<_, _, OracleError<'_>>("mana value of "),
        alt((
            tag::<_, _, OracleError<'_>>("the chosen quality"),
            tag::<_, _, OracleError<'_>>("that quality"),
        )),
    )
        .parse(trimmed)
    {
        return Some((
            FilterProp::ManaValueParity {
                parity: ParitySource::LastNamedChoice,
            },
            text.len() - after.len(),
        ));
    }

    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("with mana value "),
        tag::<_, _, OracleError<'_>>("that have mana value "),
        tag::<_, _, OracleError<'_>>("that each have mana value "),
    ))
    .parse(trimmed)
    .ok()?;

    // CR 202.3 + CR 120.3: Dynamic comparisons referencing the triggering event.
    // "that damage" → `EventContextAmount` (damage amount captured at trigger).
    // "that <type>" (e.g. "that creature", "that spell") →
    // `ObjectManaValue { CostPaidObject }` (mana value of the triggering /
    // cost-paid source object per CR 608.2k).
    // Staged checks: first detect "less than" / "greater than", then check for "or equal to".
    type Vbe<'a> = OracleError<'a>;
    let try_dynamic = |rest: &str, is_le: bool| -> Option<(FilterProp, usize)> {
        let kw_tag = if is_le { "less than" } else { "greater than" };
        let (a, _) = tag::<_, _, Vbe>(kw_tag).parse(rest).ok()?;
        let a = a.trim_start();
        let (is_equal, a) = if let Ok((a2, _)) = tag::<_, _, Vbe>("or equal to").parse(a) {
            (true, a2.trim_start())
        } else {
            (false, a)
        };
        // CR 120.3: Anaphoric "that <noun>" forms — bind to the trigger context.
        // CR 119.3: Non-anaphoric quantity-ref forms — bind to a static or
        // game-state quantity ("the number of lands you control",
        // "the number of cards in your graveyard", "the amount of life you
        // gained this turn", etc.). The two forms are mutually exclusive at
        // this position; try anaphoric first, then fall through.
        let (qty, after) = if let Ok((a2, _)) = tag::<_, _, Vbe>("that ").parse(a) {
            // CR 120.3: "that damage" — the damage amount captured by the trigger
            // (DamageDone events stamp `EventContextAmount`).
            if let Ok((a3, _)) = tag::<_, _, Vbe>("damage").parse(a2) {
                (QuantityRef::EventContextAmount, a3)
            } else {
                // Fall back to the type-word arm — "that <type>" where <type> is any
                // single word terminating at punctuation/space (e.g., "creature",
                // "spell"). Uses the source object's mana value.
                let after = a2.find([',', '.', ' ']).map_or(a2, |i| &a2[i..]);
                (
                    QuantityRef::ObjectManaValue {
                        scope: ObjectScope::CostPaidObject,
                    },
                    after,
                )
            }
        } else if let Some((rest, qty)) =
            nom_quantity::parse_quantity_ref
                .parse(a)
                .ok()
                .filter(|(rest, _)| {
                    // CR 119.3 + CR 400.1: Accept the combinator's partial parse
                    // only when the remainder is empty or a trailing zone clause
                    // recognized by `parse_zone_suffix` ("from your graveyard",
                    // "in exile", …). This leaves "the amount of life you lost this
                    // turn from your graveyard" (Betor, Ancestor's Voice) for the
                    // caller's `parse_zone_suffix` pass instead of swallowing it and
                    // failing the whole mana-value suffix — while keeping every
                    // other partial-match phrase on the punctuation-bounded path.
                    // The zone clause is detected via the nom `parse_zone_suffix`
                    // building block, never a `starts_with` string heuristic.
                    let r = rest.trim_start();
                    r.is_empty() || parse_zone_suffix(r).is_some()
                })
        {
            (qty, rest)
        } else {
            // CR 119.3: Generic quantity-ref RHS — extract the phrase up to the
            // next sentence-terminating punctuation and delegate to the shared
            // `parse_quantity_ref` building block. Unlocks Vhal's "the number
            // of study counters removed this way", Beseech the Queen's "the
            // number of lands you control", Bring to Light's "the number of
            // colors of mana spent to cast this spell", etc. The terminator
            // boundary (comma / period / end-of-input) prevents over-consuming
            // into trailing search-and-shuffle clauses ("…, reveal it, put it
            // into your hand" on Beseech the Queen).
            let phrase_end = a.find([',', '.']).unwrap_or(a.len());
            let phrase = &a[..phrase_end];
            let qty = crate::parser::oracle_quantity::parse_quantity_ref(phrase)?;
            (qty, &a[phrase_end..])
        };
        let make_value = |off: i32| {
            if off == 0 {
                QuantityExpr::Ref { qty }
            } else {
                QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Ref { qty }),
                    offset: off,
                }
            }
        };
        let prop = match (is_le, is_equal) {
            (true, true) => FilterProp::Cmc {
                comparator: Comparator::LE,
                value: make_value(0),
            },
            (true, false) => FilterProp::Cmc {
                comparator: Comparator::LE,
                value: make_value(-1),
            },
            (false, true) => FilterProp::Cmc {
                comparator: Comparator::GE,
                value: make_value(0),
            },
            (false, false) => FilterProp::Cmc {
                comparator: Comparator::GE,
                value: make_value(1),
            },
        };
        Some((prop, text.len() - after.len()))
    };
    if let Some(found) = try_dynamic(rest, true) {
        return Some(found);
    }
    if let Some(found) = try_dynamic(rest, false) {
        return Some(found);
    }

    // CR 202.3: Exact dynamic mana-value match — "with mana value equal to
    // <quantity>". The RHS composes through `parse_cda_quantity`, so offsets
    // ("1 plus the sacrificed creature's mana value"), event-context refs
    // ("that damage"), and game-state counts ("the number of lands you
    // control") share the same quantity grammar as CDA/static parsing.
    if let Ok((after_equal_to, _)) = tag::<_, _, OracleError<'_>>("equal to ").parse(rest) {
        let (after_punct, raw_phrase) =
            take_till::<_, _, OracleError<'_>>(|c: char| c == ',' || c == '.')
                .parse(after_equal_to)
                .ok()?;
        let parse_value = |phrase: &str| -> Option<QuantityExpr> {
            let phrase = phrase.trim();
            crate::parser::oracle_quantity::parse_cda_quantity(phrase).or_else(|| {
                parse_mana_value_reference_expr(phrase)
                    .and_then(|(value, after)| after.trim().is_empty().then_some(value))
            })
        };
        // CR 119.3 + CR 400.1 + CR 108.3: Resolve the dynamic quantity, preferring
        // the FULL phrase first. A quantity whose own grammar already includes a
        // zone clause ("the number of cards in your graveyard" → GraveyardSize;
        // "the total power of creatures in your graveyard") must parse whole so it
        // keeps the zone scope that belongs to the *quantity* — pre-cutting at the
        // first zone clause would strip that scope and silently drop the bound.
        //
        // Only when the full phrase is NOT a recognized quantity is a trailing
        // zone clause a separable, owner/controller-scoped clause on the *target*
        // (per-player zones are CR 400.1, keyed by owner CR 108.3). Aether Vial's
        // "the number of charge counters on ~ from your hand" parses only after
        // the "from your hand" tail is cut, leaving it for the caller's
        // `parse_zone_suffix` pass (see `parse_type_phrase_with_ctx`) to attach as
        // `InZone { Hand }` + controller; without the cut the whole tail parsed as
        // one quantity, failed, and dropped the zone scope entirely — letting the
        // resolver collect cards from every player's hand (issue #1980). Cutting
        // only on full-parse failure mirrors the `try_dynamic` branch above, which
        // lets the quantity grammar decide consumption before treating the
        // remainder as a zone suffix. The cut point is the first word-boundary
        // zone clause recognized by the `parse_zone_suffix` building block.
        let resolved = parse_value(raw_phrase)
            .map(|value| (value, after_punct))
            .or_else(|| {
                let (phrase, zone_tail) =
                    nom_primitives::scan_split_at_phrase(raw_phrase, parse_zone_suffix_nom)?;
                let offset = raw_phrase.len() - zone_tail.len();
                parse_value(phrase).map(|value| (value, &after_equal_to[offset..]))
            });
        if let Some((value, after)) = resolved {
            return Some((
                FilterProp::Cmc {
                    comparator: Comparator::EQ,
                    value,
                },
                text.len() - after.len(),
            ));
        }
    }

    // Static "N or less" / "N or greater" — also accepts literal X via
    // `parse_quantity_expr_number`, which emits `QuantityRef::Variable { "X" }`
    // resolved at effect time against the resolving ability's `chosen_x`.
    // CR 107.3a + CR 601.2b: X announced at cast, read at resolution.
    let (after_num_raw, value) = nom_quantity::parse_quantity_expr_number(rest).ok()?;
    let after_num = after_num_raw.trim_start();

    let (prop, after) =
        if let Ok((a, _)) = tag::<_, _, OracleError<'_>>("or greater").parse(after_num) {
            (
                FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value,
                },
                a,
            )
        } else if let Ok((a, _)) = tag::<_, _, OracleError<'_>>("or less").parse(after_num) {
            (
                FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value,
                },
                a,
            )
        } else if let Ok((a, _)) = tag::<_, _, OracleError<'_>>("or ").parse(after_num) {
            let (after, second_value) = nom_quantity::parse_quantity_expr_number(a).ok()?;
            (
                FilterProp::AnyOf {
                    props: vec![
                        FilterProp::Cmc {
                            comparator: Comparator::EQ,
                            value,
                        },
                        FilterProp::Cmc {
                            comparator: Comparator::EQ,
                            value: second_value,
                        },
                    ],
                },
                after,
            )
        } else {
            // CR 202.3: Exact mana value match — "with mana value N" (no "or less"/"or greater").
            (
                FilterProp::Cmc {
                    comparator: Comparator::EQ,
                    value,
                },
                after_num,
            )
        };
    Some((prop, text.len() - after.len()))
}

fn parse_relative_mana_value_suffix(text: &str) -> Option<(FilterProp, &str)> {
    type Vbe<'a> = OracleError<'a>;
    let (rest, comparator) = nom::sequence::preceded(
        tag::<_, _, Vbe>("with "),
        alt((
            value(Comparator::LT, tag::<_, _, Vbe>("lesser mana value")),
            value(Comparator::GT, tag("greater mana value")),
            value(Comparator::LE, tag("equal or lesser mana value")),
            value(Comparator::EQ, tag("the same mana value")),
            value(Comparator::EQ, tag("same mana value")),
        )),
    )
    .parse(text)
    .ok()?;

    let rest = rest.trim_start();
    let (value, after) = if matches!(comparator, Comparator::EQ) {
        let (after_as, _) = tag::<_, _, Vbe>("as ").parse(rest).ok()?;
        parse_mana_value_reference_expr(after_as)?
    } else if let Ok((after_than, _)) = tag::<_, _, Vbe>("than ").parse(rest) {
        parse_mana_value_reference_expr(after_than)?
    } else {
        (
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
            },
            rest,
        )
    };

    Some((FilterProp::Cmc { comparator, value }, after))
}

fn parse_mana_value_reference_expr(text: &str) -> Option<(QuantityExpr, &str)> {
    if let Ok((after, expr)) = parse_mana_value_of_reference_expr(text) {
        return Some((expr, after));
    }

    parse_mana_value_reference_qty(text)
        .map(|(after, qty)| {
            (
                apply_mana_value_reference_offset(QuantityExpr::Ref { qty }, after),
                after,
            )
        })
        .ok()
        .map(|(expr, after)| (expr, consume_mana_value_reference_offset(after)))
}

fn parse_mana_value_of_reference_expr(
    input: &str,
) -> super::oracle_nom::error::OracleResult<'_, QuantityExpr> {
    let (rest, _) = tag("the mana value of ").parse(input)?;
    let (rest, qty) = parse_mana_value_reference_qty(rest)?;
    let expr = apply_mana_value_reference_offset(QuantityExpr::Ref { qty }, rest);
    Ok((consume_mana_value_reference_offset(rest), expr))
}

fn apply_mana_value_reference_offset(expr: QuantityExpr, rest: &str) -> QuantityExpr {
    if parse_mana_value_reference_plus_one(rest).is_ok() {
        QuantityExpr::Offset {
            inner: Box::new(expr),
            offset: 1,
        }
    } else {
        expr
    }
}

fn consume_mana_value_reference_offset(rest: &str) -> &str {
    parse_mana_value_reference_plus_one(rest)
        .map(|(after, _)| after)
        .unwrap_or(rest)
}

fn parse_mana_value_reference_plus_one(
    input: &str,
) -> super::oracle_nom::error::OracleResult<'_, ()> {
    value(
        (),
        nom::sequence::pair(tag(" plus "), alt((tag("one"), tag("1")))),
    )
    .parse(input)
}

fn parse_mana_value_reference_qty(
    input: &str,
) -> super::oracle_nom::error::OracleResult<'_, QuantityRef> {
    type Vbe<'a> = OracleError<'a>;
    alt((
        value(
            QuantityRef::ObjectManaValue {
                scope: ObjectScope::Target,
            },
            alt((
                tag::<_, _, Vbe>("that spell's mana value"),
                tag("that card's mana value"),
                tag("that permanent's mana value"),
                tag("that creature's mana value"),
                tag("the chosen spell's mana value"),
                tag("the chosen card's mana value"),
                tag("the chosen permanent's mana value"),
                tag("the chosen creature's mana value"),
                tag("that spell"),
                tag("that card"),
                tag("that permanent"),
                tag("that creature"),
                tag("the chosen spell"),
                tag("the chosen card"),
                tag("the chosen permanent"),
                tag("the chosen creature"),
            )),
        ),
        value(
            QuantityRef::ObjectManaValue {
                scope: ObjectScope::Source,
            },
            alt((
                tag::<_, _, Vbe>("this spell's mana value"),
                tag("this card's mana value"),
                tag("this creature's mana value"),
                tag("this spell"),
                tag("this card"),
                tag("this creature"),
                tag("~"),
            )),
        ),
        value(
            QuantityRef::ObjectManaValue {
                scope: ObjectScope::CostPaidObject,
            },
            alt((
                tag::<_, _, Vbe>("that spell's mana value"),
                tag("the creature that died"),
                tag("the permanent that died"),
                tag("the creature that entered"),
                tag("the permanent that entered"),
            )),
        ),
        value(
            crate::parser::oracle_quantity::parse_quantity_ref("the mana value of the exiled card")
                .expect("linked exiled-card mana-value quantity must parse"),
            tag::<_, _, Vbe>("the exiled card"),
        ),
        parse_cost_paid_mana_value_reference,
    ))
    .parse(input)
}

fn parse_cost_paid_mana_value_reference(
    input: &str,
) -> super::oracle_nom::error::OracleResult<'_, QuantityRef> {
    let (rest, _) = opt(tag("the ")).parse(input)?;
    let (rest, _) = alt((tag("discarded "), tag("sacrificed "))).parse(rest)?;
    let (rest, _) = alt((
        tag("creature"),
        tag("card"),
        tag("permanent"),
        tag("artifact"),
        tag("enchantment"),
        tag("planeswalker"),
        tag("land"),
    ))
    .parse(rest)?;
    Ok((
        rest,
        QuantityRef::ObjectManaValue {
            scope: ObjectScope::CostPaidObject,
        },
    ))
}

fn parse_bare_any_counter_suffix(input: &str) -> super::oracle_nom::error::OracleResult<'_, ()> {
    let (input, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("any "),
        tag::<_, _, OracleError<'_>>("a "),
    )))
    .parse(input)?;
    let (input, _) = alt((
        tag::<_, _, OracleError<'_>>("counters"),
        tag::<_, _, OracleError<'_>>("counter"),
    ))
    .parse(input)?;
    let (input, _) = alt((
        tag::<_, _, OracleError<'_>>(" on it"),
        tag::<_, _, OracleError<'_>>(" on them"),
    ))
    .parse(input)?;

    Ok((input, ()))
}

/// Parse a counter-presence suffix ("with [count] [counter] counter(s) on
/// it/them", "with no counters on them", "without a +1/+1 counter on it")
/// using pure nom combinators. Returns (FilterProp, bytes consumed).
///
/// `with` is a positive (`Comparator::GE`) threshold; `with no` and `without`
/// are negated (`Comparator::EQ` against 0). `<count>` is either an article
/// ("a"/"an", implying 1) or a quantity expression (literal N or variable X);
/// in the negated branch the count is discarded — negation means exactly 0.
/// The counter axis is `CounterMatch::Any` ("a counter on it" / "no counters")
/// or `CounterMatch::OfType` ("a +1/+1 counter").
///
/// CR 122.1: counter-count predicate. CR 107.3a + CR 601.2b: X counts resolve
/// at effect time against `ResolvedAbility::chosen_x` via
/// `FilterContext::from_ability`.
pub(crate) fn parse_counter_suffix(text: &str) -> Option<(FilterProp, usize)> {
    use nom::branch::alt;
    use nom::bytes::complete::tag as tag_e;
    use nom::combinator::value;

    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();

    // CR 122.1: Leading dispatch — `with` is a positive (GE) threshold, while
    // `without` and `with no` are negated (EQ 0) filters. Longest-match-first:
    // `"with no "` / `"without "` must precede the bare `"with "`.
    let (rest, comparator) = alt((
        value(Comparator::EQ, tag_e::<_, _, OracleError<'_>>("without ")),
        value(Comparator::EQ, tag_e::<_, _, OracleError<'_>>("with no ")),
        value(Comparator::GE, tag_e::<_, _, OracleError<'_>>("with ")),
    ))
    .parse(trimmed)
    .ok()?;
    let lead_len = trimmed.len() - rest.len();

    // The shared counter-spec body, with offsets relative to `rest`. The public
    // entry adds back the leading whitespace and the consumed lead length to
    // preserve the absolute (FilterProp, bytes-from-`text`) contract.
    let (prop, consumed) = parse_counter_spec_after_lead(rest, comparator)?;
    Some((prop, leading_ws + lead_len + consumed))
}

/// CR 122.1 / CR 122.1a: parse the counter spec AFTER the lead is consumed and
/// the comparator decided. `rest` begins at "[a/an/<count>] <type> counter(s) on
/// it/them" / "counter(s) on it/them" / "no counters …". Returns `(FilterProp,
/// bytes consumed from `rest`)`.
///
/// The EQ-vs-GE selection is gated purely on the `comparator` parameter — no
/// lead-specific state leaks in — so both the `with`/`without` entry
/// (`parse_counter_suffix`) and the relative-clause entry
/// (`parse_that_clause_suffix`'s "that has a … counter on it" arm) share this
/// body and produce identical `FilterProp::Counters` shapes.
fn parse_counter_spec_after_lead(
    rest: &str,
    comparator: Comparator,
) -> Option<(FilterProp, usize)> {
    use nom::branch::alt;
    use nom::bytes::complete::{tag as tag_e, take_until};
    use nom::combinator::{opt, value};

    // CR 122.1: Negated branch — untyped FIRST, before any `take_until`. The
    // untyped negated case ("with no counters on them", "without counters")
    // never touches the typed suffix loop, so the empty-`counter_text` guard
    // there is never reached.
    if comparator == Comparator::EQ {
        let untyped = alt((
            tag_e::<_, _, OracleError<'_>>("counters on them"),
            tag_e::<_, _, OracleError<'_>>("counters on it"),
            tag_e::<_, _, OracleError<'_>>("counter on them"),
            tag_e::<_, _, OracleError<'_>>("counter on it"),
            tag_e::<_, _, OracleError<'_>>("counters"),
        ))
        .parse(rest);
        if let Ok((after, _)) = untyped {
            let consumed = rest.len() - after.len();
            return Some((
                FilterProp::Counters {
                    counters: CounterMatch::Any,
                    comparator: Comparator::EQ,
                    count: QuantityExpr::Fixed { value: 0 },
                },
                consumed,
            ));
        }
        // Negated typed case ("without a +1/+1 counter on it"): fall through to
        // the typed suffix loop below. The article-derived count is discarded —
        // negation always means exactly 0 counters of that type.
    } else {
        // CR 122.1: Bare "with a counter on it" / "with counters on them" —
        // any counter of any type. Distinct from typed "with a +1/+1 counter on
        // it". Must precede the typed-counter branch so the empty-counter-type
        // guard there doesn't fire.
        if let Ok((after, _)) = parse_bare_any_counter_suffix(rest) {
            let consumed = rest.len() - after.len();
            return Some((
                FilterProp::Counters {
                    counters: CounterMatch::Any,
                    comparator: Comparator::GE,
                    count: QuantityExpr::Fixed { value: 1 },
                },
                consumed,
            ));
        }
    }

    // Parse count: optional article ("a"/"an" → implicit 1) or an explicit
    // quantity expression followed by a space. Neither branch matching means
    // the counter type follows directly (e.g. "with ice counters on them"),
    // which is implicit count 1. In the negated branch this count is discarded.
    let count_parser = alt((
        value(
            QuantityExpr::Fixed { value: 1 },
            alt((tag_e("an "), tag_e("a "))),
        ),
        |input| {
            let (input, expr) = nom_quantity::parse_quantity_expr_number(input)?;
            let (input, _) = tag_e::<_, _, OracleError<'_>>(" ").parse(input)?;
            Ok((input, expr))
        },
    ));
    let (after_count, count_opt) = opt(count_parser).parse(rest).ok()?;
    let count = count_opt.unwrap_or(QuantityExpr::Fixed { value: 1 });

    // Try each counter suffix; pick the first that matches via `take_until`.
    // `take_until` is pure nom — the counter-type text is everything before the
    // first occurrence of the target suffix.
    for suffix in [
        " counters on them",
        " counters on it",
        " counter on them",
        " counter on it",
    ] {
        let Ok((after, counter_text)) =
            take_until::<_, _, OracleError<'_>>(suffix).parse(after_count)
        else {
            continue;
        };
        let counter_type = counter_text.trim();
        if counter_type.is_empty() {
            continue;
        }
        let consumed = rest.len() - after.len() + suffix.len();
        // CR 122.1: negated typed filter means exactly 0 counters of the type;
        // positive filter is the parsed (or implicit-1) threshold.
        let count = if comparator == Comparator::EQ {
            QuantityExpr::Fixed { value: 0 }
        } else {
            count.clone()
        };
        return Some((
            FilterProp::Counters {
                counters: CounterMatch::OfType(crate::types::counter::parse_counter_type(
                    counter_type,
                )),
                comparator,
                count,
            },
            consumed,
        ));
    }

    None
}

/// CR 122.1 + CR 122.6: Parse the relative-clause body AFTER "that " for the
/// historical counter-placement predicate "[actor] put [count] [type] counters
/// on this turn". `input` begins at the actor word. Returns
/// `(FilterProp::CountersPutOnThisTurn, bytes consumed from `input`)`.
///
/// Axes (all parameterized — covers the class, not just Kid Loki):
/// - actor: "you've"/"you have" → Controller; "an opponent has"/"an opponent's"
///   → Opponents; "a player has"/"a player's" → All.
/// - count: "one or more"/"a"/"an" → GE 1; "<N> or more" → GE N; "<N>" → EQ N.
/// - counters: a typed "+1/+1"/"<name>" → OfType; bare "counters" → Any.
fn parse_counters_put_this_turn_clause(input: &str) -> Option<(FilterProp, usize)> {
    use nom::bytes::complete::take_until;
    use nom::combinator::value;

    type VE<'a> = OracleError<'a>;

    // Actor scope (CR 122.6 + CR 109.5). Longest-match-first within each group.
    let (rest, actor) = alt((
        value(CountScope::Controller, tag::<_, _, VE>("you've put ")),
        value(CountScope::Controller, tag("you have put ")),
        value(CountScope::Opponents, tag("an opponent has put ")),
        value(CountScope::Opponents, tag("an opponent's put ")),
        value(CountScope::Opponents, tag("an opponent\u{2019}s put ")),
        value(CountScope::All, tag("a player has put ")),
        value(CountScope::All, tag("a player's put ")),
        value(CountScope::All, tag("a player\u{2019}s put ")),
    ))
    .parse(input)
    .ok()?;

    // Count threshold (CR 122.6). "one or more"/"a"/"an" all mean GE 1.
    let (rest, (comparator, count)) = alt((
        value((Comparator::GE, 1u32), tag::<_, _, VE>("one or more ")),
        value((Comparator::GE, 1u32), tag("a ")),
        value((Comparator::GE, 1u32), tag("an ")),
        |i| {
            let (i, n) = nom_primitives::parse_number(i)?;
            let (i, _) = tag::<_, _, VE>(" or more ").parse(i)?;
            Ok((i, (Comparator::GE, n)))
        },
        |i| {
            let (i, n) = nom_primitives::parse_number(i)?;
            let (i, _) = tag::<_, _, VE>(" ").parse(i)?;
            Ok((i, (Comparator::EQ, n)))
        },
    ))
    .parse(rest)
    .ok()?;

    // Counter type, then the elided-recipient terminator "counter(s) on this
    // turn". `take_until` grabs the (possibly empty) type text before the
    // terminator; a blank type text means a bare untyped counter
    // (CounterMatch::Any). The terminator carries no leading space so the bare
    // "a counter on this turn" form (no type text) matches as well as the typed
    // "+1/+1 counters on this turn" form (type text + trailing space).
    for suffix in ["counters on this turn", "counter on this turn"] {
        let Ok((after, counter_text)) = take_until::<_, _, VE>(suffix).parse(rest) else {
            continue;
        };
        let counter_type = counter_text.trim();
        let counters = if counter_type.is_empty() {
            CounterMatch::Any
        } else {
            CounterMatch::OfType(crate::types::counter::parse_counter_type(counter_type))
        };
        let consumed = input.len() - after.len() + suffix.len();
        return Some((
            FilterProp::CountersPutOnThisTurn {
                actor,
                counters,
                comparator,
                count,
            },
            consumed,
        ));
    }

    None
}

struct KeywordSuffix {
    properties: Vec<FilterProp>,
    disjunctive: bool,
}

fn parse_keyword_suffix(text: &str) -> Option<(KeywordSuffix, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();
    let (after_with, _) = tag::<_, _, OracleError<'_>>("with ").parse(trimmed).ok()?;
    let mut remaining = after_with;
    let mut consumed = leading_ws + "with ".len();
    let mut properties = Vec::new();
    let mut disjunctive = false;

    while let Some((keyword_match, keyword_len)) = parse_leading_keyword_match(remaining) {
        match keyword_match {
            KeywordMatch::Concrete(keyword) => {
                properties.push(FilterProp::WithKeyword { value: keyword });
            }
            KeywordMatch::Kind(kind) => {
                properties.push(FilterProp::HasKeywordKind { value: kind });
            }
        }
        consumed += keyword_len;
        remaining = &remaining[keyword_len..];

        // Try keyword list separators in longest-match-first order.
        let mut found_sep = false;
        for sep in &[", and ", ", or ", " and ", " or ", ", "] {
            if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(*sep).parse(remaining) {
                if matches!(*sep, ", or " | " or ") {
                    disjunctive = true;
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
        Some((
            KeywordSuffix {
                properties,
                disjunctive,
            },
            consumed,
        ))
    }
}

/// Parse "without [keyword]" suffix — negated keyword filter.
/// Handles "without flying", "without first strike", etc.
/// Parallels `parse_keyword_suffix` but emits `WithoutKeyword`.
pub(crate) fn parse_without_keyword_suffix(text: &str) -> Option<(Vec<FilterProp>, usize)> {
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

/// CR 201.2: Parse a "with the same name as <referent>" filter suffix, mapping
/// the referent class to the matching name-resolution `FilterProp`:
///   * "~" / "this <type>" → the *source* object's name (`FilterProp::SameName`).
///   * "that <type>" → the resolving ability's first object target's name
///     (`FilterProp::SameNameAsParentTarget`). This is the "destroy/exile/return
///     target X and all other Xs with the same name as that X" class — Maelstrom
///     Pulse, the Echoing cycle, Bile Blight, Homing Lightning, Detention Sphere.
///     Without it the secondary mass effect drops the name constraint and
///     degrades into an unconditional board wipe.
fn parse_same_name_suffix(text: &str) -> Option<(FilterProp, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();
    let (rest, _) = tag::<_, _, OracleError<'_>>("with the same name as ")
        .parse(trimmed)
        .ok()?;
    let (after, prop) = alt((
        value(FilterProp::SameName, tag("~")),
        value(
            FilterProp::SameName,
            (tag("this "), parse_same_name_referent_noun),
        ),
        value(
            FilterProp::SameNameAsParentTarget,
            (tag("that "), parse_same_name_referent_noun),
        ),
    ))
    .parse(rest)
    .ok()?;
    Some((prop, leading_ws + (trimmed.len() - after.len())))
}

/// CR 205: The permanent-type noun naming the "same name" referent ("that
/// permanent", "this creature", etc.). The noun only provides grammatical
/// agreement with the target — name matching is by name, not type.
fn parse_same_name_referent_noun(input: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
    alt((
        tag("permanent"),
        tag("creature"),
        tag("artifact"),
        tag("enchantment"),
        tag("planeswalker"),
        tag("land"),
        tag("card"),
    ))
    .parse(input)
}

fn parse_ownership_or_controller_suffix(
    text: &str,
    properties: &mut Vec<FilterProp>,
    controller: &mut Option<ControllerRef>,
    ctx: &ParseContext,
) -> usize {
    let own_ctrl = text.trim_start();
    let own_ctrl_offset = text.len() - own_ctrl.len();
    if tag::<_, _, OracleError<'_>>("you own and control")
        .parse(own_ctrl)
        .is_ok()
    {
        *controller = Some(ControllerRef::You);
        properties.push(FilterProp::Owned {
            controller: ControllerRef::You,
        });
        return own_ctrl_offset + "you own and control".len();
    }
    if tag::<_, _, OracleError<'_>>("you own")
        .parse(own_ctrl)
        .is_ok()
        && tag::<_, _, OracleError<'_>>("you own and")
            .parse(own_ctrl)
            .is_err()
    {
        properties.push(FilterProp::Owned {
            controller: ControllerRef::You,
        });
        return own_ctrl_offset + "you own".len();
    }
    // CR 108.3 + CR 109.4: bare "you don't own"/"you do not own" — negated
    // ownership with no "but" lead (distinct from the "but don't own" block in
    // `parse_type_phrase`, which requires a controller already set). Placed after
    // the affirmative "you own"/"you own and control" arms ("you own" is not a
    // prefix of "you don't own", so no shadowing) and before the anaphoric
    // subject×action block. `Owned { Opponent }` is runtime-evaluated as
    // owner != controller (filter.rs), i.e. "you don't own it". Does NOT set
    // *controller — ownership is independent of control; for "you control N
    // permanents you don't own" the controller is supplied upstream by
    // `inject_controller_you`. (Agent of Treachery #3304.)
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("you don't own"),
        tag::<_, _, OracleError<'_>>("you do not own"),
    ))
    .parse(own_ctrl)
    {
        properties.push(FilterProp::Owned {
            controller: ControllerRef::Opponent,
        });
        return own_ctrl_offset + (own_ctrl.len() - rest.len());
    }
    // CR 108.3: "an opponent owns" — the card belongs to an opponent, used by Eldrazi Processors.
    for phrase in ["an opponent owns", "opponents own"] {
        if tag::<_, _, OracleError<'_>>(phrase).parse(own_ctrl).is_ok() {
            properties.push(FilterProp::Owned {
                controller: ControllerRef::Opponent,
            });
            return own_ctrl_offset + phrase.len();
        }
    }
    // CR 108.3 + CR 109.4: anaphoric ownership suffix, composed as subject ×
    // action so the whole class is one combinator rather than a per-phrase tag.
    // Each subject `tag` maps directly to its owner scope:
    //   "that player owns" → the player chosen as the enclosing ability's target
    //     (Oblivion Sower: "target opponent exiles ... then you may put any
    //     number of land cards that player owns from exile ..."), resolved at
    //     runtime against the first `TargetRef::Player` in `ability.targets`, so
    //     the pool is the cards the *target* player owns — not every card, and
    //     not the controller's own;
    //   "they own"        → the iterating player in each-player effects.
    // Actions are matched longest-first ("own and control" before "owns" before
    // "own"); the trailing "and control" maps to `true` and additionally pins
    // the resolved player as the `*controller` of the filtered objects.
    let subject = alt((
        tag("that player").map(|_| ControllerRef::TargetPlayer),
        tag("they").map(|_| ControllerRef::ScopedPlayer),
    ));
    let action = alt((
        tag("own and control").map(|_| true),
        tag("owns").map(|_| false),
        tag("own").map(|_| false),
    ));
    let parsed: nom::IResult<&str, (ControllerRef, &str, bool), OracleError<'_>> =
        (subject, space1, action).parse(own_ctrl);
    if let Ok((rest, (owner, _, also_control))) = parsed {
        properties.push(FilterProp::Owned {
            controller: owner.clone(),
        });
        if also_control {
            *controller = Some(owner);
        }
        return own_ctrl_offset + (own_ctrl.len() - rest.len());
    }
    // CR 108.3 + CR 701.38d: Passive ownership form "owned by <player-ref>".
    // Expropriate: "choose a permanent owned by the voter" — the voter is the
    // scoped player during per-ballot iteration.  Compositional: every
    // player-ref recognized by the active-voice combinator above is also
    // accepted in the passive voice here.
    let passive_parsed: nom::IResult<&str, (ControllerRef, bool), OracleError<'_>> = (
        tag("owned by "),
        alt((
            tag("the voter").map(|_| ControllerRef::ScopedPlayer),
            tag("that player").map(|_| ControllerRef::TargetPlayer),
            tag("an opponent").map(|_| ControllerRef::Opponent),
            tag("you").map(|_| ControllerRef::You),
        )),
        alt((tag(" and controlled by").map(|_| true), success(false))),
    )
        .map(|(_, owner, also_control)| (owner, also_control))
        .parse(own_ctrl);
    if let Ok((rest, (owner, also_control))) = passive_parsed {
        properties.push(FilterProp::Owned {
            controller: owner.clone(),
        });
        if also_control {
            *controller = Some(owner);
        }
        return own_ctrl_offset + (own_ctrl.len() - rest.len());
    }
    // CR 102.1 + CR 302.6 + CR 508.1a: "the active player has controlled
    // continuously since the beginning of the turn" is a target-selection
    // relative clause (Nettling Imp / Norritt / Arcum's Whistle) — distinct
    // from `parse_controller_suffix`'s past-tense LOOK-BACK arm ("the active
    // player controlled", CR 608.2i, used by look-back aggregates over
    // objects that may have since left the battlefield). This clause instead
    // restricts a LIVE battlefield target: it must both (a) currently be
    // controlled by the active player and (b) have been so controlled
    // without interruption since that player's turn began — the same
    // continuity test CR 508.1a uses to gate which creatures may attack.
    // Both facts are pushed together: `*controller` pins the live
    // ActivePlayer scope, and `FilterProp::ControlledContinuouslySinceTurnBegan`
    // pins the continuity predicate (runtime-evaluated in game/filter.rs as
    // `!obj.summoning_sick`, CR 302.6's summoning-sickness flag). Only the
    // ActivePlayer subject is recognized — no card in the corpus was found
    // using a "you've controlled/you have controlled continuously..." form
    // for this clause, so that variant is not built ahead of a card that
    // needs it. The clause is sequenced from three composable atoms — subject
    // ("the active player"), verb ("has controlled"), and continuity tail
    // ("continuously since the beginning of the turn") — rather than one
    // verbatim tag, mirroring `parse_continuity_exemption_clause` (oracle.rs)
    // and the "owned by" tuple idiom immediately above.
    let active_player_continuity: nom::IResult<&str, (), OracleError<'_>> = (
        tag("the active player"),
        tag(" has controlled"),
        tag(" continuously since the beginning of the turn"),
    )
        .map(|_| ())
        .parse(own_ctrl);
    if let Ok((rest, ())) = active_player_continuity {
        *controller = Some(ControllerRef::ActivePlayer);
        properties.push(FilterProp::ControlledContinuouslySinceTurnBegan);
        return own_ctrl_offset + (own_ctrl.len() - rest.len());
    }

    // CR 109.4 + CR 608.2i: "controlled by a player who <look-back> this turn" —
    // a CONTROLLER PREDICATE on the target (not a single-player controller scope),
    // so it pushes FilterProp::ControllerMatches wrapping the recognized
    // PlayerFilter rather than setting *controller. Object-side bridge into the
    // PlayerFilter enum. Longest-match-first in the verb alt (mirrors the
    // duration guard in oracle_effect/lower.rs).
    // NOTE: the numeric "three or more" is a DEFERRED coverage gap — the bridge
    // carries the combat-damage-by-a-Pirate semantics (≥1); the count threshold
    // is intentionally not enforced yet. Do NOT silently over-narrow.
    if let Ok((rest, pf)) = parse_controller_predicate_clause(own_ctrl) {
        properties.push(FilterProp::ControllerMatches {
            player: Box::new(pf),
        });
        return own_ctrl_offset + (own_ctrl.len() - rest.len());
    }

    let (ctrl, ctrl_len) =
        parse_controller_suffix(text, ctx).map_or((None, 0), |(ctrl, len)| (Some(ctrl), len));
    if ctrl.is_some() {
        *controller = ctrl;
    }
    ctrl_len
}

/// CR 109.4 + CR 608.2i: object-side bridge parsing "controlled by a player who
/// <look-back> this turn" into a `PlayerFilter`. This is the object-axis analogue
/// of the whole `PlayerFilter` enum: it recognizes a controller predicate and
/// hands it to `FilterProp::ControllerMatches`, so ANY player look-back can scope
/// a target's controller. Longest-match-first in the verb `alt`. The trailing
/// " this turn" is required so the whole relative clause is consumed.
///
/// Supported predicates (each a leaf of the `alt`, composed — not enumerated):
///   - "was dealt combat damage by [<N> or more ]<subtype>" →
///     `OpponentDealtDamage { CombatOnly, Some(Typed(subtype)), min_sources: N }`
///     (Admiral Beckett Brass: "by three or more Pirates" → `min_sources = 3`).
///     The "<N> or more" threshold IS enforced (distinct-source count at runtime).
///   - "was dealt combat damage" (no source) → `OpponentDealtDamage {
///     CombatOnly, None, min_sources: 1 }`.
///   - "lost life" → `OpponentLostLife` (sibling unlocked by the bridge).
fn parse_controller_predicate_clause(input: &str) -> OracleResult<'_, PlayerFilter> {
    let (input, _) = tag("controlled by a player who ").parse(input)?;
    // Each leaf leaves the remainder positioned just before the trailing
    // " this turn" (leading space intact), which the outer combinator consumes
    // uniformly so the whole relative clause is required.
    let (input, pf) = alt((
        parse_controller_dealt_combat_damage_by,
        // "was dealt combat damage" with no "by <source>" restriction.
        value(
            PlayerFilter::OpponentDealtDamage {
                kind: DamageKindFilter::CombatOnly,
                source: None,
                min_sources: 1,
            },
            terminated(tag("was dealt combat damage"), peek(tag(" this turn"))),
        ),
        // CR 119.3: "lost life this turn" — sibling unlocked by the same bridge.
        value(
            PlayerFilter::OpponentLostLife,
            terminated(tag("lost life"), peek(tag(" this turn"))),
        ),
    ))
    .parse(input)?;
    let (input, _) = tag(" this turn").parse(input)?;
    Ok((input, pf))
}

/// CR 120.2a + CR 120.9 + CR 608.2i: "was dealt combat damage by [<count> or more ]<subtype>".
/// The optional leading "<N> or more " quantifier is parsed into `min_sources` so
/// the threshold is ENFORCED at runtime (Admiral Beckett Brass: "by three or more
/// Pirates" → `min_sources = 3`, requiring 3 distinct combat-damaging Pirate
/// sources); absence means the historical `min_sources = 1` (any matching source).
/// The subtype phrase (which handles plural head nouns like "Pirates" →
/// `Typed(Pirate)`) is parsed by the shared `parse_target` building block, then
/// isolated from the trailing " this turn".
fn parse_controller_dealt_combat_damage_by(input: &str) -> OracleResult<'_, PlayerFilter> {
    let (input, _) = tag("was dealt combat damage by ").parse(input)?;
    // Optional "<N> or more " count threshold → `min_sources`. `parse_number`
    // handles both digit and English number words ("three" → 3). Absent → 1.
    let (input, min_sources) = opt(terminated(nom_primitives::parse_number, tag(" or more ")))
        .map(|n| n.unwrap_or(1).max(1))
        .parse(input)?;
    // Reuse the shared target-phrase building block for the source subtype; it
    // maps "Pirates" → `Typed(Pirate)` (plural head noun handled) and stops before
    // the trailing " this turn".
    let (source, rest) = parse_target(input);
    // Require the trailing duration so a bare type phrase does not misfire. The
    // remainder is left at " this turn" (leading space intact) so the outer
    // combinator's `tag(" this turn")` consumes it uniformly with the other leaves.
    if peek(tag::<_, _, OracleError<'_>>(" this turn"))
        .parse(rest)
        .is_err()
    {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Tag,
        )));
    }
    Ok((
        rest,
        PlayerFilter::OpponentDealtDamage {
            kind: DamageKindFilter::CombatOnly,
            source: Some(Box::new(source)),
            min_sources,
        },
    ))
}

enum KeywordMatch {
    Concrete(Keyword),
    Kind(KeywordKind),
}

fn parse_leading_keyword_match(text: &str) -> Option<(KeywordMatch, usize)> {
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

pub(crate) fn parse_shared_quality(
    input: &str,
) -> nom::IResult<&str, SharedQuality, OracleError<'_>> {
    alt((
        value(
            SharedQuality::TotalPowerToughness,
            tag("total power and toughness"),
        ),
        value(SharedQuality::Name, tag("names")),
        value(SharedQuality::Name, tag("name")),
        value(SharedQuality::ManaValue, tag("mana values")),
        value(SharedQuality::ManaValue, tag("mana value")),
        value(SharedQuality::Power, tag("powers")),
        value(SharedQuality::Power, tag("power")),
        value(SharedQuality::Toughness, tag("toughnesses")),
        value(SharedQuality::Toughness, tag("toughness")),
        value(SharedQuality::CreatureType, tag("creature types")),
        value(SharedQuality::CreatureType, tag("creature type")),
        value(SharedQuality::CardType, tag("card types")),
        value(SharedQuality::CardType, tag("card type")),
        // CR 110.4: the six permanent types (artifact, battle, creature,
        // enchantment, land, planeswalker) are only a SUBSET of the card types.
        // "share a permanent type" must NOT map to SharedQuality::CardType,
        // because CR 205.2a card types also include non-permanent types like
        // Kindred/Tribal: two permanents sharing only Kindred would wrongly
        // satisfy "share a permanent type". Map to the narrower
        // SharedQuality::PermanentType instead (Role Reversal, Cloudstone Curio).
        value(SharedQuality::PermanentType, tag("permanent types")),
        value(SharedQuality::PermanentType, tag("permanent type")),
        value(SharedQuality::LandType, tag("land types")),
        value(SharedQuality::LandType, tag("land type")),
        value(SharedQuality::Color, tag("colors")),
        value(SharedQuality::Color, tag("color")),
    ))
    .parse(input)
}

fn parse_shared_quality_reference<'a>(
    input: &'a str,
    ctx: &ParseContext,
) -> nom::IResult<&'a str, TargetFilter, OracleError<'a>> {
    // Shared-quality clauses can back-reference the current ability's
    // cost-paid object ("the sacrificed creature"; "the exiled card" for
    // exile-cost abilities), so preserve the caller's cost context.
    if let Ok((rest, filter)) = parse_cost_paid_object_reference(input, ctx) {
        return Ok((rest, filter));
    }

    if let Ok((rest, filter)) = value(
        TargetFilter::TriggeringSource,
        tag::<_, _, OracleError<'_>>("one of the discarded cards"),
    )
    .parse(input)
    {
        return Ok((rest, filter));
    }

    if let Ok((rest, filter)) = value(
        TargetFilter::ParentTarget,
        tag::<_, _, OracleError<'_>>("the discarded card"),
    )
    .parse(input)
    {
        return Ok((rest, filter));
    }

    if let Ok((rest, ())) = parse_word_bounded(input, "it") {
        let mut ctx_mut = ctx.clone();
        return Ok((rest, resolve_pronoun_target(&mut ctx_mut, "it")));
    }

    let (filter, rest) = parse_target(input);
    if matches!(filter, TargetFilter::Any) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    let rest_trimmed = rest.trim_start();
    if let Ok((after_or, sep)) =
        alt((tag::<_, _, OracleError<'_>>("or "), tag(", or "))).parse(rest_trimmed)
    {
        let (filter2, rest2) = parse_target(after_or);
        if !matches!(filter2, TargetFilter::Any) {
            return Ok((
                rest2,
                TargetFilter::Or {
                    filters: vec![filter, filter2],
                },
            ));
        }
        // Fall through: only accept the first leg if the disjunction tail didn't parse.
        let _ = sep;
    }
    Ok((rest, filter))
}

/// CR 608.2k: "the sacrificed/exiled <noun>" — an untargeted reference to the
/// object referred to by this ability's cost. "sacrificed" is always a cost
/// participle. "exiled" is a cost participle ONLY when the enclosing ability
/// carries a non-self exile cost (`ctx.current_ability_exile_cost_zone`);
/// otherwise it is an effect participle and the combinator returns
/// `nom::Err::Error`, so dispatch falls through to the `TRACKED_SET_PHRASES`
/// table, which keeps "the exiled card" → `TrackedSet` for the common
/// effect-exile case.
fn parse_cost_paid_object_reference<'a>(
    input: &'a str,
    ctx: &ParseContext,
) -> nom::IResult<&'a str, TargetFilter, OracleError<'a>> {
    let (rest, _) = opt(tag("the ")).parse(input)?;
    let exile_is_cost = ctx.current_ability_exile_cost_zone.is_some();
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("sacrificed "),
        nom::combinator::verify(tag("exiled "), |_: &str| exile_is_cost),
    ))
    .parse(rest)?;
    let (rest, _) = alt((
        tag("creature"),
        tag("card"),
        tag("permanent"),
        tag("artifact"),
        tag("enchantment"),
        tag("planeswalker"),
        tag("land"),
    ))
    .parse(rest)?;
    Ok((rest, TargetFilter::CostPaidObject))
}

pub(crate) fn parse_zone_changed_this_turn_suffix(
    input: &str,
    to: Option<Zone>,
) -> Option<(FilterProp, usize)> {
    let trimmed = input.trim_start();
    let offset = input.len() - trimmed.len();
    let (rest, from) = (
        tag::<_, _, OracleError<'_>>("that "),
        alt((tag("were "), tag("was "))),
        alt((tag("put "), tag("placed "), tag("moved "))),
        tag("there from "),
        alt((
            value(Zone::Battlefield, tag("the battlefield")),
            value(Zone::Graveyard, tag("a graveyard")),
            value(Zone::Graveyard, tag("your graveyard")),
            value(Zone::Graveyard, tag("graveyard")),
            value(Zone::Exile, tag("exile")),
            value(Zone::Hand, tag("a hand")),
            value(Zone::Hand, tag("your hand")),
            value(Zone::Hand, tag("hand")),
            value(Zone::Library, tag("a library")),
            value(Zone::Library, tag("your library")),
            value(Zone::Library, tag("library")),
        )),
        opt(tag(" this turn")),
    )
        .map(|(_, _, _, _, from, _)| from)
        .parse(trimmed)
        .ok()?;
    Some((
        FilterProp::ZoneChangedThisTurn {
            from: Some(from),
            to,
        },
        offset + trimmed.len() - rest.len(),
    ))
}

fn zone_for_scope(props: &[FilterProp]) -> Option<Zone> {
    props.iter().find_map(|prop| match prop {
        FilterProp::InZone { zone } => Some(*zone),
        FilterProp::InAnyZone { zones } if zones.len() == 1 => zones.first().copied(),
        _ => None,
    })
}

pub(crate) fn parse_shared_quality_clause<'a>(
    input: &'a str,
    ctx: &ParseContext,
) -> nom::IResult<&'a str, FilterProp, OracleError<'a>> {
    type Vbe<'a> = OracleError<'a>;
    let (rest, _) = tag::<_, _, Vbe>("that ").parse(input)?;
    let (rest, relation) = alt((
        value(
            SharedQualityRelation::DoesNotShare,
            alt((
                tag::<_, _, Vbe>("don't share "),
                tag("doesn't share "),
                tag("do not share "),
                tag("does not share "),
            )),
        ),
        |i| {
            let (rest, _) = alt((tag::<_, _, Vbe>("share "), tag("shares "))).parse(i)?;
            let (rest, no_marker) = opt(tag::<_, _, Vbe>("no ")).parse(rest)?;
            let relation = if no_marker.is_some() {
                SharedQualityRelation::DoesNotShare
            } else {
                SharedQualityRelation::Shares
            };
            Ok((rest, relation))
        },
    ))
    .parse(rest)?;
    let (rest, _) = opt(alt((tag::<_, _, Vbe>("a "), tag("at least one ")))).parse(rest)?;
    let (rest, quality) = parse_shared_quality(rest)?;
    let (rest, reference) = opt(nom::sequence::preceded(tag::<_, _, Vbe>(" with "), |i| {
        parse_shared_quality_reference(i, ctx)
    }))
    .parse(rest)?;

    Ok((
        rest,
        FilterProp::SharesQuality {
            quality,
            reference: reference.map(Box::new),
            relation,
        },
    ))
}

pub(crate) fn parse_attachment_kind_disjunction(
    input: &str,
) -> nom::IResult<&str, Vec<AttachmentKind>, OracleError<'_>> {
    // Longest-match-first: handle compound forms before single-kind forms.
    alt((
        value(
            vec![AttachmentKind::Aura, AttachmentKind::Equipment],
            tag("enchanted or equipped"),
        ),
        value(
            vec![AttachmentKind::Equipment, AttachmentKind::Aura],
            tag("equipped or enchanted"),
        ),
        value(vec![AttachmentKind::Aura], tag("enchanted")),
        value(vec![AttachmentKind::Equipment], tag("equipped")),
    ))
    .parse(input)
}

pub(crate) fn attachment_kinds_filter_prop(
    kinds: Vec<AttachmentKind>,
    controller: Option<ControllerRef>,
) -> FilterProp {
    match kinds.as_slice() {
        [kind] => FilterProp::HasAttachment {
            kind: kind.clone(),
            controller,
            exclude_source: crate::types::ability::SourceExclusion::Include,
        },
        _ => FilterProp::HasAnyAttachmentOf { kinds, controller },
    }
}

/// Parse "that [verb phrase]" relative clause suffix on target noun phrases.
///
/// Handles multiple pattern classes:
/// - "that share(s) [a] [quality]" → `SharesQuality`
/// - CR 120.6 + CR 120.9: "that was dealt damage this turn" → `WasDealtDamageThisTurn`
/// - CR 400.7: "that entered (the battlefield) this turn" → `EnteredThisTurn`
/// - CR 508.1a: "that attacked this turn" → `AttackedThisTurn`
/// - CR 509.1a: "that blocked this turn" → `BlockedThisTurn`
/// - CR 301.5 + CR 303.4: "that are enchanted or equipped" → attachment predicate
///
/// Returns `(properties, bytes_consumed)` or `None` if the text doesn't match.
pub(crate) fn parse_that_clause_suffix<'a>(
    text: &'a str,
    ctx: Option<&ParseContext>,
) -> Option<(Vec<FilterProp>, usize)> {
    let default_ctx = ParseContext::default();
    let ctx = ctx.unwrap_or(&default_ctx);
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();

    // CR 303.4 + CR 301.5: "that's enchanted or equipped" / "that's enchanted" /
    // "that's equipped" — relative clause attaching an attachment-presence
    // predicate to the enclosing type phrase. Covers the compound-subject grant
    // class (Reyav, Master Smith; Dogmeat, Ever Loyal). Composes with disjunction
    // via `FilterProp::HasAnyAttachmentOf` (kinds.len() == 2 for the "or" form).
    let intro = alt((
        tag::<_, _, OracleError<'_>>("that's "),
        tag("that is "),
        tag("that are "),
    ))
    .parse(trimmed);
    if let Ok((after_intro, _)) = intro {
        // Note: `parse_that_isnt_subtype_suffix` runs first in `parse_type_phrase`
        // and consumes "that's not …", so this branch only sees positive forms.
        if let Ok((rest, kinds)) = parse_attachment_kind_disjunction(after_intro) {
            // Word-boundary check: the next char must terminate the adjective so
            // we don't false-match e.g. "that's enchanted by something else".
            // Accept end-of-string or any non-alphanumeric terminator.
            let next_char_is_boundary = rest
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_');
            if next_char_is_boundary {
                let consumed = leading_ws + trimmed.len() - rest.len();
                let prop = attachment_kinds_filter_prop(kinds, None);
                return Some((vec![prop], consumed));
            }
        }
    }

    if let Some(parsed) = parse_color_relative_clause_suffix(trimmed, leading_ws) {
        return Some(parsed);
    }

    if let Some(parsed) = parse_supertype_relative_clause_suffix(trimmed, leading_ws) {
        return Some(parsed);
    }

    if let Some(parsed) = parse_historic_relative_clause_suffix(trimmed, leading_ws) {
        return Some(parsed);
    }

    if let Ok((rest, prop)) = parse_shared_quality_clause(trimmed, ctx) {
        let consumed = trimmed.len() - rest.len();
        return Some((vec![prop], leading_ws + consumed));
    }

    let (after_that, _) = tag::<_, _, OracleError<'_>>("that ").parse(trimmed).ok()?;
    let that_len = leading_ws + "that ".len();

    // --- CR 115.9c: "that targets only [filter]" ---
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("targets only ").parse(after_that) {
        let targets_verb_len = "targets only ".len();
        if let Some((props, consumed)) =
            parse_targets_only_constraint(rest, that_len + targets_verb_len)
        {
            return Some((props, consumed));
        }
    }

    // --- CR 115.9b: "that targets [filter]" (.any() semantics) ---
    // Must come AFTER "targets only" check above (longest match first).
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("targets ").parse(after_that) {
        let targets_verb_len = "targets ".len();
        if let Some((props, consumed)) = parse_targets_constraint(rest, that_len + targets_verb_len)
        {
            return Some((props, consumed));
        }
    }

    // --- CR 608.2c (De Morgan): "that didn't <verb> [or <verb>] this turn" ---
    // Negated verb-phrase relative clause. The verbs after "didn't" are
    // present-tense/infinitive ("attack"/"block"/"enter"), distinct from the
    // past-tense positive VERB_PHRASES below ("attacked"/"entered"). Each verb
    // maps to its existing positive FilterProp wrapped in `Not`; a disjunction
    // ("attack or enter") lowers to AND-of-negations because the parsed props
    // are AND-combined in the enclosing TypedFilter ("apply the rules of
    // English", CR 608.2c). Must run BEFORE the positive VERB_PHRASES loop, but
    // there is no collision risk since past-tense and present-tense are disjoint.
    if let Ok((after_neg, _)) = tag::<_, _, OracleError<'_>>("didn't ").parse(after_that) {
        // verb token -> positive FilterProp; longest-match-first
        // ("enter the battlefield" before "enter"), mirroring VERB_PHRASES.
        // CR 508.1a (attack declaration) / CR 509.1a (block declaration) /
        // CR 400.7 (entering the battlefield is a new object).
        static NEG_VERBS: &[(&str, FilterProp)] = &[
            ("attack", FilterProp::AttackedThisTurn { defender: None }),
            ("block", FilterProp::BlockedThisTurn),
            ("enter the battlefield", FilterProp::EnteredThisTurn),
            ("enter", FilterProp::EnteredThisTurn),
        ];
        let parse_neg_verb = |i: &'a str| -> Option<(&'a str, FilterProp)> {
            NEG_VERBS.iter().find_map(|(token, prop)| {
                tag::<_, _, OracleError<'_>>(*token)
                    .parse(i)
                    .ok()
                    .map(|(rest, _)| (rest, prop.clone()))
            })
        };
        if let Some((rest1, prop1)) = parse_neg_verb(after_neg) {
            let mut props = vec![FilterProp::Not {
                prop: Box::new(prop1),
            }];
            // Optional " or <verb>" disjunction (CR 608.2c De Morgan split).
            let after_disjunction = match tag::<_, _, OracleError<'_>>(" or ").parse(rest1) {
                Ok((rest2, _)) => match parse_neg_verb(rest2) {
                    Some((rest3, prop2)) => {
                        props.push(FilterProp::Not {
                            prop: Box::new(prop2),
                        });
                        rest3
                    }
                    None => rest1,
                },
                Err(_) => rest1,
            };
            // Terminator: the canonical form carries the shared " this turn"
            // suffix ("...didn't attack or enter this turn", The Fifth Doctor).
            // Some upstream producers (e.g. the "tap all" target extractor for
            // Angel's Trumpet) strip a trailing duration before the target text
            // reaches here, leaving "...didn't attack" with the duration already
            // removed. Accept either: (a) an explicit " this turn" + boundary, or
            // (b) the verb already sitting at a clause boundary (end-of-string or
            // a "."/"," terminator) with "this turn" stripped upstream. A trailing
            // SPACE is NOT a boundary — it signals continued, unmatched text
            // ("didn't attack a player"), which must not match.
            let consumed_at =
                |remainder: &str| -> usize { leading_ws + trimmed.len() - remainder.len() };
            // (a) explicit " this turn" + word boundary (guards "this turning").
            if let Ok((after_turn, _)) =
                tag::<_, _, OracleError<'_>>(" this turn").parse(after_disjunction)
            {
                let at_boundary = after_turn
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_');
                if at_boundary {
                    return Some((props, consumed_at(after_turn)));
                }
            }
            // (b) duration stripped upstream: verb at a clause boundary.
            let at_clause_boundary = after_disjunction
                .chars()
                .next()
                .is_none_or(|c| c == '.' || c == ',');
            if at_clause_boundary {
                return Some((props, consumed_at(after_disjunction)));
            }
        }
    }

    // CR 122.1 + CR 122.6: "that you've put one or more +1/+1 counters on this
    // turn" — historical counter-placement relative clause (Kid Loki). The
    // relative pronoun is the object of "on" (counters put on THAT creature this
    // turn), so the surface form ends "...on this turn" with the recipient
    // elided. Lowers to `FilterProp::CountersPutOnThisTurn`, distinct from the
    // current-counter `FilterProp::Counters` ("that has a +1/+1 counter on it").
    if let Some((prop, consumed)) = parse_counters_put_this_turn_clause(after_that) {
        return Some((vec![prop], that_len + consumed));
    }

    // CR 122.1 / CR 122.1a: "that has a/an <type> counter on it" / "that have …
    // on them" — relative-clause counter predicate; positive (GE). Reuses the
    // shared counter-spec combinator the with-form (parse_counter_suffix) uses,
    // so the FilterProp::Counters is identical to "creature with a … counter on
    // it" (Crumbling Ashes). The article a/an is consumed inside
    // parse_counter_spec_after_lead, so the lead here is just "has "/"have ".
    // Banewhip Punisher: "Destroy target creature that has a -1/-1 counter on
    // it"; Triad of Fates: "Exile target creature that has a fate counter on it".
    if let Ok((after_verb, _)) = alt((
        tag::<_, _, OracleError<'_>>("has "),
        tag::<_, _, OracleError<'_>>("have "),
    ))
    .parse(after_that)
    {
        if let Some((prop, consumed)) = parse_counter_spec_after_lead(after_verb, Comparator::GE) {
            let verb_len = after_that.len() - after_verb.len();
            return Some((vec![prop], that_len + verb_len + consumed));
        }
    }

    // CR 508.6: "that attacked you this turn" — defender-scoped attack-history
    // relative clause (Jabari's Influence). Mirrors the PERMISSIVE VERB_PHRASES
    // return below (not parse_attacking_defender_suffix, whose terminator/
    // continuation guards would reject the non-empty " and put a -1/-0 counter
    // on it" remainder). The "this turn" in the tag is the boundary; the
    // permissive return leaves the trailing " and …" clause intact so
    // try_split_targeted_compound can auto-chain the follow-on PutCounter.
    // Placed before the bare "attacked this turn" entry — disjoint on the "you"
    // token, so no shadowing. Scoped to `ControllerRef::You` (defer opponent).
    if let Ok((_, _)) = tag::<_, _, OracleError<'_>>("attacked you this turn").parse(after_that) {
        return Some((
            vec![FilterProp::AttackedThisTurn {
                defender: Some(ControllerRef::You),
            }],
            that_len + "attacked you this turn".len(),
        ));
    }

    // --- Verb-phrase patterns: match fixed phrases after "that " ---
    // CR 120.6 + CR 120.9: "that was dealt damage this turn"
    static VERB_PHRASES: &[(&str, FilterProp)] = &[
        (
            "was dealt damage this turn",
            FilterProp::WasDealtDamageThisTurn,
        ),
        (
            "entered the battlefield this turn",
            FilterProp::EnteredThisTurn,
        ),
        ("entered this turn", FilterProp::EnteredThisTurn),
        // Compound "attacked or blocked" must precede individual variants (longest match first).
        (
            "attacked or blocked this turn",
            FilterProp::AttackedOrBlockedThisTurn,
        ),
        (
            "attacked this turn",
            FilterProp::AttackedThisTurn { defender: None },
        ),
        ("blocked this turn", FilterProp::BlockedThisTurn),
        // CR 702.171c: "that saddled it [this turn]" — the creature was tapped to
        // pay the source's saddle cost (recorded in the source's `saddled_by`,
        // cleared at end of turn so "this turn" is implicit). "it" refers to the
        // ability source. Calamity, Galloping Inferno. Longest match first.
        ("saddled it this turn", FilterProp::SaddledSource),
        ("saddled it", FilterProp::SaddledSource),
        // CR 702.51c: "that convoked this spell" / "that convoked it" — the
        // creature was tapped to pay the source spell's convoke cost (recorded in
        // the source's `convoked_creatures`). "it"/"this spell" refer to the
        // source. Everything Comes to Dust. Longest match first.
        ("convoked this spell", FilterProp::ConvokedSource),
        ("convoked it", FilterProp::ConvokedSource),
    ];

    for (phrase, prop) in VERB_PHRASES {
        if let Ok((_, _)) = tag::<_, _, OracleError<'_>>(*phrase).parse(after_that) {
            let total = that_len + phrase.len();
            return Some((vec![prop.clone()], total));
        }
    }

    None
}

fn parse_color_relative_clause_suffix(
    trimmed: &str,
    leading_ws: usize,
) -> Option<(Vec<FilterProp>, usize)> {
    let (after_intro, intro_len) =
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that's ").parse(trimmed) {
            (rest, "that's ".len())
        } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that is ").parse(trimmed) {
            (rest, "that is ".len())
        } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that are ").parse(trimmed) {
            (rest, "that are ".len())
        } else {
            return None;
        };

    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("one or more colors").parse(after_intro) {
        let next_char_is_boundary = rest
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if next_char_is_boundary {
            let consumed = leading_ws + intro_len + after_intro.len() - rest.len();
            return Some((
                vec![FilterProp::ColorCount {
                    comparator: Comparator::GE,
                    count: 1,
                }],
                consumed,
            ));
        }
    }

    // CR 105.2: "that's exactly N colors" → ColorCount{EQ, N}. (Threefold Signal.)
    if let Ok((after_n, _)) = tag::<_, _, OracleError<'_>>("exactly ").parse(after_intro) {
        if let Ok((rest, n)) = nom_primitives::parse_number(after_n) {
            if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" colors").parse(rest) {
                let next_char_is_boundary = rest
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_');
                if let (true, Ok(count)) = (next_char_is_boundary, u8::try_from(n)) {
                    let consumed = leading_ws + intro_len + after_intro.len() - rest.len();
                    return Some((
                        vec![FilterProp::ColorCount {
                            comparator: Comparator::EQ,
                            count,
                        }],
                        consumed,
                    ));
                }
            }
        }
    }

    let (rest, colors) = parse_color_disjunction(after_intro).ok()?;
    let next_char_is_boundary = rest
        .chars()
        .next()
        .is_none_or(|c| !c.is_alphanumeric() && c != '_');
    if colors.is_empty() || !next_char_is_boundary {
        return None;
    }

    let consumed = leading_ws + intro_len + after_intro.len() - rest.len();
    let props = if colors.len() == 1 {
        vec![FilterProp::HasColor { color: colors[0] }]
    } else {
        vec![FilterProp::AnyOf {
            props: colors
                .into_iter()
                .map(|color| FilterProp::HasColor { color })
                .collect(),
        }]
    };
    Some((props, consumed))
}

fn parse_relative_clause_intro(trimmed: &str) -> Option<(&str, usize, bool)> {
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that aren't ").parse(trimmed) {
        Some((rest, "that aren't ".len(), true))
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that isn't ").parse(trimmed) {
        Some((rest, "that isn't ".len(), true))
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that's not ").parse(trimmed) {
        Some((rest, "that's not ".len(), true))
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that are not ").parse(trimmed) {
        Some((rest, "that are not ".len(), true))
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that is not ").parse(trimmed) {
        Some((rest, "that is not ".len(), true))
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that's ").parse(trimmed) {
        Some((rest, "that's ".len(), false))
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that is ").parse(trimmed) {
        Some((rest, "that is ".len(), false))
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that are ").parse(trimmed) {
        Some((rest, "that are ".len(), false))
    } else {
        None
    }
}

/// CR 205.4a: "that's / that is / that are <supertype>" → `HasSupertype`;
/// "that aren't / that isn't / that's not / that are not / that is not
/// <supertype>" → `NotSupertype`. Supertypes are legendary/basic/snow
/// (CR 205.4). Mirrors `parse_color_relative_clause_suffix` and delegates the
/// supertype word to the shared `nom_target::parse_supertype_word` building
/// block. Negation intros are matched before the positive forms
/// (longest-match-first so "that are not" / "that's not" are not partially
/// eaten by "that are " / "that's "). Covers "Exile all nonland permanents that
/// aren't legendary" (Urza's Ruinous Blast) and the legendary/nonlegendary
/// trailing-clause mass-filter class.
fn parse_supertype_relative_clause_suffix(
    trimmed: &str,
    leading_ws: usize,
) -> Option<(Vec<FilterProp>, usize)> {
    let (after_intro, intro_len, negated) = parse_relative_clause_intro(trimmed)?;
    let (rest, supertype) = nom_target::parse_supertype_word(after_intro).ok()?;
    // Word-boundary check: the supertype word must terminate so we don't
    // false-match e.g. "that's basically free" (basic + "ally free").
    let next_char_is_boundary = rest
        .chars()
        .next()
        .is_none_or(|c| !c.is_alphanumeric() && c != '_');
    if !next_char_is_boundary {
        return None;
    }

    let consumed = leading_ws + intro_len + after_intro.len() - rest.len();
    let prop = if negated {
        FilterProp::NotSupertype { value: supertype }
    } else {
        FilterProp::HasSupertype { value: supertype }
    };
    Some((vec![prop], consumed))
}

/// CR 700.6: "that's historic" / "that's not historic" relative clauses on typed
/// mass-filter subjects (Desynchronization: "nonland permanent that's not historic").
fn parse_historic_relative_clause_suffix(
    trimmed: &str,
    leading_ws: usize,
) -> Option<(Vec<FilterProp>, usize)> {
    let (after_intro, intro_len, negated) = parse_relative_clause_intro(trimmed)?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("historic")
        .parse(after_intro)
        .ok()?;
    let next_char_is_boundary = rest
        .chars()
        .next()
        .is_none_or(|c| !c.is_alphanumeric() && c != '_');
    if !next_char_is_boundary {
        return None;
    }

    let consumed = leading_ws + intro_len + after_intro.len() - rest.len();
    let prop = if negated {
        FilterProp::NotHistoric
    } else {
        FilterProp::Historic
    };
    Some((vec![prop], consumed))
}

fn parse_color_disjunction(
    input: &str,
) -> super::oracle_nom::error::OracleResult<'_, Vec<ManaColor>> {
    let (rest, first) = nom_primitives::parse_color(input)?;
    let (rest, mut tail) = many0(preceded_color_separator).parse(rest)?;
    let mut colors = vec![first];
    colors.append(&mut tail);
    Ok((rest, colors))
}

fn preceded_color_separator(input: &str) -> super::oracle_nom::error::OracleResult<'_, ManaColor> {
    // CR 105.2: "black and/or red", "white and/or blue" (Rowan/Will, Scion of …)
    // join two colors disjunctively. The "and/or" forms are matched before the
    // bare "or "/", " separators (longest-match-first) so "black and/or red"
    // does not stop after parsing "black" and stranding " and/or red".
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>(", and/or "),
        tag(" and/or "),
        tag(", or "),
        tag(", "),
        tag(" or "),
    ))
    .parse(input)?;
    nom_primitives::parse_color(rest)
}

/// CR 608.2c + CR 205.2b: "<type> except for <type-1>[, <type-2>]* and <type-N>"
/// — a plain type-list exclusion suffix (Scourglass: "Destroy all permanents
/// except for artifacts and lands"; Elspeth Tirel: "except for lands and
/// tokens"). Distinct from `parse_that_isnt_subtype_suffix`/the "except those
/// that <relative-clause>" suffix in `parse_type_phrase_with_ctx`, which
/// handle predicate-based exclusions, not bare type lists.
///
/// Reuses `classify_negation` per list item — it already produces
/// `TypeFilter::Non(..)`-wrapped types and the matching `FilterProp`s
/// (`NonToken`, `NotColor`, `NotSupertype`, `NotHistoric`) that the
/// `"nonartifact"` prefix-negation loop above already feeds into
/// `neg_type_filters`/`properties`. List items are Oxford-comma-tolerant via
/// the existing `match_mass_union_separator`, reused rather than duplicated.
///
/// Guard: `classify_negation`'s catch-all treats any unrecognized word as a
/// negated Subtype (correct for its "non-<word>" prefix context — CR 205.3
/// subtype negation like "nonZombie" is a real pattern). That fallback is
/// UNSAFE here: "except for Mageta" or "except for commanders" would silently
/// classify as `Non(Subtype("Mageta"))`, which no permanent has, making the
/// exclusion a silent no-op that looks fixed but isn't. This function rejects
/// the whole clause (returns `None`) if any item resolves to a negated
/// Subtype, leaving those cards' existing (unhandled, honestly silent)
/// behavior unchanged rather than mis-firing on a named/designation exception.
fn parse_except_for_type_list_suffix(
    text: &str,
) -> Option<(Vec<TypeFilter>, Vec<FilterProp>, usize)> {
    let (mut rest, _) = tag::<_, _, OracleError<'_>>("except for ")
        .parse(text)
        .ok()?;
    let mut consumed = text.len() - rest.len();
    let mut neg_types = Vec::new();
    let mut props = Vec::new();

    loop {
        let trimmed = rest.trim_start();
        consumed += rest.len() - trimmed.len();
        rest = trimmed;

        let (after_word, word) =
            take_till1::<_, _, OracleError<'_>>(|c: char| !c.is_ascii_alphabetic())
                .parse(rest)
                .ok()?;
        let singular = word.trim_end_matches('s');
        match classify_negation(singular) {
            NegationResult::Type(TypeFilter::Non(inner))
                if matches!(*inner, TypeFilter::Subtype(_)) =>
            {
                // Unrecognized word (name, designation, etc.) — decline the
                // whole clause rather than emit a silently-vacuous exclusion.
                return None;
            }
            NegationResult::Type(tf) => neg_types.push(tf),
            NegationResult::Prop(prop) => props.push(prop),
        }
        consumed += rest.len() - after_word.len();
        rest = after_word;

        match match_mass_union_separator(rest) {
            Some(sep_len) => {
                consumed += sep_len;
                rest = &rest[sep_len..];
            }
            None => break,
        }
    }

    // GitHub #4710 CI catch (Flame Sweep): "each creature except for
    // creatures you control with flying" is a FILTERED-SUBSET exception
    // (creatures you control with flying), not a bare type list — but the
    // first word "creatures" alone is a recognized type, so the loop above
    // greedily accepts it and stops at "you", which isn't a valid separator.
    // Left unchecked, this silently emits `Non(Creature)` alongside the base
    // `Creature` filter, a self-contradictory filter matching nothing. A
    // genuine type-list exception ends the clause outright (Scourglass,
    // Elspeth Tirel both terminate at "."); if trailing text remains beyond
    // optional whitespace, this isn't a type list — decline the whole clause
    // rather than partially apply it, mirroring the Subtype-fallback guard
    // above.
    let trailing = rest.trim_start();
    if !trailing.is_empty() && !trailing.starts_with('.') {
        return None;
    }

    Some((neg_types, props, consumed))
}

/// CR 205.3 + CR 205.4b: "that isn't a <Subtype>" / "that's not a <Subtype>"
/// relative-clause negation suffix. Returns negated type filters to append to
/// the enclosing target's `neg_type_filters`. Mirrors the `non-<Subtype>`
/// prefix pattern but expressed as a trailing relative clause
/// ("target attacking Vampire that isn't a Demon" → `Non(Subtype("Demon"))`).
/// Composable with other suffix parsers — consumes only the "that isn't ..."
/// fragment and leaves the remainder intact.
fn parse_that_isnt_subtype_suffix(text: &str) -> Option<(Vec<TypeFilter>, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();

    // "that isn't" / "that's not" / "that is not" — longest-match-first.
    let (after_neg, neg_len) =
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that isn't ").parse(trimmed) {
            (rest, "that isn't ".len())
        } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that's not ").parse(trimmed) {
            (rest, "that's not ".len())
        } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that is not ").parse(trimmed) {
            (rest, "that is not ".len())
        } else {
            return None;
        };

    // Optional article: "a " / "an " before the subtype.
    let (after_article, article_len) =
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("a ").parse(after_neg) {
            (rest, "a ".len())
        } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("an ").parse(after_neg) {
            (rest, "an ".len())
        } else {
            (after_neg, 0)
        };

    // CR 205.3: Subtype token — delegates to the shared subtype recognizer.
    let (subtype, sub_len) = parse_subtype(after_article)?;
    let total = leading_ws + neg_len + article_len + sub_len;
    Some((
        vec![TypeFilter::Non(Box::new(TypeFilter::Subtype(subtype)))],
        total,
    ))
}

fn is_relative_core_type_filter(type_filter: &TypeFilter) -> bool {
    matches!(
        type_filter,
        TypeFilter::Creature
            | TypeFilter::Land
            | TypeFilter::Artifact
            | TypeFilter::Enchantment
            | TypeFilter::Instant
            | TypeFilter::Sorcery
            | TypeFilter::Planeswalker
            | TypeFilter::Battle
            | TypeFilter::Permanent
            | TypeFilter::Card
    )
}

fn parse_relative_core_type_leg(input: &str) -> OracleResult<'_, TypeFilter> {
    let (input, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("a "),
        tag::<_, _, OracleError<'_>>("an "),
    )))
    .parse(input)?;
    let (rest, type_filter) = nom_target::parse_type_filter_word(input)?;
    if is_relative_core_type_filter(&type_filter) {
        Ok((rest, type_filter))
    } else {
        Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Tag,
        )))
    }
}

fn parse_relative_core_type_separator(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>(", and/or "),
            tag(", and "),
            tag(", or "),
            tag(" and/or "),
            tag(" and "),
            tag(" or "),
            tag(", "),
        )),
    )
    .parse(input)
}

fn parse_relative_core_type_list(input: &str) -> OracleResult<'_, Vec<TypeFilter>> {
    let (mut rest, first) = parse_relative_core_type_leg(input)?;
    let mut type_filters = vec![first];

    while let Ok((after_separator, _)) = parse_relative_core_type_separator(rest) {
        let Ok((after_type, next_type)) = parse_relative_core_type_leg(after_separator) else {
            break;
        };
        type_filters.push(next_type);
        rest = after_type;
    }

    Ok((rest, type_filters))
}

/// Parses a positive relative card-type clause like
/// "that's an artifact, creature, or enchantment" into the trailing card-type
/// list. The caller applies those types as branches against the already-parsed
/// base filter, so shared prefixes like Legendary/Permanent stay attached to
/// every leg.
fn parse_that_is_core_type_suffix(text: &str) -> Option<(Vec<TypeFilter>, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();
    let (after_intro, intro_len, negated) = parse_relative_clause_intro(trimmed)?;
    if negated {
        return None;
    }

    let (rest, type_filters) = parse_relative_core_type_list(after_intro).ok()?;
    let consumed = leading_ws + intro_len + after_intro.len() - rest.len();
    Some((type_filters, consumed))
}

/// CR 205.3 (#2905): the positive counterpart of `parse_that_isnt_subtype_suffix`.
/// Parses a "that's a/an <Subtype> [or a/an <Subtype>]*" relative clause into a
/// single `Subtype` (one subtype) or an `AnyOf` of `Subtype`s (disjunction).
/// "creature you control that's an Ape or a Monkey" →
/// `AnyOf([Subtype("Ape"), Subtype("Monkey")])`, which AND-merges with the
/// Creature core type. Returns the bytes consumed (including leading whitespace).
/// Returns `None` unless the clause names at least one recognized subtype, so
/// color/supertype "that's …" relative clauses are left to their own parsers.
fn parse_that_is_subtype_suffix(text: &str) -> Option<(TypeFilter, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();

    // Positive intro only — negation is handled by `parse_that_isnt_subtype_suffix`.
    let after_intro = if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that's ").parse(trimmed)
    {
        rest
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that is ").parse(trimmed) {
        rest
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("that are ").parse(trimmed) {
        rest
    } else {
        return None;
    };

    // One "[a/an] <Subtype>" leg → `(Subtype, remaining)`.
    let parse_leg = |rest: &'_ str| -> Option<(String, usize)> {
        let after_article = if let Ok((r, _)) = tag::<_, _, OracleError<'_>>("a ").parse(rest) {
            ("a ".len(), r)
        } else if let Ok((r, _)) = tag::<_, _, OracleError<'_>>("an ").parse(rest) {
            ("an ".len(), r)
        } else {
            (0usize, rest)
        };
        let (article_len, body) = after_article;
        let (subtype, sub_len) = parse_subtype(body)?;
        Some((subtype, article_len + sub_len))
    };

    let mut subtypes: Vec<TypeFilter> = Vec::new();
    let (first, first_len) = parse_leg(after_intro)?;
    subtypes.push(TypeFilter::Subtype(first));
    let mut rest = &after_intro[first_len..];

    // Optional " or [a/an] <Subtype>" continuations.
    loop {
        let Ok((after_or, _)) = tag::<_, _, OracleError<'_>>(" or ").parse(rest) else {
            break;
        };
        let Some((next, next_len)) = parse_leg(after_or) else {
            break;
        };
        subtypes.push(TypeFilter::Subtype(next));
        rest = &after_or[next_len..];
    }

    let consumed = leading_ws + (trimmed.len() - rest.len());
    let filter = if subtypes.len() == 1 {
        subtypes.pop().expect("non-empty")
    } else {
        TypeFilter::AnyOf(subtypes)
    };
    Some((filter, consumed))
}

/// CR 115.9c: Parse the constraint after "that targets only ".
/// Returns `(properties_to_add, total_bytes_consumed)`.
///
/// Handles:
/// - "~" / "it" → `TargetsOnly { SelfRef }`
/// - "you" → `TargetsOnly { Typed { controller: You } }` (matches the player)
/// - "a single [type phrase]" → `TargetsOnly { filter }` + `HasSingleTarget`
/// - "a/an [type phrase]" → `TargetsOnly { filter }`
fn parse_targets_only_constraint(
    text: &str,
    prefix_len: usize,
) -> Option<(Vec<FilterProp>, usize)> {
    // Self-reference: "~"
    if let Ok((_, _)) = tag::<_, _, OracleError<'_>>("~").parse(text) {
        let props = vec![FilterProp::TargetsOnly {
            filter: Box::new(TargetFilter::SelfRef),
        }];
        return Some((props, prefix_len + 1));
    }
    // "it" with word boundary
    if parse_word_bounded(text, "it").is_ok() {
        let props = vec![FilterProp::TargetsOnly {
            filter: Box::new(TargetFilter::SelfRef),
        }];
        return Some((props, prefix_len + 2));
    }

    // "you" with word boundary — targets only the controller (a player)
    if parse_word_bounded(text, "you").is_ok() {
        let props = vec![FilterProp::TargetsOnly {
            filter: Box::new(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        }];
        return Some((props, prefix_len + 3));
    }

    // "a single [type phrase or player]" — TargetsOnly + HasSingleTarget
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("a single ").parse(text) {
        let single_len = "a single ".len();
        let (inner_filter, consumed) = parse_targets_only_type_or_player(rest);
        let props = vec![
            FilterProp::TargetsOnly {
                filter: Box::new(inner_filter),
            },
            FilterProp::HasSingleTarget,
        ];
        return Some((props, prefix_len + single_len + consumed));
    }

    // "a/an [type phrase or player]" — TargetsOnly without single constraint
    let article_result =
        nom::branch::alt((tag::<_, _, OracleError<'_>>("a "), tag("an "))).parse(text);
    if let Ok((rest, matched)) = article_result {
        let article_len = matched.len();
        let (inner_filter, consumed) = parse_targets_only_type_or_player(rest);
        let props = vec![FilterProp::TargetsOnly {
            filter: Box::new(inner_filter),
        }];
        return Some((props, prefix_len + article_len + consumed));
    }

    None
}

/// CR 115.9b: Parse the constraint after "that targets ".
/// Returns `(properties_to_add, total_bytes_consumed)`.
///
/// Handles:
/// - "~" / "it" / "this creature" / "this permanent" → `Targets { SelfRef }`
/// - "you" → `Targets { Controller }`
/// - "you or a [type]" → `Targets { Or(Controller, Typed) }`
/// - "one or more [type phrase]" → strip prefix, then parse type phrase
/// - "a/an [type phrase]" → `Targets { filter }`
fn parse_targets_constraint(text: &str, prefix_len: usize) -> Option<(Vec<FilterProp>, usize)> {
    // Strip "one or more " — redundant with .any() semantics
    let (text, extra_len) =
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("one or more ").parse(text) {
            (rest, "one or more ".len())
        } else {
            (text, 0)
        };
    let prefix_len = prefix_len + extra_len;

    // Self-reference: "~"
    if let Ok((_, _)) = tag::<_, _, OracleError<'_>>("~").parse(text) {
        let props = vec![FilterProp::Targets {
            filter: Box::new(TargetFilter::SelfRef),
        }];
        return Some((props, prefix_len + 1));
    }
    // "it" with word boundary
    if parse_word_bounded(text, "it").is_ok() {
        let props = vec![FilterProp::Targets {
            filter: Box::new(TargetFilter::SelfRef),
        }];
        return Some((props, prefix_len + 2));
    }

    // Self-reference: "this creature" / "this permanent" with word boundary
    for phrase in ["this creature", "this permanent"] {
        if parse_word_bounded(text, phrase).is_ok() {
            let props = vec![FilterProp::Targets {
                filter: Box::new(TargetFilter::SelfRef),
            }];
            return Some((props, prefix_len + phrase.len()));
        }
    }

    // "you or a [type]" / "you or an [type]" — compound controller + typed filter
    let lower = text.to_lowercase();
    let you_or_result =
        nom::branch::alt((tag::<_, _, OracleError<'_>>("you or an "), tag("you or a ")))
            .parse(lower.as_str());
    if let Ok((_, matched)) = you_or_result {
        let you_or_len = matched.len();
        let after_you_or = &text[you_or_len..];
        let (type_filter, remainder) = parse_type_phrase(after_you_or);
        let consumed = after_you_or.len() - remainder.len();
        let combined = TargetFilter::Or {
            filters: vec![TargetFilter::Controller, type_filter],
        };
        let props = vec![FilterProp::Targets {
            filter: Box::new(combined),
        }];
        return Some((props, prefix_len + you_or_len + consumed));
    }

    // "you" — targets the controller (a player), with word boundary
    if parse_word_bounded(lower.as_str(), "you").is_ok() {
        let props = vec![FilterProp::Targets {
            filter: Box::new(TargetFilter::Controller),
        }];
        return Some((props, prefix_len + 3));
    }

    // "a/an [type phrase or player]" — parse type, using the same helper as TargetsOnly
    let article_result =
        nom::branch::alt((tag::<_, _, OracleError<'_>>("a "), tag("an "))).parse(text);
    if let Ok((rest, matched)) = article_result {
        let article_len = matched.len();
        let (inner_filter, consumed) = parse_targets_only_type_or_player(rest);
        let props = vec![FilterProp::Targets {
            filter: Box::new(inner_filter),
        }];
        return Some((props, prefix_len + article_len + consumed));
    }

    // Bare type phrase (no article) — e.g., "creatures you control"
    let (filter, remainder) = parse_type_phrase(text);
    let consumed = text.len() - remainder.len();
    if consumed > 0 {
        let props = vec![FilterProp::Targets {
            filter: Box::new(filter),
        }];
        return Some((props, prefix_len + consumed));
    }

    None
}

/// Parse the type-or-player constraint inside "that targets only a [single] ...".
/// Handles "player" as `TargetFilter::Player` and "[type] or player" as
/// `Or(Typed(type), Player)`, since `parse_type_phrase` doesn't recognize "player".
fn parse_targets_only_type_or_player(text: &str) -> (TargetFilter, usize) {
    // Check for bare "player" at start with word boundary
    if parse_word_bounded(text, "player").is_ok() {
        return (TargetFilter::Player, 6);
    }

    // Check for "[type] or player" — parse_type_phrase would consume "or" as part of
    // its compound type handling, but "player" isn't a card type, producing a broken filter.
    // Intercept this pattern: find "or player" in the text, parse only the part before it,
    // then compose with TargetFilter::Player.
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    if let Some(or_pos) = tp.find(" or player") {
        let end = or_pos + " or player".len();
        // Only match if "or player" is followed by a delimiter or end of string
        let after = &text[end..];
        match after.chars().next() {
            None | Some(',' | '.' | ' ') => {
                let type_part = tp.split_at(or_pos).0.original;
                let (type_filter, _) = parse_type_phrase(type_part);
                let combined = TargetFilter::Or {
                    filters: vec![type_filter, TargetFilter::Player],
                };
                return (combined, end);
            }
            _ => {}
        }
    }

    let (filter, remainder) = parse_type_phrase(text);
    let consumed = text.len() - remainder.len();
    (filter, consumed)
}

fn typed(
    card_type: TypeFilter,
    subtype: Option<String>,
    properties: Vec<FilterProp>,
    extra_type_filters: Vec<TypeFilter>,
) -> TargetFilter {
    let mut type_filters = vec![card_type];
    if let Some(s) = subtype {
        type_filters.push(TypeFilter::Subtype(s));
    }
    type_filters.extend(extra_type_filters);
    TargetFilter::Typed(TypedFilter {
        type_filters,
        controller: None,
        properties,
    })
}

/// Parse "the top/bottom [N] [type] card[s] of [possessive] library/graveyard".
///
/// Returns a `TargetFilter::Typed` with `InZone` for the referenced zone and the
/// appropriate controller. Matches zone position references that appear as targets
/// in exile/mill/reveal effects (e.g., "exile the top card of each player's library").
///
/// The remainder includes any trailing text after the zone word (e.g., " face down").
fn parse_zone_position_ref<'a>(text: &'a str, lower: &str) -> Option<(TargetFilter, &'a str)> {
    // Must start with "the top " or "the bottom "
    let (after_position, _is_top) =
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("the top ").parse(lower) {
            (rest, true)
        } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("the bottom ").parse(lower) {
            (rest, false)
        } else {
            return None;
        };

    // Optional number: "three ", "two ", "x ", etc. — skip it, we only care about the zone.
    let after_number = if let Ok((rest, _)) = nom_primitives::parse_number_or_x(after_position) {
        rest.trim_start()
    } else {
        after_position
    };

    // Optional type word before "card"/"cards": "creature card", "instant card", etc.
    // CR 109.2a: "creature card" and similar descriptions restrict which
    // cards qualify in the stated zone, so preserve the type word instead of
    // only consuming it.
    let (after_type, type_filter) =
        if let Ok((rest, tf)) = nom_target::parse_type_filter_word(after_number) {
            let trimmed = rest.trim_start();
            // Only consume if followed by "card"/"cards" (not standalone)
            if parse_card_or_cards_word(trimmed).is_ok() {
                let captured = if matches!(tf, TypeFilter::Card) {
                    None
                } else {
                    Some(tf)
                };
                (trimmed, captured)
            } else {
                (after_number, None)
            }
        } else {
            (after_number, None)
        };

    // Required "card"/"cards" — may be followed by " of [zone]" or be standalone
    let (after_card, card_is_terminal) = if let Ok((rest, _)) = parse_card_or_cards_word(after_type)
    {
        let trimmed = rest.trim_start();
        (
            rest,
            trimmed.is_empty() || tag::<_, _, OracleError<'_>>("of ").parse(trimmed).is_err(),
        )
    } else {
        return None;
    };

    // Standalone "the top [N] cards" — default to your library
    if card_is_terminal {
        let consumed = lower.len() - after_card.len();
        return Some((
            TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                type_filters: type_filter.into_iter().collect(),
                properties: vec![FilterProp::InZone {
                    zone: Zone::Library,
                }],
            }),
            &text[consumed..],
        ));
    }

    // "of " followed by possessive + zone
    let after_of = tag::<_, _, OracleError<'_>>("of ")
        .parse(after_card.trim_start())
        .ok()?
        .0;

    // Possessive + zone word: "your library", "their library", "each player's library"
    // Try possessive first, then zone word
    let zone_words: &[(&str, &str, Zone)] = &[
        ("library", "libraries", Zone::Library),
        ("graveyard", "graveyards", Zone::Graveyard),
    ];

    // Check "each player's" / "each opponent's" / "target player's" / "target opponent's"
    let (controller, after_possessive) =
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("each player's ").parse(after_of) {
            (None, rest) // All players
        } else if let Ok((rest, _)) = alt((
            tag::<_, _, OracleError<'_>>("each opponent's "),
            tag("each opponents' "),
        ))
        .parse(after_of)
        {
            (Some(ControllerRef::Opponent), rest)
        } else if let Ok((rest, _)) = alt((
            tag::<_, _, OracleError<'_>>("target player's "),
            tag("target opponent's "),
        ))
        .parse(after_of)
        {
            (None, rest) // Targeted player — resolved at runtime
        } else if let Some((_, rest)) = strip_possessive(after_of) {
            // Generic possessive: "your library", "their library"
            let ctrl = if tag::<_, _, OracleError<'_>>("your ")
                .parse(after_of)
                .is_ok()
            {
                Some(ControllerRef::You)
            } else {
                None
            };
            (ctrl, rest)
        } else {
            return None;
        };

    // Required zone word.
    let type_filters_vec: Vec<TypeFilter> = type_filter.into_iter().collect();
    for &(zone_word, zone_plural, ref zone) in zone_words {
        for word in [zone_word, zone_plural] {
            if let Ok((zone_rest, _)) = tag::<_, _, OracleError<'_>>(word).parse(after_possessive) {
                let consumed = lower.len() - zone_rest.len();
                return Some((
                    TargetFilter::Typed(TypedFilter {
                        controller,
                        type_filters: type_filters_vec.clone(),
                        properties: vec![FilterProp::InZone { zone: *zone }],
                    }),
                    &text[consumed..],
                ));
            }
        }
    }

    None
}

/// Preposition introducing a zone phrase. `On` is only legal for `Zone::Battlefield`
/// (CR 400.1: "on the battlefield"); other zones use `From` / `In`.
#[derive(Copy, Clone, PartialEq)]
enum ZonePrep {
    From,
    In,
    On,
}

/// Qualifier preceding the zone word. Distinguishes ownership-bearing qualifiers
/// ("an opponent's", "your") from plain determiners ("a", "the") and bare forms.
/// The `Bare` variant is a zero-width match, so `parse_zone_qual` always succeeds.
#[derive(Copy, Clone, PartialEq)]
enum ZoneQual {
    /// "an opponent's ", "each opponent's " — produces `Owned{Opponent}`.
    Opponent,
    /// "your " — sets `ControllerRef::You` on the parent filter.
    You,
    /// "target player's " — produces `Owned{TargetPlayer}`.
    TargetPlayer,
    /// "their " — produces `Owned{ScopedPlayer}`; in an each-player iteration
    /// the third-person possessive binds to the iterated player.
    Their,
    /// "its owner's ", "that player's ", "defending player's ", "each player's ".
    /// No ownership constraint emitted; referent is resolved by context upstream.
    OtherPoss,
    /// "the chosen player's " — the player persisted on the source via an earlier
    /// "choose a player" (Haunting Apparition: "green creature cards in the chosen
    /// player's graveyard"). Sets `ControllerRef::SourceChosenPlayer`, mirroring
    /// how `You` sets `ControllerRef::You`; CR 613.1 resolves it against the
    /// source's persisted choice.
    ChosenPlayer,
    /// "a ", "the ", or nothing (e.g., "from exile").
    Plain,
}

/// Scan `text` for the first zone phrase recognized by `parse_zone_suffix`, trying
/// position 0 and each subsequent word boundary (space-separated). Returns
/// `(Zone, Option<ControllerRef>, Vec<FilterProp>)` on the first successful parse.
///
/// Callers that already know the phrase is at the start should call `parse_zone_suffix`
/// directly; this scanner is for callers whose input has a subject before the zone
/// phrase (e.g., conditions like "this creature in your graveyard").
///
/// The returned `Zone` is extracted from the `FilterProp::InZone` entry (always present
/// in a successful parse), so callers that only need the zone don't have to pattern-match
/// the returned `Vec<FilterProp>`.
pub(crate) fn scan_zone_phrase(
    text: &str,
) -> Option<(Zone, Option<ControllerRef>, Vec<FilterProp>)> {
    let mut offset = 0;
    while offset <= text.len() {
        if let Some((props, ctrl, _consumed)) = parse_zone_suffix(&text[offset..]) {
            let zone = props.iter().find_map(|p| match p {
                FilterProp::InZone { zone } => Some(*zone),
                _ => None,
            })?;
            return Some((zone, ctrl, props));
        }
        match text[offset..].find(' ') {
            Some(i) => offset += i + 1,
            None => break,
        }
    }
    None
}

/// Parse a zone suffix like "card from a graveyard", "from your graveyard", "from exile".
///
/// Combinator structure (BNF): `[ "card" | "cards" ] prep qual zone_word`
/// - `prep`     ∈ { from, in, on }
/// - `qual`     ∈ { opponent-poss, your, other-poss, a, the, ε }
/// - `zone_word`∈ { battlefield(s), graveyard(s), exile(s), hand(s), library/libraries }
///
/// Each axis is a single `alt()` — variants are never expanded combinatorially.
///
/// Handles owner semantics for player-specific non-battlefield zones:
/// - Opponent possessive: "from an opponent's graveyard", "from each opponent's graveyard"
///   → `[Owned{Opponent}, InZone]` so stolen creatures that died are still matched by owner.
/// - Your: "from your graveyard" → `InZone` + `ControllerRef::You`.
/// - Target player's: "from target player's graveyard" → `[Owned{TargetPlayer}, InZone]`
///   so the card selection is constrained by the companion player target.
/// - "Their": "from their graveyard" → `[Owned{ScopedPlayer}, InZone]` so in an
///   each-player iteration the candidate set is scoped to the iterated player's
///   own graveyard (CR 110.1/108.3: membership is owner-keyed).
/// - Other possessive / indefinite / definite / bare: → `InZone` alone.
pub(crate) fn parse_zone_suffix(
    text: &str,
) -> Option<(Vec<FilterProp>, Option<ControllerRef>, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();
    let lower = trimmed.to_lowercase();

    let (rest, (props, ctrl)) = parse_zone_suffix_nom(&lower).ok()?;
    let consumed = lower.len() - rest.len();
    Some((props, ctrl, leading_ws + consumed))
}

/// CR 601.2a: The zones a spell can be cast from, excluding the named allowed
/// zone. Used for "from anywhere other than <zone>" cast-origin predicates.
pub(crate) fn cast_capable_zones_except(allowed: Zone) -> Vec<Zone> {
    const CAST_CAPABLE_ZONES: [Zone; 5] = [
        Zone::Hand,
        Zone::Graveyard,
        Zone::Library,
        Zone::Exile,
        Zone::Command,
    ];
    CAST_CAPABLE_ZONES
        .into_iter()
        .filter(|zone| *zone != allowed)
        .collect()
}

fn parse_zone_suffix_nom(
    i: &str,
) -> super::oracle_nom::error::OracleResult<'_, (Vec<FilterProp>, Option<ControllerRef>)> {
    let (i, _) = opt(alt((tag("cards "), tag("card ")))).parse(i)?;
    let (i, prep) = alt((
        value(ZonePrep::From, tag("from ")),
        value(ZonePrep::In, tag("in ")),
        value(ZonePrep::On, tag("on ")),
    ))
    .parse(i)?;
    let (i, qual) = parse_zone_qual(i)?;
    let (i, zone) = parse_zone_word(i)?;
    let (i, _) = peek_zone_boundary(i)?;

    // CR 400.1: only the battlefield is referred to with "on"; "on <other zone>" is not
    // valid Oracle text, so reject it here rather than emitting a misleading filter.
    if prep == ZonePrep::On && zone != Zone::Battlefield {
        return Err(nom::Err::Error(nom::error::Error::new(
            i,
            nom::error::ErrorKind::Fail,
        )));
    }

    // Check for zone disjunction: "or in <zone>" or "or on <zone>" or "or from <zone>"
    let (i, zones) = if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(" or ").parse(i) {
        // Parse additional zone phrases
        let mut zones = vec![zone];
        let mut rest = rest;

        loop {
            let (next_rest, next_prep) = alt((
                value(ZonePrep::From, tag("from ")),
                value(ZonePrep::In, tag("in ")),
                value(ZonePrep::On, tag("on ")),
            ))
            .parse(rest)?;

            let (next_rest, next_qual) = parse_zone_qual(next_rest)?;
            let (next_rest, next_zone) = parse_zone_word(next_rest)?;
            let (next_rest, _) = peek_zone_boundary(next_rest)?;

            // CR 400.1: only the battlefield is referred to with "on"
            if next_prep == ZonePrep::On && next_zone != Zone::Battlefield {
                return Err(nom::Err::Error(nom::error::Error::new(
                    next_rest,
                    nom::error::ErrorKind::Fail,
                )));
            }

            // Qualifier consistency check: all zones in a disjunction should use the same qualifier
            if qual != next_qual {
                return Err(nom::Err::Error(nom::error::Error::new(
                    next_rest,
                    nom::error::ErrorKind::Fail,
                )));
            }

            zones.push(next_zone);
            rest = next_rest;

            // Check for another "or" separator
            if tag::<_, _, OracleError<'_>>(" or ").parse(rest).is_err() {
                break;
            }
        }

        (rest, zones)
    } else {
        (i, vec![zone])
    };

    let out = if zones.len() > 1 {
        // Multi-zone disjunction: use InAnyZone
        match qual {
            ZoneQual::Opponent => (
                vec![
                    FilterProp::Owned {
                        controller: ControllerRef::Opponent,
                    },
                    FilterProp::InAnyZone { zones },
                ],
                None,
            ),
            ZoneQual::You => (
                vec![FilterProp::InAnyZone { zones }],
                Some(ControllerRef::You),
            ),
            ZoneQual::ChosenPlayer => (
                vec![FilterProp::InAnyZone { zones }],
                Some(ControllerRef::SourceChosenPlayer),
            ),
            ZoneQual::TargetPlayer => (
                vec![
                    FilterProp::Owned {
                        controller: ControllerRef::TargetPlayer,
                    },
                    FilterProp::InAnyZone { zones },
                ],
                None,
            ),
            ZoneQual::Their => (
                vec![
                    FilterProp::Owned {
                        controller: ControllerRef::ScopedPlayer,
                    },
                    FilterProp::InAnyZone { zones },
                ],
                None,
            ),
            ZoneQual::OtherPoss | ZoneQual::Plain => (vec![FilterProp::InAnyZone { zones }], None),
        }
    } else {
        // Single zone: use InZone
        let zone = zones[0];
        match qual {
            ZoneQual::Opponent => (
                vec![
                    FilterProp::Owned {
                        controller: ControllerRef::Opponent,
                    },
                    FilterProp::InZone { zone },
                ],
                None,
            ),
            ZoneQual::You => (vec![FilterProp::InZone { zone }], Some(ControllerRef::You)),
            ZoneQual::ChosenPlayer => (
                vec![FilterProp::InZone { zone }],
                Some(ControllerRef::SourceChosenPlayer),
            ),
            ZoneQual::TargetPlayer => (
                vec![
                    FilterProp::Owned {
                        controller: ControllerRef::TargetPlayer,
                    },
                    FilterProp::InZone { zone },
                ],
                None,
            ),
            ZoneQual::Their => (
                vec![
                    FilterProp::Owned {
                        controller: ControllerRef::ScopedPlayer,
                    },
                    FilterProp::InZone { zone },
                ],
                None,
            ),
            ZoneQual::OtherPoss | ZoneQual::Plain => (vec![FilterProp::InZone { zone }], None),
        }
    };

    Ok((i, out))
}

fn parse_zone_qual(i: &str) -> super::oracle_nom::error::OracleResult<'_, ZoneQual> {
    alt((
        value(
            ZoneQual::Opponent,
            alt((tag("an opponent's "), tag("each opponent's "))),
        ),
        value(ZoneQual::You, tag("your ")),
        // CR 613.1: must precede the `Plain` "the " arm so "the chosen player's "
        // isn't consumed as a bare "the " article.
        value(ZoneQual::ChosenPlayer, tag("the chosen player's ")),
        value(ZoneQual::TargetPlayer, tag("target player's ")),
        value(ZoneQual::Their, tag("their ")),
        value(
            ZoneQual::OtherPoss,
            alt((
                tag("its owner's "),
                tag("that player's "),
                tag("defending player's "),
                tag("each player's "),
            )),
        ),
        // CR 400.7: Adjective- and quantity-qualified zone references — "all
        // graveyards", "each graveyard", "a single graveyard", "a random
        // graveyard" — share the indefinite-article semantics with bare
        // "a "/"the " for origin-zone tracking (the modifier constrains
        // which instance, not which zone). Longest-match-first ordering.
        value(
            ZoneQual::Plain,
            alt((
                tag("all "),
                tag("each "),
                tag("a single "),
                tag("a random "),
                tag("a "),
                tag("the "),
            )),
        ),
        // Bare form (e.g., "from exile"): zero-width match so the zone_word combinator runs next.
        value(ZoneQual::Plain, tag("")),
    ))
    .parse(i)
}

/// Recognize a bare zone word (lowercased). Returns the typed `Zone`.
///
/// Canonical entry for zone-token parsing — shared by `parse_zone_suffix_nom`
/// (origin/destination zone phrases in target filters) and by the
/// source-referential condition parser in `oracle_nom/condition.rs`. New zone
/// tokens MUST be added here, not duplicated at call sites.
///
/// "command zone" (CR 408) is recognized as a two-word token — `Zone::Command`
/// is a shared zone that always appears with the qualifier "the " in printed
/// Oracle text ("the command zone"), so it composes the same way as the
/// bare-word zones at every call site that already strips a `ZoneQual`.
pub(crate) fn parse_zone_word(i: &str) -> super::oracle_nom::error::OracleResult<'_, Zone> {
    // Longer (plural / multi-word) variants precede shorter ones so `tag` doesn't
    // prefix-match "graveyard" out of "graveyards" and leave a stray "s" that
    // peek_zone_boundary would reject.
    alt((
        value(
            Zone::Battlefield,
            alt((tag("battlefields"), tag("battlefield"))),
        ),
        // CR 408: the command zone — multi-word zone token. Placed before the
        // bare-word arms because it has no shared prefix with them and the
        // longest-prefix-first convention keeps additions ordered by length.
        value(Zone::Command, tag("command zone")),
        value(Zone::Graveyard, alt((tag("graveyards"), tag("graveyard")))),
        value(Zone::Exile, alt((tag("exiles"), tag("exile")))),
        value(Zone::Hand, alt((tag("hands"), tag("hand")))),
        value(Zone::Library, alt((tag("libraries"), tag("library")))),
    ))
    .parse(i)
}

/// Peek that the next character is a word boundary (end-of-string, space, comma, period).
/// Prevents matches like "graveyardkeeper" from succeeding as "graveyard".
pub(crate) fn peek_zone_boundary(i: &str) -> super::oracle_nom::error::OracleResult<'_, ()> {
    match i.chars().next() {
        None | Some(' ' | ',' | '.') => Ok((i, ())),
        _ => Err(nom::Err::Error(nom::error::Error::new(
            i,
            nom::error::ErrorKind::Fail,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_ir::context::ParseContext;
    use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
    use crate::types::ability::{PtStat, PtValueScope};
    use crate::types::counter::CounterType;

    fn typed_leg(filter: &TargetFilter) -> Option<&TypedFilter> {
        match filter {
            TargetFilter::Typed(tf) => Some(tf),
            TargetFilter::And { filters } => filters.iter().find_map(typed_leg),
            _ => None,
        }
    }

    /// Extract the `AggregateFunction` a superlative-property suffix encodes,
    /// regardless of whether it landed as a `PtComparison` (power/toughness) or a
    /// `Cmc` (mana value) filter prop.
    fn superlative_aggregate_function(text: &str) -> AggregateFunction {
        let mut ctx = ParseContext::default();
        let (prop, _consumed) = parse_superlative_property_suffix(text, &mut ctx)
            .unwrap_or_else(|| panic!("superlative suffix should parse: {text}"));
        let value = match prop {
            FilterProp::PtComparison { value, .. } | FilterProp::Cmc { value, .. } => value,
            other => panic!("expected PtComparison/Cmc, got {other:?}"),
        };
        match value {
            QuantityExpr::Ref {
                qty: QuantityRef::Aggregate { function, .. },
            } => function,
            other => panic!("expected Aggregate quantity, got {other:?}"),
        }
    }

    /// CR 208.1 + CR 202.3: the superlative head maps each direction word to an
    /// `AggregateFunction` — least/lowest/smallest = Min (new), greatest/highest =
    /// Max (regression). Tests the parameterized `alt` at the building-block level
    /// across its full input range, not one card.
    #[test]
    fn superlative_direction_maps_word_to_aggregate_function() {
        for word in ["least", "lowest", "smallest"] {
            let text = format!("with the {word} power among creatures you control");
            assert_eq!(
                superlative_aggregate_function(&text),
                AggregateFunction::Min,
                "{word} should map to Min"
            );
        }
        for word in ["greatest", "highest"] {
            let text = format!("with the {word} power among creatures you control");
            assert_eq!(
                superlative_aggregate_function(&text),
                AggregateFunction::Max,
                "{word} should map to Max"
            );
        }
    }

    /// CR 208.1 + CR 701.21: "with the least toughness among creatures you control"
    /// (The Dining Car's upkeep sacrifice) → a Min-aggregate toughness
    /// `PtComparison` over "creatures you control", tie-inclusive (`EQ`).
    #[test]
    fn superlative_least_toughness_suffix_emits_min_aggregate_pt_comparison() {
        let text = "with the least toughness among creatures you control";
        let mut ctx = ParseContext::default();
        let (prop, consumed) =
            parse_power_suffix(text, &mut ctx).expect("least-toughness suffix should parse");
        assert_eq!(consumed, text.len(), "the whole suffix must be consumed");
        let FilterProp::PtComparison {
            stat,
            comparator,
            value,
            ..
        } = prop
        else {
            panic!("expected PtComparison, got {prop:?}");
        };
        assert_eq!(stat, PtStat::Toughness);
        assert_eq!(comparator, Comparator::EQ);
        let QuantityExpr::Ref {
            qty:
                QuantityRef::Aggregate {
                    function,
                    property,
                    filter,
                },
        } = value
        else {
            panic!("expected Aggregate quantity, got {value:?}");
        };
        assert_eq!(function, AggregateFunction::Min);
        assert_eq!(property, ObjectProperty::Toughness);
        // The eligible set is "creatures you control".
        let tf = typed_leg(&filter).expect("aggregate filter should be a typed creature filter");
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
    }

    /// CR 122.1 + CR 702.62b (Clockspinning): "target permanent or suspended
    /// card" is a battlefield∪exile target pool. The permanent leg must carry an
    /// explicit `InZone{Battlefield}` (so `extract_explicit_zones` unions both
    /// zones) and the card leg must encode the suspended-card definition.
    #[test]
    fn parse_target_permanent_or_suspended_card() {
        let (filter, rest) = parse_target("target permanent or suspended card");
        assert_eq!(rest, "");
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or pool, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);

        let permanent_leg = filters
            .iter()
            .find_map(|f| match f {
                TargetFilter::Typed(tf) if tf.type_filters == vec![TypeFilter::Permanent] => {
                    Some(tf)
                }
                _ => None,
            })
            .expect("battlefield permanent leg");
        assert!(permanent_leg.properties.contains(&FilterProp::InZone {
            zone: Zone::Battlefield
        }));

        let card_leg = filters
            .iter()
            .find_map(|f| match f {
                TargetFilter::Typed(tf) if tf.type_filters == vec![TypeFilter::Card] => Some(tf),
                _ => None,
            })
            .expect("suspended card leg");
        assert!(card_leg
            .properties
            .contains(&FilterProp::InZone { zone: Zone::Exile }));
        assert!(card_leg.properties.contains(&FilterProp::HasKeywordKind {
            value: KeywordKind::Suspend
        }));
        assert!(card_leg.properties.iter().any(|p| matches!(
            p,
            FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Time),
                comparator: Comparator::GE,
                ..
            }
        )));
    }

    /// Issue #3677 (Flare of Denial): "sacrifice a nontoken blue creature" must
    /// capture the color AND the creature type, not just the NonToken negation.
    /// Before the fix, the color-prefix scan ran only BEFORE the `non-` negation
    /// loop, so a leading "nontoken " left "blue creature" unconsumed and the
    /// resulting filter silently matched any nontoken permanent — including a
    /// land, which is never a token, allowing the alt cost to be paid with a
    /// colorless land instead of a blue creature.
    #[test]
    fn nontoken_color_creature_captures_color_and_type() {
        let (filter, rest) = parse_type_phrase("nontoken blue creature");
        assert_eq!(rest.trim(), "");
        let tf = typed_leg(&filter).expect("expected typed filter");
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf.properties.contains(&FilterProp::NonToken));
        assert!(tf.properties.contains(&FilterProp::HasColor {
            color: ManaColor::Blue
        }));
    }

    /// Issue #3677 class (Cadric, Soul Kindler): "another nontoken legendary
    /// permanent you control" must capture the Legendary supertype, not just
    /// the NonToken negation. Same root cause as the color case above — the
    /// supertype-prefix scan only ran BEFORE the `non-` negation loop.
    #[test]
    fn nontoken_legendary_permanent_captures_supertype() {
        let (filter, rest) = parse_type_phrase("another nontoken legendary permanent you control");
        assert_eq!(rest.trim(), "");
        let tf = typed_leg(&filter).expect("expected typed filter");
        assert!(tf.type_filters.contains(&TypeFilter::Permanent));
        assert!(tf.properties.contains(&FilterProp::NonToken));
        assert!(tf.properties.contains(&FilterProp::HasSupertype {
            value: Supertype::Legendary
        }));
        assert_eq!(tf.controller, Some(ControllerRef::You));
    }

    /// Issue #3677 class (Akki Ember-Keeper): "a nontoken modified creature
    /// you control" must capture the Modified property, not just the
    /// NonToken negation. Same root cause as the color case above — the
    /// "modified" adjective scan only ran BEFORE the `non-` negation loop.
    #[test]
    fn nontoken_modified_creature_captures_modified_property() {
        let (filter, rest) = parse_type_phrase("a nontoken modified creature you control");
        assert_eq!(rest.trim(), "");
        let tf = typed_leg(&filter).expect("expected typed filter");
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf.properties.contains(&FilterProp::NonToken));
        assert!(tf.properties.contains(&FilterProp::Modified));
        assert_eq!(tf.controller, Some(ControllerRef::You));
    }

    /// GitHub #4710 (Scourglass): "permanents except for artifacts and lands"
    /// must exclude BOTH types, not silently drop the exception clause. Before
    /// the fix, `parse_type_phrase_with_ctx` had no suffix parser for "except
    /// for <type-list>" (only the predicate-based "except those that ..." was
    /// recognized), so the trailing clause was left unconsumed and the filter
    /// silently matched every permanent.
    #[test]
    fn except_for_type_list_excludes_both_types() {
        let (filter, rest) = parse_type_phrase("permanents except for artifacts and lands");
        assert_eq!(rest.trim(), "");
        let tf = typed_leg(&filter).expect("expected typed filter");
        assert!(tf.type_filters.contains(&TypeFilter::Permanent));
        assert!(tf
            .type_filters
            .contains(&TypeFilter::Non(Box::new(TypeFilter::Artifact))));
        assert!(tf
            .type_filters
            .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))));
    }

    /// Elspeth Tirel −5 ("other permanents except for lands and tokens"): the
    /// exclusion list is heterogeneous — "lands" is a `TypeFilter::Non`
    /// entry, "tokens" is a `FilterProp::NonToken` entry (tokens are a
    /// property, not a card type) — proving the mechanism routes each list
    /// item to the correct accumulator, mirroring how the pre-existing
    /// "nonartifact, nontoken permanent" prefix negation already splits the
    /// same two categories.
    #[test]
    fn except_for_type_list_splits_type_and_token_property() {
        let (filter, rest) = parse_type_phrase("other permanents except for lands and tokens");
        assert_eq!(rest.trim(), "");
        let tf = typed_leg(&filter).expect("expected typed filter");
        assert!(tf
            .type_filters
            .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))));
        assert!(tf.properties.contains(&FilterProp::NonToken));
        assert!(
            !tf.type_filters.iter().any(
                |t| matches!(t, TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Subtype(_)))
            ),
            "must not misclassify 'tokens' as a negated Subtype, got {:?}",
            tf.type_filters
        );
    }

    /// GitHub #4710 hostile fixture (Mageta the Lion class): "except for
    /// Mageta" names a specific permanent, not a type. `classify_negation`'s
    /// catch-all treats any unrecognized word as a negated Subtype, which
    /// would silently produce `Non(Subtype("Mageta"))` — a no-op exclusion
    /// (no permanent has that subtype) that looks fixed but isn't. The suffix
    /// parser must decline the whole clause instead, leaving the base filter
    /// unchanged rather than mis-firing on a named exception it can't model.
    #[test]
    fn except_for_named_exception_does_not_misfire_as_subtype_negation() {
        let (filter, rest) = parse_type_phrase("creatures except for Mageta");
        let tf = typed_leg(&filter).expect("expected typed filter");
        assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
        assert!(
            tag::<_, _, OracleError<'_>>("except for")
                .parse(rest.trim_start())
                .is_ok(),
            "the unrecognized exception clause must be left unconsumed, got rest={rest:?}"
        );
    }

    /// CR 201.2 (issue #2016): the "named <CardName>" suffix must terminate the
    /// card name at the enclosing clause boundary instead of swallowing the
    /// trailing predicate or controller suffix. Tests the boundary class, not a
    /// single card: predicate verb, controller suffix, and relative pronoun all
    /// terminate the name, while a comma-bearing legendary name is preserved.
    #[test]
    fn named_filter_terminates_at_clause_boundary() {
        fn named_of(text: &str) -> (String, String) {
            let mut ctx = ParseContext::default();
            let (filter, rest) = parse_type_phrase_with_ctx(text, &mut ctx);
            let name = typed_leg(&filter)
                .and_then(|tf| {
                    tf.properties.iter().find_map(|p| match p {
                        FilterProp::Named { name } => Some(name.clone()),
                        _ => None,
                    })
                })
                .unwrap_or_else(|| panic!("expected a Named property in {filter:?}"));
            (name, rest.to_string())
        }

        // Predicate verb terminates the name (Bonder's Ornament class).
        let (name, rest) = named_of("a permanent named Bonder's Ornament draws a card");
        assert_eq!(name, "Bonder's Ornament");
        assert_eq!(rest, " draws a card");

        // Controller suffix terminates the name.
        let (name, _) = named_of("a creature named Storm Crow you control");
        assert_eq!(name, "Storm Crow");

        // Relative pronoun terminates the name.
        let (name, _) = named_of("a creature named Storm Crow that has flying");
        assert_eq!(name, "Storm Crow");

        // Origin-zone suffix terminates the name (Deathpact Angel class).
        let (name, rest) = named_of("a card named Deathpact Angel from your graveyard");
        assert_eq!(name, "Deathpact Angel");
        assert_eq!(rest, " from your graveyard");

        // "from" inside a card name is preserved when it is not an origin-zone
        // suffix.
        let (name, _) = named_of("a card named Extract from Darkness");
        assert_eq!(name, "Extract from Darkness");

        // A comma-bearing legendary name is preserved (no split on internal
        // punctuation) when no clause boundary follows.
        let (name, _) = named_of("a creature named Bruna, the Fading Light");
        assert_eq!(name, "Bruna, the Fading Light");

        // A comma followed by the normalized self-reference opens the next
        // clause, not part of the literal name (Kookus class).
        let (name, rest) = named_of("a creature named Keeper of Kookus, ~ deals 3 damage");
        assert_eq!(name, "Keeper of Kookus");
        assert_eq!(rest.trim_start_matches([',', ' ']), "~ deals 3 damage");

        // Period still ends the name.
        let (name, _) = named_of("a creature named Storm Crow.");
        assert_eq!(name, "Storm Crow");
    }

    /// CR 201.2 + CR 400.1: A locative "in <zone>" / "on the battlefield" count
    /// scope is NOT part of the card name — it must terminate the name and
    /// re-attach as an `InZone`/`InAnyZone` filter prop. Regression guard for
    /// the "cards named X in your graveyard" misparse (Frantic Inventory,
    /// Accumulated Knowledge, Plague Rats, Undead Servant, ...), where the zone
    /// was swallowed into the name — producing `Named { name: "Frantic
    /// Inventory in your graveyard" }`, a name no card ever has, so the count
    /// always resolved to 0.
    #[test]
    fn named_filter_terminates_at_locative_zone() {
        fn named_and_props(text: &str) -> (String, Vec<FilterProp>, Option<ControllerRef>, String) {
            let (filter, rest) = parse_type_phrase(text);
            let tf = typed_leg(&filter)
                .unwrap_or_else(|| panic!("expected a Typed filter in {filter:?}"));
            let name = tf
                .properties
                .iter()
                .find_map(|p| match p {
                    FilterProp::Named { name } => Some(name.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("expected a Named property in {filter:?}"));
            (
                name,
                tf.properties.clone(),
                tf.controller.clone(),
                rest.to_string(),
            )
        }

        // "in your graveyard": name terminates, InZone Graveyard + You attached,
        // remainder fully consumed.
        let (name, props, ctrl, rest) =
            named_and_props("cards named Frantic Inventory in your graveyard");
        assert_eq!(name, "Frantic Inventory");
        assert!(
            props
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone } if *zone == Zone::Graveyard)),
            "expected InZone Graveyard, got {props:?}"
        );
        assert_eq!(ctrl, Some(ControllerRef::You));
        assert_eq!(rest, "");

        // "in all graveyards": name terminates, InZone Graveyard, no controller
        // restriction (counts every player's graveyard).
        let (name, props, ctrl, rest) =
            named_and_props("cards named Accumulated Knowledge in all graveyards");
        assert_eq!(name, "Accumulated Knowledge");
        assert!(
            props
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone } if *zone == Zone::Graveyard)),
            "expected InZone Graveyard, got {props:?}"
        );
        assert_eq!(ctrl, None);
        assert_eq!(rest, "");

        // "on the battlefield": name terminates, remainder consumed.
        let (name, _props, _ctrl, rest) =
            named_and_props("creatures named Plague Rats on the battlefield");
        assert_eq!(name, "Plague Rats");
        assert_eq!(rest, "");

        // "from <zone>" move-origin is unchanged: the name still terminates at
        // "Deathpact Angel" but the "from" suffix stays in the remainder for the
        // caller (no InZone attached here).
        let (name, props, _ctrl, rest) =
            named_and_props("card named Deathpact Angel from your graveyard");
        assert_eq!(name, "Deathpact Angel");
        assert!(
            !props.iter().any(|p| matches!(p, FilterProp::InZone { .. })),
            "'from' origin-zone must not attach InZone here, got {props:?}"
        );
        assert_eq!(rest, " from your graveyard");

        // A "from" inside a real card name is still preserved.
        let (name, _props, _ctrl, _rest) = named_and_props("a card named Extract from Darkness");
        assert_eq!(name, "Extract from Darkness");
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

    #[test]
    fn any_target() {
        let (f, rest) = parse_target("any target");
        assert_eq!(f, TargetFilter::Any);
        assert_eq!(rest, "");
    }

    /// CR 408: `parse_zone_word` recognizes "command zone" as the typed
    /// `Zone::Command` token. Locks the canonical zone vocabulary so any
    /// caller composing on top of `parse_zone_word` (e.g., the
    /// source-referential condition parser in `oracle_nom/condition.rs`)
    /// picks up the command zone without duplicating its spelling.
    #[test]
    fn parse_zone_word_recognizes_command_zone() {
        let (rest, zone) = parse_zone_word("command zone").unwrap();
        assert_eq!(rest, "");
        assert_eq!(zone, Zone::Command);
    }

    /// Sanity: existing single-word zone tokens still resolve through the
    /// same combinator after the `Command` extension.
    #[test]
    fn parse_zone_word_recognizes_graveyard_and_battlefield() {
        assert_eq!(parse_zone_word("graveyard").unwrap().1, Zone::Graveyard);
        assert_eq!(parse_zone_word("battlefield").unwrap().1, Zone::Battlefield);
    }

    #[test]
    fn target_creature() {
        let (f, _) = parse_target("target creature");
        assert_eq!(f, TargetFilter::Typed(TypedFilter::creature()));
    }

    #[test]
    fn creatures_blocking_or_blocked_by_target_creature() {
        let (filter, rest) = parse_target("creatures blocking or blocked by target creature");
        assert_eq!(rest, "");
        assert_eq!(
            filter,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::CombatRelation {
                    relation: CombatRelation::BlockingOrBlockedBy,
                    subject: CombatRelationSubject::ParentTarget,
                }
            ]))
        );
    }

    #[test]
    fn random_target_creature_marks_random_mode_on_context() {
        // CR 115.1 + CR 701.9b: "random target X" — the inner filter is parsed
        // exactly as a normal target, but the parse context records that the
        // engine (not the controller) selects the target. The chunk loop in
        // `parse_effect_chain_ir` snapshots `ctx.target_selection_mode` into the
        // produced `ClauseIr`, which lowering stamps onto the `AbilityDefinition`.
        let mut ctx = ParseContext::default();
        let (f, rest) = parse_target_with_ctx("random target creatures", &mut ctx);
        assert_eq!(f, TargetFilter::Typed(TypedFilter::creature()));
        assert_eq!(rest, "");
        assert_eq!(ctx.target_selection_mode, TargetSelectionMode::Random);
    }

    #[test]
    fn opponent_chosen_at_random_marks_random_mode() {
        // CR 115.1 + CR 701.9b: "<noun-phrase> chosen at random" — postnominal
        // random qualifier mirrors the leading "random target X" form. The
        // suffix is stripped, the inner noun phrase parses normally, and the
        // selection mode flips to Random on the parse context.
        // Repro: Zaffai, Thunder Conductor — "deals 10 damage to an opponent
        // chosen at random."
        let mut ctx = ParseContext::default();
        let (f, rest) = parse_target_with_ctx("an opponent chosen at random", &mut ctx);
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        );
        assert_eq!(rest, "");
        assert_eq!(ctx.target_selection_mode, TargetSelectionMode::Random);
    }

    #[test]
    fn creature_chosen_at_random_marks_random_mode() {
        // The postnominal "chosen at random" suffix is independent of the noun
        // phrase: the suffix-strip path applies to any noun-phrase target,
        // including type-word phrases like "a creature".
        let mut ctx = ParseContext::default();
        let (f, _rest) = parse_target_with_ctx("a creature chosen at random", &mut ctx);
        assert_eq!(f, TargetFilter::Typed(TypedFilter::creature()));
        assert_eq!(ctx.target_selection_mode, TargetSelectionMode::Random);
    }

    #[test]
    fn opponent_chosen_at_random_with_trailing_period() {
        // The suffix-strip path tolerates trailing punctuation; sentence-final
        // periods at the end of effect clauses must not break the match.
        let mut ctx = ParseContext::default();
        let (f, _rest) = parse_target_with_ctx("an opponent chosen at random.", &mut ctx);
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        );
        assert_eq!(ctx.target_selection_mode, TargetSelectionMode::Random);
    }

    #[test]
    fn graveyard_card_at_random_marks_random_mode() {
        for text in [
            "a card from your graveyard at random",
            "a card at random from your graveyard",
        ] {
            let mut ctx = ParseContext::default();
            let (filter, rest) = parse_target_with_ctx(text, &mut ctx);
            assert_eq!(rest, "");
            assert_eq!(ctx.target_selection_mode, TargetSelectionMode::Random);

            let TargetFilter::Typed(typed) = filter else {
                panic!("expected typed card filter for {text}");
            };
            assert!(typed.type_filters.contains(&TypeFilter::Card));
            assert_eq!(typed.controller, None);
            assert!(typed.properties.contains(&FilterProp::Owned {
                controller: ControllerRef::You
            }));
            assert!(
                typed.properties.contains(&FilterProp::InZone {
                    zone: Zone::Graveyard
                }),
                "expected graveyard zone property for {text}, got {:?}",
                typed.properties
            );
        }
    }

    #[test]
    fn an_opponent_target_without_random_suffix() {
        // CR 115.1: bare "an opponent" parses as an opponent reference even
        // without the "target" prefix. Used by chooser phrases like "an
        // opponent of your choice" and post-stripping recursion from the
        // "chosen at random" arm above.
        let mut ctx = ParseContext::default();
        let (f, rest) = parse_target_with_ctx("an opponent", &mut ctx);
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        );
        assert_eq!(rest, "");
        assert_eq!(ctx.target_selection_mode, TargetSelectionMode::Chosen);
    }

    #[test]
    fn first_and_second_player_cross_clause_anaphors() {
        // CR 608.2c: "the first player" / "the second player" are cross-clause
        // ordinal player anaphors used by Oath of Mages and similar patterns.
        // The first player = the chooser of the prior sentence (= triggering
        // player). The second player = the chosen target of the prior sentence
        // (parent target slot 0).
        let mut ctx = ParseContext::default();
        let (f, _) = parse_target_with_ctx("the first player", &mut ctx);
        assert_eq!(f, TargetFilter::TriggeringPlayer);
        let mut ctx = ParseContext::default();
        let (f, _) = parse_target_with_ctx("the second player", &mut ctx);
        assert_eq!(f, TargetFilter::ParentTargetSlot { index: 0 });
    }

    #[test]
    fn target_creature_keeps_chosen_mode_on_context() {
        // CR 115.1: ordinary "target X" leaves the default `Chosen` mode intact.
        let mut ctx = ParseContext::default();
        let (f, rest) = parse_target_with_ctx("target creature", &mut ctx);
        assert_eq!(f, TargetFilter::Typed(TypedFilter::creature()));
        assert_eq!(rest, "");
        assert_eq!(ctx.target_selection_mode, TargetSelectionMode::Chosen);
    }

    #[test]
    fn target_creature_you_control() {
        let (f, _) = parse_target("target creature you control");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
        );
    }

    #[test]
    fn time_lord_target_keeps_subtype_and_controller() {
        // CR 205.3m + CR 115.1: "target Time Lord you control" must keep both the
        // two-word subtype (CR 205.3m: the only two-word creature type) and the
        // controller restriction (CR 115.1: declared target). Regression: when
        // "Time Lord" was absent from the SUBTYPES registry this collapsed to
        // Typed{type_filters:[], controller:None} (Time Lord Regeneration).
        let (filter, rest) = parse_target("target Time Lord you control");
        assert_eq!(rest, "");
        let tf = typed_leg(&filter).expect("expected Typed filter");
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf
            .type_filters
            .iter()
            .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Time Lord")));
    }

    #[test]
    fn bare_commander_they_control_uses_relative_player_scope() {
        let mut ctx = ParseContext {
            relative_player_scope: Some(ControllerRef::TargetPlayer),
            ..Default::default()
        };
        let (f, rest) =
            parse_target_with_ctx("commander they control from the battlefield", &mut ctx);
        // CR 903.3: a commander is targeted on the battlefield. Routing through
        // `parse_type_phrase_with_ctx` (instead of the former bare-commander
        // branch) means the explicit "from the battlefield" zone suffix is
        // consumed into `FilterProp::InZone` like any other typed target, so
        // the remainder is empty.
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::TargetPlayer),
                properties: vec![
                    FilterProp::IsCommander,
                    FilterProp::InZone {
                        zone: Zone::Battlefield,
                    },
                ],
                ..Default::default()
            })
        );
        assert_eq!(rest, "");
    }

    /// CR 903.3 + CR 108.3: Sanctum of Eternity and the broader bare-"commander"
    /// class (Witch's Clinic, Drillworks Mole, etc.). Commander is recognized
    /// as a typed-phrase prefix that pushes `IsCommander` and lets the existing
    /// suffix machinery (ownership, control, type-word, etc.) compose uniformly.
    /// Before #608 the parser had no path to attach `IsCommander` outside
    /// possessive contexts, so every bare/owned "target commander" fell through
    /// to an empty Typed filter that matched any permanent.
    #[test]
    fn target_commander_class_lowers_with_is_commander_property() {
        // Sanctum of Eternity — ownership suffix, distinct from control.
        // CR 903.3: a targetable commander resides on the battlefield. The
        // explicit "from the battlefield" zone suffix is consumed into
        // `FilterProp::InZone` by `parse_type_phrase_with_ctx`, leaving an
        // empty remainder.
        let bf = FilterProp::InZone {
            zone: Zone::Battlefield,
        };
        let (f, rest) = parse_target("target commander you own from the battlefield");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter {
                controller: None,
                properties: vec![
                    FilterProp::IsCommander,
                    FilterProp::Owned {
                        controller: ControllerRef::You,
                    },
                    bf,
                ],
                ..Default::default()
            }),
            "'target commander you own' must lower to Typed{{IsCommander, Owned{{You}}, InZone{{BF}}}}"
        );
        assert_eq!(rest, "");

        // "Your commander" is owner-scoped. This matters for trigger subjects
        // like Tome of Legends; a stolen opponent's commander must not satisfy
        // the phrase just because its current controller is you.
        let (f, rest) = parse_type_phrase("your commander enters or attacks");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter {
                controller: None,
                properties: vec![
                    FilterProp::Owned {
                        controller: ControllerRef::You,
                    },
                    FilterProp::IsCommander,
                ],
                ..Default::default()
            }),
            "'your commander' must be owned-by-you and IsCommander, not controller-scoped"
        );
        assert_eq!(rest, "enters or attacks");

        let (f, rest) = parse_type_phrase("your commanders attack");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter {
                controller: None,
                properties: vec![
                    FilterProp::Owned {
                        controller: ControllerRef::You,
                    },
                    FilterProp::IsCommander,
                ],
                ..Default::default()
            }),
            "'your commanders' must use the same owned commander filter as the singular phrase"
        );
        assert_eq!(rest, "attack");

        // Command Beacon class — the target parser should now reach the same
        // typed-phrase commander grammar instead of owning a separate
        // possessive-commander shortcut.
        let (f, rest) = parse_target("your commander from the command zone");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter {
                controller: None,
                properties: vec![
                    FilterProp::Owned {
                        controller: ControllerRef::You,
                    },
                    FilterProp::IsCommander,
                    FilterProp::InZone {
                        zone: Zone::Command,
                    },
                ],
                ..Default::default()
            }),
            "'your commander from the command zone' must compose ownership, commander identity, and zone"
        );
        assert_eq!(rest, "");

        // Witch's Clinic — bare "target commander" with no zone suffix. No
        // explicit zone is consumed, so (like every bare type phrase, e.g.
        // "target creature") no `InZone` property is attached.
        let (f, _) = parse_target("target commander");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter {
                controller: None,
                properties: vec![FilterProp::IsCommander],
                ..Default::default()
            }),
            "bare 'target commander' must still carry IsCommander, not an empty filter"
        );

        // Controller suffix — "they control" with relative-player scope. No
        // zone suffix, so no `InZone` property.
        let mut ctx = ParseContext {
            relative_player_scope: Some(ControllerRef::TargetPlayer),
            ..Default::default()
        };
        let (f, _) = parse_target_with_ctx("target commander they control", &mut ctx);
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::TargetPlayer),
                properties: vec![FilterProp::IsCommander],
                ..Default::default()
            }),
            "'target commander they control' must lower to Typed{{IsCommander, controller=TargetPlayer}}"
        );

        // Drillworks Mole class — "commander creature" (commander as adjective
        // attached to a creature type) with control suffix.
        let (f, _) = parse_target("target commander creature you control");
        match f {
            TargetFilter::Typed(tf) => {
                assert!(
                    tf.properties.contains(&FilterProp::IsCommander),
                    "expected IsCommander, got properties {:?}",
                    tf.properties
                );
                assert!(
                    tf.type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Creature)),
                    "expected Creature type, got {:?}",
                    tf.type_filters
                );
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn indefinite_article_commander_lowers_with_is_commander() {
        // CR 903.3: Hellkite Courser (#5256) — "put a commander you own from the
        // command zone onto the battlefield". The indefinite article "a" must be
        // stripped so the commander atom fires; before this it fell through to a
        // match-anything `Any` filter (the reanimation put-path lost the subject).
        let (f, rest) = parse_target("a commander you own from the command zone");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter {
                controller: None,
                properties: vec![
                    FilterProp::IsCommander,
                    FilterProp::Owned {
                        controller: ControllerRef::You,
                    },
                    FilterProp::InZone {
                        zone: Zone::Command,
                    },
                ],
                ..Default::default()
            }),
            "'a commander you own from the command zone' must lower to \
             Typed{{IsCommander, Owned{{You}}, InZone{{Command}}}}, not Any"
        );
        assert_eq!(rest, "");

        // Bare "a commander" (no suffix) still carries IsCommander, mirroring the
        // "a creature card" indefinite form — not the empty match-anything filter.
        let (bare, _) = parse_target("a commander");
        assert_eq!(
            bare,
            TargetFilter::Typed(TypedFilter {
                controller: None,
                properties: vec![FilterProp::IsCommander],
                ..Default::default()
            }),
        );
    }

    #[test]
    fn article_status_type_phrase_parses_as_target() {
        let (f, rest) = parse_target("a tapped land you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::land()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Tapped])
            )
        );
        assert_eq!(rest, "");
    }

    // Nettling Imp / Norritt / Arcum's Whistle class: verbatim clause from
    // Nettling Imp's real Oracle text. Building-block test — isolates the new
    // controller+continuity arm from the pre-existing "non-Wall" type-filter
    // handling (already covered elsewhere).
    #[test]
    fn parse_target_active_player_controlled_continuously_since_turn_began() {
        let (f, rest) = parse_target(
            "target creature the active player has controlled continuously since the beginning of the turn",
        );
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::ActivePlayer)
                    .properties(vec![FilterProp::ControlledContinuouslySinceTurnBegan])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn saddled_type_phrase_parses_as_target() {
        // CR 702.171b: a "saddled <type>" selector must carry FilterProp::IsSaddled
        // through the full parse_target path (not just parse_property_filter) —
        // guards the parse_combat_status_prefix / is_adjective_prefix_prop allowlist
        // wiring against silent regression if the prefix allowlist is reordered.
        let (f, rest) = parse_target("saddled creature you control");
        if let TargetFilter::Typed(tf) = &f {
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "missing Creature in {tf:?}"
            );
            assert!(
                tf.properties.contains(&FilterProp::IsSaddled),
                "missing IsSaddled in {tf:?}"
            );
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!("expected Typed filter, got {f:?}");
        }
        assert_eq!(rest, "");
    }

    #[test]
    fn discarded_card_from_graveyard_refers_to_triggering_source() {
        let (f, rest) = parse_target("the discarded card from your graveyard");
        assert_eq!(f, TargetFilter::TriggeringSource);
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_warns_on_any_fallback() {
        let mut ctx = ParseContext::default();
        let (filter, rest) = parse_target_with_ctx("foobar", &mut ctx);
        assert_eq!(filter, TargetFilter::Any);
        assert_eq!(rest, "foobar");
        assert!(ctx.diagnostics.iter().any(
            |d| matches!(d, OracleDiagnostic::TargetFallback { context, text, .. }
                if context == "parse_target could not classify" && text == "foobar")
        ));
    }

    #[test]
    fn parse_type_phrase_other_attacking_creature_shares_type_with_it() {
        let mut ctx = ParseContext {
            subject: Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            )),
            ..Default::default()
        };
        let (filter, remainder) = parse_type_phrase_with_ctx(
            "other attacking creature that shares a creature type with it",
            &mut ctx,
        );
        assert!(
            remainder.trim().is_empty(),
            "expected full consume, remainder: '{remainder}' filter: {filter:?}"
        );
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected typed filter");
        };
        assert!(tf.properties.contains(&FilterProp::Another));
        assert!(tf
            .properties
            .contains(&FilterProp::Attacking { defender: None }));
        assert!(tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::SharesQuality {
                quality: SharedQuality::CreatureType,
                reference: Some(reference),
                ..
            } if matches!(reference.as_ref(), TargetFilter::TriggeringSource)
        )));
    }

    #[test]
    fn attacking_creatures_you_control() {
        let (f, rest) = parse_type_phrase("attacking creatures you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Attacking { defender: None }])
            )
        );
        assert_eq!(rest, "");
    }

    /// Issue #2386 (Lulu, Stern Guardian): "target creature attacking you"
    /// must scope attackers to the controller, not every creature.
    #[test]
    fn parse_target_creature_attacking_you() {
        let (filter, remainder) = parse_target("target creature attacking you");
        assert!(remainder.trim().is_empty(), "remainder: '{remainder}'");
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
        assert!(typed.properties.contains(&FilterProp::Attacking {
            defender: Some(ControllerRef::You),
        }));
    }

    /// CR 506.5: "attacking alone" is a targetable combat-status predicate on
    /// the candidate creature, including relative-clause wording.
    #[test]
    fn parse_target_creature_attacking_alone() {
        let (filter, remainder) = parse_target("target creature that's attacking alone");
        assert!(remainder.trim().is_empty(), "remainder: '{remainder}'");
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
        assert!(typed.properties.contains(&FilterProp::AttackingAlone));
    }

    /// CR 506.5 + CR 109.4: controller suffixes compose with the attacking-alone
    /// relative clause instead of dropping the combat-status predicate.
    #[test]
    fn parse_target_creature_you_control_attacking_alone() {
        let (filter, remainder) =
            parse_target("target creature you control that's attacking alone");
        assert!(remainder.trim().is_empty(), "remainder: '{remainder}'");
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected typed filter, got {filter:?}");
        };
        assert_eq!(typed.controller, Some(ControllerRef::You));
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
        assert!(typed.properties.contains(&FilterProp::AttackingAlone));
    }

    /// Stalking Leonin: "attacking you if it's controlled by..." must not treat
    /// the defender suffix as complete at "attacking you" — the trailing " if "
    /// clause is a separate target gate.
    #[test]
    fn parse_target_creature_attacking_you_if_controlled_does_not_consume_if_clause() {
        let phrase = "creature that's attacking you if it's controlled by the chosen player";
        let (filter, remainder) = parse_type_phrase(phrase);
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected typed filter, got {filter:?}");
        };
        assert!(!typed.properties.contains(&FilterProp::Attacking {
            defender: Some(ControllerRef::You),
        }));
        assert_eq!(
            remainder.trim_start(),
            "that's attacking you if it's controlled by the chosen player"
        );
    }

    #[test]
    fn parse_creatures_attacking_your_opponents_and_planeswalkers() {
        let (filter, remainder) =
            parse_target("creatures attacking your opponents and/or planeswalkers they control");
        assert!(remainder.trim().is_empty(), "remainder: '{remainder}'");
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
        assert!(typed.properties.contains(&FilterProp::Attacking {
            defender: Some(ControllerRef::Opponent),
        }));
    }

    // CR 701.60b: "suspected" is a battlefield designation usable as a type-phrase
    // prefix, parallel to "attacking"/"tapped". Covers Clandestine Meddler, Frantic
    // Scapegoat, Deadly Complication, and the broader suspected-creature filter class.
    #[test]
    fn suspected_creatures_you_control() {
        let (f, rest) = parse_type_phrase("suspected creatures you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Suspected])
            )
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn creature_tokens_you_control() {
        let (f, rest) = parse_type_phrase("creature tokens you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Token])
            )
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn target_nonland_permanent() {
        let (f, _) = parse_target("target nonland permanent");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent().with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
            )
        );
    }

    #[test]
    fn target_artifact_or_enchantment() {
        let (f, _) = parse_target("target artifact or enchantment");
        match f {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 2);
            }
            _ => panic!("Expected Or filter, got {:?}", f),
        }
    }

    #[test]
    fn target_player() {
        let (f, _) = parse_target("target player");
        assert_eq!(f, TargetFilter::Player);
    }

    #[test]
    fn bare_player_is_player_target() {
        let (f, rest) = parse_target("player, choose a creature card in that player's graveyard");
        assert_eq!(f, TargetFilter::Player);
        assert_eq!(rest, ", choose a creature card in that player's graveyard");
    }

    #[test]
    fn bare_graveyards_are_cards_in_graveyard_zone() {
        let (f, rest) = parse_target("graveyards");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }]))
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn bare_him_inherits_parent_target() {
        let (f, rest) = parse_target("him");
        assert_eq!(f, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn bare_her_inherits_parent_target() {
        let (f, rest) = parse_target("her");
        assert_eq!(f, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn on_it_inherits_parent_target() {
        let (f, rest) = parse_target("on it");
        assert_eq!(f, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn bare_one_inherits_parent_target() {
        let (f, rest) = parse_target("one into your hand");
        assert_eq!(f, TargetFilter::ParentTarget);
        assert_eq!(rest, " into your hand");
    }

    // CR 608.2k regression — issue #319 (Serpent's Soul-Jar)
    //
    // "Whenever an Elf you control dies, exile it" was emitting
    // `Effect::ChangeZone { target: ParentTarget }` for the bare "it"
    // pronoun, which resolved to the ability source (the Jar) rather
    // than the dying Elf. With a typed trigger subject on the parse
    // context, "it" must bind to `TriggeringSource` so the dying creature
    // is the exile subject.
    #[test]
    fn bare_it_with_typed_trigger_subject_binds_to_triggering_source() {
        let mut ctx = ParseContext {
            subject: Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .subtype("Elf".into()),
            )),
            ..Default::default()
        };
        let (f, rest) = parse_target_with_ctx("it", &mut ctx);
        assert_eq!(f, TargetFilter::TriggeringSource);
        assert_eq!(rest, "");
    }

    #[test]
    fn bare_them_with_typed_trigger_subject_binds_to_triggering_source() {
        let mut ctx = ParseContext {
            subject: Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            )),
            ..Default::default()
        };
        let (f, rest) = parse_target_with_ctx("them", &mut ctx);
        assert_eq!(f, TargetFilter::TriggeringSource);
        assert_eq!(rest, "");
    }

    #[test]
    fn bare_him_with_typed_trigger_subject_binds_to_triggering_source() {
        let mut ctx = ParseContext {
            subject: Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            )),
            ..Default::default()
        };
        let (f, rest) = parse_target_with_ctx("him", &mut ctx);
        assert_eq!(f, TargetFilter::TriggeringSource);
        assert_eq!(rest, "");
    }

    #[test]
    fn bare_it_with_attached_to_subject_binds_to_triggering_source() {
        let mut ctx = ParseContext {
            subject: Some(TargetFilter::AttachedTo),
            ..Default::default()
        };
        let (f, rest) = parse_target_with_ctx("it", &mut ctx);
        assert_eq!(f, TargetFilter::TriggeringSource);
        assert_eq!(rest, "");
    }

    // Self-ETB triggers ("When ~ enters, choose target creature. Exile it") —
    // subject is `SelfRef`, so the only valid antecedent for "it" in a
    // compound effect is the parent ability's selected target. Preserve
    // `ParentTarget` so cards like Agrus Kos exile the chosen creature, not
    // the source. The pronoun does NOT bind to the source via `SelfRef` here.
    #[test]
    fn bare_it_with_self_ref_subject_preserves_parent_target() {
        let mut ctx = ParseContext {
            subject: Some(TargetFilter::SelfRef),
            ..Default::default()
        };
        let (f, rest) = parse_target_with_ctx("it", &mut ctx);
        assert_eq!(f, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    // Player-actor triggers ("Whenever a player attacks, do X to it") — `Any`
    // subject. Same as SelfRef: preserve `ParentTarget`.
    #[test]
    fn bare_it_with_any_subject_preserves_parent_target() {
        let mut ctx = ParseContext {
            subject: Some(TargetFilter::Any),
            ..Default::default()
        };
        let (f, rest) = parse_target_with_ctx("it", &mut ctx);
        assert_eq!(f, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    // Compound spell/activated effects with no trigger subject
    // ("Tap target creature. It doesn't untap") — preserve the legacy
    // `ParentTarget` binding so the parent-ability target chain handles it.
    #[test]
    fn bare_it_without_trigger_subject_preserves_parent_target() {
        let mut ctx = ParseContext::default();
        let (f, rest) = parse_target_with_ctx("it", &mut ctx);
        assert_eq!(f, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn the_first_typed_object_inherits_parent_target() {
        let (f, rest) = parse_target("the first card to the battlefield");
        assert_eq!(f, TargetFilter::ParentTarget);
        assert_eq!(rest, " to the battlefield");
    }

    #[test]
    fn tap_or_untap_target_permanent_strips_verb_prefix() {
        let (f, rest) = parse_target("or untap target permanent");
        assert_eq!(f, TargetFilter::Typed(TypedFilter::permanent()));
        assert_eq!(rest, "");
    }

    #[test]
    fn target_count_placeholders_map_to_any_target() {
        let (f, rest) = parse_target("one or two targets");
        assert_eq!(f, TargetFilter::Any);
        assert_eq!(rest, "");
    }

    #[test]
    fn quantified_of_them_produces_tracked_set() {
        let (f, rest) = parse_target("two of them");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn quantified_cards_from_hand_parse_as_zone_filter() {
        let (f, rest) = parse_target("two cards from your hand");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::card()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone { zone: Zone::Hand }])
            )
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn enchanted_creature() {
        let (f, _) = parse_target("enchanted creature");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]))
        );
    }

    #[test]
    fn enchanted_permanent() {
        let (f, _) = parse_target("enchanted permanent");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy]))
        );
    }

    #[test]
    fn enchanted_permanents_controller() {
        let (f, _) = parse_target("enchanted permanent's controller");
        assert_eq!(f, TargetFilter::ParentTargetController);
    }

    #[test]
    fn equipped_creature() {
        let (f, _) = parse_target("equipped creature");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy]))
        );
    }

    #[test]
    fn each_opponent() {
        let (f, _) = parse_target("each opponent");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        );
    }

    #[test]
    fn target_opponent() {
        let (f, _) = parse_target("target opponent");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        );
    }

    #[test]
    fn or_type_distributes_controller() {
        // "creature or artifact you control" → both branches get You controller
        let (f, _) = parse_target("target creature or artifact you control");
        match f {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 2);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You)
                    )
                );
            }
            _ => panic!("Expected Or filter, got {:?}", f),
        }
    }

    /// CR 205.2a + CR 205.3a + CR 301.7: A multi-core-type adjective
    /// conjunction ("artifact creature card") on the left side of an `or` /
    /// `and/or` disjunction must keep every core type word that bound to that
    /// branch — the primary `card_type` AND the trailing core types in
    /// `extra_core_type_filters`. Dropping the trailing types collapses the
    /// left branch's AND-constraint into a strictly looser filter (any
    /// artifact, not only artifact creatures).
    ///
    /// Issue #1537 (Szarekh, the Silent King): Oracle text
    /// "artifact creature card or Vehicle card" was parsing to
    /// `Or[Typed{Artifact}, Typed{Subtype(Vehicle)}]`, letting `Mill 3`
    /// retrieval pull any milled artifact (e.g. an Equipment) into hand
    /// instead of restricting to artifact creatures or Vehicles.
    ///
    /// This is a building-block test: any `<typeword1> <typeword2> card or
    /// <typeword> card` shape must preserve the full type conjunction on the
    /// left branch.
    #[test]
    fn multi_core_type_disjunction_preserves_conjoined_types() {
        let (f, rest) = parse_target("artifact creature card or Vehicle card");
        assert_eq!(rest, "");
        let TargetFilter::Or { filters } = &f else {
            panic!("expected Or disjunction, got {f:?}");
        };
        assert_eq!(filters.len(), 2, "expected two disjuncts, got {filters:?}");

        // Left branch: "artifact creature card" must AND Artifact with
        // Creature — both type filters must be present so the runtime
        // `type_filters.iter().all(...)` check rejects artifacts that
        // aren't also creatures (e.g. Equipment, artifact lands).
        let TargetFilter::Typed(left) = &filters[0] else {
            panic!("expected left Typed, got {:?}", filters[0]);
        };
        assert!(
            has_type(left, TypeFilter::Artifact),
            "left branch missing Artifact: {left:?}",
        );
        assert!(
            has_type(left, TypeFilter::Creature),
            "left branch dropped the trailing Creature core type — \
             this is the #1537 regression: {left:?}",
        );

        // Right branch: "Vehicle card" — Vehicle is a creature subtype, so
        // `normalize_search_typed_filter` (and the bare subtype path in
        // `parse_specialized_type_word`) lift it onto Creature. We only
        // assert that the Vehicle subtype is present; the inferred core
        // type may or may not be Creature depending on the parse path.
        let TargetFilter::Typed(right) = &filters[1] else {
            panic!("expected right Typed, got {:?}", filters[1]);
        };
        assert!(
            has_type(right, TypeFilter::Subtype("Vehicle".into())),
            "right branch missing Vehicle subtype: {right:?}",
        );
    }

    /// Companion case to `multi_core_type_disjunction_preserves_conjoined_types`:
    /// the right branch can also carry a multi-type adjective conjunction.
    /// Both branches must independently retain their full type set.
    #[test]
    fn multi_core_type_disjunction_preserves_both_branches() {
        let (f, _) = parse_target("creature card or artifact creature card");
        let TargetFilter::Or { filters } = &f else {
            panic!("expected Or disjunction, got {f:?}");
        };
        assert_eq!(filters.len(), 2);
        let TargetFilter::Typed(left) = &filters[0] else {
            panic!("expected left Typed");
        };
        assert!(has_type(left, TypeFilter::Creature));
        let TargetFilter::Typed(right) = &filters[1] else {
            panic!("expected right Typed");
        };
        assert!(has_type(right, TypeFilter::Artifact));
        assert!(
            has_type(right, TypeFilter::Creature),
            "right branch dropped the trailing Creature core type: {right:?}",
        );
    }

    #[test]
    fn tilde_is_self_ref() {
        let (f, rest) = parse_target("~");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, "");
    }

    #[test]
    fn tilde_with_trailing_text() {
        let (f, rest) = parse_target("~ to its owner's hand");
        assert_eq!(f, TargetFilter::SelfRef);
        assert!(rest.contains("to its owner"));
    }

    #[test]
    fn this_creature_is_self_ref() {
        let (f, rest) = parse_target("this creature to its owner's hand");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, " to its owner's hand");
    }

    #[test]
    fn itself_is_self_ref() {
        let (f, rest) = parse_target("itself.");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, ".");
    }

    #[test]
    fn this_creature_exact_is_self_ref() {
        let (f, rest) = parse_target("this creature");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, "");
    }

    #[test]
    fn this_permanent_is_self_ref() {
        let (f, rest) = parse_target("this permanent");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, "");
    }

    #[test]
    fn this_enchantment_is_self_ref() {
        let (f, rest) = parse_target("this enchantment");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, "");
    }

    #[test]
    fn this_attraction_is_self_ref() {
        let (f, rest) = parse_target("this attraction");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, "");
    }

    #[test]
    fn white_creature_you_control() {
        let (f, _) = parse_type_phrase("white creature you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::HasColor {
                        color: ManaColor::White
                    }])
            )
        );
    }

    #[test]
    fn red_spell() {
        let (f, _) = parse_type_phrase("red spell");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::HasColor {
                color: ManaColor::Red
            }]))
        );
    }

    #[test]
    fn colorless_creature_card() {
        let (f, rest) = parse_type_phrase("colorless creature card with mana value 7 or greater");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::ColorCount {
                    comparator: Comparator::EQ,
                    count: 0,
                },
                FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 7 },
                }
            ]))
        );
    }

    #[test]
    fn mana_value_chosen_quality_suffix_maps_to_parity_choice() {
        let (filter, rest) = parse_target("creatures with mana value of the chosen quality");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let typed = typed_leg(&filter).expect("expected typed creature filter");
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
        assert!(typed.properties.contains(&FilterProp::ManaValueParity {
            parity: ParitySource::LastNamedChoice,
        }));
    }

    #[test]
    fn distributive_each_linker_preserves_mana_value_suffix() {
        let (f, rest) = parse_type_phrase("creatures, each with mana value 2 or less");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 2 },
            }]))
        );
    }

    #[test]
    fn comma_less_distributive_each_linker_preserves_mana_value_suffix() {
        // Dance of the Manse: "... cards each with mana value X or less" — the
        // distributive "each with" linker carries no comma, yet must still
        // normalize to the bare "with mana value …" suffix. Also exercises the
        // disjunctive (Or) form so the `Cmc` bound distributes onto every leg.
        let (f, rest) = parse_target(
            "up to x target artifact and/or non-aura enchantment cards each with mana value x or less",
        );
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = &f else {
            panic!("expected Or target, got {f:?}");
        };
        assert_eq!(filters.len(), 2);
        for leg in filters {
            let TargetFilter::Typed(tf) = leg else {
                panic!("expected typed leg, got {leg:?}");
            };
            assert!(
                tf.properties.contains(&FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                }),
                "each Or leg must carry the mana-value bound: {tf:?}"
            );
        }
    }

    #[test]
    fn distributive_each_linker_preserves_counter_suffix() {
        let (f, rest) = parse_type_phrase("creatures, each with ice counters on them");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Counters {
                    counters: CounterMatch::OfType(CounterType::Generic("ice".to_string())),
                    comparator: Comparator::GE,
                    count: QuantityExpr::Fixed { value: 1 },
                }])
            )
        );
    }

    #[test]
    fn distributive_each_linker_preserves_keyword_suffix() {
        let (f, rest) = parse_type_phrase("creatures, each with flying");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::WithKeyword {
                    value: Keyword::Flying,
                }
            ]))
        );
    }

    #[test]
    fn no_abilities_suffix_plural() {
        // CR 113.1 + CR 113.3: "creatures with no abilities" → Creature type +
        // HasNoAbilities property, fully consumed (Muraganda Petroglyphs anthem
        // subject).
        let (f, rest) = parse_type_phrase("creatures with no abilities");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = f else {
            panic!("expected Typed filter, got {f:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf.properties.contains(&FilterProp::HasNoAbilities));
    }

    #[test]
    fn no_abilities_suffix_singular() {
        // CR 113.1 + CR 113.3: singular "creature with no abilities" parses the
        // same predicate.
        let (f, rest) = parse_type_phrase("creature with no abilities");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = f else {
            panic!("expected Typed filter, got {f:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf.properties.contains(&FilterProp::HasNoAbilities));
    }

    #[test]
    fn colorless_adjective_does_not_distribute_across_or() {
        let (f, rest) = parse_type_phrase("artifact or colorless creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = f else {
            panic!("expected Or filter");
        };
        assert_eq!(filters.len(), 2);
        let TargetFilter::Typed(artifact) = &filters[0] else {
            panic!("expected artifact branch");
        };
        assert!(artifact.type_filters.contains(&TypeFilter::Artifact));
        assert!(!artifact.properties.iter().any(|property| matches!(
            property,
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 0,
            }
        )));
        let TargetFilter::Typed(creature) = &filters[1] else {
            panic!("expected creature branch");
        };
        assert!(creature.type_filters.contains(&TypeFilter::Creature));
        assert!(creature.properties.iter().any(|property| matches!(
            property,
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 0,
            }
        )));
    }

    #[test]
    fn monocolored_creature() {
        let (f, rest) = parse_type_phrase("monocolored creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::ColorCount {
                    comparator: Comparator::EQ,
                    count: 1,
                }])
            )
        );
    }

    #[test]
    fn multicolored_card() {
        let (f, rest) = parse_type_phrase("multicolored card");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::ColorCount {
                comparator: Comparator::GE,
                count: 2,
            }]))
        );
    }

    /// CR 208: "creature with power or toughness N or less" produces a
    /// disjunctive `AnyOf { [PtComparison(Power,LE,N), PtComparison(Toughness,LE,N)] }`
    /// property. Used by Arnyn Deathbloom Botanist's dies-trigger subject
    /// filter, Stern Scolding's counter target, Warping Wail mode 1, etc.
    #[test]
    fn creature_with_power_or_toughness_1_or_less() {
        let (f, _) = parse_type_phrase("creature with power or toughness 1 or less");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::AnyOf {
                props: vec![
                    FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Current,
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                    FilterProp::PtComparison {
                        stat: PtStat::Toughness,
                        scope: PtValueScope::Current,
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            }]))
        );
    }

    /// Disjunctive "or greater" form, mirror of the "or less" case.
    #[test]
    fn creature_with_power_or_toughness_3_or_greater() {
        let (f, _) = parse_type_phrase("creature with power or toughness 3 or greater");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::AnyOf {
                props: vec![
                    FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Current,
                        comparator: Comparator::GE,
                        value: QuantityExpr::Fixed { value: 3 },
                    },
                    FilterProp::PtComparison {
                        stat: PtStat::Toughness,
                        scope: PtValueScope::Current,
                        comparator: Comparator::GE,
                        value: QuantityExpr::Fixed { value: 3 },
                    },
                ],
            }]))
        );
    }

    /// Disjunctive "base" form — CR 208.4b. "creature with base power or
    /// toughness 1 or less" reads base P/T (after layer 7b, ignoring counters).
    #[test]
    fn creature_with_base_power_or_toughness_1_or_less() {
        let (f, _) = parse_type_phrase("creature with base power or toughness 1 or less");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::AnyOf {
                props: vec![
                    FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Base,
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                    FilterProp::PtComparison {
                        stat: PtStat::Toughness,
                        scope: PtValueScope::Base,
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            }]))
        );
    }

    /// Standalone "with toughness N or less" — mirror of the "with power N or
    /// less" form, routed through the shared combinator.
    #[test]
    fn creature_with_toughness_2_or_less() {
        let (f, _) = parse_type_phrase("creature with toughness 2 or less");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::PtComparison {
                    stat: PtStat::Toughness,
                    scope: PtValueScope::Current,
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 2 },
                }
            ]))
        );
    }

    #[test]
    fn creature_with_toughness_less_than_domain_count() {
        let (f, rest) = parse_type_phrase(
            "creature with toughness less than the number of basic land types among lands you control",
        );
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::PtComparison {
                    stat: PtStat::Toughness,
                    scope: PtValueScope::Current,
                    comparator: Comparator::LE,
                    value: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::BasicLandTypeCount {
                                controller: ControllerRef::You,
                            },
                        }),
                        offset: -1,
                    },
                }
            ]))
        );
    }

    #[test]
    fn creature_with_power_less_than_or_equal_to_controlled_count() {
        let (f, rest) = parse_type_phrase(
            "creature with power less than or equal to the number of allies you control",
        );
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::LE,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter {
                                type_filters: vec![TypeFilter::Subtype("Ally".to_string())],
                                controller: Some(ControllerRef::You),
                                properties: Vec::new(),
                            }),
                        },
                    },
                }
            ]))
        );
    }

    #[test]
    fn spell_with_mana_value_4_or_greater() {
        let (f, _) = parse_type_phrase("spell with mana value 4 or greater");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::Cmc {
                comparator: Comparator::GE,
                value: QuantityExpr::Fixed { value: 4 },
            }]))
        );
    }

    #[test]
    fn artifact_card_with_mana_value_4_or_5() {
        let (f, rest) = parse_type_phrase("artifact card with mana value 4 or 5, reveal it");
        assert_eq!(rest, ", reveal it");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).properties(vec![
                FilterProp::AnyOf {
                    props: vec![
                        FilterProp::Cmc {
                            comparator: Comparator::EQ,
                            value: QuantityExpr::Fixed { value: 4 },
                        },
                        FilterProp::Cmc {
                            comparator: Comparator::EQ,
                            value: QuantityExpr::Fixed { value: 5 },
                        },
                    ],
                },
            ]))
        );
    }

    /// CR 107.3a + CR 601.2b: Nature's Rhythm — "creature card with mana value X
    /// or less". The literal X must produce a `QuantityRef::Variable { "X" }`,
    /// resolved at effect time against the spell's announced X.
    #[test]
    fn creature_with_mana_value_x_or_less() {
        let (f, _) = parse_type_phrase("creature card with mana value x or less");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }]))
        );
    }

    #[test]
    fn spell_with_mana_value_x_or_greater() {
        let (f, _) = parse_type_phrase("spell with mana value x or greater");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::Cmc {
                comparator: Comparator::GE,
                value: QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }]))
        );
    }

    #[test]
    fn card_with_mana_value_equal_to_lands_you_control() {
        let (f, rest) = parse_type_phrase(
            "creature card with mana value equal to the number of lands you control",
        );
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(
                            TypedFilter::land().controller(ControllerRef::You)
                        ),
                    },
                },
            }]))
        );
    }

    /// CR 400.1 + CR 108.3 — Aether Vial class: a dynamic
    /// "with mana value equal to <quantity>" suffix must NOT swallow a trailing
    /// "from your hand" zone clause into the quantity phrase. The zone clause
    /// carries the controller scope; dropping it lets the resolver collect
    /// hand cards from every player (issue #1980). `parse_mana_value_suffix`
    /// must cut the quantity at the zone-clause boundary so the caller's
    /// `parse_zone_suffix` pass attaches `InZone { Hand }` + `controller: You`.
    #[test]
    fn dynamic_mana_value_suffix_leaves_trailing_zone_clause() {
        let (f, rest) = parse_type_phrase(
            "creature card with mana value equal to the number of charge counters on ~ from your hand",
        );
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(typed) = f else {
            panic!("expected typed filter, got {f:?}");
        };
        assert_eq!(
            typed.controller,
            Some(ControllerRef::You),
            "\"from your hand\" must scope to the controller's hand, got {:?}",
            typed.controller
        );
        assert!(
            typed
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Hand })),
            "filter must carry an InZone{{Hand}} property, got {:?}",
            typed.properties
        );
        assert!(
            typed.properties.iter().any(|p| matches!(
                p,
                FilterProp::Cmc {
                    comparator: Comparator::EQ,
                    ..
                }
            )),
            "the dynamic mana-value bound must still be parsed, got {:?}",
            typed.properties
        );
    }

    /// CR 119.3 + CR 400.1 — Regression guard (companion to
    /// `dynamic_mana_value_suffix_leaves_trailing_zone_clause`): when the
    /// quantity's OWN grammar already includes the zone clause — "the number of
    /// cards in your graveyard" canonicalizes to `GraveyardSize { Controller }`
    /// — the "in your graveyard" tail must stay attached to the *quantity*, not
    /// be cut off as a target-zone suffix. `parse_mana_value_suffix` must try the
    /// full phrase first and only cut on full-parse failure; pre-cutting left
    /// `parse_cda_quantity("the number of cards")` (which is `None`) and silently
    /// dropped the mana-value bound entirely for this whole class.
    #[test]
    fn dynamic_mana_value_suffix_keeps_zone_bearing_quantity_whole() {
        let (f, rest) = parse_type_phrase(
            "creature card with mana value equal to the number of cards in your graveyard",
        );
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::GraveyardSize {
                        player: crate::types::ability::PlayerScope::Controller,
                    },
                },
            }])),
            "the graveyard count must parse whole and stay on the quantity, not \
             leak its zone onto the target",
        );
    }

    #[test]
    fn card_with_mana_value_equal_to_offset_event_source() {
        let (f, rest) = parse_type_phrase(
            "creature card with mana value equal to 1 plus the sacrificed creature's mana value, put it",
        );
        assert_eq!(rest, ", put it");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::ObjectManaValue {
                            scope: ObjectScope::CostPaidObject,
                        },
                    }),
                    offset: 1,
                },
            }]))
        );
    }

    #[test]
    fn card_with_mana_value_equal_to_that_damage() {
        let (f, rest) = parse_type_phrase("artifact card with mana value equal to that damage");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).properties(vec![
                FilterProp::Cmc {
                    comparator: Comparator::EQ,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                }
            ]))
        );
    }

    #[test]
    fn card_with_lesser_mana_value_uses_event_source() {
        let (f, rest) = parse_type_phrase("creature card with lesser mana value, reveal it");
        assert_eq!(rest, ", reveal it");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: Comparator::LT,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::CostPaidObject,
                    },
                },
            }]))
        );
    }

    #[test]
    fn card_with_greater_mana_value_than_discarded_card() {
        let (f, rest) = parse_type_phrase("card with greater mana value than the discarded card");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::Cmc {
                comparator: Comparator::GT,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::CostPaidObject,
                    },
                },
            }]))
        );
    }

    #[test]
    fn card_with_same_mana_value_as_that_spell_uses_parent_target() {
        let (f, rest) = parse_type_phrase("card with the same mana value as that spell");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Target,
                    },
                },
            }]))
        );
    }

    #[test]
    fn card_with_same_mana_value_as_chosen_spell_uses_parent_target() {
        let (f, rest) =
            parse_type_phrase("creature card with the same mana value as the chosen spell");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Target,
                    },
                },
            }]))
        );
    }

    #[test]
    fn card_with_mana_value_equal_to_that_cards_mana_value() {
        let (f, rest) = parse_type_phrase("card with mana value equal to that card's mana value");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Target,
                    },
                },
            }]))
        );
    }

    #[test]
    fn card_with_mana_value_of_that_card_plus_one_uses_offset_target() {
        let (f, rest) = parse_type_phrase(
            "creature card with mana value equal to the mana value of that card plus one",
        );
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::ObjectManaValue {
                            scope: ObjectScope::Target,
                        },
                    }),
                    offset: 1,
                },
            }]))
        );
    }

    #[test]
    fn creature_you_control_with_power_2_or_less() {
        let (f, rest) = parse_type_phrase("creature you control with power 2 or less enter");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Current,
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 2 }
                    }])
            )
        );
        // Remaining text should be the event verb
        assert!(rest.trim_start().starts_with("enter"), "rest = {:?}", rest);
    }

    #[test]
    fn creature_with_power_3_or_greater() {
        let (f, _) = parse_type_phrase("creature with power 3 or greater");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 3 }
                }
            ]))
        );
    }

    #[test]
    fn creature_you_control_with_exact_base_power() {
        let (f, rest) = parse_type_phrase("creature you control with base power 1");
        assert_eq!(rest, "");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Base,
                        comparator: Comparator::EQ,
                        value: QuantityExpr::Fixed { value: 1 }
                    }])
            )
        );
    }

    #[test]
    fn creature_with_power_x_or_less() {
        // CR 107.3a + CR 601.2b: X is announced at cast; the filter retains the
        // `Variable("X")` marker so it can resolve against `chosen_x` at effect time.
        let (prop, _) = parse_power_suffix("with power x or less", &mut ParseContext::default())
            .expect("parses");
        assert_eq!(
            prop,
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string()
                    }
                }
            }
        );
    }

    #[test]
    fn creature_with_power_x_or_greater() {
        let (prop, _) = parse_power_suffix("with power x or greater", &mut ParseContext::default())
            .expect("parses");
        assert_eq!(
            prop,
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::GE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string()
                    }
                }
            }
        );
    }

    #[test]
    fn creatures_with_ice_counters_on_them() {
        let (f, _) = parse_type_phrase("creatures with ice counters on them");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Counters {
                    counters: CounterMatch::OfType(CounterType::Generic("ice".to_string())),
                    comparator: Comparator::GE,
                    count: QuantityExpr::Fixed { value: 1 },
                },])
            )
        );
    }

    #[test]
    fn cards_in_graveyards() {
        let (f, _) = parse_type_phrase("cards in graveyards");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }]))
        );
    }

    #[test]
    fn target_card_from_a_graveyard() {
        let (f, rest) = parse_target("target card from a graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard
            }]))
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn elf_on_the_battlefield() {
        let (f, rest) = parse_type_phrase("Elf on the battlefield");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Elf".to_string())
                    .properties(vec![FilterProp::InZone {
                        zone: Zone::Battlefield,
                    }],)
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_creature_card_in_your_graveyard() {
        let (f, rest) = parse_target("target creature card in your graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone {
                        zone: Zone::Graveyard
                    }])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_card_from_exile() {
        let (f, rest) = parse_target("target card from exile");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::InZone { zone: Zone::Exile }])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_card_in_a_graveyard() {
        let (f, _) = parse_target("target card in a graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard
            }]))
        );
    }

    /// Issue #586: Mistmoon Griffin needs "top creature card of your graveyard"
    /// to keep the creature filter, not become any card in the graveyard.
    #[test]
    fn target_top_creature_card_of_your_graveyard_keeps_type_filter() {
        let (f, rest) = parse_target("the top creature card of your graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone {
                        zone: Zone::Graveyard
                    }])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_top_instant_card_of_target_opponents_library_keeps_type_filter() {
        let (f, rest) = parse_target("the top instant card of target opponent's library");
        // The targeted player is resolved at runtime, not encoded here.
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant).properties(vec![
                FilterProp::InZone {
                    zone: Zone::Library
                }
            ]))
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_top_card_no_type_word_has_empty_type_filters() {
        // No type word before "card" means no type filter is captured.
        let (f, rest) = parse_target("the top card of your library");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::InZone {
                    zone: Zone::Library
                }],
                ..Default::default()
            })
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_top_creature_cards_plural_keeps_type_filter() {
        // Plural "cards" must thread the same filter as the singular path.
        let (f, rest) = parse_target("the top three creature cards of your library");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone {
                        zone: Zone::Library
                    }])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_top_subtype_card_of_zone_captures_subtype() {
        // Subtype words should be preserved as filters too.
        let (f, rest) = parse_target("the top spirit card of your graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Spirit".to_string())
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone {
                        zone: Zone::Graveyard
                    }])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn card_with_flashback_uses_keyword_kind_filter() {
        let (f, _) = parse_type_phrase("card with flashback");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::HasKeywordKind {
                    value: KeywordKind::Flashback,
                },])
            )
        );
    }

    #[test]
    fn card_with_augment_uses_keyword_kind_filter() {
        let (f, _) = parse_type_phrase("card with augment");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::HasKeywordKind {
                    value: KeywordKind::Augment,
                },])
            )
        );
    }

    #[test]
    fn card_with_mutate_uses_keyword_kind_filter() {
        // CR 702.140: "creature card with mutate" refers to the keyword class regardless
        // of its mana-cost parameter, so it must lower to a discriminant-level keyword-kind
        // filter rather than a concrete `Keyword::Mutate(cost)` exact match.
        let (f, _) = parse_type_phrase("creature card with mutate");
        let TargetFilter::Typed(TypedFilter {
            type_filters,
            properties,
            ..
        }) = f
        else {
            panic!("expected Typed filter, got {f:?}");
        };
        assert!(type_filters.contains(&TypeFilter::Creature));
        assert!(properties.contains(&FilterProp::HasKeywordKind {
            value: KeywordKind::Mutate,
        }));
    }

    #[test]
    fn otrimi_trigger_returns_mutate_creature_card_to_hand() {
        // CR 702.140: Otrimi's reflexive trigger returns "target creature card with mutate
        // from your graveyard to your hand" — a graveyard->hand bounce (destination None),
        // NOT a battlefield bounce. The target must be a creature card you own in your
        // graveyard that has the Mutate keyword kind.
        let (f, _) = parse_target("target creature card with mutate from your graveyard");
        let TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            properties,
            ..
        }) = f
        else {
            panic!("expected Typed filter, got {f:?}");
        };
        assert!(type_filters.contains(&TypeFilter::Creature));
        assert_eq!(controller, Some(ControllerRef::You));
        assert!(properties.contains(&FilterProp::HasKeywordKind {
            value: KeywordKind::Mutate,
        }));
        assert!(properties.contains(&FilterProp::InZone {
            zone: Zone::Graveyard
        }));
    }

    #[test]
    fn cards_with_flashback_you_own_in_exile() {
        let (f, _) = parse_type_phrase("cards with flashback you own in exile");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::HasKeywordKind {
                    value: KeywordKind::Flashback,
                },
                FilterProp::Owned {
                    controller: ControllerRef::You,
                },
                FilterProp::InZone { zone: Zone::Exile },
            ]))
        );
    }

    #[test]
    fn card_with_flashback_or_disturb_uses_keyword_kind_filters() {
        let (f, rest) =
            parse_type_phrase("card with flashback or disturb, put it into your graveyard");
        assert_eq!(rest, "put it into your graveyard");
        let TargetFilter::Or { filters } = f else {
            panic!("expected Or filter, got {f:?}");
        };
        assert_eq!(filters.len(), 2);
        for kind in [KeywordKind::Flashback, KeywordKind::Disturb] {
            assert!(
                filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(TypedFilter { type_filters, properties, .. })
                        if type_filters.contains(&TypeFilter::Card)
                            && properties.contains(&FilterProp::HasKeywordKind { value: kind })
                )),
                "missing {kind:?} branch in {filters:?}"
            );
        }
    }

    #[test]
    fn creature_of_the_chosen_type() {
        let (f, _) = parse_type_phrase("creature you control of the chosen type");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::IsChosenCreatureType])
            )
        );
    }

    #[test]
    fn creatures_you_control_with_flying() {
        let (f, _) = parse_type_phrase("creatures you control with flying");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::WithKeyword {
                        value: Keyword::Flying,
                    }])
            )
        );
    }

    #[test]
    fn creature_with_first_strike_and_vigilance() {
        let (f, _) = parse_type_phrase("creature with first strike and vigilance");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::WithKeyword {
                    value: Keyword::FirstStrike,
                },
                FilterProp::WithKeyword {
                    value: Keyword::Vigilance,
                },
            ]))
        );
    }

    #[test]
    fn creature_with_trample_or_haste_is_keyword_disjunction() {
        let (f, _) = parse_type_phrase("creature with trample or haste");
        let TargetFilter::Or { filters } = f else {
            panic!("expected Or filter, got {f:?}");
        };
        assert_eq!(filters.len(), 2);
        assert!(filters.iter().any(|filter| matches!(
            filter,
            TargetFilter::Typed(TypedFilter { type_filters, properties, .. })
                if type_filters.contains(&TypeFilter::Creature)
                    && properties.contains(&FilterProp::WithKeyword { value: Keyword::Trample })
        )));
        assert!(filters.iter().any(|filter| matches!(
            filter,
            TargetFilter::Typed(TypedFilter { type_filters, properties, .. })
                if type_filters.contains(&TypeFilter::Creature)
                    && properties.contains(&FilterProp::WithKeyword { value: Keyword::Haste })
        )));
    }

    #[test]
    fn creature_with_keyword_list_or_separator() {
        let (f, rest) = parse_type_phrase(
            "creature with deathtouch, hexproof, reach, or trample and reveal it",
        );
        assert_eq!(rest, "reveal it");
        let TargetFilter::Or { filters } = f else {
            panic!("expected Or filter, got {f:?}");
        };
        assert_eq!(filters.len(), 4);
        for keyword in [
            Keyword::Deathtouch,
            Keyword::Hexproof,
            Keyword::Reach,
            Keyword::Trample,
        ] {
            assert!(
                filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(TypedFilter { type_filters, properties, .. })
                        if type_filters.contains(&TypeFilter::Creature)
                            && properties.contains(&FilterProp::WithKeyword {
                                value: keyword.clone()
                            })
                )),
                "missing {keyword:?} in {filters:?}"
            );
        }
    }

    #[test]
    fn other_nonland_permanents_you_own_and_control() {
        let (f, _) = parse_type_phrase("other nonland permanents you own and control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent()
                    .controller(ControllerRef::You)
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
                    .properties(vec![
                        FilterProp::Another,
                        FilterProp::Owned {
                            controller: ControllerRef::You,
                        },
                    ])
            )
        );
    }

    #[test]
    fn permanents_you_own() {
        let (f, _) = parse_type_phrase("permanents you own");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::Owned {
                controller: ControllerRef::You,
            }]))
        );
    }

    // A2 (Zedruu): "you own" sets `FilterProp::Owned{You}`; the trailing
    // "that your opponents control" relative clause supplies the object
    // controller via the new `controller.is_none()`-gated "that <ctrl>" arm,
    // yielding the owned-but-opponent-controlled population. The full phrase is
    // consumed (empty remainder).
    #[test]
    fn permanents_you_own_that_your_opponents_control() {
        let (f, rest) = parse_type_phrase("permanents you own that your opponents control");
        assert_eq!(rest, "");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent()
                    .controller(ControllerRef::Opponent)
                    .properties(vec![FilterProp::Owned {
                        controller: ControllerRef::You,
                    }])
            )
        );
    }

    // A2: the same phrase routed through `parse_quantity_ref` yields an
    // ObjectCount over the owned-but-opponent-controlled population.
    #[test]
    fn quantity_ref_permanents_you_own_that_your_opponents_control() {
        use crate::parser::oracle_quantity::parse_quantity_ref;
        let qty =
            parse_quantity_ref("the number of permanents you own that your opponents control");
        match qty {
            Some(QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(typed),
            }) => {
                assert_eq!(typed.controller, Some(ControllerRef::Opponent));
                assert!(typed.properties.contains(&FilterProp::Owned {
                    controller: ControllerRef::You,
                }));
            }
            other => panic!("Expected ObjectCount{{owned-by-you,opp-controlled}}, got {other:?}"),
        }
    }

    #[test]
    fn other_creatures_you_control() {
        let (f, _) = parse_type_phrase("other creatures you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Another])
            )
        );
    }

    // ── Anaphoric pronouns (Building Block C) ──

    #[test]
    fn those_cards_produces_tracked_set() {
        let (f, rest) = parse_target("those cards");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    /// Issue #4780 — Druid of Purification: "Destroy each permanent chosen this
    /// way." The "[each] <noun> chosen this way" anaphor must resolve to the
    /// published tracked set (CR 608.2c), not a board-wide `Typed(Permanent)`
    /// filter that would destroy every permanent.
    #[test]
    fn each_permanent_chosen_this_way_produces_tracked_set() {
        for phrase in [
            "each permanent chosen this way",
            "permanent chosen this way",
            "each creature chosen this way",
            "the artifacts chosen this way",
        ] {
            let (f, rest) = parse_target(phrase);
            assert_eq!(
                f,
                TargetFilter::TrackedSet {
                    id: TrackedSetId(0)
                },
                "{phrase:?} must resolve to the published tracked set"
            );
            assert_eq!(rest, "", "{phrase:?} must be fully consumed");
        }
    }

    #[test]
    fn the_rest_produces_tracked_set() {
        let (f, rest) = parse_target("the rest");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn both_cards_produces_tracked_set() {
        // CR 608.2c: Sword of Hearth and Home — "exile up to one target
        // creature you own, then search your library for a basic land card.
        // Put both cards onto the battlefield under your control." "both
        // cards" is an anaphoric back-reference to the exiled creature + the
        // searched land, both published into the chain-scoped tracked set.
        let (f, rest) = parse_target("both cards");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn those_tokens_produces_tracked_set() {
        let (f, rest) = parse_target("those tokens");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn those_lands_produce_tracked_set() {
        let (filter, rest) = parse_target("those lands");
        assert_eq!(
            filter,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn the_token_inherits_parent_target() {
        let (filter, rest) = parse_target("the token");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn the_chosen_creature_inherits_parent_target() {
        let (filter, rest) = parse_target("the chosen creature");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn the_chosen_card_inherits_parent_target() {
        let (filter, rest) = parse_target("the chosen card");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn the_chosen_permanent_inherits_parent_target() {
        let (filter, rest) = parse_target("the chosen permanent");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn the_chosen_cards_produce_tracked_set() {
        let (filter, rest) = parse_target("the chosen cards");
        assert_eq!(
            filter,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn one_of_them_inherits_parent_target() {
        let (filter, rest) = parse_target("one of them");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn one_of_those_cards_inherits_parent_target() {
        let (filter, rest) = parse_target("one of those cards");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn selected_one_of_those_lands_with_choice_inherits_parent_target() {
        let (filter, rest) = parse_target("one of those lands of their choice and untaps it");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, " and untaps it");
    }

    #[test]
    fn different_one_of_those_creatures_inherits_parent_target() {
        let (filter, rest) = parse_target("a different one of those creatures");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn subtype_one_of_those_dragons_inherits_parent_target() {
        let (filter, rest) = parse_target("one of those Dragons");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    /// Issue #1338: "each of those Vampires" must intersect the tracked tap set,
    /// not degenerate to an empty TypedFilter over the whole battlefield.
    #[test]
    fn each_of_those_vampires_is_tracked_set_filtered() {
        use crate::types::TypeFilter;
        let (filter, rest) = parse_target("each of those Vampires");
        match filter {
            TargetFilter::TrackedSetFiltered { id, filter, .. } => {
                assert_eq!(id, TrackedSetId(0));
                match *filter {
                    TargetFilter::Typed(tf) => {
                        assert!(tf
                            .type_filters
                            .contains(&TypeFilter::Subtype("Vampire".into())));
                    }
                    other => panic!("expected Typed Vampire filter, got {other:?}"),
                }
            }
            other => panic!("expected TrackedSetFiltered, got {other:?}"),
        }
        assert_eq!(rest, "");
    }

    #[test]
    fn each_of_those_creatures_is_tracked_set() {
        let (filter, rest) = parse_target("each of those creatures");
        assert!(matches!(
            filter,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        ));
        assert_eq!(rest, "");
    }

    /// CR 601.2c: "each of <count> target <type>" must route through "target"
    /// parsing (a concrete creature filter), NOT degenerate to the bare "each "
    /// all-matching path. This guard is the safety net for any non-counter
    /// effect that reaches `parse_target` with the exact-count form.
    #[test]
    fn each_of_count_target_creatures_routes_to_target_filter() {
        let (filter, rest) = parse_target("each of two target creatures");
        assert_eq!(filter, TargetFilter::Typed(TypedFilter::creature()));
        assert_eq!(rest, "");
    }

    /// CR 608.2c: "each of them" is a plural-pronoun anaphor and must map to
    /// `ParentTarget`, not degenerate to the all-matching "each <type>" path.
    /// This guard ensures that all sibling effects (counter, destroy, exile,
    /// bounce, tap, etc.) route through the central parser rather than needing
    /// their own special-case intercepts.
    #[test]
    fn each_of_them_is_parent_target() {
        let mut ctx = ParseContext::default();
        let (filter, rest, _syntax) = parse_target_with_syntax("each of them", &mut ctx);
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    /// Word-boundary guard: "each of themselves" must NOT match the
    /// "each of them" arm — the trailing "selves" suffix makes it a distinct
    /// word that the word-boundary check (`parse_word_bounded`) must reject.
    #[test]
    fn each_of_themselves_does_not_match_each_of_them_arm() {
        let (filter, _rest) = parse_target("each of themselves");
        assert_ne!(
            filter,
            TargetFilter::ParentTarget,
            "\"each of themselves\" must not bind to ParentTarget via the \"each of them\" arm"
        );
    }

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

    /// Goblin Welder's two artifact slots: `[artifact on battlefield, artifact
    /// card in graveyard]` — the registry the "Choose target artifact a player
    /// controls and target artifact card in that player's graveyard" head
    /// declares. The generalized resolver reproduces the old hardcoded artifact
    /// disambiguation purely from these slots' zone properties.
    fn goblin_welder_slots() -> Vec<TargetFilter> {
        vec![
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                controller: None,
                properties: vec![],
            }),
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                controller: None,
                properties: vec![FilterProp::InZone {
                    zone: Zone::Graveyard,
                }],
            }),
        ]
    }

    fn parse_target_with_slots(text: &str, slots: Vec<TargetFilter>) -> (TargetFilter, &str) {
        let mut ctx = ParseContext {
            declared_target_slots: slots,
            ..ParseContext::default()
        };
        let (filter, rest, _) = parse_target_with_syntax(text, &mut ctx);
        (filter, rest)
    }

    #[test]
    fn definite_artifact_reference_binds_first_parent_target_slot() {
        // Threads the two-artifact registry so the general resolver reproduces
        // the deleted hardcoded "the artifact" → slot 0 arm.
        let (filter, rest) =
            parse_target_with_slots("the artifact and returns it", goblin_welder_slots());
        assert_eq!(filter, TargetFilter::ParentTargetSlot { index: 0 });
        assert_eq!(rest, " and returns it");
    }

    #[test]
    fn definite_artifact_card_reference_binds_second_parent_target_slot() {
        let (filter, rest) = parse_target_with_slots(
            "the artifact card to the battlefield",
            goblin_welder_slots(),
        );
        assert_eq!(filter, TargetFilter::ParentTargetSlot { index: 1 });
        assert_eq!(rest, " to the battlefield");
    }

    #[test]
    fn definite_artifact_reference_does_not_steal_type_phrase() {
        // "the artifact creature" is a fresh compound type phrase, never an
        // anaphor — even with the registry populated.
        let (filter, rest) =
            parse_target_with_slots("the artifact creature", goblin_welder_slots());
        assert_ne!(filter, TargetFilter::ParentTargetSlot { index: 0 });
        assert_ne!(rest, " creature");
    }

    #[test]
    fn definite_reference_empty_registry_is_none() {
        // Claim 4/7: with no declared slots the resolver never guesses a slot —
        // it returns None so the broad `ParentTarget`/set-selection arms win.
        assert_eq!(
            parse_definite_parent_reference("the artifact and returns it", &[]),
            None
        );
        assert_eq!(
            parse_definite_parent_reference("the chosen creature", &[]),
            None
        );
    }

    #[test]
    fn definite_reference_ambiguous_registry_is_none() {
        // Two same-type battlefield slots + a bare "the creature" anaphor: two
        // slots tie, so the resolver returns None (no silent slot-0 guess)
        // rather than binding a specific slot.
        let two_creatures = vec![
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::You),
                properties: vec![],
            }),
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::Opponent),
                properties: vec![],
            }),
        ];
        assert_eq!(
            parse_definite_parent_reference("the creature and it fights", &two_creatures),
            None
        );
    }

    #[test]
    fn set_selection_arm_unshadowed_by_empty_registry() {
        // Claim 7: a "the chosen creature" set-selection card with NO dual-target
        // declaration (empty registry) must still parse to `ParentTarget` via the
        // `SELECTED_FROM_SET_PHRASES` arm — the generalized resolver returns None
        // and does not shadow it with a `ParentTargetSlot`.
        let (filter, _rest) = parse_target_with_slots("the chosen creature", vec![]);
        assert_eq!(filter, TargetFilter::ParentTarget);
    }

    #[test]
    fn stolen_uniform_anaphors_bind_precise_slots() {
        // Claim 4 (parser shape): with the Stolen Uniform registry
        // `[creature you control, Equipment]`, "that Equipment" → slot 1 and
        // "the chosen creature" → slot 0, disambiguated purely by type.
        let slots = vec![
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::You),
                properties: vec![],
            }),
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Subtype("Equipment".to_string())],
                controller: None,
                properties: vec![],
            }),
        ];
        let (equip, rest) =
            parse_target_with_slots("that Equipment until end of turn", slots.clone());
        assert_eq!(equip, TargetFilter::ParentTargetSlot { index: 1 });
        assert_eq!(rest, " until end of turn");
        let (creature, _) = parse_target_with_slots("the chosen creature.", slots);
        assert_eq!(creature, TargetFilter::ParentTargetSlot { index: 0 });
    }

    #[test]
    fn new_targets_for_the_copy_inherits_parent_target() {
        let (filter, rest) = parse_target("new targets for the copy");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn new_targets_for_it_inherits_parent_target() {
        let (filter, rest) = parse_target("new targets for it");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn up_to_one_of_them_inherits_parent_target() {
        let (filter, rest) = parse_target("up to one of them");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn either_of_them_inherits_parent_target() {
        let (filter, rest) = parse_target("either of them");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn quantified_target_phrase_strips_prefix() {
        let (filter, rest) = parse_target("one or two target creatures");
        assert_eq!(filter, TargetFilter::Typed(TypedFilter::creature()));
        assert_eq!(rest, "");
    }

    #[test]
    fn quantified_up_to_target_phrase_strips_prefix() {
        let (filter, rest) = parse_target("up to one target creature you control");
        assert_eq!(
            filter,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn quantified_x_target_phrase_strips_prefix() {
        let (filter, rest) = parse_target("X target creature cards from your graveyard");
        let TargetFilter::Typed(tf) = filter else {
            panic!("Expected Typed filter");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf.properties.contains(&FilterProp::InZone {
            zone: Zone::Graveyard
        }));
        assert_eq!(rest, "");
    }

    #[test]
    fn of_them_produces_tracked_set() {
        let (filter, rest) = parse_target("of them");
        assert_eq!(
            filter,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn the_exiled_card_produces_tracked_set() {
        let (f, _) = parse_target("the exiled card");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
    }

    #[test]
    fn the_exiled_permanents_produces_tracked_set() {
        let (f, _) = parse_target("the exiled permanents");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
    }

    #[test]
    fn the_exiled_card_with_exile_cost_context_produces_cost_paid_object() {
        // CR 608.2k: with an active exile cost, "the exiled card" is the
        // cost-paid object (Jhoira of the Ghitu), not an effect tracked set.
        let mut ctx = ParseContext {
            current_ability_exile_cost_zone: Some(Zone::Hand),
            ..ParseContext::default()
        };
        let (f, _) = parse_target_with_ctx("the exiled card", &mut ctx);
        assert_eq!(f, TargetFilter::CostPaidObject);
    }

    #[test]
    fn the_exiled_card_without_exile_cost_stays_tracked_set() {
        // No exile cost → "exiled" is an effect participle → TrackedSet.
        let mut ctx = ParseContext::default();
        let (f, _) = parse_target_with_ctx("the exiled card", &mut ctx);
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
    }

    // ── ExiledBySource ──

    #[test]
    fn each_card_exiled_with_tilde_produces_exiled_by_source() {
        let (f, rest) = parse_target("each card exiled with ~ into its owner's graveyard");
        assert_eq!(f, TargetFilter::ExiledBySource);
        assert_eq!(rest, " into its owner's graveyard");
    }

    #[test]
    fn parse_target_it_inherits_parent_target() {
        let (filter, rest) = parse_target("it");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_them_inherits_parent_target() {
        let (filter, rest) = parse_target("them");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_standalone_that_spell_is_triggering_source() {
        let (filter, rest, _) =
            parse_target_with_syntax("that spell", &mut ParseContext::default());
        assert_eq!(filter, TargetFilter::TriggeringSource);
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_standalone_that_card_is_parent_target() {
        let (filter, rest, _) = parse_target_with_syntax("that card", &mut ParseContext::default());
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_that_spell_inherits_parent_target() {
        let (filter, rest) = parse_target("that spell is countered this way");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, " is countered this way");
    }

    #[test]
    fn parse_target_that_creature_inherits_parent_target() {
        // CR 608.2c: Without trigger context, "that creature" defaults to the
        // parent target (Twinflame Strive: "create a token that's a copy of that
        // creature"). Trigger-context resolution to `TriggeringSource` is layered
        // on top of `parse_target` by callers that thread a `ParseContext` (see
        // `resolve_counter_placement_target` in `oracle_effect/counter.rs`).
        let (filter, rest) = parse_target("that creature");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_that_creature_controller_uses_parent_target_controller() {
        let (filter, rest) = parse_target("that creature's controller");
        assert_eq!(filter, TargetFilter::ParentTargetController);
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_that_land_controller_uses_parent_target_controller() {
        let (filter, rest) = parse_target("that land's controller");
        assert_eq!(filter, TargetFilter::ParentTargetController);
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_its_owner_uses_parent_target_owner() {
        // CR 108.3 + CR 608.2c: "its owner" anaphor — owner of the parent
        // target object (Enslave: "enchanted creature deals 1 damage to its
        // owner"; Bomb Squad: "that creature deals 4 damage to its owner").
        let (filter, rest) = parse_target("its owner");
        assert_eq!(filter, TargetFilter::ParentTargetOwner);
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_their_owner_uses_parent_target_owner() {
        let (filter, rest) = parse_target("their owner");
        assert_eq!(filter, TargetFilter::ParentTargetOwner);
        assert_eq!(rest, "");
    }

    #[test]
    fn each_card_exiled_with_this_artifact_produces_exiled_by_source() {
        let (f, rest) = parse_target("each card exiled with this artifact");
        assert_eq!(f, TargetFilter::ExiledBySource);
        assert_eq!(rest, "");
    }

    #[test]
    fn card_exiled_with_this_artifact_produces_exiled_by_source() {
        let (f, rest) = parse_target("card exiled with this artifact");
        assert_eq!(f, TargetFilter::ExiledBySource);
        assert_eq!(rest, "");
    }

    #[test]
    fn cards_exiled_with_tilde_produces_exiled_by_source() {
        let (f, _) = parse_target("cards exiled with ~");
        assert_eq!(f, TargetFilter::ExiledBySource);
    }

    #[test]
    fn all_cards_they_own_exiled_with_it_produces_exiled_by_source() {
        let (f, rest) = parse_target("all cards they own exiled with it");
        assert_eq!(f, TargetFilter::ExiledBySource);
        assert_eq!(rest, "");
    }

    #[test]
    fn cards_they_own_exiled_with_it_produces_exiled_by_source() {
        let (f, rest) = parse_target("cards they own exiled with it");
        assert_eq!(f, TargetFilter::ExiledBySource);
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_type_phrase_creature_that_had_counters_put_on_it_this_way() {
        let (f, rest) = parse_type_phrase("creature that had counters put on it this way");
        assert_eq!(rest, "", "remainder was {rest:?}");
        assert_eq!(
            f,
            TargetFilter::TrackedSetFiltered {
                id: TrackedSetId(0),
                filter: Box::new(TargetFilter::Typed(TypedFilter::creature())),
                caused_by: None,
            }
        );
    }

    /// Issue #2903 — Agitator Ant: goad only creatures that received counters
    /// from the preceding instruction in the same ability.
    #[test]
    fn creature_that_had_counters_put_on_it_this_way_is_tracked_set_filtered() {
        let (f, rest) = parse_target("creature that had counters put on it this way");
        assert_eq!(rest, "");
        assert_eq!(
            f,
            TargetFilter::TrackedSetFiltered {
                id: TrackedSetId(0),
                filter: Box::new(TargetFilter::Typed(TypedFilter::creature())),
                caused_by: None,
            }
        );
    }

    /// Issue #547 — Espers to Magicite: "choose up to one target creature card
    /// exiled this way". The bare past-participle "exiled this way" (no relative
    /// "that was/were") must still compose the `ExiledBySource` linkage onto the
    /// typed filter, or the target degrades to a battlefield creature.
    #[test]
    fn singular_creature_card_exiled_this_way_composes_exiled_by_source() {
        let (f, rest) = parse_target("target creature card exiled this way");
        assert_eq!(rest, "");
        assert!(
            f.references_exiled_by_source(),
            "bare \"exiled this way\" must attach ExiledBySource, got {f:?}"
        );
        match f {
            TargetFilter::And { filters } => {
                assert!(
                    filters.contains(&TargetFilter::ExiledBySource),
                    "And must include ExiledBySource, got {filters:?}"
                );
                assert!(
                    filters.iter().any(|inner| matches!(
                        inner,
                        TargetFilter::Typed(tf)
                            if tf.type_filters.contains(&TypeFilter::Creature)
                    )),
                    "And must include a Typed creature filter, got {filters:?}"
                );
            }
            other => panic!("expected And {{ Typed, ExiledBySource }}, got {other:?}"),
        }
    }

    // ── HasChosenName suffix (CR 201.2a + CR 201.4) ──
    //
    // Building-block coverage for the "<type-phrase> with the chosen name" /
    // "<type-phrase> with a name chosen for this enchantment" suffix recognized
    // inside parse_type_phrase_with_ctx. This is verb-agnostic: every
    // object-target effect clause (goad/destroy/exile/tap/...) funnels through
    // this chokepoint, so the recognizer must compose the HasChosenName leg onto
    // the typed filter regardless of the surrounding verb. Day of the Moon is
    // the immediate unlock (goad all creatures with a name chosen for this
    // enchantment); a card-level regression for it lives in oracle.rs.

    /// Assert the filter is `And { [Typed(Creature), HasChosenName] }`.
    fn assert_chosen_name_creature_and(f: &TargetFilter) {
        match f {
            TargetFilter::And { filters } => {
                assert!(
                    filters.contains(&TargetFilter::HasChosenName),
                    "And must include HasChosenName, got {filters:?}"
                );
                assert!(
                    filters.iter().any(|inner| matches!(
                        inner,
                        TargetFilter::Typed(tf)
                            if tf.type_filters.contains(&TypeFilter::Creature)
                    )),
                    "And must include a Typed creature filter, got {filters:?}"
                );
            }
            other => panic!("expected And {{ Typed, HasChosenName }}, got {other:?}"),
        }
    }

    #[test]
    fn creatures_with_name_chosen_for_tilde_composes_has_chosen_name() {
        // `~` is the normalized self-reference for "this enchantment"/"this
        // permanent"/etc. (SELF_REF_TYPE_PHRASES). The parser sees the normalized
        // form, so the recognizer matches `~` rather than the literal noun.
        let (f, rest) = parse_target("creatures with a name chosen for ~");
        assert_eq!(rest, "", "the chosen-name suffix must be fully consumed");
        assert_chosen_name_creature_and(&f);
    }

    #[test]
    fn creatures_with_the_chosen_name_composes_has_chosen_name() {
        let (f, rest) = parse_target("creatures with the chosen name");
        assert_eq!(rest, "", "the chosen-name suffix must be fully consumed");
        assert_chosen_name_creature_and(&f);
    }

    #[test]
    fn singular_creature_with_the_chosen_name_composes_has_chosen_name() {
        // Verb-agnostic singular form (e.g. "destroy each creature with the
        // chosen name") must compose the same way as the plural goad form.
        let (f, rest) = parse_target("creature with the chosen name");
        assert_eq!(rest, "", "the chosen-name suffix must be fully consumed");
        assert_chosen_name_creature_and(&f);
    }

    #[test]
    fn creatures_with_flying_does_not_attach_has_chosen_name() {
        // Negative: an unrelated "with <keyword>" suffix must not spuriously
        // attach HasChosenName.
        let (f, _rest) = parse_target("creatures with flying");
        assert!(
            !filter_contains_has_chosen_name(&f),
            "flying must not attach HasChosenName, got {f:?}"
        );
    }

    #[test]
    fn bare_creatures_does_not_attach_has_chosen_name() {
        // Negative: a bare type phrase must stay a bare Typed filter with no
        // spurious And wrap.
        let (f, rest) = parse_target("creatures");
        assert_eq!(rest, "");
        assert!(
            matches!(&f, TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Creature)),
            "bare \"creatures\" must be a Typed creature filter, got {f:?}"
        );
    }

    /// Recursively check whether any leaf of the filter is `HasChosenName`.
    fn filter_contains_has_chosen_name(f: &TargetFilter) -> bool {
        match f {
            TargetFilter::HasChosenName => true,
            TargetFilter::And { filters } | TargetFilter::Or { filters } => {
                filters.iter().any(filter_contains_has_chosen_name)
            }
            _ => false,
        }
    }

    #[test]
    fn exiled_cards_with_named_counters_produces_exile_counter_filter() {
        let (f, rest) = parse_target("exiled cards with aegis counters on them");
        assert_eq!(rest, "");
        match f {
            TargetFilter::Typed(tf) => {
                assert!(tf
                    .properties
                    .contains(&FilterProp::InZone { zone: Zone::Exile }));
                assert!(tf.properties.iter().any(|prop| matches!(
                    prop,
                    FilterProp::Counters { counters: CounterMatch::OfType(counter_type), .. }
                        if counter_type.as_str() == "aegis"
                )));
            }
            other => panic!("expected typed exiled-card filter, got {other:?}"),
        }
    }

    #[test]
    fn target_creature_card_exiled_with_tilde_produces_and_filter() {
        // CR 406.6: Singular targeted form — composes typed filter with the
        // exile-link constraint via TargetFilter::And.
        let (f, rest) = parse_target("target creature card exiled with ~");
        assert_eq!(
            f,
            TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature()),
                    TargetFilter::ExiledBySource,
                ],
            }
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn creature_card_exiled_with_it_produces_and_filter() {
        // CR 607.2 + CR 406.6: Sothera, the Supervoid's descriptor form (no
        // "target" keyword, anaphoric "it") — "put a creature card exiled with
        // it onto the battlefield". Must compose the typed filter with the
        // exile-link constraint identically to the "exiled with ~" form; without
        // the "exiled with it" arm the suffix is dropped and the target degrades
        // to a bare battlefield "creature card".
        let (f, rest) = parse_target("a creature card exiled with it");
        assert_eq!(
            f,
            TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature()),
                    TargetFilter::ExiledBySource,
                ],
            },
            "expected And{{Typed(creature), ExiledBySource}}, got {f:?} — the \
             ExiledBySource leg (the revert-failing assertion) restricts \
             reanimation to the source's OWN linked-exile pool"
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn bare_card_exiled_with_it_unaffected_by_typed_arm() {
        // Sibling guard: the untyped "card exiled with it" form (handled by the
        // top-of-function plural/each-card block) still yields bare
        // ExiledBySource — the new typed arm must not perturb it.
        let (f, rest) = parse_target("card exiled with it");
        assert_eq!(f, TargetFilter::ExiledBySource);
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_creature_card_exiled_with_this_creature_produces_and_filter() {
        let (f, rest) = parse_target("target creature card exiled with this creature");
        assert_eq!(
            f,
            TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature()),
                    TargetFilter::ExiledBySource,
                ],
            }
        );
        assert_eq!(rest.trim(), "");
    }

    // ── "from a single graveyard" zone qualifier ──

    #[test]
    fn target_card_from_a_single_graveyard() {
        // CR 400.7: "a single graveyard" shares origin-zone semantics with
        // bare "a graveyard"; the modifier constrains which instance, not
        // which zone.
        let (f, rest) = parse_target("target card from a single graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard
            }]))
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn up_to_two_target_cards_from_a_single_graveyard() {
        // Hearse activated ability target text after "exile " is stripped.
        let (f, rest) = parse_target("up to two target cards from a single graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard
            }]))
        );
        assert_eq!(rest.trim(), "");
    }

    // ── Bare type phrase fallback ──

    #[test]
    fn bare_type_phrase_fallback() {
        let (f, _) = parse_target("other nonland permanents you own and control");
        // Should be Typed (not Any) — parse_type_phrase picks up the permanent type + properties
        match f {
            TargetFilter::Typed(tf) => {
                assert!(
                    !tf.type_filters.is_empty() || !tf.properties.is_empty(),
                    "Expected meaningful type info, got {:?}",
                    tf
                );
            }
            other => panic!("Expected Typed, got {:?}", other),
        }
    }

    #[test]
    fn unrecognized_bare_text_stays_any() {
        let (f, _) = parse_target("foobar");
        assert_eq!(f, TargetFilter::Any);
    }

    #[test]
    fn parse_cost_paid_object_reference() {
        let (filter, rest) = parse_target("the sacrificed creature");
        assert_eq!(filter, TargetFilter::CostPaidObject);
        assert!(rest.is_empty(), "remainder: {rest:?}");
    }

    #[test]
    fn parse_event_context_that_spells_controller() {
        let (filter, rem) = parse_event_context_ref("that spell's controller").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringSpellController);
        assert_eq!(rem, "");
    }

    #[test]
    fn parse_event_context_that_spells_owner() {
        let (filter, rem) = parse_event_context_ref("that spell's owner").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringSpellOwner);
        assert_eq!(rem, "");
    }

    #[test]
    fn parse_event_context_that_player() {
        let (filter, rem) = parse_event_context_ref("that player").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringPlayer);
        assert_eq!(rem, "");
    }

    #[test]
    fn parse_event_context_that_source() {
        let (filter, rem) = parse_event_context_ref("that source").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringSource);
        assert_eq!(rem, "");
    }

    #[test]
    fn parse_event_context_that_permanent() {
        let (filter, rem) = parse_event_context_ref("that permanent").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringSource);
        assert_eq!(rem, "");
    }

    #[test]
    fn parse_event_context_that_permanent_or_player_declines() {
        assert_eq!(parse_event_context_ref("that permanent or player"), None);
        assert_eq!(parse_event_context_ref("that permanent or a player"), None);
    }

    #[test]
    fn parse_event_context_returns_none_for_non_event() {
        assert_eq!(parse_event_context_ref("target creature"), None);
        assert_eq!(parse_event_context_ref("any target"), None);
    }

    #[test]
    fn parse_event_context_defending_player() {
        let (filter, rem) = parse_event_context_ref("defending player").unwrap();
        assert_eq!(filter, TargetFilter::DefendingPlayer);
        assert_eq!(rem, "");
    }

    #[test]
    fn parse_event_context_defending_player_prefix() {
        let (filter, rem) =
            parse_event_context_ref("defending player reveals the top card").unwrap();
        assert_eq!(filter, TargetFilter::DefendingPlayer);
        assert_eq!(rem, " reveals the top card");
    }

    #[test]
    fn event_context_ref_preserves_remainder() {
        // Compound remainder preserved with leading space
        let (filter, rem) = parse_event_context_ref("that player and you gain 2 life").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringPlayer);
        assert_eq!(rem, " and you gain 2 life");

        // "that source" with remainder
        let (filter, rem) = parse_event_context_ref("that source and you draw a card").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringSource);
        assert_eq!(rem, " and you draw a card");
    }

    #[test]
    fn parse_counter_suffix_stun_counter() {
        let result = parse_counter_suffix(" with a stun counter on it");
        assert!(result.is_some());
        let (prop, _consumed) = result.unwrap();
        assert!(matches!(
            prop,
            FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Stun),
                comparator: Comparator::GE,
                count: QuantityExpr::Fixed { value: 1 },
            }
        ));
    }

    #[test]
    fn parse_counter_suffix_oil_counter() {
        let result = parse_counter_suffix(" with an oil counter on it");
        assert!(result.is_some());
        let (prop, _consumed) = result.unwrap();
        assert!(matches!(
            prop,
            FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Generic(ref s)),
                comparator: Comparator::GE,
                count: QuantityExpr::Fixed { value: 1 },
            } if s == "oil"
        ));
    }

    #[test]
    fn parse_counter_suffix_not_counter_phrase() {
        let result = parse_counter_suffix(" with power 3 or greater");
        assert!(result.is_none());
    }

    /// #526 Wave Goodbye — typed negation: "without a +1/+1 counter on it"
    /// must produce a negated typed counter filter, not silently drop the clause.
    #[test]
    fn parse_counter_suffix_without_typed_counter() {
        let (prop, _consumed) =
            parse_counter_suffix(" without a +1/+1 counter on it").expect("must parse");
        assert_eq!(
            prop,
            FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                comparator: Comparator::EQ,
                count: QuantityExpr::Fixed { value: 0 },
            }
        );
    }

    /// #526 — article-free plural negated typed counter.
    #[test]
    fn parse_counter_suffix_without_typed_counter_plural() {
        let (prop, _consumed) =
            parse_counter_suffix(" without +1/+1 counters on them").expect("must parse");
        assert_eq!(
            prop,
            FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                comparator: Comparator::EQ,
                count: QuantityExpr::Fixed { value: 0 },
            }
        );
    }

    /// #527 Damning Verdict — untyped negation: "with no counters on them" must
    /// produce `Counters { Any, EQ, Fixed(0) }`, NOT `None` (the v1 plan bug).
    #[test]
    fn parse_counter_suffix_with_no_counters() {
        let (prop, _consumed) =
            parse_counter_suffix(" with no counters on them").expect("must not be None");
        assert_eq!(
            prop,
            FilterProp::Counters {
                counters: CounterMatch::Any,
                comparator: Comparator::EQ,
                count: QuantityExpr::Fixed { value: 0 },
            }
        );
    }

    /// "without counters" — bare untyped negation, no "on it/them" suffix.
    #[test]
    fn parse_counter_suffix_without_bare_counters() {
        let (prop, _consumed) =
            parse_counter_suffix(" without counters").expect("must not be None");
        assert_eq!(
            prop,
            FilterProp::Counters {
                counters: CounterMatch::Any,
                comparator: Comparator::EQ,
                count: QuantityExpr::Fixed { value: 0 },
            }
        );
    }

    /// Regression — bare positive "with a counter on it" → any-counter GE 1.
    #[test]
    fn parse_counter_suffix_bare_positive_any() {
        for phrase in [
            " with a counter on it",
            " with a counter on them",
            " with any counter on it",
            " with any counter on them",
            " with counters on it",
            " with counters on them",
        ] {
            let (prop, _consumed) = parse_counter_suffix(phrase).expect("must parse");
            assert_eq!(
                prop,
                FilterProp::Counters {
                    counters: CounterMatch::Any,
                    comparator: Comparator::GE,
                    count: QuantityExpr::Fixed { value: 1 },
                }
            );
        }
    }

    /// CR 122.1 + CR 400.1: a zone clause followed by a counter-presence clause
    /// ("creature card in exile with a takeover counter on it" — The Master,
    /// Formed Anew). The whole source-filter phrase must be consumed (no
    /// leftover) and both the zone (`InZone { Exile }`) and the counter
    /// constraint (`Counters { OfType("takeover"), GE, 1 }`) must land on the
    /// filter. Exercises the second `parse_counter_suffix` pass that runs after
    /// the zone-suffix handling; the pre-zone pass only covers counter-then-zone.
    #[test]
    fn parse_type_phrase_zone_then_counter_suffix_consumes_both() {
        let (filter, leftover) =
            parse_type_phrase("creature card in exile with a takeover counter on it");
        assert_eq!(
            leftover.trim(),
            "",
            "whole source-filter phrase must be consumed, got leftover {leftover:?}"
        );
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(
            tf.properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Exile })),
            "zone clause must lower to InZone {{ Exile }}, got {:?}",
            tf.properties
        );
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::Counters {
                    counters: CounterMatch::OfType(CounterType::Generic(ct)),
                    comparator: Comparator::GE,
                    count: QuantityExpr::Fixed { value: 1 },
                } if ct == "takeover"
            )),
            "counter clause must lower to GE-1 takeover Counters prop, got {:?}",
            tf.properties
        );
    }

    /// CR 122.1: the pre-existing counter-then-zone ordering still parses — the
    /// new post-zone pass must not regress the symmetric (pre-zone) case.
    #[test]
    fn parse_type_phrase_counter_then_zone_suffix_still_consumes_both() {
        let (filter, leftover) =
            parse_type_phrase("creature card with a takeover counter on it in exile");
        assert_eq!(leftover.trim(), "", "got leftover {leftover:?}");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Exile })));
        assert!(tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Generic(ct)),
                ..
            } if ct == "takeover"
        )));
    }

    /// "that has a <type> counter on it" relative clause — must lower to the
    /// same `FilterProp::Counters` shape as the `with`-form (Banewhip Punisher,
    /// Triad of Fates). Previously this clause was dropped entirely.
    #[test]
    fn parse_that_clause_has_minus_counter() {
        let phrase = " that has a -1/-1 counter on it";
        let (props, consumed) =
            parse_that_clause_suffix(phrase, None).expect("relative counter clause must parse");
        assert_eq!(consumed, phrase.len());
        assert_eq!(
            props,
            vec![FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Minus1Minus1),
                comparator: Comparator::GE,
                count: QuantityExpr::Fixed { value: 1 },
            }]
        );
    }

    /// Plural relative-clause form "that have a +1/+1 counter on them" → the
    /// same positive (GE) typed counter filter (Plus1Plus1).
    #[test]
    fn parse_that_clause_have_plus_counter_plural() {
        let phrase = " that have a +1/+1 counter on them";
        let (props, consumed) =
            parse_that_clause_suffix(phrase, None).expect("plural relative counter clause");
        assert_eq!(consumed, phrase.len());
        assert_eq!(
            props,
            vec![FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                comparator: Comparator::GE,
                count: QuantityExpr::Fixed { value: 1 },
            }]
        );
    }

    /// "that has a fate counter on it" → Generic("fate") (Triad of Fates).
    #[test]
    fn parse_that_clause_has_fate_counter() {
        let phrase = " that has a fate counter on it";
        let (props, consumed) =
            parse_that_clause_suffix(phrase, None).expect("generic relative counter clause");
        assert_eq!(consumed, phrase.len());
        assert_eq!(
            props,
            vec![FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Generic("fate".to_string())),
                comparator: Comparator::GE,
                count: QuantityExpr::Fixed { value: 1 },
            }]
        );
    }

    #[test]
    fn parse_type_phrase_creature_with_stun_counter() {
        let (filter, _rest) = parse_type_phrase("creature with a stun counter on it");
        match filter {
            TargetFilter::Typed(ref tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert!(tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Counters {
                        counters: CounterMatch::OfType(ref counter_type),
                        comparator: Comparator::GE,
                        count: QuantityExpr::Fixed { value: 1 },
                    } if *counter_type == CounterType::Stun
                )));
            }
            other => panic!("Expected Typed, got {:?}", other),
        }
    }

    #[test]
    fn creatures_your_opponents_control() {
        let (f, rest) = parse_type_phrase("creatures your opponents control");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent))
        );
        assert_eq!(rest.trim(), "");
    }

    /// CR 109.4 + CR 115.1: "other creature target player controls" produces
    /// a filter scoped to a chosen player target. The companion
    /// `TargetFilter::Player` target slot is surfaced by `collect_target_slots`
    /// in the engine at target-declaration time; this parser test just verifies
    /// the filter's controller marker is `TargetPlayer` and the `other` modifier
    /// is preserved.
    #[test]
    fn other_creature_target_player_controls() {
        let (f, rest) = parse_type_phrase("other creature target player controls");
        match f {
            TargetFilter::Typed(ref tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::TargetPlayer));
                assert!(
                    tf.properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::Another)),
                    "expected `Another` property for `other` modifier, got {:?}",
                    tf.properties
                );
            }
            other => panic!("Expected Typed filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    /// Issue #588 (Summon: Good King Mog XII, chapter IV): "each other Moogle
    /// you control" must retain subtype + controller + Another. When "Moogle"
    /// was missing from SUBTYPES the filter collapsed to every other permanent.
    #[test]
    fn each_other_moogle_you_control_scopes_filter_issue_588() {
        let (filter, rest) = parse_target("each other Moogle you control");
        assert_eq!(rest, "");
        let tf = typed_leg(&filter).expect("expected Typed filter");
        assert!(
            tf.type_filters
                .iter()
                .any(|f| matches!(f, TypeFilter::Subtype(s) if s == "Moogle")),
            "Moogle subtype must be captured, got {:?}",
            tf.type_filters
        );
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf.properties.contains(&FilterProp::Another));
    }

    /// Sibling coverage: bare "creatures target player controls" without
    /// "each other" prefix. Confirms the controller parser is independent of
    /// modifier words.
    #[test]
    fn creatures_target_player_controls() {
        let (f, rest) = parse_type_phrase("creatures target player controls");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::TargetPlayer))
        );
        assert_eq!(rest.trim(), "");
    }

    /// Building-block regression guard: the general compound-core-type-Or
    /// splitter (the `TYPE_SEPARATORS` recursion plus
    /// `distribute_controller_to_or`) handles the "target player/opponent
    /// controls" controller-suffix family the same way it already handles
    /// "your opponents control" (see `artifacts_and_creatures_your_opponents_control`
    /// below). This is what makes compound-subject "don't/doesn't untap"
    /// restrictions like Exhaustion ("Creatures and lands target opponent
    /// controls don't untap during their next untap step.") and Icebreaker
    /// Kraken resolve correctly through the single generic
    /// `parse_subject_application` call in `try_parse_subject_restriction_clause`
    /// — no dedicated compound-subject dispatcher needed for this predicate
    /// class (confirmed via the PR parse-diff baseline: both cards are
    /// already `supported: true` on main).
    #[test]
    fn compound_creatures_and_lands_target_opponent_controls() {
        let (f, rest) = parse_type_phrase("creatures and lands target opponent controls");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2, "expected 2 disjuncts, got {filters:?}");
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::TargetOpponent)
                    )
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::land().controller(ControllerRef::TargetOpponent)
                    )
                );
            }
            other => panic!("expected Or filter, got {other:?}"),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn creature_card_or_a_planeswalker_card_keeps_both_disjuncts() {
        // #5331 (Overlord of the Balemurk): "non-Avatar creature card or a
        // planeswalker card" — the second disjunct leads with an article ("or *a*
        // planeswalker card"), which the bare "or"/"and" separator previously
        // rejected, silently dropping the planeswalker leg so the card could only
        // return creatures. Both legs must survive.
        let (f, _rest) = parse_type_phrase("non-Avatar creature card or a planeswalker card");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2, "both disjuncts kept: {filters:?}");
                let has = |t: TypeFilter| {
                    filters.iter().any(|leg| {
                        matches!(leg, TargetFilter::Typed(tf) if tf.type_filters.contains(&t))
                    })
                };
                assert!(
                    has(TypeFilter::Creature),
                    "creature leg present: {filters:?}"
                );
                assert!(
                    has(TypeFilter::Planeswalker),
                    "planeswalker leg present: {filters:?}"
                );
            }
            other => panic!("expected Or with both disjuncts, got {other:?}"),
        }
    }

    #[test]
    fn article_led_card_disjunct_does_not_inherit_left_leg_color() {
        // Purphoros, Bronze-Blooded: "a red creature card or an artifact creature
        // card". CR 105.1 + CR 205.2: the leading "red" binds only to the creature
        // leg; the article-led "an artifact creature card" is an independent noun
        // phrase and must NOT inherit `HasColor(Red)` — otherwise a colorless
        // artifact creature (e.g. Ornithopter) would be wrongly rejected.
        let (f, _rest) = parse_type_phrase("red creature card or an artifact creature card");
        let TargetFilter::Or { filters } = f else {
            panic!("expected Or, got {f:?}");
        };
        assert_eq!(filters.len(), 2, "both disjuncts kept: {filters:?}");
        let has_red = |tf: &TypedFilter| {
            tf.properties.iter().any(|p| {
                matches!(
                    p,
                    FilterProp::HasColor {
                        color: ManaColor::Red
                    }
                )
            })
        };
        for leg in &filters {
            let TargetFilter::Typed(tf) = leg else {
                panic!("expected Typed legs, got {leg:?}");
            };
            if tf.type_filters.contains(&TypeFilter::Artifact) {
                assert!(
                    !has_red(tf),
                    "artifact-creature leg must NOT require red: {:?}",
                    tf.properties
                );
            } else {
                assert!(
                    has_red(tf),
                    "creature leg must keep its red requirement: {:?}",
                    tf.properties
                );
            }
        }
    }

    #[test]
    fn artifacts_and_creatures_your_opponents_control() {
        let (f, rest) = parse_type_phrase("artifacts and creatures your opponents control");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::Opponent)
                    )
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent)
                    )
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn creature_an_opponent_controls_still_works() {
        let (f, rest) = parse_type_phrase("creature an opponent controls");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent))
        );
        assert_eq!(rest.trim(), "");
    }

    // CR 205.3a: Comma-separated type list tests

    #[test]
    fn comma_list_three_types_with_opponent_control() {
        let (f, rest) = parse_type_phrase("artifacts, creatures, and lands your opponents control");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::Opponent)
                    )
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent)
                    )
                );
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Land).controller(ControllerRef::Opponent)
                    )
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn comma_list_three_types_no_controller() {
        let (f, rest) = parse_type_phrase("artifacts, creatures, and enchantments");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))
                );
                assert_eq!(filters[1], TargetFilter::Typed(TypedFilter::creature()));
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment))
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn comma_list_you_control() {
        let (f, rest) = parse_type_phrase("creatures, artifacts, and enchantments you control");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You)
                    )
                );
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Enchantment).controller(ControllerRef::You)
                    )
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn modified_adjective_creates_filter_prop() {
        // CR 700.4 + CR 700.9: "modified creature" is a first-class adjective
        // attaching FilterProp::Modified to a typed creature filter.
        let (f, rest) = parse_type_phrase("modified creature you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Modified])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn renowned_adjective_creates_filter_prop() {
        // CR 702.112b: "renowned creature" is a designation adjective.
        let (f, rest) = parse_type_phrase("renowned creature you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Renowned])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn modified_adjective_in_comma_list_silkguard() {
        // CR 700.4 + CR 700.9: Silkguard — "Auras, Equipment, and modified
        // creatures you control gain hexproof". The subject is a three-way OR
        // of Aura (subtype), Equipment (subtype), and creature-with-Modified.
        // The trailing "you control" controller scope distributes across all
        // three legs via `distribute_controller_to_or`.
        let (f, rest) = parse_type_phrase("auras, equipment, and modified creatures you control");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3, "expected 3-way OR, got {filters:#?}");
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(
                        TypedFilter::default()
                            .subtype("Aura".to_string())
                            .controller(ControllerRef::You)
                    ),
                    "leg 0 = Auras you control"
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::default()
                            .subtype("Equipment".to_string())
                            .controller(ControllerRef::You)
                    ),
                    "leg 1 = Equipment you control"
                );
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::Modified])
                    ),
                    "leg 2 = modified creatures you control"
                );
            }
            other => panic!("Expected Or filter, got {other:?}"),
        }
        assert_eq!(rest.trim(), "");
    }

    // CR 105.2 (color characteristic) + CR 109.2 (type-word object description):
    // when the core type noun ("creature") appears only after a later color
    // disjunct, the earlier color-only leg is assembled with `[TypeFilter::Any]`
    // before "creature" is parsed. `distribute_core_type_to_or` backfills the
    // concrete core type so EVERY leg carries the type restriction (type_filters
    // are ANDed in game/filter.rs). Without it, a green noncreature would be a
    // legal "green or white creature" target. These drive the real parse pipeline
    // and assert each flat Or leg independently.

    /// Extract the `HasColor` color from a Typed leg's properties, if present.
    fn leg_color(filter: &TargetFilter) -> Option<ManaColor> {
        typed_leg(filter).and_then(|tf| {
            tf.properties.iter().find_map(|p| match p {
                FilterProp::HasColor { color } => Some(*color),
                _ => None,
            })
        })
    }

    #[test]
    fn or_color_disjunction_backfills_core_type_deathmark() {
        // Deathmark: "Destroy target green or white creature".
        let (f, rest) = parse_target("target green or white creature");
        assert_eq!(rest.trim(), "");
        let TargetFilter::Or { filters } = f else {
            panic!("expected Or filter, got {f:?}");
        };
        assert_eq!(filters.len(), 2, "expected 2-way OR, got {filters:#?}");
        // Both legs must carry exactly [Creature] (the green leg was [Any]).
        for (i, leg) in filters.iter().enumerate() {
            let tf = typed_leg(leg).unwrap_or_else(|| panic!("leg {i} not Typed: {leg:?}"));
            assert_eq!(
                tf.type_filters,
                vec![TypeFilter::Creature],
                "leg {i} must be [Creature], got {:?}",
                tf.type_filters
            );
        }
        assert_eq!(
            leg_color(&filters[0]),
            Some(ManaColor::Green),
            "leg 0 = green"
        );
        assert_eq!(
            leg_color(&filters[1]),
            Some(ManaColor::White),
            "leg 1 = white"
        );
    }

    #[test]
    fn or_color_disjunction_backfills_core_type_tidebinder() {
        // Tidebinder Mage: "tap target red or green creature an opponent controls".
        let (f, rest) = parse_target("target red or green creature an opponent controls");
        assert_eq!(rest.trim(), "");
        let TargetFilter::Or { filters } = f else {
            panic!("expected Or filter, got {f:?}");
        };
        assert_eq!(filters.len(), 2, "expected 2-way OR, got {filters:#?}");
        for (i, leg) in filters.iter().enumerate() {
            let tf = typed_leg(leg).unwrap_or_else(|| panic!("leg {i} not Typed: {leg:?}"));
            assert_eq!(
                tf.type_filters,
                vec![TypeFilter::Creature],
                "leg {i} must be [Creature], got {:?}",
                tf.type_filters
            );
            assert_eq!(
                tf.controller,
                Some(ControllerRef::Opponent),
                "leg {i} must inherit opponent controller scope"
            );
        }
        assert_eq!(leg_color(&filters[0]), Some(ManaColor::Red), "leg 0 = red");
        assert_eq!(
            leg_color(&filters[1]),
            Some(ManaColor::Green),
            "leg 1 = green"
        );
    }

    #[test]
    fn or_color_disjunction_backfills_core_type_self_inflicted_wound() {
        // Self-Inflicted Wound: "a green or white creature of their choice".
        // The filter-phrase level (parse_type_phrase) is what the parser produces;
        // load-bearing assertion is that BOTH legs carry [Creature].
        let (f, _rest) = parse_type_phrase("green or white creature");
        let TargetFilter::Or { filters } = f else {
            panic!("expected Or filter, got {f:?}");
        };
        assert_eq!(filters.len(), 2, "expected 2-way OR, got {filters:#?}");
        for (i, leg) in filters.iter().enumerate() {
            let tf = typed_leg(leg).unwrap_or_else(|| panic!("leg {i} not Typed: {leg:?}"));
            assert_eq!(
                tf.type_filters,
                vec![TypeFilter::Creature],
                "leg {i} must be [Creature], got {:?}",
                tf.type_filters
            );
        }
        assert_eq!(
            leg_color(&filters[0]),
            Some(ManaColor::Green),
            "leg 0 = green"
        );
        assert_eq!(
            leg_color(&filters[1]),
            Some(ManaColor::White),
            "leg 1 = white"
        );
    }

    #[test]
    fn or_color_disjunction_three_colors_backfills_core_type() {
        // ≥3-color prenominal disjunction class: "target white, blue, or black
        // creature". Unlike the 2-color "green or white creature" form (which the
        // bare " or " `TYPE_SEPARATORS` arm assembles), the inner legs here are
        // comma-separated bare color words ("blue,"). `parse_color_prefix` now
        // accepts a color followed by a comma, so the leading color is consumed
        // and the ", " / ", or " separators drive the same recursion; the
        // [Any]-typed color-only legs are then backfilled to [Creature] by
        // `distribute_core_type_to_or`. This pins the full parse pipeline (the
        // surface assembly the distributor-only test below cannot reach).
        let (f, rest) = parse_target("target white, blue, or black creature");
        assert_eq!(rest.trim(), "");
        let TargetFilter::Or { filters } = f else {
            panic!("expected Or filter, got {f:?}");
        };
        assert_eq!(filters.len(), 3, "expected 3-way OR, got {filters:#?}");
        // Every leg must carry exactly [Creature] (the white/blue legs were [Any]).
        for (i, leg) in filters.iter().enumerate() {
            let tf = typed_leg(leg).unwrap_or_else(|| panic!("leg {i} not Typed: {leg:?}"));
            assert_eq!(
                tf.type_filters,
                vec![TypeFilter::Creature],
                "leg {i} must be [Creature], got {:?}",
                tf.type_filters
            );
        }
        assert_eq!(
            leg_color(&filters[0]),
            Some(ManaColor::White),
            "leg 0 = white"
        );
        assert_eq!(
            leg_color(&filters[1]),
            Some(ManaColor::Blue),
            "leg 1 = blue"
        );
        assert_eq!(
            leg_color(&filters[2]),
            Some(ManaColor::Black),
            "leg 2 = black"
        );
    }

    #[test]
    fn distribute_core_type_to_or_backfills_every_flat_any_leg() {
        // Building-block test: `merge_or_filters` flattens nested `Or`s, so a
        // ≥3-disjunct list arrives at `distribute_core_type_to_or` as flat
        // siblings. Drive the distributor directly with a flat 3-leg Or in which
        // two legs are the deferred-type `[Any]` shape (color-only) and the last
        // carries the concrete `[Creature]`. EVERY `[Any]` leg must inherit
        // `[Creature]`; the type-bearing leg is untouched. The surface parser now
        // assembles ≥3-color prenominal chains (see
        // `or_color_disjunction_three_colors_backfills_core_type`); this test pins
        // the distributor at its own seam — exactly the level `merge_or_filters`
        // feeds — independent of the surface grammar.
        let any_leg = |color: ManaColor| {
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Any],
                controller: None,
                properties: vec![FilterProp::HasColor { color }],
            })
        };
        let creature_leg = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![FilterProp::HasColor {
                color: ManaColor::Black,
            }],
        });
        let input = TargetFilter::Or {
            filters: vec![
                any_leg(ManaColor::White),
                any_leg(ManaColor::Blue),
                creature_leg,
            ],
        };
        let TargetFilter::Or { filters } = distribute_core_type_to_or(input) else {
            panic!("distributor must preserve the Or shape");
        };
        assert_eq!(filters.len(), 3);
        for (i, leg) in filters.iter().enumerate() {
            let tf = typed_leg(leg).unwrap_or_else(|| panic!("leg {i} not Typed: {leg:?}"));
            assert_eq!(
                tf.type_filters,
                vec![TypeFilter::Creature],
                "leg {i} must inherit [Creature], got {:?}",
                tf.type_filters
            );
        }
        assert_eq!(
            leg_color(&filters[0]),
            Some(ManaColor::White),
            "leg 0 = white"
        );
        assert_eq!(
            leg_color(&filters[1]),
            Some(ManaColor::Blue),
            "leg 1 = blue"
        );
        assert_eq!(
            leg_color(&filters[2]),
            Some(ManaColor::Black),
            "leg 2 = black"
        );
    }

    #[test]
    fn distribute_core_type_to_or_skips_disagreeing_type_legs() {
        // Regression (Wort, the Raidmother / conspire): a COMPOUND disjunction
        // "red or green instant or sorcery spell" yields an `[Any]`+red leg
        // alongside DISAGREEING type legs (`[Instant]` and `[Sorcery]`). There is
        // no single core type to project, so the `[Any]` leg must be LEFT
        // UNCHANGED — over-narrowing it to one branch ("[Sorcery]") would wrongly
        // stop a red *instant* from matching, so Wort would no longer grant it
        // conspire. Backfilling here is unsafe; the distributor must no-op.
        let any_red = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Any],
            controller: None,
            properties: vec![FilterProp::HasColor {
                color: ManaColor::Red,
            }],
        });
        let instant_leg = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Instant],
            controller: None,
            properties: vec![],
        });
        let sorcery_leg = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Sorcery],
            controller: None,
            properties: vec![],
        });
        let input = TargetFilter::Or {
            filters: vec![any_red, instant_leg, sorcery_leg],
        };
        let TargetFilter::Or { filters } = distribute_core_type_to_or(input) else {
            panic!("distributor must preserve the Or shape");
        };
        // The `[Any]`+red leg is unchanged (NOT narrowed to [Instant] or [Sorcery]).
        let red = typed_leg(&filters[0]).expect("leg 0 Typed");
        assert_eq!(
            red.type_filters,
            vec![TypeFilter::Any],
            "the bare color leg must stay [Any] when type legs disagree, got {:?}",
            red.type_filters
        );
        assert_eq!(
            typed_leg(&filters[1]).unwrap().type_filters,
            vec![TypeFilter::Instant]
        );
        assert_eq!(
            typed_leg(&filters[2]).unwrap().type_filters,
            vec![TypeFilter::Sorcery]
        );
    }

    #[test]
    fn or_disjunction_distinct_explicit_types_untouched() {
        // No-regression: "artifact or creature" — neither leg is [Any], so the
        // backfill must NOT collapse the distinct types into one.
        let (f, rest) = parse_type_phrase("artifact or creature");
        assert_eq!(rest.trim(), "");
        let TargetFilter::Or { filters } = f else {
            panic!("expected Or filter, got {f:?}");
        };
        assert_eq!(filters.len(), 2, "expected 2-way OR, got {filters:#?}");
        assert_eq!(
            typed_leg(&filters[0]).unwrap().type_filters,
            vec![TypeFilter::Artifact],
            "leg 0 stays [Artifact]"
        );
        assert_eq!(
            typed_leg(&filters[1]).unwrap().type_filters,
            vec![TypeFilter::Creature],
            "leg 1 stays [Creature]"
        );
    }

    #[test]
    fn or_disjunction_artifact_or_enchantment_untouched() {
        // No-regression: both legs explicit, neither [Any] — untouched.
        let (f, rest) = parse_type_phrase("artifact or enchantment");
        assert_eq!(rest.trim(), "");
        let TargetFilter::Or { filters } = f else {
            panic!("expected Or filter, got {f:?}");
        };
        assert_eq!(filters.len(), 2);
        assert_eq!(
            typed_leg(&filters[0]).unwrap().type_filters,
            vec![TypeFilter::Artifact]
        );
        assert_eq!(
            typed_leg(&filters[1]).unwrap().type_filters,
            vec![TypeFilter::Enchantment]
        );
    }

    #[test]
    fn single_green_creature_not_or_early_returns() {
        // No-regression: a non-Or phrase early-returns from the distributor.
        let (f, rest) = parse_type_phrase("green creature");
        assert_eq!(rest.trim(), "");
        match f {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                assert!(
                    has_prop(
                        &tf,
                        FilterProp::HasColor {
                            color: ManaColor::Green
                        }
                    ),
                    "expected green color prop, got {tf:?}"
                );
            }
            other => panic!("expected single Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn or_spell_or_permanent_leaves_non_any_legs_alone() {
        // Reviewer's extra guard: "target spell or permanent that's red or green"
        // parses to an Or with a StackSpell-bearing leg + a [Permanent] leg.
        // Neither leg is exactly [Any], so the backfill must leave the StackSpell
        // leg and the [Permanent] leg untouched (no source → no-op anyway).
        let (f, rest) = parse_target("target spell or permanent that's red or green");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = f else {
            panic!("expected Or filter, got {f:?}");
        };
        assert_eq!(filters.len(), 2);
        // The spell leg must remain a StackSpell (not rewritten into a Typed type).
        assert!(
            filters.iter().any(is_stack_spell_leg),
            "spell leg must remain StackSpell: {filters:#?}"
        );
        // The permanent leg keeps [Permanent] — not collapsed to [Any] or rewritten.
        assert!(
            filters
                .iter()
                .filter_map(typed_leg)
                .any(|tf| tf.type_filters == vec![TypeFilter::Permanent]),
            "permanent leg must keep [Permanent]: {filters:#?}"
        );
    }

    // CR 508.5 / CR 508.5a: the "defending player controls" controller suffix
    // scopes attack-trigger targets to the defending player (Kogla, The
    // Tarrasque, ~42 cards). These tests pin the class-level combinator
    // behavior across the bug-card path: the high-level controller-suffix
    // delegate, the end-to-end target verb path, and Or-target propagation.

    // High-level `parse_controller_suffix` (the runtime function the bug-card
    // path relies on). The direct assertion guarantees the `parse_zone_controller`
    // delegate is actually reached and not shadowed by an earlier past-tense or
    // "that player controls" arm.
    #[test]
    fn parse_controller_suffix_defending_player() {
        let ctx = ParseContext::default();
        let (ctrl, len) = parse_controller_suffix("defending player controls", &ctx)
            .expect("defending player controls should resolve a controller scope");
        assert_eq!(ctrl, ControllerRef::DefendingPlayer);
        assert_eq!(len, "defending player controls".len());

        // Leading whitespace is included in the consumed length (the type-phrase
        // suffix step passes the post-type-word remainder, which begins with a
        // space).
        let (ctrl_ws, len_ws) = parse_controller_suffix(" defending player controls", &ctx)
            .expect("leading-space variant should resolve");
        assert_eq!(ctrl_ws, ControllerRef::DefendingPlayer);
        assert_eq!(len_ws, " defending player controls".len());
    }

    // CR 508.1 + CR 608.2c: the "its controller controls" anaphoric suffix binds
    // to the controller of "it". Mondassian Colony Ship class: "for each other
    // creature its controller controls that shares a creature type with it". In a
    // trigger-subject context (subject = the attacking creature) the anaphor is
    // the triggering source, so the controller is the triggering player; with no
    // subject (or a self/any subject) the anaphor is a chosen parent target, so
    // the controller is that target's controller.
    #[test]
    fn parse_controller_suffix_its_controller_controls_anaphor() {
        // Trigger-subject context → TriggeringPlayer (the attacking player).
        let trigger_ctx = ParseContext {
            subject: Some(TargetFilter::Typed(TypedFilter::creature())),
            ..Default::default()
        };
        let (ctrl, len) = parse_controller_suffix("its controller controls", &trigger_ctx)
            .expect("its controller controls should resolve a controller scope");
        assert_eq!(ctrl, ControllerRef::TriggeringPlayer);
        assert_eq!(len, "its controller controls".len());

        // "their controller controls" is the same anaphor (plural pronoun).
        let (ctrl_their, _) =
            parse_controller_suffix("their controller controls", &trigger_ctx).unwrap();
        assert_eq!(ctrl_their, ControllerRef::TriggeringPlayer);

        // No-subject context → ParentTargetController (compound-effect anaphor),
        // mirroring `resolve_pronoun_target`'s `None`/`SelfRef`/`Any` arm.
        let default_ctx = ParseContext::default();
        let (ctrl_parent, len_parent) =
            parse_controller_suffix(" its controller controls", &default_ctx)
                .expect("no-subject variant should resolve");
        assert_eq!(ctrl_parent, ControllerRef::ParentTargetController);
        assert_eq!(len_parent, " its controller controls".len());

        // SelfRef subject is a self-ETB context — no non-source triggering
        // object — so it also binds to the parent target, not the source.
        let selfref_ctx = ParseContext {
            subject: Some(TargetFilter::SelfRef),
            ..Default::default()
        };
        let (ctrl_self, _) =
            parse_controller_suffix("its controller controls", &selfref_ctx).unwrap();
        assert_eq!(ctrl_self, ControllerRef::ParentTargetController);
    }

    // End-to-end target verb path: a representative effect phrase parses to a
    // Typed filter scoped to the defending player. Generic type phrase, not a
    // card name (The Tarrasque class: "fights target creature defending player
    // controls").
    #[test]
    fn parse_target_defending_player_controls_single_type() {
        let (f, rest) = parse_target("target creature defending player controls");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::DefendingPlayer))
        );
        assert_eq!(rest.trim(), "");
    }

    // Or-target propagation: an Or-target phrase ending in "defending player
    // controls" fans the DefendingPlayer scope onto each disjunct via
    // `distribute_controller_to_or` (Kogla class: "destroy target artifact or
    // enchantment defending player controls").
    #[test]
    fn parse_target_defending_player_controls_or_target() {
        let (f, rest) = parse_target("target artifact or enchantment defending player controls");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2, "expected 2-way OR, got {filters:#?}");
                for (i, leg) in filters.iter().enumerate() {
                    match leg {
                        TargetFilter::Typed(tf) => assert_eq!(
                            tf.controller,
                            Some(ControllerRef::DefendingPlayer),
                            "leg {i} must inherit the defending-player scope"
                        ),
                        other => panic!("leg {i} expected Typed, got {other:?}"),
                    }
                }
            }
            other => panic!("Expected Or filter, got {other:?}"),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn historic_adjective_creates_filter_prop() {
        // CR 700.6: "historic permanent" is a first-class adjective attaching
        // FilterProp::Historic to a typed permanent filter.
        let (f, rest) = parse_type_phrase("historic permanent you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Historic])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn historic_adjective_after_nontoken_arbaaz() {
        // CR 700.6: Arbaaz Mir's "another nontoken historic permanent you
        // control" composes token identity (`NonToken`), the Historic
        // adjective, the Another property, and the You controller — all in
        // sequence. The historic adjective parses AFTER the `non` negation
        // sweep, exercising the post-negation arm.
        let (f, rest) = parse_type_phrase("another nontoken historic permanent you control");
        match f {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(
                    tf.type_filters.contains(&TypeFilter::Permanent),
                    "expected Permanent in {:?}",
                    tf.type_filters,
                );
                assert!(
                    tf.properties.contains(&FilterProp::NonToken),
                    "expected NonToken in {:?}",
                    tf.properties,
                );
                assert!(
                    tf.properties.contains(&FilterProp::Historic),
                    "expected Historic in {:?}",
                    tf.properties,
                );
                assert!(
                    tf.properties.contains(&FilterProp::Another),
                    "expected Another in {:?}",
                    tf.properties,
                );
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn historic_adjective_does_not_propagate_to_or_legs() {
        // CR 700.6 + CR 700.4: `FilterProp::Historic` is leg-local — in a
        // comma OR list it must NOT distribute back to earlier legs. Mirrors
        // the Modified adjective handling for Silkguard.
        let (f, _rest) = parse_type_phrase("artifacts and historic creatures you control");
        let TargetFilter::Or { ref filters } = f else {
            panic!("Expected Or filter, got {f:?}");
        };
        let leg_has_historic = |idx: usize| -> bool {
            matches!(
                filters.get(idx),
                Some(TargetFilter::Typed(tf)) if tf.properties.contains(&FilterProp::Historic)
            )
        };
        assert!(
            !leg_has_historic(0),
            "Historic must not propagate to artifact leg in {filters:#?}",
        );
        assert!(
            leg_has_historic(filters.len() - 1),
            "creature leg must keep Historic in {filters:#?}",
        );
    }

    #[test]
    fn comma_list_four_elements() {
        let (f, rest) = parse_type_phrase("artifacts, creatures, enchantments, and lands");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 4);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))
                );
                assert_eq!(filters[1], TargetFilter::Typed(TypedFilter::creature()));
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment))
                );
                assert_eq!(
                    filters[3],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Land))
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn comma_list_per_item_articles() {
        let (f, rest) = parse_type_phrase("an artifact, a creature, or a land");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))
                );
                assert_eq!(filters[1], TargetFilter::Typed(TypedFilter::creature()));
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Land))
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn comma_list_no_oxford_comma() {
        let (f, rest) = parse_type_phrase("artifacts, creatures and lands your opponents control");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::Opponent)
                    )
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent)
                    )
                );
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Land).controller(ControllerRef::Opponent)
                    )
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn comma_list_remainder() {
        let (f, rest) = parse_type_phrase("artifacts, creatures, and lands enter tapped");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3);
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest, " enter tapped");
    }

    // ── Feature 1: Stacked negation ──

    #[test]
    fn noncreature_nonland_permanent() {
        let (f, rest) = parse_type_phrase("noncreature, nonland permanent");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent()
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Creature)))
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn noncreature_nonland_permanents_you_control() {
        let (f, rest) = parse_type_phrase("noncreature, nonland permanents you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent()
                    .controller(ControllerRef::You)
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Creature)))
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn nonartifact_nonblack_creature() {
        // CR 205.4b: "nonartifact" → Non(Artifact) in type_filters, "nonblack" → NotColor in properties
        let (f, rest) = parse_type_phrase("nonartifact, nonblack creature");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Artifact)))
                    .properties(vec![FilterProp::NotColor {
                        color: ManaColor::Black,
                    },])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn triple_stacked_negation() {
        let (f, _) = parse_type_phrase("noncreature, nonland, nonartifact permanent");
        match f {
            TargetFilter::Typed(ref tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Permanent));
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Creature))));
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))));
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Artifact))));
            }
            other => panic!("Expected Typed, got {:?}", other),
        }
    }

    // ── Cluster 59: convoke-relative filter + "except those" exclusion + mass union ──

    #[test]
    fn creature_that_convoked_this_spell_is_convoked_source() {
        // CR 702.51c: "a creature that convoked this spell" → creature +
        // ConvokedSource. The "this spell" self-reference must NOT scope the
        // result to the stack (the spell-suffix guard).
        let (f, rest) = parse_target("a creature that convoked this spell");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::ConvokedSource])
            ),
            "must stay a battlefield creature filter, not a stack spell"
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn convoked_it_alias_is_convoked_source() {
        let (f, _) = parse_target("a creature that convoked it");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::ConvokedSource])
            )
        );
    }

    #[test]
    fn except_those_sharing_type_with_convoker_negates() {
        // CR 608.2c: "creatures except those that share a creature type with a
        // creature that convoked this spell" → creature + Not(SharesQuality).
        let (f, _) = parse_type_phrase(
            "creatures except those that share a creature type with a creature that convoked this spell",
        );
        let expected_ref = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::ConvokedSource]),
        );
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Not {
                prop: Box::new(FilterProp::SharesQuality {
                    quality: SharedQuality::CreatureType,
                    reference: Some(Box::new(expected_ref)),
                    relation: SharedQualityRelation::Shares,
                }),
            }]))
        );
    }

    #[test]
    fn except_those_multi_predicate_folds_to_disjunction_of_negations() {
        // CR 608.2c De Morgan: "except those that <X> and <Y>" excludes only
        // objects matching the FULL conjunction X AND Y, so the complement kept by
        // the leg is the disjunction Not(X) OR Not(Y) — a single `AnyOf`, NEVER
        // per-prop `Not(X) AND Not(Y)` (which would exclude objects matching X *or*
        // Y, far too many). Exercised with a clause that `parse_that_clause_suffix`
        // returns as two props ([Not(AttackedThisTurn), Not(EnteredThisTurn)]).
        let (f, _) =
            parse_type_phrase("creatures except those that didn't attack or enter this turn");
        // The two negated-verb predicates negate (double `Not`) and fold into one
        // `AnyOf` — the structural signature that distinguishes the De Morgan-correct
        // disjunction from the broken per-prop conjunction.
        let expected_props = vec![FilterProp::AnyOf {
            props: vec![
                FilterProp::Not {
                    prop: Box::new(FilterProp::Not {
                        prop: Box::new(FilterProp::AttackedThisTurn { defender: None }),
                    }),
                },
                FilterProp::Not {
                    prop: Box::new(FilterProp::Not {
                        prop: Box::new(FilterProp::EnteredThisTurn),
                    }),
                },
            ],
        }];
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(expected_props)),
            "multi-predicate exclusion must fold to AnyOf of negations, not per-prop Not"
        );
    }

    #[test]
    fn mass_type_union_repeated_all() {
        // CR 205.2a: "creatures, all artifacts, and all enchantments" →
        // Or[creature, artifact, enchantment] (repeated-`all` continuation over
        // card types).
        let mut ctx = ParseContext::default();
        let (f, rest) =
            parse_mass_type_union("creatures, all artifacts, and all enchantments", &mut ctx);
        assert_eq!(
            f,
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature()),
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
                ],
            }
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn mass_type_union_single_leg_matches_parse_target() {
        // Regression: inputs without a repeated-`all` continuation must equal the
        // bare `parse_target` result (the loop must not fire on within-leg unions).
        let mut ctx = ParseContext::default();
        for phrase in ["artifacts", "artifacts and creatures", "other spells"] {
            let (f, _) = parse_mass_type_union(phrase, &mut ctx);
            let (baseline, _) = parse_target(phrase);
            assert_eq!(f, baseline, "mass union changed bare parse for {phrase:?}");
        }
    }

    // ── Feature 1: starts_with_type_word guard ──

    #[test]
    fn starts_with_type_word_core_types() {
        assert!(starts_with_type_word("creatures"));
        assert!(starts_with_type_word("artifact"));
        assert!(starts_with_type_word("permanents you control"));
    }

    #[test]
    fn starts_with_type_word_negated() {
        assert!(starts_with_type_word("noncreature spell"));
        assert!(starts_with_type_word("nonland permanent"));
    }

    #[test]
    fn starts_with_type_word_subtypes() {
        assert!(starts_with_type_word("zombie"));
        assert!(starts_with_type_word("vampires"));
        assert!(starts_with_type_word("elves"));
    }

    #[test]
    fn starts_with_type_word_rejects_non_types() {
        assert!(!starts_with_type_word("draw a card"));
        assert!(!starts_with_type_word("destroy target"));
        assert!(!starts_with_type_word("you control"));
    }

    // ── Feature 2: Subtype recognition ──

    #[test]
    fn zombies_you_control() {
        let (f, rest) = parse_type_phrase("zombies you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Zombie".to_string())
                    .controller(ControllerRef::You)
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn elves_you_control_irregular_plural() {
        let (f, rest) = parse_type_phrase("elves you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Elf".to_string())
                    .controller(ControllerRef::You)
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn equipment_subtype() {
        let (f, _) = parse_type_phrase("equipment you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Equipment".to_string())
                    .controller(ControllerRef::You)
            )
        );
    }

    #[test]
    fn spacecraft_artifact_subtype() {
        let (f, _) = parse_type_phrase("Spacecraft");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::default().subtype("Spacecraft".to_string()))
        );
    }

    #[test]
    fn creatures_and_spacecraft_type_union() {
        let (f, rest) = parse_type_phrase("creatures and Spacecraft");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2);
                assert_eq!(filters[0], TargetFilter::Typed(TypedFilter::creature()));
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(TypedFilter::default().subtype("Spacecraft".to_string()))
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn forest_land_subtype() {
        let (f, _) = parse_type_phrase("forest");
        match f {
            TargetFilter::Typed(ref tf) => {
                assert_eq!(tf.get_subtype(), Some("Forest"));
            }
            other => panic!("Expected Typed, got {:?}", other),
        }
    }

    // ── Feature 3: Supertype prefixes ──

    #[test]
    fn legendary_creature() {
        let (f, _) = parse_type_phrase("legendary creature");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::HasSupertype {
                    value: Supertype::Legendary,
                }
            ]))
        );
    }

    #[test]
    fn basic_lands_you_control() {
        let (f, _) = parse_type_phrase("basic lands you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::land()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::HasSupertype {
                        value: Supertype::Basic,
                    }])
            )
        );
    }

    #[test]
    fn parse_target_article_basic_land_you_control() {
        let (filter, rest) = parse_target("a basic land you control");
        assert_eq!(
            filter,
            TargetFilter::Typed(
                TypedFilter::land()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::HasSupertype {
                        value: Supertype::Basic,
                    }])
            )
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_article_basic_land_card_from_hand() {
        let (filter, rest) = parse_target("a basic land card from your hand");
        assert_eq!(
            filter,
            TargetFilter::Typed(
                TypedFilter::land()
                    .controller(ControllerRef::You)
                    .properties(vec![
                        FilterProp::HasSupertype {
                            value: Supertype::Basic,
                        },
                        FilterProp::InZone { zone: Zone::Hand },
                    ])
            )
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn snow_permanents() {
        let (f, _) = parse_type_phrase("snow permanents");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                FilterProp::HasSupertype {
                    value: Supertype::Snow,
                }
            ]))
        );
    }

    #[test]
    fn legendary_white_creature() {
        // CR 205.4a: Supertype + color compose in properties
        let (f, _) = parse_type_phrase("legendary white creature");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::HasSupertype {
                    value: Supertype::Legendary
                },
                FilterProp::HasColor {
                    color: ManaColor::White
                },
            ]))
        );
    }

    #[test]
    fn nonbasic_land() {
        // CR 205.4a: "nonbasic" → NotSupertype (property), not TypeFilter::Non
        let (f, _) = parse_type_phrase("nonbasic land");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::land().properties(vec![FilterProp::NotSupertype {
                    value: Supertype::Basic,
                }])
            )
        );
    }

    #[test]
    fn nonbasic_lands_opponent_controls() {
        let (f, _) = parse_type_phrase("nonbasic lands an opponent controls");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::land()
                    .controller(ControllerRef::Opponent)
                    .properties(vec![FilterProp::NotSupertype {
                        value: Supertype::Basic,
                    }])
            )
        );
    }

    // ── Feature 4: "and/or" separator ──

    /// CR 608.2b: "creature and/or land" composes via existing "and/or"
    /// support to `TargetFilter::Or { [Creature, Land] }`. Regression guard
    /// for Zimone's Experiment: the compound type filter on Dig's reveal
    /// gate must produce `Or` (not drop to `Any`) so the Dig's `filter`
    /// correctly restricts the player's selectable set during DigChoice.
    #[test]
    fn creature_and_or_land_composes_to_or_filter() {
        let (f, _) = parse_type_phrase("creature and/or land");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature))
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Land))
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
    }

    #[test]
    fn artifact_and_or_enchantment() {
        let (f, _) = parse_type_phrase("artifact and/or enchantment");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment))
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
    }

    #[test]
    fn instant_and_or_sorcery() {
        let (f, _) = parse_type_phrase("instant and/or sorcery");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2);
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
    }

    #[test]
    fn creature_and_or_planeswalker_you_control() {
        let (f, _) = parse_type_phrase("creature and/or planeswalker you control");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2);
                // Both branches should have controller distributed
                for filter in filters {
                    if let TargetFilter::Typed(typed) = filter {
                        assert_eq!(typed.controller, Some(ControllerRef::You));
                    } else {
                        panic!("Expected Typed in Or, got {:?}", filter);
                    }
                }
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
    }

    // ── Regression: existing tests still pass with new features ──

    #[test]
    fn existing_nonland_still_works() {
        // Single non-prefix (not stacked) should work as before
        let (f, _) = parse_type_phrase("nonland permanent");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent().with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
            )
        );
    }

    #[test]
    fn and_still_works_with_non_type_text() {
        // "creature and draw a card" — "and" should NOT recurse because "draw" isn't a type
        let (f, rest) = parse_type_phrase("creature and draw a card");
        assert_eq!(f, TargetFilter::Typed(TypedFilter::creature()));
        assert!(rest.contains("and draw"), "rest = {:?}", rest);
    }

    #[test]
    fn comma_or_keyword_suffix_stays_on_final_disjunct_only() {
        // Issue #2941 (Vivien Reid): "artifact, enchantment, or creature with
        // flying" — flying applies only to the creature leg.
        let (f, rest) = parse_target("target artifact, enchantment, or creature with flying");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = &f else {
            panic!("expected Or filter, got {f:?}");
        };
        assert_eq!(
            filters.len(),
            3,
            "expected three disjuncts, got {filters:?}"
        );

        let artifact = &filters[0];
        let enchantment = &filters[1];
        let creature = &filters[2];

        let TargetFilter::Typed(artifact_typed) = artifact else {
            panic!("artifact leg should be Typed, got {artifact:?}");
        };
        assert!(has_type(artifact_typed, TypeFilter::Artifact));
        assert!(
            !artifact_typed
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::WithKeyword { .. })),
            "flying must not distribute onto artifact leg: {artifact_typed:?}"
        );

        let TargetFilter::Typed(enchantment_typed) = enchantment else {
            panic!("enchantment leg should be Typed, got {enchantment:?}");
        };
        assert!(has_type(enchantment_typed, TypeFilter::Enchantment));
        assert!(
            !enchantment_typed
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::WithKeyword { .. })),
            "flying must not distribute onto enchantment leg: {enchantment_typed:?}"
        );

        let TargetFilter::Typed(creature_typed) = creature else {
            panic!("creature leg should be Typed, got {creature:?}");
        };
        assert!(has_type(creature_typed, TypeFilter::Creature));
        assert!(
            creature_typed
                .properties
                .contains(&FilterProp::WithKeyword {
                    value: Keyword::Flying
                }),
            "creature leg must retain flying: {creature_typed:?}"
        );
    }

    #[test]
    fn comma_or_without_keyword_suffix_stays_on_final_disjunct_only() {
        let (f, rest) = parse_target("target artifact or creature without flying");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = &f else {
            panic!("expected Or filter, got {f:?}");
        };
        assert_eq!(filters.len(), 2);

        let TargetFilter::Typed(artifact_typed) = &filters[0] else {
            panic!("expected artifact Typed");
        };
        assert!(
            !artifact_typed
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::WithoutKeyword { .. })),
            "without-flying must not distribute onto artifact leg: {artifact_typed:?}"
        );

        let TargetFilter::Typed(creature_typed) = &filters[1] else {
            panic!("expected creature Typed");
        };
        assert!(
            creature_typed
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::WithoutKeyword { .. })),
            "creature leg must retain without-flying: {creature_typed:?}"
        );
    }

    #[test]
    fn distribute_properties_across_or_branches() {
        // "artifacts and creatures with mana value 2 or less" → both branches get CmcLE(2)
        let (f, _) = parse_type_phrase("artifacts and creatures with mana value 2 or less");
        if let TargetFilter::Or { filters } = &f {
            assert_eq!(filters.len(), 2, "should have 2 Or branches");
            for branch in filters {
                if let TargetFilter::Typed(typed) = branch {
                    assert!(
                        typed.properties.iter().any(|p| matches!(
                            p,
                            FilterProp::Cmc {
                                comparator: Comparator::LE,
                                value: QuantityExpr::Fixed { value: 2 }
                            }
                        )),
                        "branch {:?} should have CmcLE(2)",
                        typed.get_primary_type()
                    );
                } else {
                    panic!("expected Typed branch, got {branch:?}");
                }
            }
        } else {
            panic!("expected Or filter, got {f:?}");
        }
    }

    /// #2912 (CR 208.1): a leading "N/M" P/T designation must be parsed as
    /// power/toughness constraints, not fused into a `Subtype("1/1 Creature")`.
    #[test]
    fn parse_type_phrase_pt_designation_is_not_a_subtype() {
        use crate::types::ability::{
            Comparator, FilterProp, PtStat, PtValueScope, QuantityExpr, TypeFilter,
        };
        let (filter, _rest) = parse_type_phrase("a 1/1 creature you control");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(
            tf.type_filters.contains(&TypeFilter::Creature),
            "must be a Creature type, got {:?}",
            tf.type_filters
        );
        assert!(
            !tf.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s.contains('/'))),
            "the P/T designation must NOT be a subtype: {:?}",
            tf.type_filters
        );
        let pt = |stat| FilterProp::PtComparison {
            stat,
            scope: PtValueScope::Current,
            comparator: Comparator::EQ,
            value: QuantityExpr::Fixed { value: 1 },
        };
        assert!(
            tf.properties.contains(&pt(PtStat::Power)),
            "expected power == 1, got {:?}",
            tf.properties
        );
        assert!(
            tf.properties.contains(&pt(PtStat::Toughness)),
            "expected toughness == 1, got {:?}",
            tf.properties
        );

        let (colored_filter, _rest) = parse_type_phrase("a 1/1 white creature you control");
        let TargetFilter::Typed(colored_tf) = colored_filter else {
            panic!("expected Typed filter, got {colored_filter:?}");
        };
        assert!(
            colored_tf.properties.contains(&FilterProp::HasColor {
                color: ManaColor::White
            }),
            "P/T designation must compose with color prefixes, got {:?}",
            colored_tf.properties
        );
        assert!(colored_tf.properties.contains(&pt(PtStat::Power)));
        assert!(colored_tf.properties.contains(&pt(PtStat::Toughness)));

        // End-to-end: Sword of the Meek's trigger filter must no longer be a
        // bogus `Subtype("1/1 Creature")`.
        let parsed = crate::parser::oracle::parse_oracle_text(
            "Whenever a 1/1 creature you control enters, draw a card.",
            "Sword of the Meek",
            &[],
            &["Artifact".into()],
            &[],
        );
        let valid = parsed.triggers[0]
            .valid_card
            .as_ref()
            .expect("trigger has a valid_card filter");
        let TargetFilter::Typed(vtf) = valid else {
            panic!("expected Typed valid_card, got {valid:?}");
        };
        assert!(
            vtf.type_filters.contains(&TypeFilter::Creature)
                && !vtf
                    .type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Subtype(s) if s.contains('/'))),
            "trigger filter must be Creature + P/T, not a '1/1 Creature' subtype: {:?}",
            vtf.type_filters
        );
        assert!(vtf.properties.contains(&pt(PtStat::Power)));
    }

    /// #2905 (CR 205.3): a positive "that's a/an <Subtype> [or a/an <Subtype>]"
    /// relative clause must restrict by subtype, not be dropped (Kibo, Uktabi
    /// Prince put counters on every creature instead of only Apes and Monkeys).
    #[test]
    fn parse_type_phrase_positive_subtype_relative_clause() {
        use crate::types::ability::TypeFilter;

        let (filter, _rest) = parse_type_phrase("creature you control that's an Ape or a Monkey");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(
            tf.type_filters.contains(&TypeFilter::Creature),
            "must keep the Creature core type, got {:?}",
            tf.type_filters
        );
        assert!(
            tf.type_filters.contains(&TypeFilter::AnyOf(vec![
                TypeFilter::Subtype("Ape".to_string()),
                TypeFilter::Subtype("Monkey".to_string()),
            ])),
            "the 'that's an Ape or a Monkey' restriction must AND-merge as an \
             AnyOf subtype disjunction, got {:?}",
            tf.type_filters
        );
        assert_eq!(tf.controller, Some(ControllerRef::You));

        // Single-subtype form → a bare Subtype (no AnyOf wrapper).
        let (single, _) = parse_type_phrase("creature you control that's a Goblin");
        let TargetFilter::Typed(stf) = single else {
            panic!("expected Typed filter");
        };
        assert!(stf
            .type_filters
            .contains(&TypeFilter::Subtype("Goblin".to_string())));
    }

    #[test]
    fn parse_type_phrase_ninja_or_rogue_creatures_you_control() {
        // CR 205.3a: "ninja or rogue creatures you control" — compound subtype+type phrase.
        // parse_type_phrase handles "or" between subtypes when the second branch includes
        // a core type ("rogue creatures"), producing an Or filter.
        let (filter, remainder) = parse_type_phrase("ninja or rogue creatures you control");
        assert!(
            remainder.trim().is_empty(),
            "remainder should be empty, got: '{remainder}'"
        );
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(filters.len(), 2, "expected 2 Or branches, got {filters:?}");
        } else {
            panic!("expected Or filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_outlaw_creatures_you_control() {
        let (filter, remainder) = parse_type_phrase("outlaw creatures you control");
        assert!(
            remainder.trim().is_empty(),
            "remainder should be empty, got: '{remainder}'"
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert_eq!(typed.controller, Some(ControllerRef::You));
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
        assert!(typed.type_filters.iter().any(|type_filter| {
            matches!(type_filter, TypeFilter::AnyOf(filters) if filters.len() == 5)
        }));
    }

    #[test]
    fn parse_type_phrase_handles_plural_head_subtype() {
        let (filter, remainder) = parse_type_phrase("Heads");
        assert!(
            remainder.trim().is_empty(),
            "remainder should be empty, got: '{remainder}'"
        );
        match filter {
            TargetFilter::Typed(typed) => {
                assert!(typed
                    .type_filters
                    .contains(&TypeFilter::Subtype("Head".to_string())));
            }
            other => panic!("expected Head subtype filter, got {other:?}"),
        }
    }

    #[test]
    fn parse_type_phrase_comma_or_three_types() {
        // CR 205.3a: "artifact, creature, or enchantment" — all 3 must appear in Or
        let (filter, rest) = parse_type_phrase("artifact, creature, or enchantment");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(
                filters.len(),
                3,
                "expected 3 Or branches, got {}",
                filters.len()
            );
        } else {
            panic!("Expected Or filter");
        }
    }

    #[test]
    fn parse_type_phrase_comma_or_with_controller() {
        // "artifact, creature, or enchantment you control" — controller distributes
        let (filter, rest) = parse_type_phrase("artifact, creature, or enchantment you control");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(filters.len(), 3);
            for f in filters {
                if let TargetFilter::Typed(tf) = f {
                    assert_eq!(
                        tf.controller,
                        Some(ControllerRef::You),
                        "controller missing on {:?}",
                        tf.get_primary_type()
                    );
                } else {
                    panic!("Expected Typed in Or");
                }
            }
        } else {
            panic!("Expected Or filter");
        }
    }

    #[test]
    fn parse_type_phrase_aura_card_stays_generic() {
        let (filter, rest) =
            parse_type_phrase("Aura card with mana value less than or equal to that Aura");
        assert_eq!(rest.trim(), "Aura", "remainder: '{rest}'");
        let TargetFilter::Typed(typed) = filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert_eq!(typed.get_subtype(), Some("Aura"));
        assert!(
            typed
                .type_filters
                .iter()
                .position(|type_filter| *type_filter == TypeFilter::Enchantment)
                .is_none(),
            "search-only normalization should not happen in parse_type_phrase: {typed:?}"
        );
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::CostPaidObject
                    }
                }
            }
        )));
    }

    #[test]
    fn combat_status_prefix_unblocked() {
        let result = parse_combat_status_prefix("unblocked attacking creatures");
        assert_eq!(result, Some((FilterProp::Unblocked, 10)));
        // Second call on remainder should get Attacking
        let result2 = parse_combat_status_prefix("attacking creatures");
        assert_eq!(
            result2,
            Some((FilterProp::Attacking { defender: None }, 10))
        );
    }

    #[test]
    fn parse_type_phrase_unblocked_attacking_creatures_you_control() {
        let (filter, remainder) = parse_type_phrase("unblocked attacking creatures you control");
        assert!(remainder.trim().is_empty(), "remainder: '{remainder}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.properties.contains(&FilterProp::Unblocked));
            assert!(tf
                .properties
                .contains(&FilterProp::Attacking { defender: None }));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_attacking_or_blocking_creature() {
        let (filter, remainder) = parse_type_phrase("attacking or blocking creature");
        assert!(remainder.trim().is_empty(), "remainder: '{remainder}'");
        let TargetFilter::Or { filters } = &filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);
        let first = typed_leg(&filters[0]).expect("first branch should be typed");
        let second = typed_leg(&filters[1]).expect("second branch should be typed");
        assert!(first.type_filters.contains(&TypeFilter::Creature));
        assert!(second.type_filters.contains(&TypeFilter::Creature));
        assert!(first
            .properties
            .contains(&FilterProp::Attacking { defender: None }));
        assert!(second.properties.contains(&FilterProp::Blocking));
    }

    #[test]
    fn parse_type_phrase_cross_products_multiple_property_disjunctions() {
        let (filter, remainder) =
            parse_type_phrase("attacking or blocking creature with flying or vigilance");
        assert!(remainder.trim().is_empty(), "remainder: '{remainder}'");
        let TargetFilter::Or { filters } = &filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 4);
        let expected = [
            (FilterProp::Attacking { defender: None }, Keyword::Flying),
            (FilterProp::Attacking { defender: None }, Keyword::Vigilance),
            (FilterProp::Blocking, Keyword::Flying),
            (FilterProp::Blocking, Keyword::Vigilance),
        ];
        for (filter, (combat_prop, keyword)) in filters.iter().zip(expected) {
            let typed = typed_leg(filter).expect("branch should be typed");
            assert!(typed.type_filters.contains(&TypeFilter::Creature));
            assert!(
                typed.properties.contains(&combat_prop),
                "missing {combat_prop:?} in {typed:?}"
            );
            assert!(
                typed.properties.contains(&FilterProp::WithKeyword {
                    value: keyword.clone()
                }),
                "missing {keyword:?} in {typed:?}"
            );
        }
    }

    #[test]
    fn parse_type_phrase_tapped_creature() {
        let (filter, rest) = parse_type_phrase("tapped creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf.properties.contains(&FilterProp::Tapped));
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_untapped_land() {
        let (filter, rest) = parse_type_phrase("untapped land");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
            assert!(tf.properties.contains(&FilterProp::Untapped));
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_tapped_artifact_or_creature() {
        // "tapped artifact or creature" — tapped is a leading prefix, applied to the left branch.
        // The "or" handler applies right→left property distribution only, so tapped stays
        // on the artifact branch. (Full leading-property distribution is a separate concern.)
        let (filter, rest) = parse_type_phrase("tapped artifact or creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(filters.len(), 2);
            // Left branch: Artifact with Tapped
            if let TargetFilter::Typed(tf) = &filters[0] {
                assert!(tf.type_filters.contains(&TypeFilter::Artifact));
                assert!(tf.properties.contains(&FilterProp::Tapped));
            } else {
                panic!("Expected Typed, got {:?}", filters[0]);
            }
            // Right branch: Creature (no Tapped — not distributed from left)
            if let TargetFilter::Typed(tf) = &filters[1] {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            } else {
                panic!("Expected Typed, got {:?}", filters[1]);
            }
        } else {
            panic!("Expected Or filter, got {filter:?}");
        }
    }

    #[test]
    fn that_share_creature_type_consumed() {
        // "that share a creature type" is consumed into SharesQuality.
        let (filter, rest) = parse_type_phrase("creatures you control that share a creature type");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf
                .type_filters
                .iter()
                .any(|type_filter| matches!(type_filter, TypeFilter::Creature)));
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.iter().any(
                |p| matches!(p, FilterProp::SharesQuality { quality, .. } if *quality == SharedQuality::CreatureType)
            ));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_share_no_creature_types_consumed() {
        let (filter, rest) = parse_type_phrase("creatures that share no creature types");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::SharesQuality {
                    quality: SharedQuality::CreatureType,
                    reference: None,
                    relation: SharedQualityRelation::DoesNotShare,
                }
            )));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_shares_card_type_with_exiled_card_consumed() {
        let (filter, rest) =
            parse_type_phrase("permanent that shares a card type with the exiled card");
        let TargetFilter::Typed(ref tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(tf
            .type_filters
            .iter()
            .any(|type_filter| matches!(type_filter, TypeFilter::Permanent)));
        assert!(tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::SharesQuality {
                quality: SharedQuality::CardType,
                reference: Some(reference),
                relation: SharedQualityRelation::Shares,
            } if matches!(reference.as_ref(), TargetFilter::TrackedSet { id } if *id == TrackedSetId(0))
        )));
        assert!(rest.trim().is_empty(), "remainder: {rest:?}");
    }

    #[test]
    fn parse_shared_quality_permanent_type_maps_to_permanent_type() {
        // CR 110.4: "permanent type" names only the six permanent types, a
        // strict subset of the card types (CR 205.2a). The recognizer maps
        // both the singular and plural forms to SharedQuality::PermanentType
        // (NOT CardType), so a shared non-permanent card type like Kindred
        // cannot satisfy "share a permanent type" (Role Reversal wording).
        let (rest, q) = parse_shared_quality("permanent type").expect("singular");
        assert_eq!(q, SharedQuality::PermanentType);
        assert!(rest.is_empty(), "remainder: {rest:?}");
        let (rest, q) = parse_shared_quality("permanent types").expect("plural");
        assert_eq!(q, SharedQuality::PermanentType);
        assert!(rest.is_empty(), "remainder: {rest:?}");
    }

    #[test]
    fn that_shares_permanent_type_with_it_consumed() {
        // Cloudstone Curio: "... permanent you control that shares a permanent
        // type with it ...". The relative clause must be consumed and lowered
        // to a SharesQuality{PermanentType} constraint (CR 110.4 narrowing:
        // NOT CardType, so a shared Kindred-only pairing does not match;
        // previously the clause was silently dropped).
        let (filter, rest) = parse_type_phrase("permanent that shares a permanent type with it");
        let TargetFilter::Typed(ref tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::SharesQuality {
                quality: SharedQuality::PermanentType,
                relation: SharedQualityRelation::Shares,
                ..
            }
        )));
        assert!(rest.trim().is_empty(), "remainder: {rest:?}");
    }

    #[test]
    fn that_dont_share_card_type_with_discarded_card_consumed() {
        let (filter, rest) =
            parse_type_phrase("cards that don't share a card type with the discarded card");
        let TargetFilter::Typed(ref tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::SharesQuality {
                quality: SharedQuality::CardType,
                reference: Some(reference),
                relation: SharedQualityRelation::DoesNotShare,
            } if matches!(reference.as_ref(), TargetFilter::ParentTarget)
        )));
        assert!(rest.trim().is_empty(), "remainder: {rest:?}");
    }

    #[test]
    fn that_shares_card_type_with_one_discarded_card_consumed() {
        let (filter, rest) =
            parse_type_phrase("card that shares a card type with one of the discarded cards");
        let TargetFilter::Typed(ref tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::SharesQuality {
                quality: SharedQuality::CardType,
                reference: Some(reference),
                relation: SharedQualityRelation::Shares,
            } if matches!(reference.as_ref(), TargetFilter::TriggeringSource)
        )));
        assert!(rest.trim().is_empty(), "remainder: {rest:?}");
    }

    #[test]
    fn that_doesnt_share_land_type_with_land_you_control_consumed() {
        let (filter, rest) =
            parse_type_phrase("land that doesn't share a land type with a land you control");
        let TargetFilter::Typed(ref tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(tf
            .type_filters
            .iter()
            .any(|type_filter| matches!(type_filter, TypeFilter::Land)));
        assert!(tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::SharesQuality {
                quality: SharedQuality::LandType,
                reference: Some(reference),
                relation: SharedQualityRelation::DoesNotShare,
            } if matches!(
                reference.as_ref(),
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller: Some(ControllerRef::You),
                    ..
                }) if type_filters.iter().any(|type_filter| matches!(type_filter, TypeFilter::Land))
            )
        )));
        assert!(rest.trim().is_empty(), "remainder: {rest:?}");
    }

    #[test]
    fn target_that_share_full_parse() {
        let (filter, rest) =
            parse_target("target creatures you control that share a creature type");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::SharesQuality { .. })));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_was_dealt_damage_this_turn() {
        let (filter, rest) = parse_target("target creature that was dealt damage this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::WasDealtDamageThisTurn)));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_was_dealt_damage_with_controller() {
        let (filter, rest) =
            parse_target("target creature an opponent controls that was dealt damage this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::WasDealtDamageThisTurn)),
                "Expected WasDealtDamageThisTurn in properties: {:?}",
                tf.properties
            );
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_entered_this_turn() {
        let (filter, rest) = parse_type_phrase("token you control that entered this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.iter().any(|p| matches!(p, FilterProp::Token)));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::EnteredThisTurn)));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_entered_the_battlefield_this_turn() {
        let (filter, rest) = parse_type_phrase("creature that entered the battlefield this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::EnteredThisTurn)));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn type_phrase_cards_put_there_from_battlefield_this_turn() {
        let (filter, rest) = parse_type_phrase(
            "artifact and creature cards in your graveyard that were put there from the battlefield this turn",
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected OR filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);
        for filter in filters {
            let TargetFilter::Typed(tf) = filter else {
                panic!("expected typed leg, got {filter:?}");
            };
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.contains(&FilterProp::InZone {
                zone: Zone::Graveyard
            }));
            assert!(tf.properties.iter().any(|prop| matches!(
                prop,
                FilterProp::ZoneChangedThisTurn {
                    from: Some(Zone::Battlefield),
                    to: Some(Zone::Graveyard),
                }
            )));
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_attacked_this_turn() {
        let (filter, rest) = parse_target("target creature that attacked this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::AttackedThisTurn { defender: None })));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    /// CR 508.6: "that attacked you this turn" scopes the attack-history filter to
    /// the ability controller as defending player (Jabari's Influence). The bare
    /// "attacked this turn" path must stay board-wide (`defender: None`).
    #[test]
    fn that_attacked_you_this_turn_scopes_defender_to_you() {
        let (filter, _rest) = parse_target("target creature that attacked you this turn");
        let TargetFilter::Typed(ref tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::AttackedThisTurn {
                    defender: Some(ControllerRef::You)
                }
            )),
            "expected defender-scoped AttackedThisTurn(Some(You)), got {tf:?}"
        );
    }

    /// CR 508.6: the defender-scoped arm leaves a trailing " and …" clause intact
    /// so `try_split_targeted_compound` can auto-chain a follow-on effect. The
    /// permissive return (no terminator/continuation guard) is what preserves the
    /// coupling between the target restriction and the counter clause.
    #[test]
    fn attacked_you_this_turn_leaves_trailing_and_clause() {
        let (props, consumed) = parse_that_clause_suffix(
            " that attacked you this turn and put a -1/-0 counter on it",
            None,
        )
        .expect("defender-scoped clause must parse");
        assert_eq!(
            props,
            vec![FilterProp::AttackedThisTurn {
                defender: Some(ControllerRef::You)
            }]
        );
        // Consumed exactly through "attacked you this turn"; the " and put …"
        // remainder is left for the compound splitter.
        assert_eq!(
            consumed,
            " that attacked you this turn".len(),
            "must not consume the trailing ' and …' clause"
        );
    }

    /// CR 608.2c: the negated present-tense arm ("didn't attack this turn")
    /// De Morgan-decomposes to `Not(AttackedThisTurn { defender: None })` — still
    /// board-wide, unaffected by the defender parameterization.
    #[test]
    fn didnt_attack_this_turn_negates_board_wide() {
        let (props, _consumed) = parse_that_clause_suffix(" that didn't attack this turn", None)
            .expect("negated clause must parse");
        assert_eq!(
            props,
            vec![FilterProp::Not {
                prop: Box::new(FilterProp::AttackedThisTurn { defender: None })
            }]
        );
    }

    #[test]
    fn that_blocked_this_turn() {
        let (filter, rest) = parse_target("target creature that blocked this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::BlockedThisTurn)));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_attacked_or_blocked_this_turn() {
        let (filter, rest) = parse_target("target creature that attacked or blocked this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::AttackedOrBlockedThisTurn)));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    // --- CR 303.4 + CR 301.5: "that's enchanted or equipped" relative-clause tests ---
    // Compound-subject grant class (Reyav, Master Smith; Dogmeat, Ever Loyal).

    #[test]
    fn that_s_enchanted_or_equipped_emits_disjunction() {
        let result = parse_that_clause_suffix(" that's enchanted or equipped", None);
        let (props, _consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        match &props[0] {
            FilterProp::HasAnyAttachmentOf { kinds, controller } => {
                assert_eq!(
                    kinds,
                    &vec![AttachmentKind::Aura, AttachmentKind::Equipment]
                );
                assert_eq!(controller, &None);
            }
            other => panic!("expected HasAnyAttachmentOf, got {other:?}"),
        }
    }

    #[test]
    fn that_s_equipped_or_enchanted_emits_disjunction() {
        let result = parse_that_clause_suffix(" that's equipped or enchanted", None);
        let (props, _consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::HasAnyAttachmentOf { kinds, .. }
                if kinds.len() == 2 && kinds.contains(&AttachmentKind::Aura)
                    && kinds.contains(&AttachmentKind::Equipment)
        ));
    }

    #[test]
    fn that_are_enchanted_or_equipped_emits_disjunction() {
        let result = parse_that_clause_suffix(" that are enchanted or equipped", None);
        let (props, consumed) = result.expect("should parse");
        assert_eq!(consumed, " that are enchanted or equipped".len());
        assert!(matches!(
            &props[0],
            FilterProp::HasAnyAttachmentOf { kinds, .. }
                if kinds.len() == 2 && kinds.contains(&AttachmentKind::Aura)
                    && kinds.contains(&AttachmentKind::Equipment)
        ));
    }

    #[test]
    fn that_s_enchanted_only_emits_single_kind() {
        let result = parse_that_clause_suffix(" that's enchanted", None);
        let (props, _consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::HasAttachment {
                kind: AttachmentKind::Aura,
                controller: None,
                ..
            }
        ));
    }

    #[test]
    fn that_s_equipped_only_emits_single_kind() {
        let result = parse_that_clause_suffix(" that's equipped", None);
        let (props, _consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::HasAttachment {
                kind: AttachmentKind::Equipment,
                controller: None,
                ..
            }
        ));
    }

    #[test]
    fn that_s_red_or_green_emits_color_disjunction() {
        let result = parse_that_clause_suffix(" that's red or green", None);
        let (props, consumed) = result.expect("should parse");
        assert_eq!(consumed, " that's red or green".len());
        assert_eq!(
            props,
            vec![FilterProp::AnyOf {
                props: vec![
                    FilterProp::HasColor {
                        color: ManaColor::Red,
                    },
                    FilterProp::HasColor {
                        color: ManaColor::Green,
                    },
                ],
            }]
        );
    }

    /// #641 (Urza's Ruinous Blast — "Exile all nonland permanents that aren't
    /// legendary"): the "that aren't legendary" relative clause was dropped, so
    /// the filter exiled every nonland permanent (legendary included). The
    /// plural "that aren't" negation form was missing AND supertypes were not
    /// handled in any relative-clause parser. Regression guard for the negation.
    #[test]
    fn that_arent_legendary_emits_not_supertype() {
        let result = parse_that_clause_suffix(" that aren't legendary", None);
        let (props, consumed) = result.expect("should parse");
        assert_eq!(consumed, " that aren't legendary".len());
        assert_eq!(
            props,
            vec![FilterProp::NotSupertype {
                value: Supertype::Legendary,
            }]
        );
    }

    /// CR 205.4a: sibling positive form — "that's legendary" → `HasSupertype`.
    /// Confirms the building block covers both polarities, not just the
    /// reported negation.
    #[test]
    fn thats_legendary_emits_has_supertype() {
        let result = parse_that_clause_suffix(" that's legendary", None);
        let (props, consumed) = result.expect("should parse");
        assert_eq!(consumed, " that's legendary".len());
        assert_eq!(
            props,
            vec![FilterProp::HasSupertype {
                value: Supertype::Legendary,
            }]
        );
    }

    /// #641 end-to-end: the full Urza's Ruinous Blast target phrase must carry
    /// the `NotSupertype(Legendary)` property alongside the nonland-permanent
    /// type filters, so the mass-exile excludes legendary permanents.
    #[test]
    fn nonland_permanents_that_arent_legendary_full_target() {
        let (filter, rest) = parse_target("all nonland permanents that aren't legendary");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(
            tf.properties.contains(&FilterProp::NotSupertype {
                value: Supertype::Legendary,
            }),
            "must exclude legendary permanents, got {:?}",
            tf.properties
        );
    }

    /// Cluster 15 (The Fifth Doctor / Angel's Trumpet): the negated verb-phrase
    /// relative clause "that didn't <verb> this turn" was dropped, so the
    /// mass effect applied to every creature. CR 608.2c De Morgan: each verb
    /// becomes its positive FilterProp wrapped in `Not`.
    #[test]
    fn that_didnt_attack_emits_not_attacked() {
        let (props, consumed) = parse_that_clause_suffix(" that didn't attack this turn", None)
            .expect("should parse negated attack clause");
        assert_eq!(consumed, " that didn't attack this turn".len());
        assert_eq!(
            props,
            vec![FilterProp::Not {
                prop: Box::new(FilterProp::AttackedThisTurn { defender: None }),
            }]
        );
    }

    #[test]
    fn that_didnt_attack_or_enter_emits_de_morgan_pair() {
        let (props, consumed) =
            parse_that_clause_suffix(" that didn't attack or enter this turn", None)
                .expect("should parse negated attack-or-enter clause");
        assert_eq!(consumed, " that didn't attack or enter this turn".len());
        assert_eq!(
            props,
            vec![
                FilterProp::Not {
                    prop: Box::new(FilterProp::AttackedThisTurn { defender: None }),
                },
                FilterProp::Not {
                    prop: Box::new(FilterProp::EnteredThisTurn),
                },
            ]
        );
    }

    #[test]
    fn that_didnt_enter_the_battlefield_emits_not_entered() {
        let (props, consumed) =
            parse_that_clause_suffix(" that didn't enter the battlefield this turn", None)
                .expect("should parse negated enter-the-battlefield clause");
        assert_eq!(
            consumed,
            " that didn't enter the battlefield this turn".len()
        );
        assert_eq!(
            props,
            vec![FilterProp::Not {
                prop: Box::new(FilterProp::EnteredThisTurn),
            }]
        );
    }

    #[test]
    fn that_didnt_block_emits_not_blocked() {
        let (props, consumed) = parse_that_clause_suffix(" that didn't block this turn", None)
            .expect("should parse negated block clause");
        assert_eq!(consumed, " that didn't block this turn".len());
        assert_eq!(
            props,
            vec![FilterProp::Not {
                prop: Box::new(FilterProp::BlockedThisTurn),
            }]
        );
    }

    /// Word-boundary guard: " this turning" must NOT match (the negated arm
    /// requires a boundary after "this turn", unlike the positive VERB_PHRASES).
    #[test]
    fn that_didnt_attack_this_turning_does_not_match() {
        assert!(parse_that_clause_suffix(" that didn't attack this turning", None).is_none());
    }

    /// Regression: the negated arm must not shadow the positive past-tense path.
    #[test]
    fn that_attacked_still_emits_positive_attacked() {
        let (props, _) = parse_that_clause_suffix(" that attacked this turn", None)
            .expect("positive past-tense clause must still parse");
        assert_eq!(props, vec![FilterProp::AttackedThisTurn { defender: None }]);
    }

    /// Upstream-truncated form: some producers (the "tap all" target extractor)
    /// strip the trailing " this turn" duration before the target text reaches
    /// the type-phrase parser, leaving "that didn't attack" at end-of-string.
    /// The negated arm must still match when the verb sits at a clause boundary.
    #[test]
    fn that_didnt_attack_without_this_turn_at_boundary_matches() {
        let (props, _) = parse_that_clause_suffix(" that didn't attack", None)
            .expect("duration-stripped form must still parse at end-of-string");
        assert_eq!(
            props,
            vec![FilterProp::Not {
                prop: Box::new(FilterProp::AttackedThisTurn { defender: None }),
            }]
        );

        // Also accepts a "."/"," clause terminator.
        let (props, _) = parse_that_clause_suffix(" that didn't attack.", None)
            .expect("duration-stripped form must parse before a period");
        assert_eq!(
            props,
            vec![FilterProp::Not {
                prop: Box::new(FilterProp::AttackedThisTurn { defender: None }),
            }]
        );
    }

    /// Guard: a verb followed by a SPACE + more words (no "this turn", no clause
    /// boundary) must NOT match — that is unmatched continued text, not a
    /// complete negated relative clause.
    #[test]
    fn that_didnt_attack_with_trailing_words_does_not_match() {
        assert!(parse_that_clause_suffix(" that didn't attack a player", None).is_none());
    }

    /// The Fifth Doctor end-to-end: the mass-target type phrase (the "each"
    /// quantifier is stripped upstream before `parse_type_phrase` is reached)
    /// must carry both negated props alongside the controller scope, so the
    /// counter (and the chained TrackedSet untap) follow only the qualifying
    /// subset.
    #[test]
    fn creature_you_control_that_didnt_attack_or_enter_full_phrase() {
        let (filter, rest) =
            parse_type_phrase("creature you control that didn't attack or enter this turn");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(
            tf.properties.contains(&FilterProp::Not {
                prop: Box::new(FilterProp::AttackedThisTurn { defender: None }),
            }),
            "must exclude attackers, got {:?}",
            tf.properties
        );
        assert!(
            tf.properties.contains(&FilterProp::Not {
                prop: Box::new(FilterProp::EnteredThisTurn),
            }),
            "must exclude this-turn entrants, got {:?}",
            tf.properties
        );
    }

    /// Angel's Trumpet end-to-end: a negated verb clause that FOLLOWS a
    /// controller clause ("untapped creatures that player controls that didn't
    /// attack this turn") must still attach. The controller clause is consumed
    /// first, then the trailing relative clause is re-parsed — so `Untapped`
    /// (from "untapped creatures"), the `ScopedPlayer` controller, AND
    /// `Not(AttackedThisTurn)` all land together.
    #[test]
    fn untapped_creatures_that_player_controls_that_didnt_attack_full_phrase() {
        let mut ctx = ParseContext {
            relative_player_scope: Some(ControllerRef::ScopedPlayer),
            ..ParseContext::default()
        };
        let (filter, rest) = parse_type_phrase_with_ctx(
            "untapped creatures that player controls that didn't attack this turn",
            &mut ctx,
        );
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert_eq!(tf.controller, Some(ControllerRef::ScopedPlayer));
        assert!(
            tf.properties.contains(&FilterProp::Untapped),
            "must keep the untapped restriction, got {:?}",
            tf.properties
        );
        assert!(
            tf.properties.contains(&FilterProp::Not {
                prop: Box::new(FilterProp::AttackedThisTurn { defender: None }),
            }),
            "trailing negated clause must attach after the controller clause, got {:?}",
            tf.properties
        );
    }

    #[test]
    fn permanents_that_are_one_or_more_colors_full_target() {
        let (filter, rest) = parse_target("all permanents that are one or more colors");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Permanent));
        assert!(
            tf.properties.contains(&FilterProp::ColorCount {
                comparator: Comparator::GE,
                count: 1,
            }),
            "must require colored permanents, got {:?}",
            tf.properties
        );
    }

    #[test]
    fn that_clause_suffix_exactly_three_colors() {
        // CR 105.2: "that's exactly three colors" → ColorCount{EQ,3}.
        let (props, consumed) =
            parse_that_clause_suffix("that's exactly three colors", None).expect("must parse");
        assert_eq!(
            props,
            vec![FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 3,
            }]
        );
        assert_eq!(consumed, "that's exactly three colors".len());
    }

    #[test]
    fn that_clause_suffix_one_or_more_colors() {
        // CR 105.2: "that's one or more colors" → ColorCount{GE,1}.
        let (props, consumed) =
            parse_that_clause_suffix("that's one or more colors", None).expect("must parse");
        assert_eq!(
            props,
            vec![FilterProp::ColorCount {
                comparator: Comparator::GE,
                count: 1,
            }]
        );
        assert_eq!(consumed, "that's one or more colors".len());
    }

    #[test]
    fn target_spell_or_permanent_thats_red_or_green_distributes_color_to_both_legs() {
        let (filter, rest) = parse_target("target spell or permanent that's red or green");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = filter else {
            panic!("Expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);
        assert!(filters.iter().all(|filter| {
            typed_leg(filter).is_some_and(|tf| {
                tf.properties.iter().any(|prop| {
                    matches!(
                        prop,
                        FilterProp::AnyOf { props }
                            if props.iter().any(|prop| prop == &FilterProp::HasColor { color: ManaColor::Red })
                                && props.iter().any(|prop| prop == &FilterProp::HasColor { color: ManaColor::Green })
                    )
                })
            })
        }));
        assert!(filters.iter().any(is_stack_spell_leg));
    }

    #[test]
    fn that_s_enchanted_or_equipped_in_full_target() {
        // Reyav / Dogmeat trigger subject form.
        let (filter, _rest) = parse_target("a creature you control that's enchanted or equipped");
        match filter {
            TargetFilter::Typed(tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::HasAnyAttachmentOf { kinds, .. } if kinds.len() == 2
                )));
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    // --- CR 115.9c: "that targets only [X]" tests ---

    #[test]
    fn that_targets_only_self_ref() {
        let result = parse_that_clause_suffix(" that targets only ~", None);
        let (props, _consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::TargetsOnly { filter } if **filter == TargetFilter::SelfRef
        ));
    }

    #[test]
    fn that_targets_only_it() {
        let result = parse_that_clause_suffix(" that targets only it,", None);
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::TargetsOnly { filter } if **filter == TargetFilter::SelfRef
        ));
        // Should consume up to "it" but not the comma
        assert_eq!(consumed, " that targets only it".len());
    }

    #[test]
    fn that_targets_only_you() {
        let result = parse_that_clause_suffix(" that targets only you,", None);
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::TargetsOnly { filter }
                if matches!(&**filter, TargetFilter::Typed(TypedFilter { controller: Some(ControllerRef::You), .. }))
        ));
        assert_eq!(consumed, " that targets only you".len());
    }

    #[test]
    fn that_targets_only_single_creature_you_control() {
        let result =
            parse_that_clause_suffix(" that targets only a single creature you control,", None);
        let (props, consumed) = result.expect("should parse");
        // Should produce TargetsOnly + HasSingleTarget
        assert_eq!(props.len(), 2);
        assert!(matches!(&props[0], FilterProp::TargetsOnly { .. }));
        assert!(matches!(&props[1], FilterProp::HasSingleTarget));
        if let FilterProp::TargetsOnly { filter } = &props[0] {
            if let TargetFilter::Typed(tf) = &**filter {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            } else {
                panic!("expected Typed inner filter, got {filter:?}");
            }
        }
        assert_eq!(
            consumed,
            " that targets only a single creature you control".len()
        );
    }

    #[test]
    fn that_targets_only_single_permanent_or_player() {
        let result =
            parse_that_clause_suffix(" that targets only a single permanent or player", None);
        let (props, _consumed) = result.expect("should parse");
        assert_eq!(props.len(), 2);
        assert!(matches!(&props[0], FilterProp::TargetsOnly { .. }));
        assert!(matches!(&props[1], FilterProp::HasSingleTarget));
        if let FilterProp::TargetsOnly { filter } = &props[0] {
            assert!(
                matches!(&**filter, TargetFilter::Or { .. }),
                "expected Or filter for 'permanent or player', got {filter:?}"
            );
        }
    }

    #[test]
    fn type_phrase_with_targets_only_self() {
        // "instant or sorcery spell that targets only ~"
        let (filter, rest) =
            parse_type_phrase("instant or sorcery spell that targets only ~, copy");
        assert_eq!(rest.trim_start().trim_start_matches(',').trim(), "copy");
        // The filter should be Or(Instant + TargetsOnly, Sorcery + TargetsOnly)
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(filters.len(), 2);
            for f in filters {
                if let TargetFilter::Typed(tf) = f {
                    assert!(
                        tf.properties
                            .iter()
                            .any(|p| matches!(p, FilterProp::TargetsOnly { .. })),
                        "expected TargetsOnly in properties of {tf:?}"
                    );
                } else {
                    panic!("expected Typed filter in Or, got {f:?}");
                }
            }
        } else {
            panic!("expected Or filter, got {filter:?}");
        }
    }

    // --- CR 115.9b: "that targets [X]" tests (.any() semantics) ---

    #[test]
    fn that_targets_self_ref() {
        let result = parse_that_clause_suffix(" that targets this creature,", None);
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::Targets { filter } if **filter == TargetFilter::SelfRef
        ));
        assert_eq!(consumed, " that targets this creature".len());
    }

    #[test]
    fn that_targets_tilde() {
        let result = parse_that_clause_suffix(" that targets ~,", None);
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::Targets { filter } if **filter == TargetFilter::SelfRef
        ));
        assert_eq!(consumed, " that targets ~".len());
    }

    #[test]
    fn that_targets_this_permanent() {
        let result = parse_that_clause_suffix(" that targets this permanent,", None);
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::Targets { filter } if **filter == TargetFilter::SelfRef
        ));
        assert_eq!(consumed, " that targets this permanent".len());
    }

    #[test]
    fn that_targets_you() {
        let result = parse_that_clause_suffix(" that targets you,", None);
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::Targets { filter } if **filter == TargetFilter::Controller
        ));
        assert_eq!(consumed, " that targets you".len());
    }

    #[test]
    fn that_targets_you_or_a_creature() {
        let result = parse_that_clause_suffix(" that targets you or a creature you control,", None);
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        if let FilterProp::Targets { filter } = &props[0] {
            if let TargetFilter::Or { filters } = &**filter {
                assert_eq!(filters.len(), 2);
                assert_eq!(filters[0], TargetFilter::Controller);
                if let TargetFilter::Typed(tf) = &filters[1] {
                    assert!(tf.type_filters.contains(&TypeFilter::Creature));
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                } else {
                    panic!("expected Typed filter, got {:?}", filters[1]);
                }
            } else {
                panic!("expected Or filter, got {filter:?}");
            }
        } else {
            panic!("expected Targets prop, got {:?}", props[0]);
        }
        assert_eq!(
            consumed,
            " that targets you or a creature you control".len()
        );
    }

    #[test]
    fn that_targets_one_or_more_creatures() {
        // "one or more" prefix is stripped (redundant with .any() semantics)
        let result =
            parse_that_clause_suffix(" that targets one or more creatures you control,", None);
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        if let FilterProp::Targets { filter } = &props[0] {
            if let TargetFilter::Typed(tf) = &**filter {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            } else {
                panic!("expected Typed filter, got {filter:?}");
            }
        } else {
            panic!("expected Targets prop, got {:?}", props[0]);
        }
        assert_eq!(
            consumed,
            " that targets one or more creatures you control".len()
        );
    }

    #[test]
    fn type_phrase_spell_that_targets_self() {
        // "spell that targets this creature" via parse_type_phrase
        let (filter, rest) = parse_type_phrase("spell that targets this creature, put");
        assert_eq!(rest.trim_start().trim_start_matches(',').trim(), "put");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Card));
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::Targets { filter } if **filter == TargetFilter::SelfRef)),
                "expected Targets {{ SelfRef }} in properties: {:?}",
                tf.properties
            );
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
    }

    // ── VERB-01: Compound target type patterns ──

    #[test]
    fn parse_type_phrase_creature_or_planeswalker() {
        let (filter, rest) = parse_type_phrase("creature or planeswalker");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(filters.len(), 2);
            assert_eq!(filters[0], TargetFilter::Typed(TypedFilter::creature()));
            assert_eq!(
                filters[1],
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker))
            );
        } else {
            panic!("Expected Or filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_nonland_permanent() {
        let (filter, rest) = parse_type_phrase("nonland permanent");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Permanent));
            assert!(tf
                .type_filters
                .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))));
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_creature_with_power_3_or_greater() {
        let (filter, rest) = parse_type_phrase("creature with power 3 or greater");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(
                tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Current,
                        comparator: Comparator::GE,
                        value: QuantityExpr::Fixed { value: 3 }
                    }
                )),
                "Expected PtComparison(Power, GE, 3) in {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_creature_with_greater_power() {
        // CR 509.1b: "creatures with greater power" — relative to source
        let (filter, rest) = parse_type_phrase("creatures with greater power");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::PowerGTSource)),
                "Expected PowerGTSource in {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_creature_without_flying() {
        let (filter, rest) = parse_type_phrase("creature without flying");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(
                tf.properties.iter().any(
                    |p| matches!(p, FilterProp::WithoutKeyword { value } if *value == Keyword::Flying)
                ),
                "Expected WithoutKeyword(Flying) in {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_creature_without_first_strike() {
        let (filter, rest) = parse_type_phrase("creature without first strike");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(
                tf.properties.iter().any(
                    |p| matches!(p, FilterProp::WithoutKeyword { value } if *value == Keyword::FirstStrike)
                ),
                "Expected WithoutKeyword(FirstStrike) in {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_another_creature() {
        let (filter, rest) = parse_type_phrase("another creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(
                tf.properties.contains(&FilterProp::Another),
                "Expected Another property in {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_another_creature_you_control() {
        let (filter, rest) = parse_type_phrase("another creature you control");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf.properties.contains(&FilterProp::Another));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    /// CR 109.2: A leading universal quantifier ("all"/"each"/"every") ranging
    /// over a type/subtype subject must be stripped to reach the type word — it
    /// must NOT leak into the subtype string (e.g. Subtype("Each Vehicle")) and
    /// must NOT add `FilterProp::Another` (it selects the source too, unlike
    /// "other"). Consumers of `parse_type_phrase` that exercise this: the
    /// "sacrifice all <type> you control" additional/activation cost filters
    /// (Soulblast — creatures; Kaervek's Spite — permanents; Tomb of Urami —
    /// lands) and the "Whenever all <type> you control attack" trigger
    /// `valid_card` (Mob Mentality — non-Wall creatures). Before this fix those
    /// filters were left empty/untyped (matching every object) or the trigger
    /// failed to classify.
    #[test]
    fn parse_type_phrase_universal_quantifier_stripped_no_leak() {
        for (text, subtype) in [
            ("each Vehicle you control", "Vehicle"),
            ("all Cats you control", "Cat"),
            ("every Skeleton you control", "Skeleton"),
        ] {
            let (filter, rest) = parse_type_phrase(text);
            assert!(rest.trim().is_empty(), "remainder for '{text}': '{rest}'");
            let TargetFilter::Typed(tf) = &filter else {
                panic!("Expected Typed filter for '{text}', got {filter:?}");
            };
            assert!(
                tf.type_filters
                    .contains(&TypeFilter::Subtype(subtype.to_string())),
                "expected Subtype(\"{subtype}\") for '{text}', got {:?}",
                tf.type_filters
            );
            // The quantifier must NOT survive inside the subtype string.
            assert!(
                !tf.type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Subtype(s) if s.contains(' '))),
                "quantifier leaked into subtype for '{text}': {:?}",
                tf.type_filters
            );
            // Universal quantifiers select the source too — no Another exclusion.
            assert!(
                !tf.properties.contains(&FilterProp::Another),
                "unexpected Another for '{text}': {:?}",
                tf.properties
            );
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }

        // "all/each/every OTHER <type>" must strip the quantifier AND still carry
        // the type plus `FilterProp::Another` (source excluded) — the quantifier
        // must not leave the "other" exclusion stranded. Covers "each other
        // creature" / "all other creatures" (review-flagged gap).
        for text in [
            "each other creature you control",
            "all other creatures you control",
        ] {
            let (filter, rest) = parse_type_phrase(text);
            assert!(rest.trim().is_empty(), "remainder for '{text}': '{rest}'");
            let TargetFilter::Typed(tf) = &filter else {
                panic!("Expected Typed filter for '{text}', got {filter:?}");
            };
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "expected Creature for '{text}', got {:?}",
                tf.type_filters
            );
            assert!(
                !tf.type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Subtype(s) if s.contains(' '))),
                "quantifier/other leaked into subtype for '{text}': {:?}",
                tf.type_filters
            );
            // "other" excludes the source → Another IS present here.
            assert!(
                tf.properties.contains(&FilterProp::Another),
                "expected Another for '{text}': {:?}",
                tf.properties
            );
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
    }

    /// CR 700.9 + CR 109.4: "modified creatures you control other than ~"
    /// (Thundering Raiju). The "modified" adjective adds `FilterProp::Modified`
    /// and the trailing "other than ~" adds `FilterProp::Another` so the count
    /// omits the source permanent.
    #[test]
    fn parse_type_phrase_modified_creatures_other_than_self() {
        let (filter, rest) = parse_type_phrase("modified creatures you control other than ~");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(
            tf.properties.contains(&FilterProp::Modified),
            "missing Modified in {:?}",
            tf.properties
        );
        assert!(
            tf.properties.contains(&FilterProp::Another),
            "missing Another in {:?}",
            tf.properties
        );
    }

    /// CR 109.4: "other than this creature" (the un-normalized form) also adds
    /// `FilterProp::Another` via the "other than <self-ref>" suffix.
    #[test]
    fn parse_type_phrase_other_than_this_creature() {
        let (filter, rest) = parse_type_phrase("creatures you control other than this creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(
            tf.properties.contains(&FilterProp::Another),
            "missing Another in {:?}",
            tf.properties
        );
    }

    /// CR 700.9 + CR 109.4: end-to-end quantity ref for Thundering Raiju —
    /// "the number of modified creatures you control other than ~" →
    /// `ObjectCount { Typed(Creature, You, [Modified, Another]) }`.
    #[test]
    fn parse_quantity_ref_modified_creatures_other_than_self() {
        let q = crate::parser::oracle_quantity::parse_quantity_ref(
            "the number of modified creatures you control other than ~",
        )
        .expect("should parse");
        let QuantityRef::ObjectCount { filter } = q else {
            panic!("Expected ObjectCount, got {q:?}");
        };
        let TargetFilter::Typed(tf) = &filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf.properties.contains(&FilterProp::Modified));
        assert!(tf.properties.contains(&FilterProp::Another));
    }

    #[test]
    fn parse_target_another_target_creature() {
        // "another target creature" via parse_target: "target " prefix consumed,
        // then parse_type_phrase("another creature") should add Another property.
        let (filter, rest) = parse_target("target another creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(
                tf.properties.contains(&FilterProp::Another),
                "Expected Another property in {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_target_a_second_target_creature_you_control() {
        let (filter, rest) = parse_target("a second target creature you control");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_target_other_target_creature_or_spell() {
        let (filter, rest) = parse_target("other target creature or spell");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = filter else {
            panic!("Expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);
        assert!(filters.iter().any(|filter| matches!(
            filter,
            TargetFilter::Typed(tf)
                if has_type(tf, TypeFilter::Creature)
                    && has_prop(tf, FilterProp::Another)
        )));
        assert!(filters.iter().any(|filter| matches!(
            filter,
            TargetFilter::And { filters }
                if filters.iter().any(|filter| matches!(filter, TargetFilter::StackSpell))
                    && filters.iter().any(|filter| matches!(
                        filter,
                        TargetFilter::Typed(tf)
                            if has_prop(tf, FilterProp::Another)
                    ))
        )));
    }

    #[test]
    fn parse_target_spell_or_creature_uses_stack_spell_leg() {
        let (filter, rest) = parse_target("target spell or creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = filter else {
            panic!("Expected Or filter, got {filter:?}");
        };
        assert!(filters
            .iter()
            .any(|filter| matches!(filter, TargetFilter::StackSpell)));
        assert!(filters.iter().any(|filter| matches!(
            filter,
            TargetFilter::Typed(tf)
                if has_type(tf, TypeFilter::Creature)
                    && !has_prop(tf, FilterProp::InZone { zone: Zone::Stack })
        )));
    }

    #[test]
    fn parse_target_artifact_or_enchantment_spell_scopes_all_legs_to_stack() {
        let (filter, rest) = parse_target("target artifact or enchantment spell");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = filter else {
            panic!("Expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);
        assert!(filters.iter().all(|filter| matches!(
            filter,
            TargetFilter::And { filters }
                if filters.iter().any(|filter| matches!(filter, TargetFilter::StackSpell))
                    && filters.iter().any(|filter| matches!(
                        filter,
                        TargetFilter::Typed(tf)
                            if has_type(tf, TypeFilter::Artifact)
                                || has_type(tf, TypeFilter::Enchantment)
                    ))
        )));
    }

    #[test]
    fn parse_type_phrase_artifact_creature_or_enchantment() {
        // 3-way Or: "artifact, creature, or enchantment"
        let (filter, rest) = parse_type_phrase("artifact, creature, or enchantment");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(
                filters.len(),
                3,
                "expected 3 branches, got {}",
                filters.len()
            );
            // Verify each branch has the correct type
            let types: Vec<_> = filters
                .iter()
                .filter_map(|f| {
                    if let TargetFilter::Typed(tf) = f {
                        tf.get_primary_type()
                    } else {
                        None
                    }
                })
                .collect();
            assert!(types.contains(&&TypeFilter::Artifact));
            assert!(types.contains(&&TypeFilter::Creature));
            assert!(types.contains(&&TypeFilter::Enchantment));
        } else {
            panic!("Expected Or filter, got {filter:?}");
        }
    }

    /// CR 205.2a: "artifact creature" is the conjunction of two core card types,
    /// not a sole Artifact filter. Regression for Lux Artillery: "whenever you
    /// cast an artifact creature spell" previously dropped the Creature type.
    #[test]
    fn parse_type_phrase_artifact_creature_conjunction() {
        let (filter, rest) = parse_type_phrase("artifact creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(
            tf.type_filters.contains(&TypeFilter::Artifact),
            "expected Artifact in {:?}",
            tf.type_filters
        );
        assert!(
            tf.type_filters.contains(&TypeFilter::Creature),
            "expected Creature in {:?}",
            tf.type_filters
        );
    }

    /// CR 205.2a + CR 601.2: "artifact creature spell" — the trailing "spell"
    /// suffix is informational and should be stripped after the conjunction.
    #[test]
    fn parse_type_phrase_artifact_creature_spell() {
        let (filter, rest) = parse_type_phrase("artifact creature spell");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Artifact));
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
    }

    /// CR 205.2a + CR 205.4b: "noncreature artifact" — negation followed by a
    /// concrete core type. The Non(Creature) negation should land in
    /// type_filters alongside Artifact.
    #[test]
    fn parse_type_phrase_noncreature_artifact() {
        let (filter, rest) = parse_type_phrase("noncreature artifact");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Artifact));
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Non(Box::new(TypeFilter::Creature))),
            "expected Non(Creature) in {:?}",
            tf.type_filters
        );
    }

    /// CR 205.4a: "legendary creature" — legendary is a supertype, not a core
    /// type. Must remain a single-type filter with a HasSupertype property.
    #[test]
    fn parse_type_phrase_legendary_creature_keeps_supertype_prop() {
        let (filter, rest) = parse_type_phrase("legendary creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(
            tf.properties.iter().any(|prop| matches!(
                prop,
                FilterProp::HasSupertype {
                    value: Supertype::Legendary
                }
            )),
            "expected HasSupertype(Legendary) in {:?}",
            tf.properties
        );
    }

    /// Bounty Agent: "target legendary permanent that's an artifact, creature,
    /// or enchantment" must keep the Legendary restriction and distribute the
    /// relative card-type disjunction across three permanent-type legs.
    #[test]
    fn parse_target_legendary_permanent_with_relative_type_union() {
        let (filter, rest) =
            parse_target("target legendary permanent that's an artifact, creature, or enchantment");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = &filter else {
            panic!("Expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 3, "expected three type legs: {filters:?}");
        for ty in [
            TypeFilter::Artifact,
            TypeFilter::Creature,
            TypeFilter::Enchantment,
        ] {
            let Some(TargetFilter::Typed(tf)) = filters.iter().find(
                |filter| matches!(filter, TargetFilter::Typed(tf) if has_type(tf, ty.clone())),
            ) else {
                panic!("missing {ty:?} leg in {filters:?}");
            };
            assert!(has_type(tf, TypeFilter::Permanent));
            assert!(tf.properties.iter().any(|prop| matches!(
                prop,
                FilterProp::HasSupertype {
                    value: Supertype::Legendary
                }
            )));
        }
    }

    /// CR 205.2a: "artifact or creature" is an OR-union of the two core types,
    /// NOT a conjunction. The separator " or " breaks out of the conjunction
    /// loop and builds a TargetFilter::Or with two branches.
    #[test]
    fn parse_type_phrase_artifact_or_creature_stays_union() {
        let (filter, rest) = parse_type_phrase("artifact or creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = &filter else {
            panic!("Expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);
    }

    /// CR 205.2a + CR 110.1: "artifact creature you control" — conjunction
    /// followed by a controller suffix.
    #[test]
    fn parse_type_phrase_artifact_creature_you_control() {
        let (filter, rest) = parse_type_phrase("artifact creature you control");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Artifact));
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert_eq!(tf.controller, Some(ControllerRef::You));
    }

    /// CR 102.1 + CR 103.1: "the player to your right/left" parses to a
    /// seating-relative `Neighbor` filter. Right = previous seat (clockwise
    /// turn order proceeds to the left).
    #[test]
    fn parse_target_player_to_your_right_is_neighbor_right() {
        let (f, rest) = parse_target("the player to your right");
        assert_eq!(
            f,
            TargetFilter::Neighbor {
                direction: SeatDirection::Right
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_player_to_your_left_is_neighbor_left() {
        let (f, rest) = parse_target("the player to your left");
        assert_eq!(
            f,
            TargetFilter::Neighbor {
                direction: SeatDirection::Left
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_bare_possessive_graveyard() {
        // CR 110.1/108.3/109.5: bare "their graveyard" scopes by owner to the
        // iterated player (ScopedPlayer), not by controller to the caster.
        let (f, rest) = parse_target("their graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter {
                controller: None,
                properties: vec![
                    FilterProp::Owned {
                        controller: ControllerRef::ScopedPlayer,
                    },
                    FilterProp::InZone {
                        zone: Zone::Graveyard
                    }
                ],
                ..Default::default()
            })
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_their_graveyard_scopes_to_owner() {
        // "from their graveyard" routes through parse_zone_suffix_nom; the
        // possessive owner must survive as Owned{ScopedPlayer}.
        let (f, _) = parse_target("a creature card from their graveyard");
        let tf = typed_leg(&f).expect("typed filter");
        assert_eq!(tf.controller, None);
        assert!(has_prop(
            tf,
            FilterProp::Owned {
                controller: ControllerRef::ScopedPlayer,
            }
        ));
        assert!(has_prop(
            tf,
            FilterProp::InZone {
                zone: Zone::Graveyard,
            }
        ));
    }

    #[test]
    fn parse_target_bare_their_graveyard_scopes_to_owner() {
        // Part B bare-possessive path: bare "their graveyard" must match the
        // owner-scoped shape produced by parse_zone_suffix_nom's ZoneQual::Their.
        let (f, _) = parse_target("their graveyard");
        let tf = typed_leg(&f).expect("typed filter");
        assert_eq!(tf.controller, None);
        assert!(has_prop(
            tf,
            FilterProp::Owned {
                controller: ControllerRef::ScopedPlayer,
            }
        ));
        assert!(has_prop(
            tf,
            FilterProp::InZone {
                zone: Zone::Graveyard,
            }
        ));
    }

    #[test]
    fn parse_target_that_players_graveyard_unchanged() {
        // The OtherPoss split must not regress non-"their" possessives:
        // "that player's graveyard" emits InZone with no Owned prop.
        let (f, _) = parse_target("a card from that player's graveyard");
        let tf = typed_leg(&f).expect("typed filter");
        assert_eq!(tf.controller, None);
        assert!(has_prop(
            tf,
            FilterProp::InZone {
                zone: Zone::Graveyard,
            }
        ));
        assert!(!tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::Owned { .. })));
    }

    #[test]
    fn parse_target_bare_possessive_library() {
        let (f, rest) = parse_target("your library");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::InZone {
                    zone: Zone::Library
                }],
                ..Default::default()
            })
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_opponents_graveyard() {
        let (filter, rest) = parse_target("opponent's graveyard");
        assert_eq!(
            filter,
            TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::Owned {
                    controller: ControllerRef::Opponent,
                },
                FilterProp::InZone {
                    zone: Zone::Graveyard,
                },
            ]))
        );
        assert_eq!(rest, "");
    }

    /// Regression: the "an" possessive form must agree with the no-"an" sibling
    /// above. Before the graveyard branch was ordered ahead of the opponent-
    /// player references, the un-bounded `tag("an opponent")` arm matched the
    /// "an opponent" prefix of "an opponent's graveyard" and returned a bare
    /// Opponent-player filter, leaving "'s graveyard" as an unconsumed remainder.
    #[test]
    fn parse_target_an_opponents_graveyard_is_graveyard_filter() {
        let (filter, rest) = parse_target("an opponent's graveyard");
        assert_eq!(
            filter,
            TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::Owned {
                    controller: ControllerRef::Opponent,
                },
                FilterProp::InZone {
                    zone: Zone::Graveyard,
                },
            ]))
        );
        assert_eq!(rest, "");
    }

    /// Guard: reordering the graveyard branch above the opponent-player arm must
    /// not disturb the bare "an opponent" player reference (Zaffai — "an opponent
    /// chosen at random"), which contains no "graveyard" token.
    #[test]
    fn parse_target_bare_an_opponent_still_player() {
        let (filter, _rest) = parse_target("an opponent chosen at random");
        assert_eq!(
            filter,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        );
    }

    #[test]
    fn target_card_from_an_opponents_graveyard() {
        // Lord Skitter, Sewer King: "exile up to one target card from an opponent's graveyard"
        // Uses Owned{Opponent} (checks obj.owner) so stolen creatures that died and went to
        // their owner's graveyard are correctly included per CR 404.2.
        let (filter, rest) = parse_target("target card from an opponent's graveyard");
        assert_eq!(
            filter,
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Card],
                controller: None,
                properties: vec![
                    FilterProp::Owned {
                        controller: ControllerRef::Opponent,
                    },
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ],
            })
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn scan_zone_phrase_finds_trailing_zone_after_subject() {
        // "this card is in your graveyard" — scanner must skip "this card is" and
        // find the zone phrase at a later word boundary.
        let (zone, ctrl, _props) = scan_zone_phrase("this card is in your graveyard").unwrap();
        assert_eq!(zone, Zone::Graveyard);
        assert_eq!(ctrl, Some(ControllerRef::You));
    }

    #[test]
    fn scan_zone_phrase_finds_exile_and_hand() {
        // Delegation from oracle_condition now picks up non-graveyard zones, which
        // SourceInZone supports uniformly — lock in that behavior.
        assert_eq!(
            scan_zone_phrase("~ in exile").map(|(z, _, _)| z),
            Some(Zone::Exile)
        );
        assert_eq!(
            scan_zone_phrase("this card from your hand").map(|(z, _, _)| z),
            Some(Zone::Hand)
        );
    }

    #[test]
    fn scan_zone_phrase_returns_none_without_zone() {
        assert!(scan_zone_phrase("this creature is attacking").is_none());
        assert!(scan_zone_phrase("you control a legendary creature").is_none());
        // Word-boundary safety: "graveyardkeeper" must not match as "graveyard".
        assert!(scan_zone_phrase("from your graveyardkeeper").is_none());
    }

    #[test]
    fn target_card_from_each_opponents_graveyard() {
        // Regression: "each opponent's" is in POSSESSIVES, so without the dedicated
        // opponent branch it would fall through to the generic possessive arm with
        // no ownership constraint. Mirrors the "an opponent's" case per CR 404.2.
        let (filter, rest) = parse_target("target card from each opponent's graveyard");
        assert_eq!(
            filter,
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Card],
                controller: None,
                properties: vec![
                    FilterProp::Owned {
                        controller: ControllerRef::Opponent,
                    },
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ],
            })
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_the_creatures_controller() {
        let (filter, rest) = parse_target("the creature's controller");
        assert_eq!(filter, TargetFilter::ParentTargetController);
        assert_eq!(rest, "");
    }

    /// CR 108.3 + CR 110.2: ownership and control are distinct. "You control
    /// but don't own" must match permanents controlled by you while excluding
    /// objects you own, so stolen objects count and native objects do not.
    #[test]
    fn parse_type_phrase_you_control_but_dont_own_composes_not_owned() {
        let (filter, rest) = parse_type_phrase("land you control but don't own");
        assert_eq!(rest, "");
        match filter {
            TargetFilter::And { filters } => {
                assert!(matches!(
                    filters.first(),
                    Some(TargetFilter::Typed(TypedFilter {
                        type_filters,
                        controller: Some(ControllerRef::You),
                        ..
                    })) if type_filters == &vec![TypeFilter::Land]
                ));
                assert!(matches!(
                    filters.get(1),
                    Some(TargetFilter::Not { filter }) if matches!(
                        filter.as_ref(),
                        TargetFilter::Typed(TypedFilter {
                            properties,
                            ..
                        }) if properties == &vec![FilterProp::Owned {
                            controller: ControllerRef::You
                        }]
                    )
                ));
            }
            other => panic!("expected And filter, got {other:?}"),
        }
    }

    #[test]
    fn parse_type_phrase_opponent_controls_but_doesnt_own_composes_not_owned() {
        let (filter, rest) = parse_type_phrase("creature an opponent controls but doesn't own");
        assert_eq!(rest, "");
        match filter {
            TargetFilter::And { filters } => {
                assert!(matches!(
                    filters.first(),
                    Some(TargetFilter::Typed(TypedFilter {
                        type_filters,
                        controller: Some(ControllerRef::Opponent),
                        ..
                    })) if type_filters == &vec![TypeFilter::Creature]
                ));
                assert!(matches!(
                    filters.get(1),
                    Some(TargetFilter::Not { filter }) if matches!(
                        filter.as_ref(),
                        TargetFilter::Typed(TypedFilter {
                            properties,
                            ..
                        }) if properties == &vec![FilterProp::Owned {
                            controller: ControllerRef::Opponent
                        }]
                    )
                ));
            }
            other => panic!("expected And filter, got {other:?}"),
        }
    }

    /// CR 108.3 + CR 109.4: bare "permanents you don't own" — the new
    /// negated-ownership suffix in `parse_ownership_or_controller_suffix`. With
    /// no controller and no "but" lead it pushes `Owned { Opponent }` directly
    /// onto a single `Typed` filter (runtime: owner != controller, i.e. "you
    /// don't own it"). Distinct from the "but don't own" `And[Typed, Not(..)]`
    /// shape below, which is left UNCHANGED — proving the bare arm is additive.
    /// (Agent of Treachery #3304.)
    #[test]
    fn parse_type_phrase_permanents_you_dont_own_pushes_owned_opponent() {
        for text in ["permanents you don't own", "permanents you do not own"] {
            let (filter, rest) = parse_type_phrase(text);
            assert_eq!(rest, "", "fully consumed for {text:?}");
            let TargetFilter::Typed(tf) = filter else {
                panic!("expected single Typed filter for {text:?}, got {filter:?}");
            };
            assert!(
                tf.properties.contains(&FilterProp::Owned {
                    controller: ControllerRef::Opponent
                }),
                "{text:?} must push Owned{{Opponent}}, got {:?}",
                tf.properties
            );
            // Ownership is independent of control: the bare suffix must NOT pin
            // a controller (CR 109.4).
            assert_eq!(
                tf.controller, None,
                "bare ownership suffix must not set controller for {text:?}"
            );
        }
    }

    /// No-regression guard: the "but don't own" path (controller already set)
    /// still yields the `And[Typed(You), Not(Owned{You})]` shape, UNCHANGED by
    /// the additive bare "you don't own" arm. (CR 108.3 + CR 109.4.)
    #[test]
    fn parse_type_phrase_but_dont_own_shape_unchanged_by_bare_arm() {
        let (filter, rest) = parse_type_phrase("creature you control but don't own");
        assert_eq!(rest, "");
        let TargetFilter::And { filters } = filter else {
            panic!("expected And filter, got {filter:?}");
        };
        assert!(matches!(
            filters.first(),
            Some(TargetFilter::Typed(TypedFilter {
                type_filters,
                controller: Some(ControllerRef::You),
                ..
            })) if type_filters == &vec![TypeFilter::Creature]
        ));
        assert!(matches!(
            filters.get(1),
            Some(TargetFilter::Not { filter }) if matches!(
                filter.as_ref(),
                TargetFilter::Typed(TypedFilter { properties, .. })
                    if properties == &vec![FilterProp::Owned { controller: ControllerRef::You }]
            )
        ));
    }

    /// CR 205.3 + CR 205.4b: "target attacking Vampire that isn't a Demon" — the
    /// subtype-negation relative clause must append `Non(Subtype("Demon"))` to
    /// the target's type filters so a Vampire Demon is rejected.
    #[test]
    fn parse_target_that_isnt_subtype_appends_negation() {
        let (filter, _) = parse_target("target attacking Vampire that isn't a Demon");
        match filter {
            TargetFilter::Typed(tf) => {
                assert!(
                    tf.type_filters
                        .contains(&TypeFilter::Subtype("Vampire".into())),
                    "expected Vampire subtype in type_filters, got {:?}",
                    tf.type_filters
                );
                assert!(
                    tf.type_filters
                        .contains(&TypeFilter::Non(Box::new(TypeFilter::Subtype(
                            "Demon".into()
                        )))),
                    "expected Non(Subtype(Demon)) in type_filters, got {:?}",
                    tf.type_filters
                );
                assert!(
                    tf.properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::Attacking { defender: None })),
                    "expected Attacking property, got {:?}",
                    tf.properties
                );
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    /// CR 205.3: "that's not a <Subtype>" — contraction form.
    #[test]
    fn parse_target_thats_not_subtype_appends_negation() {
        let (filter, _) = parse_target("target Vampire that's not a Demon");
        match filter {
            TargetFilter::Typed(tf) => assert!(
                tf.type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Subtype(
                        "Demon".into()
                    )))),
                "expected Non(Subtype(Demon)) in type_filters, got {:?}",
                tf.type_filters
            ),
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    /// CR 205.3: "that is not <Subtype>" — unabridged variant without article.
    #[test]
    fn parse_target_that_is_not_subtype_appends_negation() {
        let (filter, _) = parse_target("target creature that is not Human");
        match filter {
            TargetFilter::Typed(tf) => assert!(
                tf.type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Subtype(
                        "Human".into()
                    )))),
                "expected Non(Subtype(Human)) in type_filters, got {:?}",
                tf.type_filters
            ),
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    /// CR 202.3 + CR 608.2h: the superlative "with the greatest mana value
    /// among <set>" suffix must emit a `FilterProp::Cmc { EQ, Aggregate { Max,
    /// ManaValue, <eligible set> } }`, not be silently dropped (issue #463).
    #[test]
    fn superlative_mana_value_suffix_emits_aggregate_cmc() {
        let mut ctx = ParseContext::default();
        let input = "with the greatest mana value among creatures and planeswalkers they control";
        let (prop, consumed) =
            parse_mana_value_suffix(input, &mut ctx).expect("superlative suffix should parse");
        assert_eq!(consumed, input.len(), "should consume the whole suffix");
        let FilterProp::Cmc { comparator, value } = prop else {
            panic!("expected FilterProp::Cmc, got {prop:?}");
        };
        assert_eq!(comparator, Comparator::EQ);
        let QuantityExpr::Ref {
            qty:
                QuantityRef::Aggregate {
                    function,
                    property,
                    filter,
                },
        } = value
        else {
            panic!("expected QuantityRef::Aggregate, got {value:?}");
        };
        assert_eq!(function, AggregateFunction::Max);
        assert_eq!(property, ObjectProperty::ManaValue);
        // The eligible set is an Or of Creature/Planeswalker, controller You.
        match filter {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 2);
                for leg in &filters {
                    let tf = typed_leg(leg).expect("each leg is Typed");
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                }
                assert!(filters
                    .iter()
                    .any(|f| typed_leg(f).is_some_and(|tf| has_type(tf, TypeFilter::Creature))));
                assert!(
                    filters
                        .iter()
                        .any(|f| typed_leg(f)
                            .is_some_and(|tf| has_type(tf, TypeFilter::Planeswalker)))
                );
            }
            other => panic!("expected Or eligible set, got {other:?}"),
        }
    }

    #[test]
    fn superlative_power_suffix_emits_aggregate_pt_comparison() {
        let mut ctx = ParseContext::default();
        let input = "with the greatest power among creatures they control";
        let (prop, consumed) =
            parse_power_suffix(input, &mut ctx).expect("superlative suffix should parse");
        assert_eq!(consumed, input.len(), "should consume the whole suffix");
        let FilterProp::PtComparison {
            stat,
            scope,
            comparator,
            value,
        } = prop
        else {
            panic!("expected FilterProp::PtComparison, got {prop:?}");
        };
        assert_eq!(stat, PtStat::Power);
        assert_eq!(scope, PtValueScope::Current);
        assert_eq!(comparator, Comparator::EQ);
        let QuantityExpr::Ref {
            qty:
                QuantityRef::Aggregate {
                    function,
                    property,
                    filter,
                },
        } = value
        else {
            panic!("expected QuantityRef::Aggregate, got {value:?}");
        };
        assert_eq!(function, AggregateFunction::Max);
        assert_eq!(property, ObjectProperty::Power);
        let tf = typed_leg(&filter).expect("eligible set should be Typed");
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(has_type(tf, TypeFilter::Creature));
    }

    /// Issue #463: Soul Shatter's full target phrase must carry the superlative
    /// `FilterProp::Cmc` on **both** Or legs (Creature and Planeswalker).
    #[test]
    fn soul_shatter_target_carries_superlative_on_both_legs() {
        let mut ctx = ParseContext::default();
        let (filter, _rest) = parse_target_with_ctx(
            "a creature or planeswalker with the greatest mana value among creatures and \
             planeswalkers they control",
            &mut ctx,
        );
        let TargetFilter::Or { filters } = &filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);
        for leg in filters {
            let tf = typed_leg(leg).expect("each leg is Typed");
            let has_superlative = tf.properties.iter().any(|p| {
                matches!(
                    p,
                    FilterProp::Cmc {
                        comparator: Comparator::EQ,
                        value: QuantityExpr::Ref {
                            qty: QuantityRef::Aggregate {
                                function: AggregateFunction::Max,
                                property: ObjectProperty::ManaValue,
                                ..
                            },
                        },
                    }
                )
            });
            assert!(
                has_superlative,
                "leg {tf:?} missing superlative Cmc/Aggregate prop"
            );
        }
    }

    /// Issue #2016: "a permanent named Bonder's Ornament draws a card" must
    /// terminate the card name at the verb "draws" so the remainder carries
    /// the verb phrase. Without the verb-boundary scan, the name swallows
    /// "draws a card" and the remainder is empty.
    #[test]
    fn named_card_terminates_at_verb_boundary() {
        let (filter, rest) = parse_type_phrase("a permanent named Bonder's Ornament draws a card");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::Named { name } if name == "Bonder's Ornament"
            )),
            "expected Named prop with 'Bonder's Ornament', got {tf:?}"
        );
        assert_eq!(rest.trim(), "draws a card");
    }

    /// Ensure the verb-boundary scan does not fire on card names that happen
    /// to contain verb-like substrings when followed by a comma delimiter.
    #[test]
    fn named_card_with_comma_delimiter_still_works() {
        let (filter, rest) = parse_type_phrase("a creature named Falkenrath Gorger, it gains");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::Named { name } if name == "Falkenrath Gorger"
            )),
            "expected Named prop with 'Falkenrath Gorger', got {tf:?}"
        );
        assert_eq!(rest.trim_start_matches([',', ' ']), "it gains");
    }

    #[test]
    fn parse_non_saga_token_you_control_issue_3294() {
        use crate::types::ability::{ControllerRef, FilterProp, TypeFilter};

        let (filter, rest) = parse_type_phrase("non-saga token you control");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Non(Box::new(TypeFilter::Subtype(
                    "Saga".to_string()
                )))),
            "expected Non(Saga), got {:?}",
            tf.type_filters
        );
        assert!(tf.properties.contains(&FilterProp::Token));
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(rest.is_empty(), "expected empty remainder, got {rest:?}");

        let (filter2, rest2) = parse_target("a non-Saga token you control");
        let TargetFilter::Typed(tf2) = filter2 else {
            panic!("parse_target must not collapse to Any, got {filter2:?}");
        };
        assert!(tf2.properties.contains(&FilterProp::Token));
        assert!(rest2.is_empty(), "expected empty remainder, got {rest2:?}");
    }

    #[test]
    fn parse_non_lesson_instant_and_sorcery_distributes_negation() {
        let (filter, rest) =
            parse_type_phrase("non-Lesson instant and sorcery card in your graveyard");
        assert!(rest.trim().is_empty(), "unexpected remainder: {rest:?}");
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);
        for branch in filters {
            let TargetFilter::Typed(tf) = branch else {
                panic!("expected typed branch");
            };
            assert!(
                tf.type_filters.iter().any(|f| matches!(
                    f,
                    TypeFilter::Non(boxed) if matches!(**boxed, TypeFilter::Subtype(ref s) if s == "Lesson")
                )),
                "each branch must exclude Lesson: {:?}",
                tf.type_filters
            );
        }
    }

    #[test]
    fn parse_artifact_or_noncreature_permanent_keeps_negation_on_second_branch() {
        let (filter, rest) = parse_type_phrase("artifact or noncreature permanent");
        assert!(rest.trim().is_empty(), "unexpected remainder: {rest:?}");
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };

        let has_artifact = |filter: &TargetFilter| {
            let TargetFilter::Typed(tf) = filter else {
                return false;
            };
            tf.type_filters.contains(&TypeFilter::Artifact)
        };
        let has_noncreature = |filter: &TargetFilter| {
            let TargetFilter::Typed(tf) = filter else {
                return false;
            };
            tf.type_filters
                .contains(&TypeFilter::Non(Box::new(TypeFilter::Creature)))
        };

        let artifact_branch = filters
            .iter()
            .find(|branch| has_artifact(branch))
            .expect("artifact branch");
        assert!(
            !has_noncreature(artifact_branch),
            "noncreature must not distribute back onto artifact branch: {artifact_branch:?}"
        );
        assert!(
            filters.iter().any(has_noncreature),
            "expected a noncreature branch in {filters:?}"
        );
    }

    /// CR 122.1 + CR 122.6: the "that [actor] put [count] [type] counters on this
    /// turn" relative clause lowers to `FilterProp::CountersPutOnThisTurn` with
    /// the right actor/counter/comparator/count axes — across "you've put", the
    /// "an opponent has put" actor scope, the "N or more" / "a" count forms, and
    /// the bare untyped "counters" form.
    #[test]
    fn counters_put_this_turn_clause_kid_loki_form() {
        // Kid Loki: "that you've put one or more +1/+1 counters on this turn".
        let (prop, _) = parse_counters_put_this_turn_clause(
            "you've put one or more +1/+1 counters on this turn",
        )
        .expect("clause parses");
        assert_eq!(
            prop,
            FilterProp::CountersPutOnThisTurn {
                actor: CountScope::Controller,
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                comparator: Comparator::GE,
                count: 1,
            }
        );
    }

    #[test]
    fn counters_put_this_turn_clause_opponent_and_numeric_count() {
        // Opponent actor scope + explicit "two or more" threshold.
        let (prop, _) = parse_counters_put_this_turn_clause(
            "an opponent has put two or more +1/+1 counters on this turn",
        )
        .expect("clause parses");
        assert_eq!(
            prop,
            FilterProp::CountersPutOnThisTurn {
                actor: CountScope::Opponents,
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                comparator: Comparator::GE,
                count: 2,
            }
        );
    }

    #[test]
    fn counters_put_this_turn_clause_bare_untyped_singular() {
        // Bare untyped "a counter ... on this turn" → CounterMatch::Any, GE 1.
        let (prop, _) = parse_counters_put_this_turn_clause("you've put a counter on this turn")
            .expect("clause parses");
        assert_eq!(
            prop,
            FilterProp::CountersPutOnThisTurn {
                actor: CountScope::Controller,
                counters: CounterMatch::Any,
                comparator: Comparator::GE,
                count: 1,
            }
        );
    }

    #[test]
    fn counters_put_this_turn_clause_rejects_non_matching() {
        // No actor/verb → not this clause.
        assert!(parse_counters_put_this_turn_clause("attacked this turn").is_none());
        // Missing the "on this turn" terminator → not this clause.
        assert!(parse_counters_put_this_turn_clause("you've put a +1/+1 counter on it").is_none());
    }

    /// CR 508.1b (Oviya, Automech Artisan): the relative-clause attacking suffix
    /// "that's attacking one of your opponents" must fully consume and emit
    /// `Attacking { defender: Some(Opponent) }`.
    #[test]
    fn that_s_attacking_one_of_your_opponents_suffix() {
        let (filter, rest) = parse_target("each creature that's attacking one of your opponents");
        assert!(
            rest.trim().is_empty(),
            "suffix must be fully consumed: {rest:?}"
        );
        let tf = typed_leg(&filter).expect("typed filter");
        assert!(
            tf.properties.contains(&FilterProp::Attacking {
                defender: Some(ControllerRef::Opponent),
            }),
            "must carry Attacking{{defender: Opponent}}, got {:?}",
            tf.properties
        );
    }

    /// CR 205.3m + CR 608.2c (Selfless Safewright): the anaphor suffix "of that
    /// type" must be recognized identically to "of the chosen type" and emit
    /// `IsChosenCreatureType` for a non-card-typed base.
    #[test]
    fn of_that_type_anaphor_suffix_equals_of_the_chosen_type() {
        let (filter, rest) = parse_target("other permanents you control of that type");
        assert!(
            rest.trim().is_empty(),
            "suffix must be fully consumed: {rest:?}"
        );
        let tf = typed_leg(&filter).expect("typed filter");
        assert!(
            tf.properties.contains(&FilterProp::IsChosenCreatureType),
            "must carry IsChosenCreatureType, got {:?}",
            tf.properties
        );
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf.properties.contains(&FilterProp::Another));
    }

    /// CR 205.3h: `parse_type_phrase` parses "a Plan" — "Plan" is an enchantment
    /// subtype (Marvel's Spider-Man) — to `Typed{[Subtype("Plan")]}`, fully
    /// consumed. The elided-verb "or" disjunction ("you control an artifact
    /// creature or a Plan") is assembled one level up in `parse_you_control_a`,
    /// so `parse_type_phrase` itself stops at the first segment and leaves the
    /// connector as remainder (asserted below).
    #[test]
    fn parse_type_phrase_recognizes_plan() {
        let (f, rest) = parse_type_phrase("a Plan");
        assert!(rest.trim().is_empty(), "remainder must be empty: {rest:?}");
        let TargetFilter::Typed(tf) = f else {
            panic!("expected single Typed filter, got {f:?}");
        };
        assert_eq!(
            tf.type_filters,
            vec![TypeFilter::Subtype("Plan".to_string())]
        );
    }

    /// `parse_type_phrase` does NOT swallow an article-led "or" RHS — it stops at
    /// the first segment and leaves " or a Plan" as remainder. This is the
    /// load-bearing precondition for the `parse_you_control_a` elided-verb loop:
    /// the connector must survive so the condition layer can fold the disjuncts.
    #[test]
    fn parse_type_phrase_leaves_article_led_or_rhs_as_remainder() {
        let (f, rest) = parse_type_phrase("an artifact creature or a Plan");
        assert_eq!(rest, " or a Plan", "article-led or RHS must remain");
        let TargetFilter::Typed(tf) = f else {
            panic!("expected single Typed filter, got {f:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Artifact));
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
    }

    /// Regression: a single article-led conjunction with no connector still
    /// parses to a single Typed filter (not an Or).
    #[test]
    fn single_artifact_creature_still_typed_not_or() {
        let (f, rest) = parse_type_phrase("an artifact creature");
        assert!(rest.trim().is_empty(), "remainder must be empty: {rest:?}");
        let TargetFilter::Typed(tf) = f else {
            panic!("expected single Typed filter, got {f:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Artifact));
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
    }

    /// Regression: a bare (no-article) connector RHS still parses to an Or via
    /// the existing non-comma separator branch (unchanged by this work).
    #[test]
    fn bare_connector_rhs_still_or() {
        let (f, rest) = parse_type_phrase("artifact creature or enchantment");
        assert!(rest.trim().is_empty(), "remainder must be empty: {rest:?}");
        assert!(
            matches!(f, TargetFilter::Or { .. }),
            "expected Or filter, got {f:?}"
        );
    }
}
