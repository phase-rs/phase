// CR 301.5b / CR 303.4j — positive "can be attached only to" restrictions.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

/// Parse "~ can be attached only to {filter}" / "equipped creature can be
/// attached only to {filter}" into `StaticMode::CanBeAttachedOnlyTo`.
pub(crate) fn parse_can_be_attached_only_to(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    const MARKERS: [&str; 2] = ["can be attached only to ", "may be attached only to "];
    let mut filter_start = None;
    for marker in MARKERS {
        if let Some(idx) = tp.lower.find(marker) {
            filter_start = Some(idx + marker.len());
            break;
        }
    }
    let start = filter_start?;

    // Optional leading subject before the restriction clause.
    let filter_text = tp.original[start..].trim();
    let filter_text = filter_text.trim_end_matches('.');
    if filter_text.is_empty() {
        return None;
    }

    let filter_text_tp = TextPair::new(filter_text, filter_text);
    let filter = parse_chosen_qualifier_subject(&filter_text_tp).unwrap_or_else(|| {
        let (f, remainder) = parse_type_phrase(filter_text);
        if !remainder.trim().is_empty() && !remainder.trim().starts_with('.') {
            return TargetFilter::Any;
        }
        f
    });
    if matches!(filter, TargetFilter::Any) {
        return None;
    }

    Some(
        StaticDefinition::new(StaticMode::CanBeAttachedOnlyTo { filter })
            .affected(TargetFilter::SelfRef)
            .description(text.to_string()),
    )
}
