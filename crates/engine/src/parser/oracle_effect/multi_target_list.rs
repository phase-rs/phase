//! One instruction naming several independent targets.
//!
//! "Destroy up to one target artifact, up to one target creature, and up to one
//! target land." / "Return up to one target A card and up to one target B card
//! from your graveyard to your hand." Each conjunct is its own announced target
//! slot, so the instruction is lowered as one clause per slot: the first stays
//! on the primary effect and the rest chain as `sub_ability` links, each with
//! its own filter and `multi_target` cardinality — the same representation the
//! bare-`and` splitter (`try_split_targeted_compound`) produces.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::{one_of, space0, space1};
use nom::combinator::{fail, opt, peek, value};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use crate::parser::oracle_ir::ast::ParsedEffectClause;
use crate::parser::oracle_nom::error::{OracleError, OracleResult};
use crate::parser::oracle_nom::primitives as nom_primitives;
use crate::parser::oracle_target::{parse_target_with_ctx, parse_zone_suffix};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, MultiTargetSpec, TargetFilter,
};

use super::{parse_effect_clause_inner, ParseContext};

/// The start of a target-led conjunct: "up to one target …" or "target …".
fn slot_start(i: &str) -> OracleResult<'_, ()> {
    value((), alt((tag("up to one target "), tag("target ")))).parse(i)
}

/// The list connector before the next slot: ", and/or ", ", and ", ", ",
/// "and/or " or "and ". A target parser may or may not have consumed the space
/// (or the comma) before the connector, so leading whitespace and the connector
/// itself are optional; what must follow is a further target-led conjunct.
fn slot_separator(i: &str) -> OracleResult<'_, ()> {
    value(
        (),
        preceded(
            space0,
            (
                opt(alt((
                    tag(", and/or "),
                    tag(", and "),
                    tag(", "),
                    tag("and/or "),
                    tag("and "),
                ))),
                peek(slot_start),
            ),
        ),
    )
    .parse(i)
}

/// The "card(s)" noun the target parser leaves behind when a type phrase such
/// as "instant card" is followed by a list comma ("… instant card, up to one …").
fn leftover_card_noun(rem: &str) -> &str {
    let lower = rem.to_ascii_lowercase();
    let mut noun = preceded(
        space1::<_, OracleError<'_>>,
        terminated(alt((tag("cards"), tag("card"))), peek(one_of(",. "))),
    );
    match noun.parse(lower.as_str()) {
        Ok((after, _)) => &rem[rem.len() - after.len()..],
        Err(_) => rem,
    }
}

/// A trailing conjunction ("... and it gains flying") that is not another target.
fn starts_conjunction(i: &str) -> OracleResult<'_, ()> {
    value((), alt((tag(","), tag("and "), tag("and/or "), tag("or ")))).parse(i)
}

/// A zone phrase ("from your graveyard") running to the end of the slot text.
fn trailing_zone_phrase(i: &str) -> OracleResult<'_, ()> {
    tag("from ").parse(i)?;
    match parse_zone_suffix(i) {
        // allow-noncombinator: structural end-of-text check after the zone combinator consumed its phrase.
        Some((_, _, consumed)) if i[consumed..].trim_matches(['.', ' ']).is_empty() => Ok(("", ())),
        _ => fail().parse(i),
    }
}

/// Joins two clause fragments with a single space unless `b` is punctuation.
fn join(a: &str, b: &str) -> String {
    let (a, b) = (a.trim_end(), b.trim());
    if b.is_empty() || tag::<_, _, OracleError<'_>>(".").parse(b).is_ok() {
        format!("{a}{b}")
    } else {
        format!("{a} {b}")
    }
}

/// CR 601.2c + CR 608.2c: Split "<verb> <slot>, <slot>, and <slot> <shared tail>"
/// into one clause per target slot, each carrying the shared verb head and the
/// shared tail (destination / zone phrase). Returns `None` unless the text has
/// two or more target-led conjuncts and every rebuilt clause parses cleanly
/// with no rider of its own, so a shape this splitter does not understand keeps
/// falling through to the single-clause path.
pub(super) fn try_split_multi_target_list(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    let lower = text.to_ascii_lowercase();
    let (head_lower, _) = nom_primitives::scan_split_at_phrase(&lower, slot_start)?;
    // "Choose target A and target B" declares its slots through its own
    // dedicated parser (`try_parse_two_targets`).
    if head_lower.is_empty()
        || tag::<_, _, OracleError<'_>>("choose ")
            .parse(head_lower)
            .is_ok()
    {
        return None;
    }
    let head = &text[..head_lower.len()];

    // Probe pass on a throwaway context: collect each slot's source text.
    let mut probe_ctx = ctx.clone_throwaway();
    let mut slots: Vec<&str> = Vec::new();
    let mut cursor = &text[head.len()..];
    let tail = loop {
        let (filter, rem) = parse_target_with_ctx(cursor, &mut probe_ctx);
        if matches!(filter, TargetFilter::Any) {
            return None;
        }
        let rem = leftover_card_noun(rem);
        // allow-noncombinator: punctuation trim of a slot span already delimited by parse_target.
        slots.push(cursor[..cursor.len() - rem.len()].trim_end_matches([' ', ',']));
        let rem_lower = rem.to_ascii_lowercase();
        match slot_separator(&rem_lower) {
            Ok((after, ())) => cursor = &rem[rem.len() - after.len()..],
            Err(_) => break rem,
        }
    };
    if slots.len() < 2 {
        return None;
    }
    if starts_conjunction(&tail.trim_start().to_ascii_lowercase()).is_ok() {
        return None;
    }

    // The final slot's own zone phrase ("from your graveyard") scopes every slot.
    let last = slots.last()?;
    let last_lower = last.to_ascii_lowercase();
    let zone_phrase = nom_primitives::scan_split_at_phrase(&last_lower, trailing_zone_phrase)
        .map(|(_, zone)| &last[last.len() - zone.len()..]);

    let last_index = slots.len() - 1;
    let bodies: Vec<String> = slots
        .iter()
        .enumerate()
        .map(|(index, slot)| {
            let slot_has_zone = nom_primitives::scan_split_at_phrase(
                &slot.to_ascii_lowercase(),
                trailing_zone_phrase,
            )
            .is_some();
            let mut body = join(head, slot);
            if index != last_index && !slot_has_zone {
                if let Some(zone) = zone_phrase {
                    body = join(&body, zone);
                }
            }
            join(&body, tail)
        })
        .collect();

    // CR 601.2c: each later slot's announcer override travels with its own
    // link. Links parse on throwaway contexts so a rejected split leaves the
    // enclosing context untouched for the single-clause fallback.
    let mut links = Vec::with_capacity(last_index);
    for body in &bodies[1..] {
        let mut link_ctx = ctx.clone_throwaway();
        link_ctx.target_chooser = None;
        let clause = parse_effect_clause_inner(body, &mut link_ctx);
        if matches!(clause.effect, Effect::Unimplemented { .. })
            || clause.sub_ability.is_some()
            || clause.duration.is_some()
            || clause.distribute.is_some()
            || clause.condition.is_some()
            || clause.optional
            || clause.unless_pay.is_some()
            || clause.unlowered_guard.is_some()
        {
            return None;
        }
        // The probe only proves the slot phrase parses as a target; an effect
        // that lowers it to the untyped `Any` filter (a cast clause that drops
        // the slot's type) would copy a broken slot into the chain.
        if clause.effect.target_filter() == Some(&TargetFilter::Any) {
            return None;
        }
        links.push((clause, link_ctx.target_chooser));
    }

    // The primary parses last, on a context inheriting the enclosing one; it
    // replaces the enclosing context only once the split is accepted, so its
    // clause snapshot keeps the announcer override and a rejected split leaves
    // the enclosing context untouched for the single-clause fallback.
    let mut tentative = ctx.clone();
    tentative.target_chooser = None;
    let mut primary = parse_effect_clause_inner(&bodies[0], &mut tentative);
    if matches!(primary.effect, Effect::Unimplemented { .. })
        || primary.sub_ability.is_some()
        || primary.effect.target_filter() == Some(&TargetFilter::Any)
    {
        return None;
    }
    *ctx = tentative;
    primary.sub_ability = links
        .into_iter()
        .rev()
        .fold(None, |chain, (clause, chooser)| {
            let mut def = AbilityDefinition::new(AbilityKind::Spell, clause.effect);
            def.multi_target = clause.multi_target;
            def.target_chooser = chooser;
            def.sub_ability = chain;
            Some(Box::new(def))
        });
    Some(primary)
}

/// True when `clause` is a multi-slot target list: a primary slot plus
/// `sub_ability` links that each carry their own `multi_target` cardinality —
/// the representation both this splitter and `try_split_targeted_compound`
/// produce. Evaluated on the clause as parsed, before later sentences chain on.
/// The head's cardinality lives on the chunk (`head_multi_target`) or, before
/// it is lifted there, on the clause itself. A "choose target A and target B"
/// declaration list (`Effect::TargetOnly` slots) is excluded: its following
/// instruction consumes every declared target at once ("Each of those creatures
/// deals damage … to the other"), so no per-link `ParentTarget` binding arises.
pub(super) fn is_multi_slot_list(
    head_multi_target: Option<&MultiTargetSpec>,
    clause: &ParsedEffectClause,
) -> bool {
    fn every_link_is_a_slot(link: &AbilityDefinition) -> bool {
        link.multi_target.is_some() && link.sub_ability.as_deref().is_none_or(every_link_is_a_slot)
    }
    !matches!(clause.effect, Effect::TargetOnly { .. })
        && (head_multi_target.is_some() || clause.multi_target.is_some())
        && clause
            .sub_ability
            .as_deref()
            .is_some_and(every_link_is_a_slot)
}

/// CR 608.2d: a "you may" printed before the list is one choice over the whole
/// instruction. Per-link `optional` would let a player accept one slot and
/// decline another, and the head alone cannot gate its links, so an optional
/// list has no faithful lowering and fails closed.
pub(super) fn fail_closed_optional_list(clause: &mut ParsedEffectClause, text: &str) {
    clause.effect = Effect::unimplemented("multi_slot_list_optional", text);
    clause.sub_ability = None;
}
