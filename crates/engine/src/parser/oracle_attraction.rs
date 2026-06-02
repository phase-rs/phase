//! CR 717.5 + CR 702.159a: Attraction visit abilities and numbered visit lines.

use crate::types::ability::{AbilityKind, TriggerCondition, TriggerDefinition};
use crate::types::triggers::TriggerMode;

use super::oracle_effect::parse_effect_chain;
use super::oracle_util::strip_reminder_text;

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

/// Returns line indices consumed by visit triggers (for oracle.rs dispatcher).
pub(crate) fn parse_attraction_visit_triggers(
    lines: &[&str],
    card_name: &str,
) -> (Vec<TriggerDefinition>, std::collections::HashSet<usize>) {
    let mut triggers = Vec::new();
    let mut consumed = std::collections::HashSet::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("visit ")
            || lower.starts_with("visit—")
            || lower.starts_with("visit-")
            || is_numbered_visit_line(&lower)
        {
            if let Some(trigger) = parse_visit_trigger(trimmed, card_name) {
                triggers.push(trigger);
                consumed.insert(idx);
            }
        }
    }
    (triggers, consumed)
}

fn strip_visit_effect_text(line: &str) -> Option<&str> {
    let mut rest = line.strip_prefix("Visit")?;
    rest = rest.trim_start();
    if let Some((_, effect)) = rest.split_once(" — ") {
        return Some(effect.trim());
    }
    if let Some((_, effect)) = rest.split_once(" - ") {
        return Some(effect.trim());
    }
    if let Some((_, effect)) = rest.split_once('—') {
        return Some(effect.trim());
    }
    if let Some((_, effect)) = rest.split_once('-') {
        return Some(effect.trim());
    }
    if let Some((_, effect)) = rest.split_once(':') {
        return Some(effect.trim());
    }
    if rest.is_empty() {
        None
    } else {
        Some(rest)
    }
}

fn is_numbered_visit_line(lower: &str) -> bool {
    parse_numbered_visit_line(lower, lower).is_some()
}

fn parse_numbered_visit_line(lower: &str, original: &str) -> Option<(u8, u8, String)> {
    let pipe_pos = lower.find(" | ")?;
    let prefix = lower[..pipe_pos].trim();
    let effect = original[pipe_pos + 3..].trim().to_string();
    if effect.is_empty() {
        return None;
    }
    let (min, max) = if let Some((a, b)) = prefix
        .split_once('\u{2014}')
        .or_else(|| prefix.split_once('-'))
    {
        let min: u8 = a.trim().parse().ok()?;
        let max: u8 = b.trim().parse().ok()?;
        (min, max)
    } else {
        let n: u8 = prefix.parse().ok()?;
        (n, n)
    };
    if (1..=6).contains(&min) && (1..=6).contains(&max) && min <= max {
        Some((min, max, effect))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::triggers::TriggerMode;

    #[test]
    fn parse_oracle_text_open_an_attraction() {
        let parsed = parse_oracle_text("Open an Attraction.", "Opener", &[], &[], &[]);
        assert!(
            parsed
                .abilities
                .iter()
                .any(|a| { matches!(*a.effect, crate::types::ability::Effect::OpenAttraction) }),
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
        assert!(trigger.execute.is_some());
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
