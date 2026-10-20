//! CR 701.13a + CR 202.3 + CR 608.2c: the top-card exile contest — "each player
//! exiles the top card of their library. The player who exiled the card with
//! the greatest mana value <instruction>. If two or more players' cards are
//! tied for greatest, the tied players repeat this process until the tie is
//! broken." (Timesifter).
//!
//! The three sentences are one instruction: the second names its subject by
//! the first's outcome and the third loops the first two (CR 608.2c — read the
//! whole text), so the chain lowers to a single `Effect::ExileTopCardContest`
//! whose nested `winner_effect` is the winner's instruction.
//!
//! Each sentence is composed from its grammatical dimensions — the player scope
//! and library owner (shared subject/owner authorities), the extremum adjective
//! and compared property (shared aggregate vocabulary), and the tie rule — and
//! the winner's instruction is delegated to the ordinary chain parser.

use nom::bytes::complete::tag;
use nom::combinator::{eof, opt, verify};
use nom::Parser;

use super::imperative::parse_library_player_suffix;
use super::lower::strip_each_player_subject;
use super::parse_effect_chain_with_context;
use crate::parser::oracle_ir::ast::parsed_clause;
use crate::parser::oracle_ir::context::ParseContext;
use crate::parser::oracle_ir::effect_chain::EffectChainIr;
use crate::parser::oracle_nom::bridge::nom_on_lower;
use crate::parser::oracle_nom::error::{OracleError, OracleResult};
use crate::parser::oracle_nom::quantity::{parse_aggregate_property, parse_max_extremum_adjective};
use crate::types::ability::{
    AbilityKind, ControllerRef, Effect, ObjectProperty, PlayerFilter, TargetFilter,
};

/// CR 101.4 + CR 701.13a: "each player exiles the top card of their library" —
/// the contest action. The subject must scope every player (the resolver seats
/// every living player), and the library must be each scoped player's own.
/// Returns the text after the action sentence's terminal period.
fn parse_contest_action(text: &str) -> Option<String> {
    let (scope, rest) = strip_each_player_subject(text);
    if scope != Some(PlayerFilter::All) {
        return None;
    }
    // The subject strip deconjugates the verb ("exiles" → "exile").
    let lower = rest.to_lowercase();
    let (after_top, _) = (tag::<_, _, OracleError<'_>>("exile "), tag("the top "))
        .parse(lower.as_str())
        .ok()?;
    // CR 109.5: under an "each player" subject, "their library" is the scoped
    // player's own library.
    let scoped = ParseContext {
        relative_player_scope: Some(ControllerRef::ScopedPlayer),
        ..Default::default()
    };
    let (after_owner, owner) = parse_library_player_suffix(after_top, &scoped)?;
    if owner != TargetFilter::ScopedPlayer {
        return None;
    }
    let (after_period, _) = tag::<_, _, OracleError<'_>>(". ").parse(after_owner).ok()?;
    Some(rest[rest.len() - after_period.len()..].to_string())
}

/// CR 202.3: "the greatest mana value" — a maximizing extremum over the mana
/// value property, the only comparison the contest resolver performs.
fn parse_greatest_mana_value(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = parse_max_extremum_adjective(input)?;
    let (input, _) = tag(" ").parse(input)?;
    let (input, _) = verify(parse_aggregate_property, |property| {
        *property == ObjectProperty::ManaValue
    })
    .parse(input)?;
    Ok((input, ()))
}

/// "the player who exiled the card with the greatest mana value " — the winner
/// subject: a player noun, a relative clause naming the action's result, and
/// the comparison that selects it.
fn parse_winner_subject(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = tag("the player who ").parse(input)?;
    let (input, _) = tag("exiled ").parse(input)?;
    let (input, _) = tag("the card").parse(input)?;
    let (input, _) = tag(" with the ").parse(input)?;
    let (input, _) = parse_greatest_mana_value(input)?;
    let (input, _) = tag(" ").parse(input)?;
    Ok((input, ()))
}

/// "if two or more players' cards are tied for greatest, the tied players
/// repeat this process until the tie is broken." — the tie condition and its
/// repeat-among-the-tied instruction. Any other tie rule is a different contest.
fn parse_repeat_among_tied(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = tag("if two or more ").parse(input)?;
    let (input, _) = tag("players' cards").parse(input)?;
    let (input, _) = tag(" are tied for ").parse(input)?;
    let (input, _) = parse_max_extremum_adjective(input)?;
    let (input, _) = tag(", ").parse(input)?;
    let (input, _) = tag("the tied players repeat this process").parse(input)?;
    let (input, _) = tag(" until the tie is broken").parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    let (input, _) = eof.parse(input)?;
    Ok((input, ()))
}

/// Split the winner's instruction from the trailing tie rule: scan the
/// instruction's sentence boundaries and take the first one at which the tie
/// rule parses to the end of the text.
fn split_winner_instruction(text: &str) -> Option<&str> {
    let lower = text.to_lowercase();
    lower
        .match_indices(". ")
        .find(|&(index, sep)| parse_repeat_among_tied(&lower[index + sep.len()..]).is_ok())
        .map(|(index, _)| &text[..index])
}

/// Parse the whole contest chain, or `None` (fall through to the generic chain
/// parser) when the text is not this shape or the winner's instruction does not
/// parse.
pub(super) fn parse_exile_top_card_contest_ir(
    text: &str,
    kind: AbilityKind,
    ctx: &ParseContext,
) -> Option<EffectChainIr> {
    let text = text.trim();
    let after_action = parse_contest_action(text)?;
    let lower = after_action.to_lowercase();
    let ((), rest) = nom_on_lower(&after_action, &lower, parse_winner_subject)?;
    let instruction = split_winner_instruction(rest)?;

    // The winner's instruction is a predicate whose subject is the winner
    // ("takes an extra turn after this one"); it parses like any "each player"
    // predicate, acting for the player the resolver binds.
    let winner_effect = parse_effect_chain_with_context(
        instruction,
        kind,
        &mut ParseContext {
            card_name: ctx.card_name.clone(),
            in_trigger: ctx.in_trigger,
            ..Default::default()
        },
    );
    if matches!(*winner_effect.effect, Effect::Unimplemented { .. }) {
        return None;
    }

    Some(EffectChainIr::single_clause(
        text,
        kind,
        parsed_clause(Effect::ExileTopCardContest {
            winner_effect: Box::new(winner_effect),
        }),
        None,
        ctx.actor.clone(),
        ctx.in_trigger,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{QuantityExpr, TargetFilter};

    const TIMESIFTER: &str = "each player exiles the top card of their library. The player \
        who exiled the card with the greatest mana value takes an extra turn after this one. \
        If two or more players' cards are tied for greatest, the tied players repeat this \
        process until the tie is broken.";

    fn contest_winner_effect(text: &str) -> Option<Effect> {
        let ir =
            parse_exile_top_card_contest_ir(text, AbilityKind::Spell, &ParseContext::default())?;
        let def = super::super::lower::lower_effect_chain_ir(&ir);
        let Effect::ExileTopCardContest { winner_effect } = *def.effect else {
            panic!("expected ExileTopCardContest, got {:?}", def.effect);
        };
        Some(*winner_effect.effect)
    }

    #[test]
    fn contest_lowers_winner_instruction_as_nested_effect() {
        let winner = contest_winner_effect(TIMESIFTER).expect("Timesifter contest must parse");
        assert_eq!(
            winner,
            Effect::ExtraTurn {
                target: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
            }
        );
    }

    #[test]
    fn contest_requires_the_repeat_tie_rule() {
        // A different tie rule is a different contest; decline it.
        let no_repeat = "each player exiles the top card of their library. The player who \
            exiled the card with the greatest mana value takes an extra turn after this one. \
            If two or more players' cards are tied for greatest, nothing happens.";
        assert!(contest_winner_effect(no_repeat).is_none());
        // Reach-guard: the same chain with the repeat rule does parse.
        assert!(contest_winner_effect(TIMESIFTER).is_some());
    }

    #[test]
    fn contest_declines_an_unparsed_winner_instruction() {
        let unparsed = "each player exiles the top card of their library. The player who \
            exiled the card with the greatest mana value frobnicates wildly. If two or more \
            players' cards are tied for greatest, the tied players repeat this process until \
            the tie is broken.";
        assert!(contest_winner_effect(unparsed).is_none());
    }

    /// Rebuilds the Timesifter text with one grammatical dimension replaced.
    fn contest_text(action: &str, comparison: &str) -> String {
        format!(
            "{action}. The player who exiled the card with the {comparison} takes an extra \
             turn after this one. If two or more players' cards are tied for greatest, the \
             tied players repeat this process until the tie is broken."
        )
    }

    const EACH_PLAYER_EXILES: &str = "each player exiles the top card of their library";

    #[test]
    fn contest_comparison_reads_the_shared_extremum_vocabulary() {
        // The extremum adjective is one axis ("greatest" / "highest"), and the
        // property axis accepts every mana-value spelling.
        for comparison in [
            "greatest mana value",
            "highest mana value",
            "greatest converted mana cost",
        ] {
            assert!(
                contest_winner_effect(&contest_text(EACH_PLAYER_EXILES, comparison)).is_some(),
                "{comparison}"
            );
        }
    }

    #[test]
    fn contest_declines_a_comparison_the_resolver_does_not_perform() {
        // The resolver compares mana value only (CR 202.3).
        assert!(
            contest_winner_effect(&contest_text(EACH_PLAYER_EXILES, "greatest power")).is_none()
        );
    }

    #[test]
    fn contest_requires_every_player_exiling_from_their_own_library() {
        for action in [
            "each opponent exiles the top card of their library",
            "each player exiles the top card of your library",
        ] {
            assert!(
                contest_winner_effect(&contest_text(action, "greatest mana value")).is_none(),
                "{action}"
            );
        }
        // Reach-guard: the printed action parses.
        assert!(
            contest_winner_effect(&contest_text(EACH_PLAYER_EXILES, "greatest mana value"))
                .is_some()
        );
    }
}
