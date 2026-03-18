use std::str::FromStr;

use crate::types::ability::{ControllerRef, FilterProp, TargetFilter, TypeFilter, TypedFilter};
use crate::types::identifiers::TrackedSetId;
use crate::types::keywords::Keyword;
use crate::types::zones::Zone;

use super::oracle_util::{contains_possessive, merge_or_filters};

/// Parse an event-context possessive reference from Oracle text.
/// These resolve from the triggering event, not from player targeting.
/// Must be checked BEFORE standard `parse_target` for trigger-based effects.
pub fn parse_event_context_ref(text: &str) -> Option<TargetFilter> {
    let lower = text.to_lowercase();
    let lower = lower.trim();

    if lower == "that spell's controller" || lower.starts_with("that spell's controller") {
        return Some(TargetFilter::TriggeringSpellController);
    }
    if lower == "that spell's owner" || lower.starts_with("that spell's owner") {
        return Some(TargetFilter::TriggeringSpellOwner);
    }
    if lower == "that player" || lower.starts_with("that player") {
        return Some(TargetFilter::TriggeringPlayer);
    }
    if lower == "that source" || lower.starts_with("that source") {
        return Some(TargetFilter::TriggeringSource);
    }
    if lower == "that permanent" || lower.starts_with("that permanent") {
        return Some(TargetFilter::TriggeringSource);
    }
    // CR 506.3d: "defending player" — the player being attacked by the source creature.
    if lower == "defending player" || lower.starts_with("defending player") {
        return Some(TargetFilter::DefendingPlayer);
    }

    None
}

/// Parse a target description from Oracle text, returning (filter, remaining_text).
/// Consumes the longest matching target phrase.
pub fn parse_target(text: &str) -> (TargetFilter, &str) {
    let text = text.trim_start();
    let lower = text.to_lowercase();

    // Self-reference: "~" (normalized from card name / "this creature" etc.)
    if let Some(rest) = text.strip_prefix('~') {
        return (TargetFilter::SelfRef, rest.trim_start());
    }

    // "any target"
    if lower.starts_with("any target") {
        return (TargetFilter::Any, &text[10..]);
    }

    // "target player or planeswalker"
    if lower.starts_with("target player or planeswalker") {
        return (
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::Player,
                    typed(TypeFilter::Planeswalker, None, vec![]),
                ],
            },
            &text[29..],
        );
    }

    // "target opponent"
    if lower.starts_with("target opponent") {
        return (
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            &text[15..],
        );
    }

    // "target player"
    if lower.starts_with("target player") {
        return (TargetFilter::Player, &text[13..]);
    }

    // "each opponent"
    if lower.starts_with("each opponent") {
        return (
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            &text[13..],
        );
    }

    // "target" + type phrase
    if lower.starts_with("target ") {
        let (filter, rest) = parse_type_phrase(&text[7..]);
        return (filter, rest);
    }

    // "all" / "each" + type phrase (for *All effects)
    if lower.starts_with("all ") {
        let (filter, rest) = parse_type_phrase(&text[4..]);
        return (filter, rest);
    }
    if lower.starts_with("each ") {
        let (filter, rest) = parse_type_phrase(&text[5..]);
        return (filter, rest);
    }

    // "enchanted creature"
    if lower.starts_with("enchanted creature") {
        return (
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy])),
            &text[18..],
        );
    }

    // "equipped creature"
    if lower.starts_with("equipped creature") {
        return (
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy])),
            &text[17..],
        );
    }

    // CR 603.7: Anaphoric pronouns referencing previously affected objects.
    // Parallel to LastCreated — a parse-time marker resolved at runtime.
    // TrackedSetId(0) is a safe sentinel (next_tracked_set_id starts at 1).
    for prefix in [
        "those cards",
        "those permanents",
        "those creatures",
        "the exiled cards",
        "the exiled card",
        "the exiled permanents",
        "the exiled permanent",
        "the exiled creature",
    ] {
        if lower.starts_with(prefix) {
            return (
                TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                &text[prefix.len()..],
            );
        }
    }

    // Bare type phrase fallback: try parse_type_phrase before giving up.
    // Handles "other nonland permanents you own and control" after quantifier stripping.
    let (filter, rest) = parse_type_phrase(text);
    match &filter {
        // parse_type_phrase recognized a card type, subtype, or meaningful properties
        TargetFilter::Typed(tf)
            if tf.card_type.is_some() || tf.subtype.is_some() || !tf.properties.is_empty() =>
        {
            (filter, rest)
        }
        // No meaningful content parsed — preserve original fallback behavior
        _ => (TargetFilter::Any, text),
    }
}

/// Parse a type phrase like "creature", "nonland permanent", "artifact or enchantment",
/// "creature you control", "creature an opponent controls".
pub fn parse_type_phrase(text: &str) -> (TargetFilter, &str) {
    let lower = text.to_lowercase();
    let mut pos = 0;
    let mut properties = Vec::new();
    let lower_trimmed = lower.trim_start();
    let offset = lower.len() - lower_trimmed.len();
    pos += offset;

    // Handle "other" prefix: "other creatures", "other nonland permanents"
    if lower_trimmed.starts_with("other ") {
        properties.push(FilterProp::Another);
        pos += offset + "other ".len();
    }

    if let Some((prop, consumed)) = parse_combat_status_prefix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    // Handle color prefix: "white creature", "red spell", etc.
    let color_prop = parse_color_prefix(&lower[pos..]);
    if let Some((ref prop, color_len)) = color_prop {
        properties.push(prop.clone());
        pos += color_len;
    }

    // Handle "non" prefix
    let (negated_type, non_prefix) = parse_non_prefix(&lower[pos..]);
    if non_prefix > 0 {
        pos += non_prefix;
    }

    // Parse the core type
    let (card_type, subtype, type_len) = parse_core_type(&lower[pos..]);
    pos += type_len;

    if let Some(neg) = negated_type {
        properties.push(FilterProp::NonType { value: neg });
    }

    if let Some(consumed) = parse_token_suffix(&lower[pos..]) {
        properties.push(FilterProp::Token);
        pos += consumed;
    }

    // CR 205.3a: Comma-separated type lists ("artifacts, creatures, and lands") are
    // syntactic sugar for set-union, same as "and" between two types.
    let rest_lower = lower[pos..].trim_start();
    let rest_offset = lower[pos..].len() - rest_lower.len();

    // Check ", and " first (Oxford comma before final element) since it starts with ", "
    if let Some(after_comma_and) = rest_lower.strip_prefix(", and ") {
        let after_trimmed = after_comma_and.trim_start();
        if parse_core_type(after_trimmed).0.is_some() {
            let comma_and_text = &text[pos + rest_offset + ", and ".len()..];
            let (other_filter, final_rest) = parse_type_phrase(comma_and_text);
            let left = typed(card_type.unwrap_or(TypeFilter::Any), subtype, properties);
            let combined = merge_or_filters(left, other_filter);
            return (distribute_controller_to_or(combined), final_rest);
        }
    }

    // CR 205.3a: Comma between non-final elements ("artifacts, creatures, ...")
    if let Some(after_comma) = rest_lower.strip_prefix(", ") {
        let after_trimmed = after_comma.trim_start();
        if parse_core_type(after_trimmed).0.is_some() {
            let comma_text = &text[pos + rest_offset + ", ".len()..];
            let (other_filter, final_rest) = parse_type_phrase(comma_text);
            let left = typed(card_type.unwrap_or(TypeFilter::Any), subtype, properties);
            let combined = merge_or_filters(left, other_filter);
            return (distribute_controller_to_or(combined), final_rest);
        }
    }

    // Check for "or" combinator: "artifact or enchantment", "creature or artifact you control"
    if rest_lower.starts_with("or ") {
        let or_text = &text[pos + rest_offset + 3..];
        let (other_filter, final_rest) = parse_type_phrase(or_text);
        let mut left = typed(card_type.unwrap_or(TypeFilter::Any), subtype, properties);

        // Distribute shared controller suffix from right branch to left:
        // "creature or artifact you control" → both get "you control"
        if let TargetFilter::Typed(TypedFilter {
            controller: Some(ref ctrl),
            ..
        }) = other_filter
        {
            if let TargetFilter::Typed(TypedFilter {
                controller: ref mut left_ctrl,
                ..
            }) = left
            {
                if left_ctrl.is_none() {
                    *left_ctrl = Some(ctrl.clone());
                }
            }
        }

        return (
            TargetFilter::Or {
                filters: vec![left, other_filter],
            },
            final_rest,
        );
    }

    // CR 205.3a: Oracle "and" between type words is set-union ("artifacts and creatures"
    // = any object that is an artifact OR a creature), not set-intersection.
    // TargetFilter::Or is correct here.
    // Only recurse when the word after "and" is a recognized card type — prevents
    // false matches on effect text like "destroy target creature and draw a card".
    if let Some(after_and_kw) = rest_lower.strip_prefix("and ") {
        let after_and = after_and_kw.trim_start();
        let (next_type, _, _) = parse_core_type(after_and);
        if next_type.is_some() {
            let and_text = &text[pos + rest_offset + 4..];
            let (other_filter, final_rest) = parse_type_phrase(and_text);
            let mut left = typed(card_type.unwrap_or(TypeFilter::Any), subtype, properties);

            // Distribute shared controller suffix from right branch to left
            if let TargetFilter::Typed(TypedFilter {
                controller: Some(ref ctrl),
                ..
            }) = other_filter
            {
                if let TargetFilter::Typed(TypedFilter {
                    controller: ref mut left_ctrl,
                    ..
                }) = left
                {
                    if left_ctrl.is_none() {
                        *left_ctrl = Some(ctrl.clone());
                    }
                }
            }

            return (
                TargetFilter::Or {
                    filters: vec![left, other_filter],
                },
                final_rest,
            );
        }
    }

    // CR 108.3 + CR 110.2: Ownership and control are distinct; "you own and control" satisfies both.
    let mut controller = None;
    let own_ctrl = lower[pos..].trim_start();
    let own_ctrl_offset = lower[pos..].len() - own_ctrl.len();
    if own_ctrl.starts_with("you own and control") {
        controller = Some(ControllerRef::You);
        properties.push(FilterProp::Owned {
            controller: ControllerRef::You,
        });
        pos += own_ctrl_offset + "you own and control".len();
    } else if own_ctrl.starts_with("you own") && !own_ctrl.starts_with("you own and") {
        properties.push(FilterProp::Owned {
            controller: ControllerRef::You,
        });
        pos += own_ctrl_offset + "you own".len();
    } else {
        let (ctrl, ctrl_len) =
            parse_controller_suffix(&lower[pos..]).map_or((None, 0), |(c, len)| (Some(c), len));
        controller = ctrl;
        pos += ctrl_len;
    }

    // Check "with power N or less/greater" suffix
    if let Some((prop, consumed)) = parse_mana_value_suffix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    // Check "with power N or less/greater" suffix
    if let Some((prop, consumed)) = parse_power_suffix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    // Check "with [counter] counter(s) on it/them" suffix
    if let Some((prop, consumed)) = parse_counter_suffix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    if let Some((keyword_props, consumed)) = parse_keyword_suffix(&lower[pos..]) {
        properties.extend(keyword_props);
        pos += consumed;
    }

    // Check zone suffix: "card from a graveyard", "card in your graveyard", "from exile", etc.
    if let Some((zone_prop, zone_ctrl, consumed)) = parse_zone_suffix(&lower[pos..]) {
        properties.push(zone_prop);
        pos += consumed;
        // Apply zone-derived controller if we don't already have one
        if controller.is_none() {
            controller = zone_ctrl;
        }
    }

    // Check "of the chosen type" suffix (Cavern of Souls, Metallic Mimic, etc.)
    let remaining = lower[pos..].trim_start();
    let remaining_offset = lower[pos..].len() - remaining.len();
    if remaining.starts_with("of the chosen type") {
        properties.push(FilterProp::IsChosenCreatureType);
        pos += remaining_offset + "of the chosen type".len();
    }

    let filter = TargetFilter::Typed(TypedFilter {
        card_type,
        subtype,
        controller,
        properties,
    });

    (filter, &text[pos..])
}

fn parse_non_prefix(text: &str) -> (Option<String>, usize) {
    if let Some(rest) = text.strip_prefix("non") {
        // Strip optional hyphen: "non-Human" and "nonland" both valid
        let rest = rest.strip_prefix('-').unwrap_or(rest);
        let consumed_prefix = text.len() - rest.len(); // "non" or "non-"
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        let negated = rest[..end].to_string();
        // We consumed "non[-]{type} " but the core type is the NEXT word, so return just the negated type
        (
            Some(negated),
            consumed_prefix + end + if rest.len() > end { 1 } else { 0 },
        )
    } else {
        (None, 0)
    }
}

/// Distribute the controller from the last `Typed` element in an `Or` filter
/// to all preceding `Typed` elements that have `controller: None`.
/// Handles "artifacts, creatures, and lands your opponents control" where only
/// the final type parses the controller suffix.
fn distribute_controller_to_or(filter: TargetFilter) -> TargetFilter {
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

fn parse_core_type(text: &str) -> (Option<TypeFilter>, Option<String>, usize) {
    let types: &[(&str, TypeFilter)] = &[
        ("creatures", TypeFilter::Creature),
        ("creature", TypeFilter::Creature),
        ("permanents", TypeFilter::Permanent),
        ("permanent", TypeFilter::Permanent),
        ("artifacts", TypeFilter::Artifact),
        ("artifact", TypeFilter::Artifact),
        ("enchantments", TypeFilter::Enchantment),
        ("enchantment", TypeFilter::Enchantment),
        ("instants", TypeFilter::Instant),
        ("instant", TypeFilter::Instant),
        ("sorceries", TypeFilter::Sorcery),
        ("sorcery", TypeFilter::Sorcery),
        ("planeswalkers", TypeFilter::Planeswalker),
        ("planeswalker", TypeFilter::Planeswalker),
        ("lands", TypeFilter::Land),
        ("land", TypeFilter::Land),
        ("spells", TypeFilter::Card),
        ("spell", TypeFilter::Card),
        ("cards", TypeFilter::Card),
        ("card", TypeFilter::Card),
    ];

    for (word, tf) in types {
        if text.starts_with(word) {
            return (Some(tf.clone()), None, word.len());
        }
    }

    (None, None, 0)
}

/// Parse a controller suffix like " you control", " an opponent controls", " your opponents control".
/// Returns `(ControllerRef, bytes_consumed)` where consumed includes leading whitespace.
fn parse_controller_suffix(text: &str) -> Option<(ControllerRef, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();
    if trimmed.starts_with("you control") {
        Some((ControllerRef::You, leading_ws + "you control".len()))
    } else if trimmed.starts_with("your opponents control") {
        Some((
            ControllerRef::Opponent,
            leading_ws + "your opponents control".len(),
        ))
    } else if trimmed.starts_with("an opponent controls") {
        Some((
            ControllerRef::Opponent,
            leading_ws + "an opponent controls".len(),
        ))
    } else {
        None
    }
}

fn parse_token_suffix(text: &str) -> Option<usize> {
    let trimmed = text.trim_start();

    for prefix in ["tokens", "token"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            if rest.is_empty()
                || rest.starts_with(|c: char| c.is_whitespace() || c == ',' || c == '.')
            {
                return Some(text.len() - rest.len());
            }
        }
    }

    None
}

/// Parse a color adjective prefix: "white ", "blue ", "black ", "red ", "green ".
/// Returns (FilterProp::HasColor, bytes consumed including trailing space).
fn parse_color_prefix(text: &str) -> Option<(FilterProp, usize)> {
    let colors = [
        ("white ", "White"),
        ("blue ", "Blue"),
        ("black ", "Black"),
        ("red ", "Red"),
        ("green ", "Green"),
    ];
    for (prefix, color_name) in &colors {
        if text.starts_with(prefix) {
            return Some((
                FilterProp::HasColor {
                    color: color_name.to_string(),
                },
                prefix.len(),
            ));
        }
    }
    None
}

fn parse_combat_status_prefix(text: &str) -> Option<(FilterProp, usize)> {
    for (prefix, prop) in [("attacking ", FilterProp::Attacking)] {
        if text.starts_with(prefix) {
            return Some((prop, prefix.len()));
        }
    }

    None
}

/// Parse "with power N or less" / "with power N or greater" suffix.
/// Returns (FilterProp, bytes consumed from the original text).
fn parse_power_suffix(text: &str) -> Option<(FilterProp, usize)> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix("with power ")?;
    let num_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if num_end == 0 {
        return None;
    }
    let value: i32 = rest[..num_end].parse().ok()?;
    let after_num = rest[num_end..].trim_start();

    let (prop, after) = if let Some(a) = after_num.strip_prefix("or less") {
        (FilterProp::PowerLE { value }, a)
    } else if let Some(a) = after_num.strip_prefix("or greater") {
        (FilterProp::PowerGE { value }, a)
    } else {
        return None;
    };
    Some((prop, text.len() - after.len()))
}

/// Parse "with mana value N or less" / "with mana value N or greater" suffix.
/// Returns (FilterProp, bytes consumed from the original text).
fn parse_mana_value_suffix(text: &str) -> Option<(FilterProp, usize)> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix("with mana value ")?;
    let num_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if num_end == 0 {
        return None;
    }
    let value: u32 = rest[..num_end].parse().ok()?;
    let after_num = rest[num_end..].trim_start();

    let (prop, after) = if let Some(a) = after_num.strip_prefix("or greater") {
        (FilterProp::CmcGE { value }, a)
    } else if let Some(a) = after_num.strip_prefix("or less") {
        (FilterProp::CmcLE { value }, a)
    } else {
        return None;
    };
    Some((prop, text.len() - after.len()))
}

/// Parse "with [counter] counter(s) on it/them".
/// Returns (FilterProp, bytes consumed from the original text).
fn parse_counter_suffix(text: &str) -> Option<(FilterProp, usize)> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix("with ")?;

    for suffix in [
        " counters on them",
        " counters on it",
        " counter on them",
        " counter on it",
    ] {
        let Some(counter_end) = rest.find(suffix) else {
            continue;
        };
        let mut counter_type = rest[..counter_end].trim();
        counter_type = counter_type
            .strip_prefix("an ")
            .or_else(|| counter_type.strip_prefix("a "))
            .unwrap_or(counter_type)
            .trim();

        if counter_type.is_empty() {
            continue;
        }

        let consumed = text.len() - rest[counter_end + suffix.len()..].len();
        return Some((
            FilterProp::CountersGE {
                counter_type: counter_type.to_string(),
                count: 1,
            },
            consumed,
        ));
    }

    None
}

fn parse_keyword_suffix(text: &str) -> Option<(Vec<FilterProp>, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();
    let mut remaining = trimmed.strip_prefix("with ")?;
    let mut consumed = leading_ws + "with ".len();
    let mut properties = Vec::new();

    while let Some((keyword, keyword_len)) = parse_leading_keyword(remaining) {
        properties.push(FilterProp::WithKeyword {
            value: keyword.to_string(),
        });
        consumed += keyword_len;
        remaining = &remaining[keyword_len..];

        if let Some(rest) = remaining.strip_prefix(", and ") {
            consumed += ", and ".len();
            remaining = rest;
            continue;
        }
        if let Some(rest) = remaining.strip_prefix(" and ") {
            consumed += " and ".len();
            remaining = rest;
            continue;
        }
        if let Some(rest) = remaining.strip_prefix(", ") {
            consumed += ", ".len();
            remaining = rest;
            continue;
        }

        break;
    }

    if properties.is_empty() {
        None
    } else {
        Some((properties, consumed))
    }
}

fn parse_leading_keyword(text: &str) -> Option<(&str, usize)> {
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
        if is_recognized_keyword(candidate) {
            return Some((candidate, leading_ws + end));
        }
    }

    None
}

fn is_recognized_keyword(text: &str) -> bool {
    matches!(
        Keyword::from_str(text),
        Ok(keyword) if !matches!(keyword, Keyword::Unknown(_))
    ) || matches!(
        text,
        "plainswalk" | "islandwalk" | "swampwalk" | "mountainwalk" | "forestwalk"
    )
}

fn typed(
    card_type: TypeFilter,
    subtype: Option<String>,
    properties: Vec<FilterProp>,
) -> TargetFilter {
    TargetFilter::Typed(TypedFilter {
        card_type: Some(card_type),
        subtype,
        controller: None,
        properties,
    })
}

/// Parse a zone suffix like "card from a graveyard", "from your graveyard", "from exile".
/// Returns (FilterProp::InZone, optional ControllerRef, bytes consumed).
///
/// Handles:
/// - Possessive: "from your graveyard", "from their graveyard", "from its owner's graveyard"
/// - Indefinite: "from a graveyard", "in a graveyard"
/// - Direct: "from exile", "in exile"
///
/// Skips optional leading "card"/"cards" before zone detection.
fn parse_zone_suffix(text: &str) -> Option<(FilterProp, Option<ControllerRef>, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();

    // Skip optional "card"/"cards" before zone preposition
    let (after_card, card_skip) = if let Some(rest) = trimmed.strip_prefix("cards ") {
        (rest, "cards ".len())
    } else if let Some(rest) = trimmed.strip_prefix("card ") {
        (rest, "card ".len())
    } else {
        (trimmed, 0)
    };

    let zones: &[(&str, &str, Zone)] = &[
        ("graveyard", "graveyards", Zone::Graveyard),
        ("exile", "exiles", Zone::Exile),
        ("hand", "hands", Zone::Hand),
        ("library", "libraries", Zone::Library),
    ];

    for prep in &["from", "in"] {
        for &(zone_word, zone_plural, ref zone) in zones {
            // Possessive: "from your graveyard", "from their graveyard"
            if contains_possessive(after_card, prep, zone_word) {
                let pattern = format!("{prep} your {zone_word}");
                let ctrl = if after_card.to_lowercase().contains(&pattern) {
                    Some(ControllerRef::You)
                } else {
                    None
                };
                // Find end of the zone word in after_card
                let zone_end = after_card
                    .to_lowercase()
                    .find(zone_word)
                    .map(|i| i + zone_word.len())
                    .unwrap_or(after_card.len());
                return Some((
                    FilterProp::InZone { zone: *zone },
                    ctrl,
                    leading_ws + card_skip + zone_end,
                ));
            }

            // Indefinite: "from a graveyard", "in a graveyard"
            let indef = format!("{prep} a {zone_word}");
            if after_card.to_lowercase().starts_with(&indef) {
                return Some((
                    FilterProp::InZone { zone: *zone },
                    None,
                    leading_ws + card_skip + indef.len(),
                ));
            }

            // Direct (no article): "from exile", "in graveyards"
            for direct in [
                format!("{prep} {zone_word}"),
                format!("{prep} {zone_plural}"),
            ] {
                if after_card.to_lowercase().starts_with(&direct) {
                    // Make sure it's not a possessive that we missed
                    let after = &after_card[direct.len()..];
                    if after.is_empty()
                        || after.starts_with(|c: char| c.is_whitespace() || c == ',' || c == '.')
                    {
                        return Some((
                            FilterProp::InZone { zone: *zone },
                            None,
                            leading_ws + card_skip + direct.len(),
                        ));
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_target() {
        let (f, rest) = parse_target("any target");
        assert_eq!(f, TargetFilter::Any);
        assert_eq!(rest, "");
    }

    #[test]
    fn target_creature() {
        let (f, _) = parse_target("target creature");
        assert_eq!(f, TargetFilter::Typed(TypedFilter::creature()));
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
    fn attacking_creatures_you_control() {
        let (f, rest) = parse_type_phrase("attacking creatures you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Attacking])
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
                TypedFilter::permanent().properties(vec![FilterProp::NonType {
                    value: "land".to_string()
                }])
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
    fn enchanted_creature() {
        let (f, _) = parse_target("enchanted creature");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]))
        );
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
    fn white_creature_you_control() {
        let (f, _) = parse_type_phrase("white creature you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::HasColor {
                        color: "White".to_string()
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
                color: "Red".to_string()
            }]))
        );
    }

    #[test]
    fn spell_with_mana_value_4_or_greater() {
        let (f, _) = parse_type_phrase("spell with mana value 4 or greater");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::CmcGE { value: 4 }])
            )
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
                    .properties(vec![FilterProp::PowerLE { value: 2 }])
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
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::PowerGE { value: 3 }])
            )
        );
    }

    #[test]
    fn creatures_with_ice_counters_on_them() {
        let (f, _) = parse_type_phrase("creatures with ice counters on them");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::CountersGE {
                    counter_type: "ice".to_string(),
                    count: 1,
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
                        value: "flying".to_string(),
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
                    value: "first strike".to_string(),
                },
                FilterProp::WithKeyword {
                    value: "vigilance".to_string(),
                },
            ]))
        );
    }

    #[test]
    fn other_nonland_permanents_you_own_and_control() {
        let (f, _) = parse_type_phrase("other nonland permanents you own and control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent()
                    .controller(ControllerRef::You)
                    .properties(vec![
                        FilterProp::Another,
                        FilterProp::NonType {
                            value: "land".to_string(),
                        },
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

    // ── Bare type phrase fallback ──

    #[test]
    fn bare_type_phrase_fallback() {
        let (f, _) = parse_target("other nonland permanents you own and control");
        // Should be Typed (not Any) — parse_type_phrase picks up the permanent type + properties
        match f {
            TargetFilter::Typed(tf) => {
                assert!(
                    tf.card_type.is_some() || !tf.properties.is_empty(),
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
    fn parse_event_context_that_spells_controller() {
        let filter = parse_event_context_ref("that spell's controller");
        assert_eq!(filter, Some(TargetFilter::TriggeringSpellController));
    }

    #[test]
    fn parse_event_context_that_spells_owner() {
        let filter = parse_event_context_ref("that spell's owner");
        assert_eq!(filter, Some(TargetFilter::TriggeringSpellOwner));
    }

    #[test]
    fn parse_event_context_that_player() {
        let filter = parse_event_context_ref("that player");
        assert_eq!(filter, Some(TargetFilter::TriggeringPlayer));
    }

    #[test]
    fn parse_event_context_that_source() {
        let filter = parse_event_context_ref("that source");
        assert_eq!(filter, Some(TargetFilter::TriggeringSource));
    }

    #[test]
    fn parse_event_context_that_permanent() {
        let filter = parse_event_context_ref("that permanent");
        assert_eq!(filter, Some(TargetFilter::TriggeringSource));
    }

    #[test]
    fn parse_event_context_returns_none_for_non_event() {
        assert_eq!(parse_event_context_ref("target creature"), None);
        assert_eq!(parse_event_context_ref("any target"), None);
    }

    #[test]
    fn parse_event_context_defending_player() {
        let filter = parse_event_context_ref("defending player");
        assert_eq!(filter, Some(TargetFilter::DefendingPlayer));
    }

    #[test]
    fn parse_event_context_defending_player_prefix() {
        let filter = parse_event_context_ref("defending player reveals the top card");
        assert_eq!(filter, Some(TargetFilter::DefendingPlayer));
    }

    #[test]
    fn parse_counter_suffix_stun_counter() {
        let result = parse_counter_suffix(" with a stun counter on it");
        assert!(result.is_some());
        let (prop, _consumed) = result.unwrap();
        assert!(matches!(
            prop,
            FilterProp::CountersGE {
                ref counter_type,
                count: 1,
            } if counter_type == "stun"
        ));
    }

    #[test]
    fn parse_counter_suffix_oil_counter() {
        let result = parse_counter_suffix(" with an oil counter on it");
        assert!(result.is_some());
        let (prop, _consumed) = result.unwrap();
        assert!(matches!(
            prop,
            FilterProp::CountersGE {
                ref counter_type,
                count: 1,
            } if counter_type == "oil"
        ));
    }

    #[test]
    fn parse_counter_suffix_not_counter_phrase() {
        let result = parse_counter_suffix(" with power 3 or greater");
        assert!(result.is_none());
    }

    #[test]
    fn parse_type_phrase_creature_with_stun_counter() {
        let (filter, _rest) = parse_type_phrase("creature with a stun counter on it");
        match filter {
            TargetFilter::Typed(TypedFilter {
                card_type,
                properties,
                ..
            }) => {
                assert_eq!(card_type, Some(TypeFilter::Creature));
                assert!(properties.iter().any(|p| matches!(
                    p,
                    FilterProp::CountersGE {
                        ref counter_type,
                        count: 1,
                    } if counter_type == "stun"
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
        let (f, rest) =
            parse_type_phrase("artifacts, creatures, and lands your opponents control");
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
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(TypedFilter::creature())
                );
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
        let (f, rest) =
            parse_type_phrase("creatures, artifacts, and enchantments you control");
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
    fn comma_list_four_elements() {
        let (f, rest) =
            parse_type_phrase("artifacts, creatures, enchantments, and lands");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 4);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(TypedFilter::creature())
                );
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
    fn comma_list_no_oxford_comma() {
        let (f, rest) =
            parse_type_phrase("artifacts, creatures and lands your opponents control");
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
        let (f, rest) =
            parse_type_phrase("artifacts, creatures, and lands enter tapped");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3);
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest, " enter tapped");
    }
}
