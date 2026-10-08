//! CR 102.2 + CR 608.2c + CR 608.2d: the reader rule for a per-opponent
//! battlefield choice ("For each opponent, choose an artifact or land that
//! player controls. Destroy the chosen permanents." — Ultimate Magic: Meteor).
//!
//! The choice publishes the chosen permanents as the chain's tracked set and
//! writes no targets. Exactly one follow-up is supported: the chain's final
//! clause, immediately after the choice, destroying the whole chosen set. Every
//! other clause after the choice is an honest gap. The rule is positional on
//! purpose: it never has to recognize which later clause "reads" the chosen
//! objects, so no reader shape can slip past it.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::{alphanumeric1, space0};
use nom::combinator::{all_consuming, not, opt, value};
use nom::sequence::terminated;
use nom::Parser;

use crate::parser::oracle_ir::effect_chain::{ClauseDisposition, EffectChainIr};
use crate::parser::oracle_nom::error::OracleError;
use crate::types::ability::{Effect, PerPlayerScope, TargetFilter, TypeFilter, ZoneOwner};
use crate::types::identifiers::TrackedSetId;
use crate::types::zones::Zone;

/// The type a chosen-set reference names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChosenSetNoun {
    Permanent,
    Creature,
    Artifact,
    Land,
    Enchantment,
    Planeswalker,
    Card,
}

/// How a reference quantifies the chosen set. Only `Whole` can name every
/// permanent chosen for every opponent; a singular definite ("the chosen
/// artifact") names one object, which a per-opponent choice of several does not
/// supply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChosenSetQuantity {
    Whole,
    SingularDefinite,
}

type E<'a> = OracleError<'a>;

fn plural_noun(input: &str) -> nom::IResult<&str, ChosenSetNoun, E<'_>> {
    terminated(
        alt((
            value(ChosenSetNoun::Permanent, tag("permanents")),
            value(ChosenSetNoun::Creature, tag("creatures")),
            value(ChosenSetNoun::Artifact, tag("artifacts")),
            value(ChosenSetNoun::Land, tag("lands")),
            value(ChosenSetNoun::Enchantment, tag("enchantments")),
            value(ChosenSetNoun::Planeswalker, tag("planeswalkers")),
            value(ChosenSetNoun::Card, tag("cards")),
        )),
        not(alphanumeric1),
    )
    .parse(input)
}

fn singular_noun(input: &str) -> nom::IResult<&str, ChosenSetNoun, E<'_>> {
    terminated(
        alt((
            value(ChosenSetNoun::Permanent, tag("permanent")),
            value(ChosenSetNoun::Creature, tag("creature")),
            value(ChosenSetNoun::Artifact, tag("artifact")),
            value(ChosenSetNoun::Land, tag("land")),
            value(ChosenSetNoun::Enchantment, tag("enchantment")),
            value(ChosenSetNoun::Planeswalker, tag("planeswalker")),
            value(ChosenSetNoun::Card, tag("card")),
        )),
        not(alphanumeric1),
    )
    .parse(input)
}

/// A reference to the chosen set, with its grammatical quantity:
/// - whole (plural): "the chosen X", "those X", "each of the chosen X",
///   "each of those X", "[the] X chosen this way";
/// - whole (distributive singular): "each X chosen this way" (Druid of
///   Purification's wording);
/// - singular definite: "the chosen X", "that X", "the X chosen this way".
fn chosen_set_reference(
    input: &str,
) -> nom::IResult<&str, (ChosenSetQuantity, ChosenSetNoun), E<'_>> {
    let chosen_this_way = |i| tag::<_, _, E>(" chosen this way").parse(i);
    alt((
        // Plural heads.
        (
            value(
                ChosenSetQuantity::Whole,
                alt((
                    tag("the chosen "),
                    tag("those "),
                    tag("each of the chosen "),
                    tag("each of those "),
                )),
            ),
            plural_noun,
        ),
        terminated(
            (
                value(ChosenSetQuantity::Whole, opt(tag("the "))),
                plural_noun,
            ),
            chosen_this_way,
        ),
        // Distributive singular.
        terminated(
            (value(ChosenSetQuantity::Whole, tag("each ")), singular_noun),
            chosen_this_way,
        ),
        // Singular definite heads: recognized only to be classified.
        terminated(
            (
                value(ChosenSetQuantity::SingularDefinite, tag("the ")),
                singular_noun,
            ),
            chosen_this_way,
        ),
        (
            value(
                ChosenSetQuantity::SingularDefinite,
                alt((tag("the chosen "), tag("that "))),
            ),
            singular_noun,
        ),
    ))
    .parse(input)
}

/// "destroy <whole-set reference>" and nothing after it but terminal punctuation.
fn destroy_chosen_set(lower: &str) -> Option<(ChosenSetQuantity, ChosenSetNoun)> {
    all_consuming(terminated(
        nom::sequence::preceded(tag::<_, _, E>("destroy "), chosen_set_reference),
        (space0, opt(tag(".")), space0),
    ))
    .parse(lower.trim())
    .ok()
    .map(|(_, reference)| reference)
}

/// Does every permanent `filter` admits have the noun's type? `Permanent` is
/// implied by any battlefield choice; a `Card` never is (the choice picks
/// permanents on the battlefield).
fn choice_filter_implies(filter: &TargetFilter, noun: ChosenSetNoun) -> bool {
    let required = match noun {
        ChosenSetNoun::Permanent => return true,
        ChosenSetNoun::Card => return false,
        ChosenSetNoun::Creature => TypeFilter::Creature,
        ChosenSetNoun::Artifact => TypeFilter::Artifact,
        ChosenSetNoun::Land => TypeFilter::Land,
        ChosenSetNoun::Enchantment => TypeFilter::Enchantment,
        ChosenSetNoun::Planeswalker => TypeFilter::Planeswalker,
    };
    match filter {
        TargetFilter::Typed(typed) => typed.type_filters.contains(&required),
        TargetFilter::Or { filters } => filters.iter().all(|f| choice_filter_implies(f, noun)),
        TargetFilter::And { filters } => filters.iter().any(|f| choice_filter_implies(f, noun)),
        // Any other filter shape proves nothing about the chosen permanents'
        // types, so the narrower noun is not implied (fail closed).
        _ => false,
    }
}

/// The producer this rule governs: a per-opponent choice of battlefield
/// permanents, the only shape `parse_for_each_opponent_choose_controlled`
/// emits. Every other per-player population and zone is left as it was.
fn per_opponent_battlefield_choice_filter(effect: &Effect) -> Option<Option<&TargetFilter>> {
    match effect {
        Effect::ChooseFromZone {
            zone_owner: ZoneOwner::Each(PerPlayerScope::Opponents),
            zone: Zone::Battlefield,
            filter,
            ..
        } => Some(filter.as_ref()),
        _ => None,
    }
}

/// CR 608.2c + CR 608.2d: apply the reader rule to one chain's IR.
///
/// Find the FIRST per-opponent battlefield choice. The only supported
/// continuation is exactly one more clause — the chain's last — reading
/// "destroy <whole-set reference>" with a noun the choice's filter implies;
/// that clause becomes `DestroyAll { TrackedSet(0) }`. Every other clause after
/// the choice (including a second choice) is replaced by an honest gap carrying
/// its printed text, and loses any patch it would have made to an earlier
/// clause. Clauses before the choice are untouched.
///
/// Runs on every `EffectChainIr` `parse_effect_chain_ir` returns. A conditional
/// body is parsed as its own chain and then absorbed flat into the enclosing
/// chain, so the enclosing pass sees the choice together with every later
/// clause. Re-running the pass is safe: a clause it already rejected carries
/// the gap and is never upgraded to supported, and a bound destroy is
/// re-derived from its printed text. Separate abilities, modes and trigger
/// bodies are separate chains and never see another chain's choice.
///
/// A rejected clause is reset through `ClauseIr::replace_with_gap`, so it
/// keeps no optional gate, unless-payment, repeat count or other executable
/// metadata: it prompts no one and does nothing.
pub(super) fn enforce_per_opponent_choice_tail(ir: &mut EffectChainIr) {
    let Some(p) = ir
        .clauses
        .iter()
        .position(|clause| per_opponent_battlefield_choice_filter(&clause.parsed.effect).is_some())
    else {
        return;
    };
    let choice_filter = per_opponent_battlefield_choice_filter(&ir.clauses[p].parsed.effect)
        .flatten()
        .cloned();

    let tail = &mut ir.clauses[p + 1..];
    // A clause this pass already turned into a gap (a nested body's pass, whose
    // clauses the enclosing chain then absorbs) never becomes supported again:
    // the gap is irreversible on re-entry.
    let already_rejected = tail.iter().any(|clause| {
        matches!(
            &clause.parsed.effect,
            Effect::Unimplemented { name, .. } if name == TRAILING_CLAUSE_GAP
        )
    });
    let supported_final_destroy = !already_rejected && tail.len() == 1 && {
        let clause = &tail[0];
        let lower = clause
            .source
            .fragment()
            .map(str::to_lowercase)
            .unwrap_or_default();
        clause.parsed.sub_ability.is_none()
            && matches!(
                clause.disposition,
                ClauseDisposition::Emit {
                    followup: None,
                    intrinsic: None
                }
            )
            && destroy_chosen_set(&lower).is_some_and(|(quantity, noun)| {
                quantity == ChosenSetQuantity::Whole
                    && choice_filter
                        .as_ref()
                        .is_some_and(|filter| choice_filter_implies(filter, noun))
            })
    };

    if supported_final_destroy {
        // CR 701.8a: destroy every permanent chosen for every opponent at once.
        tail[0].parsed.effect = Effect::DestroyAll {
            target: TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
            cant_regenerate: false,
        };
        return;
    }

    for clause in tail.iter_mut() {
        clause.replace_with_gap(TRAILING_CLAUSE_GAP);
    }
}

/// The gap name every clause this rule rejects carries.
const TRAILING_CLAUSE_GAP: &str = "per_opponent_choice_trailing_clause";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_set_references_parse_with_their_quantity() {
        for (text, quantity, noun) in [
            (
                "destroy the chosen permanents.",
                ChosenSetQuantity::Whole,
                ChosenSetNoun::Permanent,
            ),
            (
                "destroy those creatures.",
                ChosenSetQuantity::Whole,
                ChosenSetNoun::Creature,
            ),
            (
                "destroy each of the chosen lands",
                ChosenSetQuantity::Whole,
                ChosenSetNoun::Land,
            ),
            (
                "destroy each permanent chosen this way.",
                ChosenSetQuantity::Whole,
                ChosenSetNoun::Permanent,
            ),
            (
                "destroy the artifacts chosen this way.",
                ChosenSetQuantity::Whole,
                ChosenSetNoun::Artifact,
            ),
            (
                "destroy the chosen artifact.",
                ChosenSetQuantity::SingularDefinite,
                ChosenSetNoun::Artifact,
            ),
            (
                "destroy that artifact.",
                ChosenSetQuantity::SingularDefinite,
                ChosenSetNoun::Artifact,
            ),
            (
                "destroy the permanent chosen this way.",
                ChosenSetQuantity::SingularDefinite,
                ChosenSetNoun::Permanent,
            ),
        ] {
            assert_eq!(destroy_chosen_set(text), Some((quantity, noun)), "{text}");
        }
        for text in [
            "destroy them.",
            "destroy it.",
            "destroy one of them.",
            "destroy the chosen permanents and draw a card.",
            "destroy the chosen permanentsx.",
        ] {
            assert_eq!(destroy_chosen_set(text), None, "{text}");
        }
    }

    /// Re-entry: a clause the pass rejected stays rejected when a later pass
    /// sees it as the chain's only trailing clause (the shape an enclosing
    /// chain's pass meets after absorbing a nested body whose own pass already
    /// ran). Hand-built from a parsed chain: after the first pass rejects both
    /// trailing clauses, drop the last one so the rejected "destroy the chosen
    /// permanents" is now the only, final clause after the choice, and run the
    /// pass again.
    #[test]
    fn a_rejected_clause_is_never_upgraded_on_re_entry() {
        let mut ir = super::super::parse_effect_chain_ir(
            "For each opponent, choose an artifact that player controls. Destroy the chosen permanents. You gain 1 life.",
            crate::types::ability::AbilityKind::Spell,
            &mut crate::parser::oracle_ir::context::ParseContext::default(),
        );
        let is_gap = |effect: &Effect| matches!(effect, Effect::Unimplemented { name, .. } if name == TRAILING_CLAUSE_GAP);
        assert_eq!(ir.clauses.len(), 3, "reach: choice + two trailing clauses");
        assert!(ir.clauses[1..].iter().all(|c| is_gap(&c.parsed.effect)));

        ir.clauses.truncate(2);
        enforce_per_opponent_choice_tail(&mut ir);
        assert!(
            is_gap(&ir.clauses[1].parsed.effect),
            "the rejected destroy stays a gap: {:?}",
            ir.clauses[1].parsed.effect
        );
    }

    /// A rejected clause keeps no executable metadata: an optional "you may"
    /// or an unless-payment would otherwise still prompt at runtime.
    #[test]
    fn a_rejected_clause_keeps_no_executable_metadata() {
        for text in [
            "For each opponent, choose an artifact that player controls. You may draw a card.",
            "For each opponent, choose an artifact that player controls. You gain 1 life unless you pay {2}.",
        ] {
            let ir = super::super::parse_effect_chain_ir(
                text,
                crate::types::ability::AbilityKind::Spell,
                &mut crate::parser::oracle_ir::context::ParseContext::default(),
            );
            let tail = ir.clauses.last().expect("trailing clause");
            assert!(
                matches!(&tail.parsed.effect, Effect::Unimplemented { name, .. } if name == TRAILING_CLAUSE_GAP),
                "{text}: {:?}",
                tail.parsed.effect
            );
            assert!(!tail.is_optional && !tail.parsed.optional, "{text}");
            assert!(tail.unless_pay.is_none() && tail.parsed.unless_pay.is_none(), "{text}");
            assert!(tail.repeat_for.is_none() && tail.player_scope.is_none(), "{text}");
            assert!(tail.condition.is_none() && tail.parsed.sub_ability.is_none(), "{text}");
        }
    }

    /// Meteor's cast-from-exile body is one chunk, so its choice is the
    /// conditional chunk's own clause in the enclosing chain (the nested-body
    /// path, which parses a multi-chunk body as its own chain and absorbs it,
    /// is not taken). Pinned so a splitter change that routes it through the
    /// nested path is seen here first.
    #[test]
    fn meteor_conditional_body_is_a_single_chunk() {
        let chunks = super::super::sequence::split_clause_sequence(
            "for each opponent, choose an artifact or land that player controls",
        );
        assert_eq!(chunks.len(), 1, "{chunks:?}");
    }
}
