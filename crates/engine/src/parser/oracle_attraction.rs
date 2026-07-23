//! CR 717.5 + CR 702.159a: Attraction visit abilities and numbered visit lines.

use nom::branch::alt;
use nom::bytes::complete::{tag, take_while1};
use nom::character::complete::multispace0;
use nom::combinator::{map, opt, value};
use nom::sequence::preceded;
use nom::Parser;

use crate::parser::oracle_nom::bridge::{nom_parse_lower, split_once_on_lower};
use crate::types::ability::{AbilityKind, TriggerCondition, TriggerDefinition};
use crate::types::triggers::TriggerMode;

use super::oracle_effect::parse_effect_chain;
use super::oracle_util::strip_reminder_text;

const NUMBERED_VISIT_PIPE: &str = " | ";

/// Parse `"Visit — …"` or `"N—M | …"` / `"N | …"` attraction visit lines.
pub(crate) fn parse_visit_trigger(line: &str, card_name: &str) -> Option<TriggerDefinition> {
    let stripped = strip_reminder_text(line);
    let lower = stripped.to_ascii_lowercase();

    if let Some((min, max, effect_text)) = parse_numbered_visit_line(&lower, &stripped) {
        let mut trigger = TriggerDefinition::new(TriggerMode::VisitAttraction)
            .valid_card(crate::types::ability::TargetFilter::SelfRef)
            .execute(parse_effect_chain(&effect_text, AbilityKind::Spell));
        if min != 1 || max != 6 {
            trigger.condition = Some(TriggerCondition::AttractionVisitRoll { min, max });
        }
        return Some(trigger);
    }

    let effect = strip_visit_effect_text(&stripped)?;
    let _ = card_name;
    Some(
        TriggerDefinition::new(TriggerMode::VisitAttraction)
            .valid_card(crate::types::ability::TargetFilter::SelfRef)
            .execute(parse_effect_chain(effect, AbilityKind::Spell)),
    )
}

/// Returns visit triggers paired with their source line index (for source-order
/// emission in `parse_oracle_ir`, unit-4 c2) plus the consumed line indices.
///
/// CR 717: each Attraction visit line yields at most one trigger, emitted at its
/// own printed line.
pub(crate) fn parse_attraction_visit_triggers(
    lines: &[&str],
    card_name: &str,
) -> (
    Vec<(usize, TriggerDefinition)>,
    std::collections::HashSet<usize>,
) {
    let mut triggers = Vec::new();
    let mut consumed = std::collections::HashSet::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if is_attraction_visit_line(&lower) {
            if let Some(trigger) = parse_visit_trigger(trimmed, card_name) {
                triggers.push((idx, trigger));
                consumed.insert(idx);
            }
        }
    }
    (triggers, consumed)
}

fn is_attraction_visit_line(lower: &str) -> bool {
    parse_visit_line_header(lower).is_ok() || parse_numbered_visit_line(lower, lower).is_some()
}

fn parse_visit_line_header(input: &str) -> nom::IResult<&str, ()> {
    preceded(
        tag("visit"),
        alt((
            value((), multispace0),
            value((), tag("—")),
            value((), tag("-")),
        )),
    )
    .parse(input)
}

fn strip_visit_effect_text(line: &str) -> Option<&str> {
    let lower = line.to_ascii_lowercase();
    let (effect_start, effect_end) = nom_parse_lower(&lower, |input| {
        let (rest, _) = tag("visit").parse(input)?;
        let (rest, _) = multispace0.parse(rest)?;
        let (rest, _) = opt((alt((tag("—"), tag("-"), tag(":"))), multispace0)).parse(rest)?;
        let effect_body = rest.trim();
        if effect_body.is_empty() {
            return Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Fail,
            )));
        }
        let body_start = input.len() - rest.len() + (rest.len() - rest.trim_start().len());
        Ok(("", (body_start, body_start + effect_body.len())))
    })?;
    Some(line[effect_start..effect_end].trim())
}

fn parse_attraction_die_face(input: &str) -> nom::IResult<&str, u8> {
    let (rest, digits) = take_while1(|c: char| c.is_ascii_digit()).parse(input)?;
    let n = digits
        .parse::<u8>()
        .map_err(|_| nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Fail)))?;
    if (1..=6).contains(&n) {
        Ok((rest, n))
    } else {
        Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )))
    }
}

fn parse_roll_range_prefix(input: &str) -> nom::IResult<&str, (u8, u8)> {
    let (input, _) = multispace0.parse(input)?;
    alt((
        map(
            (
                parse_attraction_die_face,
                alt((tag("\u{2014}"), tag("-"))),
                parse_attraction_die_face,
            ),
            |(min, _, max)| (min, max),
        ),
        map(parse_attraction_die_face, |n| (n, n)),
    ))
    .parse(input)
}

fn parse_numbered_visit_line(lower: &str, original: &str) -> Option<(u8, u8, String)> {
    let (prefix_lower, effect_original) =
        split_once_on_lower(original, lower, NUMBERED_VISIT_PIPE)?;
    let (min, max) = nom_parse_lower(prefix_lower, parse_roll_range_prefix)?;
    let effect = effect_original.trim();
    if effect.is_empty() || min > max {
        return None;
    }
    Some((min, max, effect.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::Effect;
    use crate::types::triggers::TriggerMode;

    #[test]
    fn parse_oracle_text_open_an_attraction() {
        let parsed = parse_oracle_text("Open an Attraction.", "Opener", &[], &[], &[]);
        assert!(
            parsed.abilities.iter().any(|a| {
                matches!(
                    *a.effect,
                    crate::types::ability::Effect::OpenAttractions { count: 1 }
                )
            }),
            "abilities: {:?}",
            parsed
                .abilities
                .iter()
                .map(|a| &a.effect)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_oracle_text_includes_visit_trigger_for_attraction_subtype() {
        let parsed = parse_oracle_text(
            "Visit — Draw a card.",
            "Test Attraction",
            &[],
            &[],
            &["Attraction".to_string()],
        );
        assert!(
            parsed
                .triggers
                .iter()
                .any(|t| t.mode == TriggerMode::VisitAttraction),
            "triggers: {:?}",
            parsed.triggers
        );
    }

    #[test]
    fn visit_dash_parses_draw() {
        let trigger = parse_visit_trigger("Visit — Draw a card.", "Test Attraction").unwrap();
        assert_eq!(trigger.mode, TriggerMode::VisitAttraction);
        let execute = trigger.execute.as_ref().expect("visit execute effect");
        assert!(
            matches!(*execute.effect, Effect::Draw { .. }),
            "expected Draw, got {:?}",
            execute.effect
        );
    }

    #[test]
    fn numbered_line_parses_range_condition() {
        let trigger =
            parse_visit_trigger("2—5 | Create a Treasure token.", "Test Attraction").unwrap();
        assert_eq!(trigger.mode, TriggerMode::VisitAttraction);
        assert!(matches!(
            trigger.condition,
            Some(TriggerCondition::AttractionVisitRoll { min: 2, max: 5 })
        ));
    }
}
