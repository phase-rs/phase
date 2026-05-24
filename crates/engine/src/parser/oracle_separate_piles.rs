//! CR 700.3 + CR 608: Pile-separation parser.
//!
//! Recognises the three-sentence Make-an-Example shape:
//!
//! ```text
//! Each opponent separates the creatures they control into two piles.
//! For each opponent, you choose one of their piles.
//! Each opponent sacrifices the creatures in their chosen pile.
//! ```
//!
//! Output: a single `Effect::SeparateIntoPiles` whose `chosen_pile_effect`
//! is the trailing-sentence sub-effect (a `Sacrifice` for Make an Example).
//! Replaces the prior `Unimplemented{name:"separate"} → Unimplemented{name:"choose"}
//! → Sacrifice` chain plus the spurious `repeat_for` sub-ability.
//!
//! Architectural rules:
//! * Nom combinators for ALL dispatch — never `find` / `contains` /
//!   `split_once` / `starts_with` for parsing.
//! * Builds for the *class* of cards (any "each opponent separates ... into
//!   two piles. For each opponent, you choose ... Each opponent <effect> ...
//!   their chosen pile" card), not just Make an Example. The trailing
//!   sub-effect is parsed by the existing imperative chain parser so future
//!   variants (mill, exile, return-to-hand) come for free.

use nom::branch::alt;
use nom::bytes::complete::tag_no_case;
use nom::combinator::value;
use nom::Parser;

use crate::parser::oracle_nom::error::OracleError;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, PlayerScope, QuantityExpr, TargetFilter, TypeFilter,
    TypedFilter, VoterScope,
};

use super::oracle_effect::parse_effect_chain_with_context;
use super::oracle_ir::context::ParseContext;

/// CR 700.3: Detect and parse the full pile-separation block. Returns a
/// synthesized `AbilityDefinition` wrapping the new `Effect::SeparateIntoPiles`,
/// or `None` if the input doesn't match.
///
/// The input is the joined effect-body text (multi-sentence). The dispatcher
/// in `parser/oracle.rs` calls this BEFORE generic chain parsing so the
/// three-sentence chain is consumed as a single unit rather than parsed into
/// three Unimplemented chunks.
pub(crate) fn parse_separate_into_piles(
    text: &str,
    kind: AbilityKind,
) -> Option<AbilityDefinition> {
    let (rest, partition_subject) = parse_separates_line(text)?;
    let (rest, chooser) = parse_choose_line(rest)?;
    let trailing = rest.trim_start();
    if trailing.is_empty() {
        return None;
    }
    // Parse the trailing sentence (the per-pile sub-effect) through the
    // standard imperative chain parser. For Make an Example this yields a
    // `Sacrifice { target: ParentTarget }` chain — the runtime resolver
    // re-binds `controller` to each subject before applying it.
    //
    // CR 700.3b: the pile is not an object — the sub-effect's target is
    // wired by the resolver per-object, not via the parsed `target_filter`.
    let parsed = parse_effect_chain_with_context(trailing, kind, &mut ParseContext::default());
    // Reject if the trailing sentence didn't yield a real effect (the parser
    // returns an Unimplemented stub on failure).
    if matches!(*parsed.effect, Effect::Unimplemented { .. }) {
        return None;
    }
    // CR 700.3 + CR 608.2c: Build a sub-effect with a generic ParentTarget
    // filter so the runtime's per-object loop in `apply_pile_effect` sets
    // the target via `TargetRef::Object`. Force-rewrite the sub-effect's
    // target filter to `ParentTarget` so the per-object pipeline routes
    // through the standard sacrifice handler unambiguously.
    let mut sub_def = parsed;
    rewrite_sub_effect_target_to_parent(&mut sub_def.effect);

    Some(AbilityDefinition::new(
        kind,
        Effect::SeparateIntoPiles {
            partition_subject,
            // CR 700.3: Make an Example partitions creatures specifically;
            // the Liliana −6 follow-up will pass a wider filter. Defaulting
            // to the parsed subject filter is a future extension — for now
            // we hardcode Creature, which is the only printed shape.
            object_filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            chooser,
            chosen_pile_effect: Box::new(sub_def),
        },
    ))
}

/// CR 700.3 + CR 700.3a: Consume the "Each opponent separates the creatures
/// they control into two piles." opener. Returns the remainder and the
/// `VoterScope` for the partitioning subject. Currently supports the
/// "each opponent" shape; "target player separates ..." (Liliana −6) is a
/// leaf extension on `VoterScope` and slots in here as another `alt()`
/// branch.
fn parse_separates_line(input: &str) -> Option<(&str, VoterScope)> {
    let res: nom::IResult<&str, VoterScope, OracleError<'_>> = value(
        VoterScope::EachOpponent,
        tag_no_case("each opponent separates "),
    )
    .parse(input);
    let (rest, scope) = res.ok()?;
    // Consume "the creatures they control " — the subject filter is fixed
    // for the current shape (see comment in caller about Creature default).
    let rest = consume_creatures_they_control(rest)?;
    let res: nom::IResult<&str, (), OracleError<'_>> =
        value((), tag_no_case("into two piles")).parse(rest);
    let (rest, ()) = res.ok()?;
    // Optional trailing period and whitespace.
    let rest = rest.trim_start_matches('.').trim_start();
    Some((rest, scope))
}

fn consume_creatures_they_control(input: &str) -> Option<&str> {
    // Two variants: "the creatures they control " and the rarer "creatures
    // they control " (no article). Both are nom alternatives.
    let res: nom::IResult<&str, (), OracleError<'_>> = value(
        (),
        alt((
            tag_no_case("the creatures they control "),
            tag_no_case("creatures they control "),
        )),
    )
    .parse(input);
    res.ok().map(|(rest, ())| rest)
}

/// CR 700.3 + CR 608.2c: Consume "For each opponent, you choose one of their
/// piles." (or the bare "You choose one of their piles."). Returns the
/// remainder and the chooser scope.
fn parse_choose_line(input: &str) -> Option<(&str, PlayerScope)> {
    let res: nom::IResult<&str, (), OracleError<'_>> = value(
        (),
        alt((
            tag_no_case("for each opponent, you choose "),
            tag_no_case("you choose "),
        )),
    )
    .parse(input);
    let (rest, ()) = res.ok()?;
    let res: nom::IResult<&str, (), OracleError<'_>> = value(
        (),
        alt((tag_no_case("one of their piles"), tag_no_case("one pile"))),
    )
    .parse(rest);
    let (rest, ()) = res.ok()?;
    let rest = rest.trim_start_matches('.').trim_start();
    Some((rest, PlayerScope::Controller))
}

/// Rewrite the sub-effect's primary target filter to `TargetFilter::ParentTarget`
/// so the runtime per-object loop in `effects/separate_piles::apply_pile_effect`
/// can pass each pile object through `targets[0]` and have the standard
/// sacrifice/exile/bounce handler pick it up. Currently only `Effect::Sacrifice`
/// is exercised; extend with new effect arms as new pile-effect shapes ship.
fn rewrite_sub_effect_target_to_parent(effect: &mut Effect) {
    if let Effect::Sacrifice { target, count, .. } = effect {
        // `SeparateIntoPiles` applies this sub-effect once per object in the
        // chosen pile. Keep the sub-effect canonical as "sacrifice this one
        // parent target"; the parsed "all" cardinality belongs to the pile loop.
        *target = TargetFilter::ParentTarget;
        *count = QuantityExpr::Fixed { value: 1 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CR 700.3: Make-an-Example body parses to a single
    /// `Effect::SeparateIntoPiles` with `EachOpponent` partition,
    /// `Controller` chooser, and a Sacrifice sub-effect.
    #[test]
    fn parses_make_an_example_body() {
        let text = "Each opponent separates the creatures they control into two piles. \
                    For each opponent, you choose one of their piles. \
                    Each opponent sacrifices the creatures in their chosen pile.";
        let def = parse_separate_into_piles(text, AbilityKind::Spell)
            .expect("Make an Example body parses");
        match &*def.effect {
            Effect::SeparateIntoPiles {
                partition_subject,
                chooser,
                chosen_pile_effect,
                ..
            } => {
                assert!(matches!(partition_subject, VoterScope::EachOpponent));
                assert!(matches!(chooser, PlayerScope::Controller));
                assert!(
                    matches!(*chosen_pile_effect.effect, Effect::Sacrifice { .. }),
                    "expected Sacrifice sub-effect, got {:?}",
                    chosen_pile_effect.effect
                );
            }
            other => panic!("expected SeparateIntoPiles, got {other:?}"),
        }
    }

    /// Non-matching body returns None — the dispatcher must fall back to
    /// generic chain parsing.
    #[test]
    fn rejects_non_pile_body() {
        let text = "Destroy target creature. Draw a card.";
        assert!(parse_separate_into_piles(text, AbilityKind::Spell).is_none());
    }
}
