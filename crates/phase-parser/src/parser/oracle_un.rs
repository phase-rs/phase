//! Un-card Oracle lines (silver-border sets; no CR entry).
//!
//! `Gotcha — If an opponent <behavior>, you may say "Gotcha!" When you do, <effect>.`
//! parses to a [`TriggerMode::GotchaSaid`] trigger that functions from the
//! zone its effect returns the card from (the graveyard, for every printed
//! Gotcha card but Name Dropping, which works from the battlefield). The
//! `<behavior>` is a typed [`GotchaCondition`]; the "When you do" clause is the
//! reflexive trigger's effect chain (CR 603.12), parsed by the ordinary
//! effect-chain parser and handed back to the caller.

use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::{char, space0};
use nom::combinator::{opt, value};
use nom::multi::separated_list1;
use nom::sequence::{delimited, preceded};
use nom::Parser;

use crate::types::ability::{AbilityDefinition, Effect, TargetFilter, TriggerDefinition};
use crate::types::triggers::TriggerMode;
use crate::types::un_actions::{GotchaCondition, PhysicalTell};
use crate::types::zones::Zone;

use super::oracle_nom::bridge::nom_on_lower;
use super::oracle_nom::error::OracleResult;

/// The `Gotcha — ` ability-word label.
fn parse_gotcha_label(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            tag("gotcha"),
            space0,
            alt((char('\u{2014}'), char('-'))),
            space0,
        ),
    )
    .parse(input)
}

/// `"word"` or `"word1" or "word2"` — the spoken words a Gotcha watches for.
fn parse_quoted_word_list(input: &str) -> OracleResult<'_, Vec<String>> {
    separated_list1(
        tag(" or "),
        delimited(char('"'), take_until("\""), char('"')),
    )
    // American punctuation puts the clause comma inside the closing quote
    // (`"guy,"`); it is not part of the spoken word.
    .map(|words: Vec<&str>| {
        words
            .into_iter()
            .map(|word| {
                word.trim_end_matches(|c: char| c.is_ascii_punctuation())
                    .to_string()
            })
            .collect()
    })
    .parse(input)
}

/// The opponent behavior after `if an opponent `.
fn parse_gotcha_condition(input: &str) -> OracleResult<'_, GotchaCondition> {
    alt((
        value(
            GotchaCondition::Physical(PhysicalTell::TouchFace),
            tag("touches their face"),
        ),
        value(
            GotchaCondition::Physical(PhysicalTell::TouchTable),
            tag("touches the table with their hand"),
        ),
        value(
            GotchaCondition::Physical(PhysicalTell::Laugh),
            tag("laughs"),
        ),
        value(
            GotchaCondition::Physical(PhysicalTell::FlickCards),
            tag("audibly flicks the cards in their hand"),
        ),
        value(GotchaCondition::SaysNumber, tag("says a number")),
        value(
            GotchaCondition::SaysWordInGraveyardCardName,
            (
                tag("says a word that"),
                alt((tag("'s"), tag("\u{2019}s"))),
                tag(" in the name of a card in your graveyard"),
            ),
        ),
        preceded(tag("says "), parse_quoted_word_list)
            .map(|words| GotchaCondition::SaysAnyOf { words }),
    ))
    .parse(input)
}

/// `gotcha — if an opponent <condition>, you may say "gotcha!" when you do, `
fn parse_gotcha_head(input: &str) -> OracleResult<'_, GotchaCondition> {
    delimited(
        (parse_gotcha_label, tag("if an opponent ")),
        parse_gotcha_condition,
        (
            opt(tag(",")),
            tag(" you may say \"gotcha!\""),
            alt((tag(" when you do, "), tag(". when you do, "))),
        ),
    )
    .parse(input)
}

/// Split a Gotcha line into its typed condition and the original-case text of
/// the "When you do" effect clause. `None` when `line` is not a Gotcha ability.
pub(crate) fn split_gotcha_line<'a>(
    line: &'a str,
    lower: &str,
) -> Option<(GotchaCondition, &'a str)> {
    nom_on_lower(line, lower, parse_gotcha_head)
}

/// Assemble the Gotcha trigger around its parsed "When you do" effect chain.
pub(crate) fn gotcha_trigger(
    condition: GotchaCondition,
    mut effect: AbilityDefinition,
    description: &str,
) -> TriggerDefinition {
    // "return that card to your hand": the antecedent is the card the Gotcha
    // event names (`GameEvent::GotchaSaid::returns`), which the ordinary effect
    // parser can only guess is a `ParentTarget`.
    if let Effect::Bounce {
        target: target @ TargetFilter::ParentTarget,
        ..
    } = &mut *effect.effect
    {
        *target = TargetFilter::TriggeringSource;
    }
    // The ability functions from the zone it returns its own card from; a Gotcha
    // that names another card ("return that card to your hand") is a permanent
    // ability and functions on the battlefield.
    let zone = match &*effect.effect {
        Effect::ChangeZone {
            target: TargetFilter::SelfRef,
            origin: Some(origin),
            ..
        } => *origin,
        _ => Zone::Battlefield,
    };
    let mut trigger = TriggerDefinition::new(TriggerMode::GotchaSaid)
        .trigger_zones(vec![zone])
        .execute(effect)
        .description(description.to_string());
    trigger.gotcha_condition = Some(condition);
    trigger
}
