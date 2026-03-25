use std::str::FromStr;

use crate::types::ability::{
    ControllerRef, FilterProp, QuantityExpr, QuantityRef, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::identifiers::TrackedSetId;
use crate::types::keywords::Keyword;
use crate::types::zones::Zone;

use super::oracle_quantity::capitalize_first;
use super::oracle_util::{
    merge_or_filters, parse_subtype, starts_with_possessive, TextPair, SELF_REF_TYPE_PHRASES,
};

/// Parse an event-context possessive reference from Oracle text.
/// These resolve from the triggering event, not from player targeting.
/// Must be checked BEFORE standard `parse_target` for trigger-based effects.
/// CR 608.2k: Parse event-context references ("that player", "that permanent", etc.)
/// that refer back to objects/players mentioned in a trigger condition or cost.
/// Returns the matched filter and unconsumed remainder text.
pub fn parse_event_context_ref(text: &str) -> Option<(TargetFilter, &str)> {
    let text = text.trim();
    let lower = text.to_lowercase();

    // Longest-match-first ordering within shared prefixes.
    if let Some(rest) = lower.strip_prefix("that spell's controller") {
        return Some((
            TargetFilter::TriggeringSpellController,
            &text[text.len() - rest.len()..],
        ));
    }
    if let Some(rest) = lower.strip_prefix("that spell's owner") {
        return Some((
            TargetFilter::TriggeringSpellOwner,
            &text[text.len() - rest.len()..],
        ));
    }
    if let Some(rest) = lower.strip_prefix("that player") {
        return Some((
            TargetFilter::TriggeringPlayer,
            &text[text.len() - rest.len()..],
        ));
    }
    if let Some(rest) = lower.strip_prefix("that source") {
        return Some((
            TargetFilter::TriggeringSource,
            &text[text.len() - rest.len()..],
        ));
    }
    // "that permanent or player" before "that permanent" — longest match first.
    if let Some(rest) = lower.strip_prefix("that permanent or player") {
        return Some((
            TargetFilter::TriggeringSource,
            &text[text.len() - rest.len()..],
        ));
    }
    if let Some(rest) = lower.strip_prefix("that permanent") {
        return Some((
            TargetFilter::TriggeringSource,
            &text[text.len() - rest.len()..],
        ));
    }
    // CR 506.3d: "defending player" — the player being attacked by the source creature.
    if let Some(rest) = lower.strip_prefix("defending player") {
        return Some((
            TargetFilter::DefendingPlayer,
            &text[text.len() - rest.len()..],
        ));
    }

    None
}

/// Parse a target description from Oracle text, returning (filter, remaining_text).
/// Consumes the longest matching target phrase.
///
/// Uses first-character dispatch to group `starts_with` checks, reducing average
/// comparisons from ~12 to ~3-4 per call.
pub fn parse_target(text: &str) -> (TargetFilter, &str) {
    let text = text.trim_start();
    let lower = text.to_lowercase();

    // First-character dispatch for prefix matching
    match lower.as_bytes().first().copied() {
        // "~" — self-reference (normalized from card name)
        Some(b'~') => {
            return (TargetFilter::SelfRef, text[1..].trim_start());
        }

        // "a" — "any target", "all " + type phrase
        Some(b'a') => {
            if lower.starts_with("any target") {
                return (TargetFilter::Any, &text[10..]);
            }
            if lower.starts_with("all ") {
                let (filter, rest) = parse_type_phrase(&text[4..]);
                return (filter, rest);
            }
        }

        // "t" — "this creature" (self-ref), "target ...", "those ...", "the exiled ..."
        Some(b't') => {
            // "this creature", "this permanent", etc. — self-reference
            for phrase in SELF_REF_TYPE_PHRASES {
                if lower.starts_with(phrase) {
                    return (TargetFilter::SelfRef, &text[phrase.len()..]);
                }
            }
            if lower.starts_with("target ") {
                // Longest-match-first within "target" group
                if lower.starts_with("target player or planeswalker") {
                    return (
                        TargetFilter::Or {
                            filters: vec![
                                TargetFilter::Player,
                                typed(TypeFilter::Planeswalker, None, vec![], vec![]),
                            ],
                        },
                        &text[29..],
                    );
                }
                if lower.starts_with("target opponent") {
                    return (
                        TargetFilter::Typed(
                            TypedFilter::default().controller(ControllerRef::Opponent),
                        ),
                        &text[15..],
                    );
                }
                if lower.starts_with("target player") {
                    return (TargetFilter::Player, &text[13..]);
                }
                // "target" + type phrase (generic)
                let (filter, rest) = parse_type_phrase(&text[7..]);
                return (filter, rest);
            }
            // CR 603.7: Anaphoric tracked-set pronouns
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
        }

        // "e" — "each ...", "enchanted creature", "equipped creature"
        Some(b'e') => {
            if lower.starts_with("each opponent") {
                return (
                    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                    &text[13..],
                );
            }
            // CR 610.3: "each card exiled with ~" / "each card exiled with this <type>"
            if let Some(rest) = lower.strip_prefix("each card exiled with ~") {
                return (
                    TargetFilter::ExiledBySource,
                    &text[text.len() - rest.len()..],
                );
            }
            if let Some(rest) = lower.strip_prefix("each card exiled with this ") {
                let after_type = rest.find(' ').map_or("", |i| &rest[i..]);
                return (
                    TargetFilter::ExiledBySource,
                    &text[text.len() - after_type.len()..],
                );
            }
            if lower.starts_with("each ") {
                let (filter, rest) = parse_type_phrase(&text[5..]);
                return (filter, rest);
            }
            if lower.starts_with("enchanted creature") {
                return (
                    TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
                    ),
                    &text[18..],
                );
            }
            if lower.starts_with("equipped creature") {
                return (
                    TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
                    ),
                    &text[17..],
                );
            }
        }

        // "c" — "cards exiled with ~" / "cards exiled with this <type>"
        Some(b'c') => {
            if let Some(rest) = lower.strip_prefix("cards exiled with ~") {
                return (
                    TargetFilter::ExiledBySource,
                    &text[text.len() - rest.len()..],
                );
            }
            if let Some(rest) = lower.strip_prefix("cards exiled with this ") {
                let after_type = rest.find(' ').map_or("", |i| &rest[i..]);
                return (
                    TargetFilter::ExiledBySource,
                    &text[text.len() - after_type.len()..],
                );
            }
        }

        _ => {}
    }

    // "you" — the controller (not a targeted player)
    if lower.starts_with("you") && (lower.len() == 3 || lower[3..].starts_with([',', '.', ' '])) {
        return (TargetFilter::Controller, &text[3..]);
    }

    // Bare type phrase fallback: try parse_type_phrase before giving up.
    // Handles "other nonland permanents you own and control" after quantifier stripping.
    let (filter, rest) = parse_type_phrase(text);
    match &filter {
        // parse_type_phrase recognized a card type, subtype, or meaningful properties
        TargetFilter::Typed(tf) if !tf.type_filters.is_empty() || !tf.properties.is_empty() => {
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

    // Handle "other"/"another" prefix: "other creatures", "another creature",
    // "other nonland permanents", "another target creature"
    if lower_trimmed.starts_with("other ") {
        properties.push(FilterProp::Another);
        pos += offset + "other ".len();
    } else if lower_trimmed.starts_with("another ") {
        properties.push(FilterProp::Another);
        pos += offset + "another ".len();
    }

    // CR 509.1h: Consume combat status prefixes (unblocked, attacking)
    while let Some((prop, consumed)) = parse_combat_status_prefix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    // CR 205.4a: Parse supertype prefix: "legendary", "basic", "snow"
    // Must come BEFORE color prefix so "legendary white creature" works:
    // supertype consumed first, then color at the new position.
    for &(prefix, supertype_name) in &[
        ("legendary ", "Legendary"),
        ("basic ", "Basic"),
        ("snow ", "Snow"),
    ] {
        if lower[pos..].starts_with(prefix) {
            properties.push(FilterProp::HasSupertype {
                value: supertype_name.to_string(),
            });
            pos += prefix.len();
            break;
        }
    }

    // Handle color prefix: "white creature", "red spell", etc.
    let color_prop = parse_color_prefix(&lower[pos..]);
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
        let Some(after_non) = remaining.strip_prefix("non") else {
            break;
        };
        let after_non = after_non.strip_prefix('-').unwrap_or(after_non);
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
        if let Some(rest) = lower[pos..].strip_prefix(", ") {
            if rest.starts_with("non") {
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

    // Skip redundant trailing "spell"/"spells"/"card"/"cards" after a specific type like
    // "sorcery spell", "creature card". When the core type is already Instant/Sorcery/etc.,
    // the word is informational — consuming it allows suffix parsers (e.g., "that targets only")
    // and event verb parsers to see what follows.
    if card_type.is_some() && !matches!(card_type, Some(TypeFilter::Card) | Some(TypeFilter::Any)) {
        let rest_trimmed = lower[pos..].trim_start();
        let ws_len = lower[pos..].len() - rest_trimmed.len();
        // CR 108.1: "spell" and "card" are informational suffixes after a typed qualifier.
        let redundant_suffixes = ["spells ", "spell ", "cards ", "card "];
        let mut consumed_suffix = false;
        for suffix in &redundant_suffixes {
            if let Some(after) = rest_trimmed.strip_prefix(suffix) {
                let suffix_len = rest_trimmed.len() - after.len();
                pos += ws_len + suffix_len;
                consumed_suffix = true;
                break;
            }
        }
        if !consumed_suffix {
            // Check end-of-input variants (no trailing space)
            for suffix in &["spell", "spells", "card", "cards"] {
                if rest_trimmed == *suffix {
                    pos += ws_len + suffix.len();
                    break;
                }
            }
        }
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
        if starts_with_type_word(after_trimmed) {
            let comma_and_text = &text[pos + rest_offset + ", and ".len()..];
            let (other_filter, final_rest) = parse_type_phrase(comma_and_text);
            let left = typed(
                card_type.unwrap_or(TypeFilter::Any),
                subtype,
                properties,
                neg_type_filters.clone(),
            );
            let combined = merge_or_filters(left, other_filter);
            let combined = distribute_controller_to_or(combined);
            return (distribute_properties_to_or(combined), final_rest);
        }
    }

    // CR 205.3a: Oxford comma before "or" in type lists ("artifact, creature, or enchantment")
    if let Some(after_comma_or) = rest_lower.strip_prefix(", or ") {
        let after_trimmed = after_comma_or.trim_start();
        if starts_with_type_word(after_trimmed) {
            let comma_or_text = &text[pos + rest_offset + ", or ".len()..];
            let (other_filter, final_rest) = parse_type_phrase(comma_or_text);
            let left = typed(
                card_type.unwrap_or(TypeFilter::Any),
                subtype,
                properties,
                neg_type_filters.clone(),
            );
            let combined = merge_or_filters(left, other_filter);
            let combined = distribute_controller_to_or(combined);
            return (distribute_properties_to_or(combined), final_rest);
        }
    }

    // CR 205.3a: Comma between non-final elements ("artifacts, creatures, ...")
    if let Some(after_comma) = rest_lower.strip_prefix(", ") {
        let after_trimmed = after_comma.trim_start();
        if starts_with_type_word(after_trimmed) {
            let comma_text = &text[pos + rest_offset + ", ".len()..];
            let (other_filter, final_rest) = parse_type_phrase(comma_text);
            let left = typed(
                card_type.unwrap_or(TypeFilter::Any),
                subtype,
                properties,
                neg_type_filters.clone(),
            );
            let combined = merge_or_filters(left, other_filter);
            let combined = distribute_controller_to_or(combined);
            return (distribute_properties_to_or(combined), final_rest);
        }
    }

    // Check for "or" combinator: "artifact or enchantment", "creature or artifact you control"
    if rest_lower.starts_with("or ") {
        let or_text = &text[pos + rest_offset + 3..];
        let (other_filter, final_rest) = parse_type_phrase(or_text);
        let mut left = typed(
            card_type.unwrap_or(TypeFilter::Any),
            subtype,
            properties,
            neg_type_filters.clone(),
        );

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

        // Distribute shared properties from right branch to left:
        // "artifacts or creatures with mana value 2 or less" → both get CmcLE(2)
        if let TargetFilter::Typed(TypedFilter {
            properties: ref right_props,
            ..
        }) = other_filter
        {
            if let TargetFilter::Typed(TypedFilter {
                properties: ref mut left_props,
                ..
            }) = left
            {
                for prop in right_props {
                    if !left_props.iter().any(|p| prop.same_kind(p)) {
                        left_props.push(prop.clone());
                    }
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

    // CR 601.2d: "and/or" between types — for filter purposes, equivalent to Or.
    // Must be checked BEFORE "and " to prevent "and " from consuming "and/or ".
    if let Some(after_and_or) = rest_lower.strip_prefix("and/or ") {
        let after_trimmed = after_and_or.trim_start();
        if starts_with_type_word(after_trimmed) {
            let and_or_text = &text[pos + rest_offset + "and/or ".len()..];
            let (other_filter, final_rest) = parse_type_phrase(and_or_text);
            let left = typed(
                card_type.unwrap_or(TypeFilter::Any),
                subtype,
                properties,
                neg_type_filters.clone(),
            );
            let combined = merge_or_filters(left, other_filter);
            let combined = distribute_controller_to_or(combined);
            return (distribute_properties_to_or(combined), final_rest);
        }
    }

    // CR 205.3a: Oracle "and" between type words is set-union ("artifacts and creatures"
    // = any object that is an artifact OR a creature), not set-intersection.
    // TargetFilter::Or is correct here.
    // Only recurse when the word after "and" is a recognized card type — prevents
    // false matches on effect text like "destroy target creature and draw a card".
    if let Some(after_and_kw) = rest_lower.strip_prefix("and ") {
        let after_and = after_and_kw.trim_start();
        if starts_with_type_word(after_and) {
            let and_text = &text[pos + rest_offset + 4..];
            let (other_filter, final_rest) = parse_type_phrase(and_text);
            let mut left = typed(
                card_type.unwrap_or(TypeFilter::Any),
                subtype,
                properties,
                neg_type_filters.clone(),
            );

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

            // Distribute shared properties from right branch to left
            if let TargetFilter::Typed(TypedFilter {
                properties: ref right_props,
                ..
            }) = other_filter
            {
                if let TargetFilter::Typed(TypedFilter {
                    properties: ref mut left_props,
                    ..
                }) = left
                {
                    for prop in right_props {
                        if !left_props.iter().any(|p| prop.same_kind(p)) {
                            left_props.push(prop.clone());
                        }
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

    // CR 700.5: "that share(s) a creature type" / "that has/have [keyword]" relative clause.
    if let Some((that_props, consumed)) = parse_that_clause_suffix(&lower[pos..]) {
        properties.extend(that_props);
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

    // CR 608.2d: "of their choice" / "of his or her choice" — informational qualifier
    // on opponent-choice effects. The actual choice is handled by the WaitingFor state machine.
    let remaining_choice = lower[pos..].trim_start();
    let choice_offset = lower[pos..].len() - remaining_choice.len();
    for suffix in &["of their choice", "of his or her choice"] {
        if remaining_choice.starts_with(suffix) {
            pos += choice_offset + suffix.len();
            break;
        }
    }

    let filter = TargetFilter::Typed(TypedFilter {
        type_filters: [
            card_type.map(|ct| vec![ct]).unwrap_or_default(),
            subtype
                .map(|s| vec![TypeFilter::Subtype(s)])
                .unwrap_or_default(),
            neg_type_filters,
        ]
        .concat(),
        controller,
        properties,
    });

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
    match negated {
        // Color negation — parallel to HasColor
        "white" => NegationResult::Prop(FilterProp::NotColor {
            color: "White".to_string(),
        }),
        "blue" => NegationResult::Prop(FilterProp::NotColor {
            color: "Blue".to_string(),
        }),
        "black" => NegationResult::Prop(FilterProp::NotColor {
            color: "Black".to_string(),
        }),
        "red" => NegationResult::Prop(FilterProp::NotColor {
            color: "Red".to_string(),
        }),
        "green" => NegationResult::Prop(FilterProp::NotColor {
            color: "Green".to_string(),
        }),
        // CR 205.4a: Supertype negation — parallel to HasSupertype
        "basic" => NegationResult::Prop(FilterProp::NotSupertype {
            value: "Basic".to_string(),
        }),
        "legendary" => NegationResult::Prop(FilterProp::NotSupertype {
            value: "Legendary".to_string(),
        }),
        "snow" => NegationResult::Prop(FilterProp::NotSupertype {
            value: "Snow".to_string(),
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

/// Guard: does text start with something `parse_type_phrase` would recognize?
/// Used to prevent comma/and/or recursion on non-type text.
fn starts_with_type_word(text: &str) -> bool {
    // Core type: "creature", "artifact", "permanent", etc.
    if parse_core_type(text).0.is_some() {
        return true;
    }
    // Subtype: "zombie", "vampires", "elves", etc.
    if parse_subtype(text).is_some() {
        return true;
    }
    // CR 205.4b: Negated type prefix: "noncreature spell", "nonland permanent"
    if let Some(after_non) = text.strip_prefix("non") {
        let after_non = after_non.strip_prefix('-').unwrap_or(after_non);
        if let Some(ws_pos) = after_non.find(|c: char| c.is_whitespace()) {
            let after_negated = after_non[ws_pos..].trim_start();
            return parse_core_type(after_negated).0.is_some();
        }
    }
    false
}

/// Distribute trailing filter properties (CmcLE, CmcGE, PowerLE, PowerGE, etc.)
/// from the last `Typed` element in an `Or` filter to all preceding `Typed`
/// elements that lack a property of the same kind.
/// Handles "artifacts and creatures with mana value 2 or less" where only the
/// final type parses the "with mana value N or less/greater" suffix.
fn distribute_properties_to_or(filter: TargetFilter) -> TargetFilter {
    let TargetFilter::Or { mut filters } = filter else {
        return filter;
    };

    // Collect properties from the last Typed element
    let trailing_props: Vec<FilterProp> = filters
        .iter()
        .rev()
        .find_map(|f| {
            if let TargetFilter::Typed(TypedFilter { properties, .. }) = f {
                if properties.is_empty() {
                    None
                } else {
                    Some(properties.clone())
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
    } else if trimmed.starts_with("that player controls") {
        // "that player controls" → ControllerRef::You, resolved against scope_player
        // at runtime by resolve_quantity_scoped() for per-player iteration contexts.
        Some((
            ControllerRef::You,
            leading_ws + "that player controls".len(),
        ))
    } else if trimmed.starts_with("they control") {
        // CR 608.2d: "they control" → ControllerRef::You, resolved against
        // accepting_player during "any opponent may" resolution.
        Some((ControllerRef::You, leading_ws + "they control".len()))
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

/// CR 509.1h / CR 302.6: Parse status prefixes from type phrases.
/// Called in a loop to consume multiple prefixes (e.g. "unblocked attacking ").
/// Handles combat status (attacking, unblocked) and tap status (tapped, untapped).
pub(crate) fn parse_combat_status_prefix(text: &str) -> Option<(FilterProp, usize)> {
    for (prefix, prop) in [
        ("unblocked ", FilterProp::Unblocked),
        ("attacking ", FilterProp::Attacking),
        // CR 302.6 / CR 110.5: Tapped/untapped status as targeting qualifier.
        ("tapped ", FilterProp::Tapped),
        ("untapped ", FilterProp::Untapped),
        // CR 707.2: Face-down status prefix for trigger subjects.
        ("face-down ", FilterProp::FaceDown),
    ] {
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

/// Parse "with mana value N or less" / "with mana value N or greater" suffix,
/// and dynamic "with mana value less than or equal to that [type]" patterns.
/// Returns (FilterProp, bytes consumed from the original text).
pub(crate) fn parse_mana_value_suffix(text: &str) -> Option<(FilterProp, usize)> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix("with mana value ")?;

    // CR 202.3: Dynamic comparisons referencing the triggering event source's mana value.
    // Staged checks: first detect "less than" / "greater than", then check for "or equal to".
    if let Some(a) = rest.strip_prefix("less than") {
        let a = a.trim_start();
        let (is_equal, a) = if let Some(a2) = a.strip_prefix("or equal to") {
            (true, a2.trim_start())
        } else {
            (false, a)
        };
        if let Some(a) = a.strip_prefix("that ") {
            let after = a.find([',', '.', ' ']).map_or(a, |i| &a[i..]);
            return Some((
                if is_equal {
                    FilterProp::CmcLE {
                        value: QuantityExpr::Ref {
                            qty: QuantityRef::EventContextSourceManaValue,
                        },
                    }
                } else {
                    // Strict "less than" → CmcLE with offset -1
                    FilterProp::CmcLE {
                        value: QuantityExpr::Offset {
                            inner: Box::new(QuantityExpr::Ref {
                                qty: QuantityRef::EventContextSourceManaValue,
                            }),
                            offset: -1,
                        },
                    }
                },
                text.len() - after.len(),
            ));
        }
    }
    if let Some(a) = rest.strip_prefix("greater than") {
        let a = a.trim_start();
        let (is_equal, a) = if let Some(a2) = a.strip_prefix("or equal to") {
            (true, a2.trim_start())
        } else {
            (false, a)
        };
        if let Some(a) = a.strip_prefix("that ") {
            let after = a.find([',', '.', ' ']).map_or(a, |i| &a[i..]);
            return Some((
                if is_equal {
                    FilterProp::CmcGE {
                        value: QuantityExpr::Ref {
                            qty: QuantityRef::EventContextSourceManaValue,
                        },
                    }
                } else {
                    // Strict "greater than" → CmcGE with offset +1
                    FilterProp::CmcGE {
                        value: QuantityExpr::Offset {
                            inner: Box::new(QuantityExpr::Ref {
                                qty: QuantityRef::EventContextSourceManaValue,
                            }),
                            offset: 1,
                        },
                    }
                },
                text.len() - after.len(),
            ));
        }
    }

    // Static "N or less" / "N or greater"
    let num_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if num_end == 0 {
        return None;
    }
    let value: u32 = rest[..num_end].parse().ok()?;
    let after_num = rest[num_end..].trim_start();

    let (prop, after) = if let Some(a) = after_num.strip_prefix("or greater") {
        (
            FilterProp::CmcGE {
                value: QuantityExpr::Fixed {
                    value: value as i32,
                },
            },
            a,
        )
    } else if let Some(a) = after_num.strip_prefix("or less") {
        (
            FilterProp::CmcLE {
                value: QuantityExpr::Fixed {
                    value: value as i32,
                },
            },
            a,
        )
    } else {
        // CR 202.3: Exact mana value match — "with mana value N" (no "or less"/"or greater").
        (
            FilterProp::CmcEQ {
                value: QuantityExpr::Fixed {
                    value: value as i32,
                },
            },
            after_num,
        )
    };
    Some((prop, text.len() - after.len()))
}

/// Parse "with [counter] counter(s) on it/them".
/// Returns (FilterProp, bytes consumed from the original text).
pub(crate) fn parse_counter_suffix(text: &str) -> Option<(FilterProp, usize)> {
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

/// Parse "that [verb phrase]" relative clause suffix on target noun phrases.
///
/// Handles multiple pattern classes:
/// - CR 700.5: "that share(s) [a] [quality]" → `SharesQuality`
/// - CR 510.1: "that was dealt damage this turn" → `WasDealtDamageThisTurn`
/// - CR 400.7: "that entered (the battlefield) this turn" → `EnteredThisTurn`
/// - CR 508.1a: "that attacked this turn" → `AttackedThisTurn`
/// - CR 509.1a: "that blocked this turn" → `BlockedThisTurn`
///
/// Returns `(properties, bytes_consumed)` or `None` if the text doesn't match.
fn parse_that_clause_suffix(text: &str) -> Option<(Vec<FilterProp>, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();

    let after_that = trimmed.strip_prefix("that ")?;
    let that_len = leading_ws + "that ".len();

    // --- "that share(s) [no] [a] [quality]" ---
    if let Some(rest) = after_that
        .strip_prefix("share ")
        .or_else(|| after_that.strip_prefix("shares "))
    {
        let share_verb_len = after_that.len() - rest.len();

        // Optional negation: "that share no creature types"
        let (rest, negated) = rest
            .strip_prefix("no ")
            .map(|r| (r, true))
            .unwrap_or((rest, false));
        let neg_len = if negated { "no ".len() } else { 0 };

        // Optional article: "a creature type" vs "creature types"
        let (rest, a_len) = rest
            .strip_prefix("a ")
            .map(|r| (r, "a ".len()))
            .unwrap_or((rest, 0));

        // Consume the quality phrase up to end of clause
        let quality_end = rest.find([',', '.']).unwrap_or(rest.len());
        let quality = rest[..quality_end].trim();
        if !quality.is_empty() {
            let total = that_len + share_verb_len + neg_len + a_len + quality_end;
            return Some((
                vec![FilterProp::SharesQuality {
                    quality: quality.to_string(),
                }],
                total,
            ));
        }
    }

    // --- CR 115.9c: "that targets only [filter]" ---
    if let Some(rest) = after_that.strip_prefix("targets only ") {
        let targets_verb_len = "targets only ".len();
        if let Some((props, consumed)) =
            parse_targets_only_constraint(rest, that_len + targets_verb_len)
        {
            return Some((props, consumed));
        }
    }

    // --- Verb-phrase patterns: match fixed phrases after "that " ---
    // CR 510.1: "that was dealt damage this turn"
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
        ("attacked this turn", FilterProp::AttackedThisTurn),
        ("blocked this turn", FilterProp::BlockedThisTurn),
    ];

    for (phrase, prop) in VERB_PHRASES {
        if let Some(_rest) = after_that.strip_prefix(phrase) {
            let total = that_len + phrase.len();
            return Some((vec![prop.clone()], total));
        }
    }

    None
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
    // Self-reference: "~" or "it"
    if text.starts_with('~') {
        let props = vec![FilterProp::TargetsOnly {
            filter: Box::new(TargetFilter::SelfRef),
        }];
        return Some((props, prefix_len + 1));
    }
    if text.starts_with("it") && (text.len() == 2 || text[2..].starts_with([',', '.', ' '])) {
        let props = vec![FilterProp::TargetsOnly {
            filter: Box::new(TargetFilter::SelfRef),
        }];
        return Some((props, prefix_len + 2));
    }

    // "you" — targets only the controller (a player)
    if text.starts_with("you") && (text.len() == 3 || text[3..].starts_with([',', '.', ' '])) {
        let props = vec![FilterProp::TargetsOnly {
            filter: Box::new(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        }];
        return Some((props, prefix_len + 3));
    }

    // "a single [type phrase or player]" — TargetsOnly + HasSingleTarget
    if let Some(rest) = text.strip_prefix("a single ") {
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
    if let Some(rest) = text.strip_prefix("a ").or_else(|| text.strip_prefix("an ")) {
        let article_len = text.len() - rest.len();
        let (inner_filter, consumed) = parse_targets_only_type_or_player(rest);
        let props = vec![FilterProp::TargetsOnly {
            filter: Box::new(inner_filter),
        }];
        return Some((props, prefix_len + article_len + consumed));
    }

    None
}

/// Parse the type-or-player constraint inside "that targets only a [single] ...".
/// Handles "player" as `TargetFilter::Player` and "[type] or player" as
/// `Or(Typed(type), Player)`, since `parse_type_phrase` doesn't recognize "player".
fn parse_targets_only_type_or_player(text: &str) -> (TargetFilter, usize) {
    // Check for bare "player" at start
    if text.starts_with("player") && (text.len() == 6 || text[6..].starts_with([',', '.', ' '])) {
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
        if end == text.len() || text[end..].starts_with([',', '.', ' ']) {
            let type_part = tp.split_at(or_pos).0.original;
            let (type_filter, _) = parse_type_phrase(type_part);
            let combined = TargetFilter::Or {
                filters: vec![type_filter, TargetFilter::Player],
            };
            return (combined, end);
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
            // Use starts_with to avoid false matches where "in" is part of "into"
            // (e.g., "is put into your graveyard from your library" should NOT match
            // as a zone suffix — it's a trigger event, not a type qualifier).
            if starts_with_possessive(after_card, prep, zone_word) {
                let pattern = format!("{prep} your {zone_word}");
                let ctrl = if after_card.to_lowercase().starts_with(&pattern) {
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
    fn this_creature_is_self_ref() {
        let (f, rest) = parse_target("this creature to its owner's hand");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, " to its owner's hand");
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
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::CmcGE {
                value: QuantityExpr::Fixed { value: 4 },
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

    // ── ExiledBySource ──

    #[test]
    fn each_card_exiled_with_tilde_produces_exiled_by_source() {
        let (f, rest) = parse_target("each card exiled with ~ into its owner's graveyard");
        assert_eq!(f, TargetFilter::ExiledBySource);
        assert_eq!(rest, " into its owner's graveyard");
    }

    #[test]
    fn each_card_exiled_with_this_artifact_produces_exiled_by_source() {
        let (f, rest) = parse_target("each card exiled with this artifact");
        assert_eq!(f, TargetFilter::ExiledBySource);
        assert_eq!(rest, "");
    }

    #[test]
    fn cards_exiled_with_tilde_produces_exiled_by_source() {
        let (f, _) = parse_target("cards exiled with ~");
        assert_eq!(f, TargetFilter::ExiledBySource);
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

        // "that permanent or player" — longest-match-first, no bogus " or player" remainder
        let (filter, rem) =
            parse_event_context_ref("that permanent or player and the damage can't be prevented")
                .unwrap();
        assert_eq!(filter, TargetFilter::TriggeringSource);
        assert_eq!(rem, " and the damage can't be prevented");

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
            TargetFilter::Typed(ref tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert!(tf.properties.iter().any(|p| matches!(
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
                        color: "Black".to_string()
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
                    value: "Legendary".to_string(),
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
                        value: "Basic".to_string(),
                    }])
            )
        );
    }

    #[test]
    fn snow_permanents() {
        let (f, _) = parse_type_phrase("snow permanents");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                FilterProp::HasSupertype {
                    value: "Snow".to_string(),
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
                    value: "Legendary".to_string()
                },
                FilterProp::HasColor {
                    color: "White".to_string()
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
                    value: "Basic".to_string(),
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
                        value: "Basic".to_string(),
                    }])
            )
        );
    }

    // ── Feature 4: "and/or" separator ──

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
                            FilterProp::CmcLE {
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

    #[test]
    fn parse_type_phrase_ninja_or_rogue_creatures_you_control() {
        // parse_type_phrase doesn't handle "or" compound subtypes natively —
        // it parses "ninja" as subtype and leaves "or rogue creatures you control" as remainder.
        // The fix is in try_parse_one_or_more_combat_damage_to_player which manually splits on "or".
        let (_filter, remainder) = parse_type_phrase("ninja or rogue creatures you control");
        assert!(
            !remainder.trim().is_empty(),
            "parse_type_phrase unexpectedly consumed the whole phrase"
        );
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
    fn combat_status_prefix_unblocked() {
        let result = parse_combat_status_prefix("unblocked attacking creatures");
        assert_eq!(result, Some((FilterProp::Unblocked, 10)));
        // Second call on remainder should get Attacking
        let result2 = parse_combat_status_prefix("attacking creatures");
        assert_eq!(result2, Some((FilterProp::Attacking, 10)));
    }

    #[test]
    fn parse_type_phrase_unblocked_attacking_creatures_you_control() {
        let (filter, remainder) = parse_type_phrase("unblocked attacking creatures you control");
        assert!(remainder.trim().is_empty(), "remainder: '{remainder}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.properties.contains(&FilterProp::Unblocked));
            assert!(tf.properties.contains(&FilterProp::Attacking));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!("Expected Typed filter, got {filter:?}");
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
        // CR 700.5: "that share a creature type" is consumed into SharesQuality.
        let (filter, rest) = parse_type_phrase("creatures you control that share a creature type");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.iter().any(
                |p| matches!(p, FilterProp::SharesQuality { quality } if quality == "creature type")
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
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::SharesQuality { quality } if quality == "creature types")));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
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
    fn that_attacked_this_turn() {
        let (filter, rest) = parse_target("target creature that attacked this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::AttackedThisTurn)));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
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

    // --- CR 115.9c: "that targets only [X]" tests ---

    #[test]
    fn that_targets_only_self_ref() {
        let result = parse_that_clause_suffix(" that targets only ~");
        let (props, _consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::TargetsOnly { filter } if **filter == TargetFilter::SelfRef
        ));
    }

    #[test]
    fn that_targets_only_it() {
        let result = parse_that_clause_suffix(" that targets only it,");
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
        let result = parse_that_clause_suffix(" that targets only you,");
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
        let result = parse_that_clause_suffix(" that targets only a single creature you control,");
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
        let result = parse_that_clause_suffix(" that targets only a single permanent or player");
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
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::PowerGE { value: 3 })),
                "Expected PowerGE(3) in {:?}",
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
}
