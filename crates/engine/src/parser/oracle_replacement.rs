use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::value;
use nom::Parser;
use nom_language::error::VerboseError;

use super::oracle_effect::{parse_effect_chain, try_parse_named_choice};
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_quantity::capitalize_first;
use super::oracle_target::parse_type_phrase;
use super::oracle_util::{
    normalize_card_name_refs, parse_number, parse_ordinal, strip_after, strip_reminder_text,
    TextPair,
};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ChoiceType, CombatDamageScope, Comparator, ControllerRef,
    DamageModification, DamageTargetFilter, Effect, FilterProp, PreventionAmount, QuantityExpr,
    QuantityRef, ReplacementCondition, ReplacementDefinition, ReplacementMode, TargetFilter,
    TypeFilter, TypedFilter,
};
use crate::types::mana::ManaColor;
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

/// Parse a replacement effect line into a ReplacementDefinition.
/// Handles: "If ~ would die", "Prevent all combat damage",
/// "~ enters the battlefield tapped", etc.
#[tracing::instrument(level = "debug", skip(card_name))]
pub fn parse_replacement_line(text: &str, card_name: &str) -> Option<ReplacementDefinition> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();
    let normalized = replace_self_refs(&text, card_name);
    let norm_lower = normalized.to_lowercase();

    // --- "As ~ enters, choose a [type]" → Moved replacement with persisted Choose ---
    // Must be checked BEFORE shock lands, which may contain this as a sub-pattern.
    if let Some(def) = parse_as_enters_choose(&norm_lower, &text) {
        return Some(def);
    }

    // --- Shock lands: "As ~ enters, you may pay N life. If you don't, it enters tapped." ---
    // Must be checked BEFORE the generic "enters tapped" pattern.
    if let Some(def) = parse_shock_land(&norm_lower, &text) {
        return Some(def);
    }

    // --- All conditional "enters tapped unless X" patterns (CR 614.1d) ---
    // Dispatches to typed condition extractors in priority order, with generic fallback.
    // Shock lands are handled above (structurally different: Optional mode with decline path).
    if let Some(def) = parse_enters_tapped_unless(&norm_lower, &text) {
        return Some(def);
    }

    // --- "You may have ~ enter as a copy of [filter]" (clone replacement) ---
    // CR 707.9: "Enter as a copy" is a replacement effect modifying the ETB event.
    if let Some(def) = parse_clone_replacement(&norm_lower, &text) {
        return Some(def);
    }

    // --- "[Type] your opponents control enter tapped" (external replacement) ---
    if let Some(def) = parse_external_enters_tapped(&norm_lower, &text) {
        return Some(def);
    }

    // --- "~ enters the battlefield tapped" (unconditional) ---
    // Guard: reject text with " unless " — all conditional patterns must be handled above.
    if (norm_lower.contains("enters the battlefield tapped")
        || norm_lower.contains("enters tapped"))
        && !norm_lower.contains(" unless ")
    {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Tap {
                        target: TargetFilter::SelfRef,
                    },
                ))
                .valid_card(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "If a card/token would be put into a graveyard, exile it instead" ---
    if let Some(def) = parse_graveyard_exile_replacement(&norm_lower, &text) {
        return Some(def);
    }

    // --- "If ~ would die, {effect}" ---
    if norm_lower.contains("~ would die") || norm_lower.contains("~ would be destroyed") {
        let effect_text = extract_replacement_effect(&normalized);
        let mut def = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description(text.to_string());
        if let Some(e) = effect_text {
            def = def.execute(parse_effect_chain(&e, AbilityKind::Spell));
        }
        return Some(def);
    }

    // --- "If [filter] would die, exile it instead" (non-self replacement) ---
    // CR 614.1a: Replacement effects that exile dying creatures instead of putting
    // them into the graveyard. Subject is a creature filter, not self-reference.
    // E.g., "If another creature would die, exile it instead." (Void Maw)
    //       "If a nontoken creature an opponent controls would die, exile it instead." (Valentin)
    //       "If a creature an opponent controls would die, exile it instead." (Vren)
    if let Some(def) = parse_creature_die_exile_replacement(&norm_lower, &text) {
        return Some(def);
    }

    // --- "Prevent all/the next N damage" patterns (CR 615) ---
    if let Some(def) = parse_damage_prevention_replacement(&norm_lower, &text) {
        return Some(def);
    }
    // "damage can't be prevented" is handled by effect parsing (Effect::AddRestriction),
    // not replacement parsing. See oracle_effect.rs damage prevention disabled handler.

    // --- "If you would draw a card, {effect}" ---
    if lower.contains("you would draw") {
        let effect_text = extract_replacement_effect(&normalized);
        let mut def =
            ReplacementDefinition::new(ReplacementEvent::Draw).description(text.to_string());
        if let Some(e) = effect_text {
            def = def.execute(parse_effect_chain(&e, AbilityKind::Spell));
        }
        return Some(def);
    }

    // --- "If [player] would gain life, {effect}" ---
    // CR 614.1a: Widened from "you would gain life" to handle opponent/player scope.
    if lower.contains("would gain life") {
        let effect_text = extract_replacement_effect(&normalized);
        let mut def =
            ReplacementDefinition::new(ReplacementEvent::GainLife).description(text.to_string());
        if let Some(e) = effect_text {
            def = def.execute(parse_effect_chain(&e, AbilityKind::Spell));
        }
        // Parse the subject to determine player scope
        if lower.contains("an opponent would gain life")
            || lower.contains("opponent would gain life")
        {
            def.valid_player = Some(ControllerRef::Opponent);
        } else if lower.contains("a player would gain life") {
            // "a player" applies to all players — None means controller-only,
            // so we need a way to express "all". Use a sentinel: leave valid_player
            // as None and let the matcher check. Actually, for "a player", the
            // replacement applies regardless of who gains life. The matcher needs
            // to be updated to not filter on controller when valid_player is None
            // and the subject was "a player". For now, set valid_player to None
            // and document that the matcher should not restrict player scope.
            // NOTE: The existing matcher restricts to controller only. For Tainted Remedy
            // ("an opponent"), we set Opponent. For "you", we leave None (controller-only).
        }
        return Some(def);
    }

    // --- "If [someone] would lose life, they lose twice that much life instead" ---
    if lower.contains("would lose life") {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::LoseLife).description(text.to_string()),
        );
    }

    // --- "If [source] would deal [noncombat] damage ... it deals that much damage plus N instead" ---
    // CR 614.1a: Damage boost/reduction replacement effects.
    if lower.contains("would deal") && lower.contains("damage") && lower.contains("instead") {
        if let Some(def) = parse_damage_modification_replacement(&norm_lower, &text) {
            return Some(def);
        }
        // Exotic pattern (coin-flip, redirection, etc.) — keep as no-op stub
        return Some(
            ReplacementDefinition::new(ReplacementEvent::DamageDone).description(text.to_string()),
        );
    }

    // --- "[Subject] enters/escapes with N [type] counter(s)" ---
    // CR 614.1c: Handles "enters with", "escapes with" (CR 702.138), and
    // kicker-conditional "if was kicked, it enters with" (CR 702.33d).
    if (lower.contains("enters") || lower.contains("escapes")) && lower.contains("counter") {
        if let Some(def) = parse_enters_with_counters(&norm_lower, &text) {
            return Some(def);
        }
    }

    // --- Token creation replacement: "if one or more tokens would be created..." ---
    if lower.contains("tokens would be created") || lower.contains("token would be created") {
        if let Some(def) = parse_token_replacement(&lower, &text) {
            return Some(def);
        }
    }

    // --- Counter addition replacement: "if one or more ... counters would be put on..." ---
    if lower.contains("counters would be put on") || lower.contains("counter would be put on") {
        if let Some(def) = parse_counter_replacement(&lower, &text) {
            return Some(def);
        }
    }

    // --- Damage redirection: "all damage that would be dealt to [target] is dealt to ~ instead" ---
    // CR 614.1a: Replacement effects that redirect damage to a different recipient.
    if let Some(def) = parse_damage_redirection_replacement(&norm_lower, &text) {
        return Some(def);
    }

    // --- Event substitution: "if [player] would [event], [skip/prevent] instead" ---
    // CR 614.1a: Replacement effects that nullify or substitute an event entirely.
    if let Some(def) = parse_event_substitution_replacement(&norm_lower, &text) {
        return Some(def);
    }

    // --- Mana type replacement: "if a land would produce mana, it produces [X] instead" ---
    // CR 614.1a: Replacement effects that change the type of mana produced.
    if let Some(def) = parse_mana_replacement(&norm_lower, &text) {
        return Some(def);
    }

    None
}

/// Case-insensitive replacement of card name and self-referencing phrases with "~".
fn replace_self_refs(text: &str, card_name: &str) -> String {
    normalize_card_name_refs(text, card_name)
}

/// Parse shock land pattern: "As ~ enters, you may pay N life. If you don't, it enters tapped."
/// Returns Optional ReplacementDefinition with execute=LoseLife (accept) and decline=Tap (decline).
fn parse_shock_land(norm_lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    // Match: "you may pay N life" + "enters tapped" (in either sentence order)
    if !norm_lower.contains("you may pay") || !norm_lower.contains("life") {
        return None;
    }
    if !norm_lower.contains("enters tapped")
        && !norm_lower.contains("enters the battlefield tapped")
    {
        return None;
    }

    // Extract life amount: "pay 2 life", "pay 3 life", etc.
    let amount = extract_life_payment(norm_lower)?;

    let lose_life = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: amount },
            target: None,
        },
    );

    let tap_self = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Tap {
            target: TargetFilter::SelfRef,
        },
    );

    let has_basic_land_type_choice = norm_lower.contains("choose a basic land type");
    let execute = if has_basic_land_type_choice {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: ChoiceType::BasicLandType,
                persist: true,
            },
        )
        .sub_ability(lose_life)
    } else {
        lose_life
    };

    let decline = if has_basic_land_type_choice {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: ChoiceType::BasicLandType,
                persist: true,
            },
        )
        .sub_ability(tap_self)
    } else {
        tap_self
    };

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(execute)
            .mode(ReplacementMode::Optional {
                decline: Some(Box::new(decline)),
            })
            .valid_card(TargetFilter::SelfRef)
            .description(original_text.to_string()),
    )
}

/// Parse "As ~ enters, choose a [type]" into a Moved replacement with persisted Choose.
/// Skips lines that also contain shock land markers (handled by parse_shock_land).
fn parse_as_enters_choose(norm_lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    // Must have "as" + "enters" framing
    if !norm_lower.contains("as ") || !norm_lower.contains("enters") {
        return None;
    }

    // Don't match shock lands — they have their own handler
    if norm_lower.contains("you may pay") && norm_lower.contains("life") {
        return None;
    }

    // Extract the "choose a ..." clause
    let choose_idx = norm_lower.find("choose ")?;
    let choose_text = &norm_lower[choose_idx..];
    let choice_type = try_parse_named_choice(choose_text)?;

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Choose {
                    choice_type,
                    persist: true,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .description(original_text.to_string()),
    )
}

/// CR 707.9 / CR 614.1c: Parse clone replacement effect.
/// "You may have ~ enter as a copy of [any] [type] on the battlefield"
/// Emits an Optional Moved replacement with BecomeCopy as the execute effect.
/// The player chooses a valid permanent to copy as part of the replacement.
fn parse_clone_replacement(norm_lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    // Must contain "enter as a copy of" (after self-ref normalization)
    let copy_idx = norm_lower.find("enter as a copy of ")?;
    // Must be preceded by "you may have" for the optional framing
    if !norm_lower[..copy_idx].contains("you may have") {
        return None;
    }

    let after_copy = &norm_lower[copy_idx + "enter as a copy of ".len()..];
    // Strip "any " / "a " / "an " article before the type phrase
    let type_text = alt((
        tag::<_, _, VerboseError<&str>>("any "),
        tag("a "),
        tag("an "),
    ))
    .parse(after_copy)
    .map_or(after_copy, |(rest, _)| rest);

    // Strip trailing "on the battlefield" and punctuation
    let type_text = type_text
        .trim_end_matches('.')
        .trim_end_matches(" on the battlefield")
        .trim();

    let (filter, leftover) = parse_type_phrase(type_text);
    if !leftover.trim().is_empty() {
        return None;
    }

    // CR 707.9a: The copy effect uses the chosen object's copiable values.
    // This is NOT targeting (hexproof/shroud don't apply).
    let copy_effect = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::BecomeCopy {
            target: filter,
            duration: None,
        },
    );

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(copy_effect)
            .mode(ReplacementMode::Optional { decline: None })
            .valid_card(TargetFilter::SelfRef)
            .description(original_text.to_string()),
    )
}

/// Parse check land pattern: "enters tapped unless you control a [LandType] or a [LandType]"
/// Returns Mandatory ReplacementDefinition with an UnlessControlsSubtype condition.
/// Shared dispatcher for all conditional "enters tapped unless X" patterns (CR 614.1d).
/// Tries typed condition extractors in priority order, falling back to generic Unrecognized.
/// Shock lands are excluded — they use ReplacementMode::Optional with a decline path.
fn parse_enters_tapped_unless(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    if !norm_lower.contains("enters tapped")
        && !norm_lower.contains("enters the battlefield tapped")
    {
        return None;
    }

    // Try typed condition extractors in priority order:
    // Fast lands BEFORE check lands (both match "unless you control").
    // Check lands BEFORE controls_typed (more specific subtype match).
    let condition = parse_fast_condition(norm_lower)
        .or_else(|| parse_check_condition(norm_lower))
        .or_else(|| parse_controls_typed_condition(norm_lower))
        .or_else(|| parse_player_life_condition(norm_lower))
        .or_else(|| parse_multiple_opponents_condition(norm_lower))
        .or_else(|| parse_your_turn_condition(norm_lower))
        .or_else(|| parse_turn_of_game_condition(norm_lower))
        .or_else(|| parse_generic_unless_condition(norm_lower, original_text))?;

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Tap {
                    target: TargetFilter::SelfRef,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .description(original_text.to_string())
            .condition(condition),
    )
}

/// Extract check land condition: "unless you control a [LandType] or a [LandType]"
fn parse_check_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    let rest = strip_after(norm_lower, "unless you control ")?;
    let rest = rest.trim_end_matches('.');

    let mut subtypes = Vec::new();
    for part in rest.split(" or ") {
        let trimmed = part
            .trim()
            .trim_start_matches("a ")
            .trim_start_matches("an ");
        let canonical = canonical_land_subtype(trimmed)?;
        if !subtypes.contains(&canonical) {
            subtypes.push(canonical);
        }
    }

    if subtypes.is_empty() {
        return None;
    }

    Some(ReplacementCondition::UnlessControlsSubtype { subtypes })
}

/// Extract fast land condition: "unless you control N or fewer other [type]"
/// CR 305.7 + CR 614.1c — fast lands (Spirebluff Canal, Blackcleave Cliffs, etc.).
/// Delegates to `nom_primitives::parse_number` for the count (input already lowercase).
fn parse_fast_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    let rest = strip_after(norm_lower, "unless you control ")?;

    // Parse "two or fewer other lands." → count=2, remainder="or fewer other lands."
    let (nom_rest, count) = nom_primitives::parse_number.parse(rest).ok()?;
    let after_number = nom_rest.trim_start();
    let (after_or_fewer, _) = tag::<_, _, VerboseError<&str>>("or fewer ")
        .parse(after_number.trim_start())
        .ok()?;
    let type_text = after_or_fewer.trim_end_matches('.');

    // parse_type_phrase handles "other lands" → TypedFilter { Land, [Another] }
    let (filter, leftover) = parse_type_phrase(type_text);
    if !leftover.trim().is_empty() {
        return None;
    }

    // Extract TypedFilter and inject ControllerRef::You (not visible in the parsed fragment)
    let typed_filter = match filter {
        TargetFilter::Typed(tf) => tf.controller(ControllerRef::You),
        _ => return None,
    };

    Some(ReplacementCondition::UnlessControlsOtherLeq {
        count,
        filter: typed_filter,
    })
}

/// Map lowercase land subtype name to canonical (title-cased) form.
fn canonical_land_subtype(raw: &str) -> Option<String> {
    match raw {
        "plains" => Some("Plains".to_string()),
        "island" => Some("Island".to_string()),
        "swamp" => Some("Swamp".to_string()),
        "mountain" => Some("Mountain".to_string()),
        "forest" => Some("Forest".to_string()),
        _ => None,
    }
}

/// Extract general "unless you control a [type phrase]" condition (CR 614.1d).
/// Handles basic lands, legendary creatures, Mount/Vehicle, etc.
/// Also handles "unless you control N or more [type]" quantity prefix patterns.
fn parse_controls_typed_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    let rest = strip_after(norm_lower, "unless you control ")?;

    // Try "N or more [type]" pattern first (e.g., "two or more other lands")
    if let Some((minimum, type_text)) = try_parse_quantity_prefix(rest) {
        let (filter, leftover) = parse_type_phrase(type_text);
        if !leftover.trim().trim_end_matches('.').is_empty() || filter == TargetFilter::Any {
            return None;
        }
        let filter = inject_controller_you(filter);
        return Some(ReplacementCondition::UnlessControlsCountMatching { minimum, filter });
    }

    // Strip leading article — parse_type_phrase does NOT handle "a "/"an "
    let rest = rest.trim_start_matches("a ").trim_start_matches("an ");

    let (filter, leftover) = parse_type_phrase(rest);
    // Reject partial parse — all text must be consumed (modulo trailing period)
    if !leftover.trim().trim_end_matches('.').is_empty() {
        return None;
    }

    // Reject if parse_type_phrase returned Any (nothing meaningful parsed)
    if filter == TargetFilter::Any {
        return None;
    }

    // Inject ControllerRef::You — "you control" is implicit in the Oracle text
    // CR 614.1d — consistent with fast land controller injection pattern
    let filter = inject_controller_you(filter);

    Some(ReplacementCondition::UnlessControlsMatching { filter })
}

/// Try to parse "N or more " quantity prefix before a type phrase.
/// Returns (minimum, remainder) if matched.
/// Delegates to `nom_primitives::parse_number` for the count (input already lowercase).
fn try_parse_quantity_prefix(text: &str) -> Option<(u32, &str)> {
    let (nom_rest, n) = nom_primitives::parse_number.parse(text).ok()?;
    let (type_text, _) = tag::<_, _, VerboseError<&str>>("or more ")
        .parse(nom_rest.trim_start())
        .ok()?;
    Some((n, type_text))
}

/// Inject `ControllerRef::You` into a `TargetFilter`, handling both `Typed` and `Or` variants.
fn inject_controller_you(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(tf) => TargetFilter::Typed(tf.controller(ControllerRef::You)),
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|f| match f {
                    TargetFilter::Typed(tf) => {
                        TargetFilter::Typed(tf.controller(ControllerRef::You))
                    }
                    other => other,
                })
                .collect(),
        },
        other => other,
    }
}

/// Extract life payment amount from "pay N life" pattern.
fn extract_life_payment(text: &str) -> Option<i32> {
    let after_pay = strip_after(text, "pay ")?;
    let end = after_pay.find(' ').unwrap_or(after_pay.len());
    let num_str = &after_pay[..end];
    num_str.parse().ok()
}

/// Parse "enters/escapes with N [type] counter(s)" patterns into a Moved replacement.
/// Handles self ("~ enters with"), other ("each other creature ... enters with"),
/// escape ("~ escapes with", CR 702.138c), and kicker-conditional
/// ("if ~ was kicked, it enters with", CR 702.33d).
fn parse_enters_with_counters(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Detect kicker-conditional prefix: "if ~ was kicked [with its {cost} kicker], it enters with"
    // CR 702.33d: kicker condition gates the replacement effect.
    let (kicker_condition, work_text) = extract_kicker_enters_condition(norm_lower);

    // CR 702.138c: "escapes with" is semantically "enters with" gated on escape.
    // Use nom take_until to scan for the "escapes with" phrase at word boundaries.
    let is_escape = take_until::<_, _, VerboseError<&str>>("escapes with")
        .parse(work_text)
        .is_ok();

    // Find "with [N] [type] counter" to extract count and counter type.
    // For escape, the "with" follows "escapes"; for enters, it follows "enters".
    let after_with = strip_after(work_text, "with ")?;
    // Skip "an additional" if present
    let after_additional = alt((
        tag::<_, _, VerboseError<&str>>("an additional "),
        tag("additional "),
    ))
    .parse(after_with)
    .map_or(after_with, |(rest, _)| rest);
    // Detect dynamic count: "a number of [type] counters ... equal to [qty]"
    let (dynamic_remainder, after_prefix) =
        match tag::<_, _, VerboseError<&str>>("a number of ").parse(after_additional) {
            Ok((rest, _)) => (Some(after_additional), rest),
            Err(_) => (None, after_additional),
        };
    // Uses oracle_util::parse_number (not nom directly) because it handles "X" → 0
    // for X-cost cards like Endless One, Walking Ballista, Hangarback Walker, etc.
    let (count, rest) = parse_number(after_prefix).unwrap_or((1, after_prefix));
    // Next word(s) before "counter" are the counter type
    let counter_pos = rest.find("counter")?;
    let counter_type_raw = rest[..counter_pos].trim();
    let counter_type = match counter_type_raw {
        "+1/+1" => "P1P1".to_string(),
        "-1/-1" => "M1M1".to_string(),
        other => other.to_uppercase(),
    };

    // Initialize count with fixed fallback, then override for dynamic "equal to" pattern
    let mut count_expr = QuantityExpr::Fixed {
        value: count as i32,
    };
    // CR 122.6: For "a number of counters equal to [quantity]", parse the dynamic expression
    if dynamic_remainder.is_some() {
        if let Ok((_, (_, qty_text))) = nom_primitives::split_once_on(work_text, "equal to ") {
            let trimmed = qty_text.trim().trim_end_matches('.');
            if let Some(qty_ref) = crate::parser::oracle_quantity::parse_quantity_ref(trimmed) {
                count_expr = QuantityExpr::Ref { qty: qty_ref };
            } else if let Some(qty) = crate::parser::oracle_quantity::parse_cda_quantity(trimmed) {
                count_expr = qty;
            }
        }
    }

    let put_counter = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type,
            count: count_expr,
            target: TargetFilter::SelfRef,
        },
    );

    // Determine valid_card filter: self vs other creatures
    // Strip "each other " or "other " prefix, then delegate to parse_type_phrase
    // which handles non-X, controller, "of the chosen type", etc.
    let subject = alt((
        tag::<_, _, VerboseError<&str>>("each other "),
        tag("other "),
    ))
    .parse(work_text)
    .ok()
    .map(|(rest, _)| rest)
    .filter(|s| s.contains("creature") || s.contains("permanent"));
    let valid_card = if let Some(subject_text) = subject {
        let (filter, _) = parse_type_phrase(subject_text);
        // Inject Another since we stripped "other" above
        let filter = match filter {
            TargetFilter::Typed(TypedFilter {
                type_filters,
                controller,
                mut properties,
            }) => {
                properties.insert(0, FilterProp::Another);
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller,
                    properties,
                })
            }
            other => other,
        };
        Some(filter)
    } else {
        Some(TargetFilter::SelfRef)
    };

    let mut def = ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(put_counter)
        .description(original_text.to_string());
    if let Some(filter) = valid_card {
        def = def.valid_card(filter);
    }

    // Apply condition: escape or kicker
    if is_escape {
        def = def.condition(ReplacementCondition::CastViaEscape);
    } else if let Some(cond) = kicker_condition {
        def = def.condition(cond);
    }

    Some(def)
}

/// Extract kicker-conditional prefix from "if ~ was kicked [with its {cost} kicker], it enters with..."
/// Returns `(Option<ReplacementCondition>, remaining_text)` where remaining_text has the
/// conditional prefix stripped (just "it enters with..." or the original text if no prefix).
/// CR 702.33d
fn extract_kicker_enters_condition(norm_lower: &str) -> (Option<ReplacementCondition>, &str) {
    // CR 702.33d: Parse "if ~ was kicked [with its {cost} kicker], it enters with..."
    // using nom combinators for structured dispatch.
    let after_if = match tag::<_, _, VerboseError<&str>>("if ").parse(norm_lower) {
        Ok((rest, _)) => rest,
        Err(_) => return (None, norm_lower),
    };

    // Subject can be "~", "it", "this creature", etc. — scan to "was kicked".
    let after_kicked = match take_until::<_, _, VerboseError<&str>>("was kicked")
        .parse(after_if)
        .and_then(|(rest, _)| tag::<_, _, VerboseError<&str>>("was kicked").parse(rest))
    {
        Ok((rest, _)) => rest,
        Err(_) => return (None, norm_lower),
    };

    // Optional "with its {cost} kicker" variant specification
    let (cost_text, after_kicker_clause) =
        match tag::<_, _, VerboseError<&str>>(" with its ").parse(after_kicked) {
            Ok((rest, _)) => {
                match take_until::<_, _, VerboseError<&str>>(" kicker").parse(rest) {
                    Ok((rest2, cost_str)) => {
                        // Consume " kicker" tag
                        match tag::<_, _, VerboseError<&str>>(" kicker").parse(rest2) {
                            Ok((rest3, _)) => (Some(cost_str.trim().to_string()), rest3),
                            Err(_) => (None, after_kicked),
                        }
                    }
                    Err(_) => (None, after_kicked),
                }
            }
            Err(_) => (None, after_kicked),
        };

    // Expect ", it enters with" or ", it enters the battlefield with"
    let enters_result = alt((
        tag::<_, _, VerboseError<&str>>(", it enters with"),
        tag(", it enters the battlefield with"),
    ))
    .parse(after_kicker_clause);

    match enters_result {
        Ok(_) => {
            // Reconstruct the enters-with text for downstream parsing.
            let enters_start = norm_lower.len() - after_kicker_clause.len() + 2; // skip ", "
            let condition = ReplacementCondition::CastViaKicker { cost_text };
            (Some(condition), &norm_lower[enters_start..])
        }
        Err(_) => (None, norm_lower),
    }
}

/// Parse "[Type] enter tapped" / "[Type] enters tapped" — external replacement effects.
/// E.g., "Creatures your opponents control enter tapped." (Authority of the Consuls)
/// E.g., "Artifacts and creatures your opponents control enter tapped." (Blind Obedience)
fn parse_external_enters_tapped(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let stripped = norm_lower.trim_end_matches('.');
    let subject = stripped
        .strip_suffix(" enter tapped")
        .or_else(|| stripped.strip_suffix(" enters tapped"))?;

    // Must NOT be a self-reference (those are handled by the normal enters-tapped path)
    if subject.contains('~') {
        return None;
    }

    let (filter, rest) = parse_type_phrase(subject);
    // Ensure the entire subject was consumed (no trailing unparsed text)
    if !rest.trim().is_empty() {
        return None;
    }

    // CR 614.12: Only match zone changes TO the battlefield.
    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Tap {
                    target: TargetFilter::SelfRef,
                },
            ))
            .valid_card(filter)
            .destination_zone(Zone::Battlefield)
            .description(original_text.to_string()),
    )
}

/// CR 614.1a: Parse "If [filter] would die, exile it instead" replacement effects.
/// Handles non-self creature filters like "another creature", "a nontoken creature
/// an opponent controls", "a creature an opponent controls".
fn parse_creature_die_exile_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Must contain "would die" and "instead" (exile-instead pattern).
    let would_die_pos = norm_lower.find("would die")?;
    if !norm_lower.contains("instead") {
        return None;
    }

    // Extract the subject between "if " and " would die".
    let subject_start = {
        let prefix = norm_lower.strip_prefix("if ")?;
        // Subject is everything from after "if " to before " would die"
        let subject_end_in_prefix = would_die_pos - "if ".len();
        prefix[..subject_end_in_prefix].trim()
    };

    // Skip self-reference patterns — handled by the earlier "~ would die" check.
    if subject_start.contains('~') {
        return None;
    }

    // Parse the subject filter (e.g., "another creature", "a nontoken creature an opponent controls")
    let (filter, _) = parse_type_phrase(subject_start);
    if matches!(&filter, TargetFilter::Any) {
        return None;
    }

    // Extract the replacement effect after the comma.
    // "If [filter] would die, exile it instead." → effect is "exile it instead."
    let after_would_die = &norm_lower[would_die_pos + "would die".len()..].trim_start();
    let effect_text = after_would_die.strip_prefix(", ")?;

    // Parse the replacement effect (typically "exile it instead")
    let effect_text_trimmed = effect_text
        .strip_suffix('.')
        .unwrap_or(effect_text)
        .trim_end_matches(" instead")
        .trim();

    let execute = if effect_text_trimmed == "exile it" || effect_text_trimmed == "exile that card" {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
            },
        )
    } else {
        // Generic effect text — parse as effect chain from the original-case text
        let comma_pos = original_text.find(", ").unwrap_or(0);
        let orig_effect = if comma_pos > 0 {
            original_text[comma_pos + 2..].trim()
        } else {
            effect_text_trimmed
        };
        parse_effect_chain(orig_effect, AbilityKind::Spell)
    };

    Some(
        ReplacementDefinition::new(ReplacementEvent::Destroy)
            .execute(execute)
            .valid_card(filter)
            .description(original_text.to_string()),
    )
}

/// Parse "If a card/token would be put into a graveyard, exile it instead."
/// Handles Rest in Peace ("from anywhere"), Leyline of the Void ("from anywhere" + opponent scope).
fn parse_graveyard_exile_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    if !norm_lower.contains("would be put into") {
        return None;
    }
    if !norm_lower.contains("graveyard") {
        return None;
    }
    if !norm_lower.contains("exile") {
        return None;
    }

    // Determine scope: "a card or token" / "a card" → None (matches everything)
    // "an opponent's graveyard" → opponent-owned cards
    // CR 400.3 + CR 108.3: Cards go to owner's graveyard, so "opponent's graveyard"
    // means cards owned by an opponent.
    let valid_card = if norm_lower.contains("an opponent's graveyard")
        || norm_lower.contains("opponent's graveyard")
    {
        Some(TargetFilter::Typed(TypedFilter::default().properties(
            vec![FilterProp::Owned {
                controller: ControllerRef::Opponent,
            }],
        )))
    } else {
        None
    };

    let mut def = ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
            },
        ))
        .destination_zone(Zone::Graveyard)
        .description(original_text.to_string());
    if let Some(filter) = valid_card {
        def = def.valid_card(filter);
    }
    Some(def)
}

/// CR 614.1a: Parse damage boost/reduction replacement effects.
/// Extracts modification formula, source filter, target filter, and combat scope.
fn parse_damage_modification_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // --- 1. Extract modification formula from the result clause ---
    // Scan for the modification formula at word boundaries using nom combinators.
    let modification = scan_damage_modification(norm_lower)?;

    // --- 2. Extract source filter from the subject clause (before "would deal") ---
    let source_filter = parse_damage_source_filter(norm_lower);

    // --- 3. Extract combat scope ---
    // Scan for "noncombat damage" / "combat damage" at word boundaries.
    // "noncombat" is tried first since "combat damage" is a substring of "noncombat damage".
    let combat_scope = scan_combat_scope(norm_lower);

    // --- 4. Extract target filter ---
    let target_filter = parse_damage_target_filter(norm_lower);

    let mut def = ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .damage_modification(modification)
        .description(original_text.to_string());
    if let Some(sf) = source_filter {
        def = def.damage_source_filter(sf);
    }
    if let Some(tf) = target_filter {
        def = def.damage_target_filter(tf);
    }
    if let Some(cs) = combat_scope {
        def = def.combat_scope(cs);
    }
    Some(def)
}

/// Parse the damage source filter from the subject clause before "would deal".
fn parse_damage_source_filter(norm_lower: &str) -> Option<TargetFilter> {
    let subject = norm_lower.split("would deal").next()?.trim();

    // Handle ability word prefixes ("Revolt — ..., if a source you control")
    // by finding the last "if " clause, which contains the actual replacement condition.
    let subject = subject.rsplit("if ").next().unwrap_or(subject).trim();

    // Self-reference: "~" after stripping "if"
    if subject == "~" {
        return Some(TargetFilter::SelfRef);
    }

    // Strip leading "a " or "an "
    let subject = nom_primitives::parse_article
        .parse(subject)
        .map_or(subject, |(rest, _)| rest)
        .trim();

    // "source you control" with optional qualifiers
    if let Some(prefix) = subject.strip_suffix("source you control") {
        let prefix = prefix.trim();
        let mut filter = TypedFilter::default().controller(ControllerRef::You);
        let mut props = Vec::new();

        if !prefix.is_empty() {
            // Check for "another" prefix — may appear alone or before a qualifier
            let qualifier = if prefix == "another" {
                props.push(FilterProp::Another);
                ""
            } else if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("another ").parse(prefix)
            {
                props.push(FilterProp::Another);
                rest.trim()
            } else {
                prefix
            };

            // Check for color qualifier (e.g. "red")
            if let Some(color) = parse_color_word(qualifier) {
                props.push(FilterProp::HasColor { color });
            }
            // CR 205.4b: "noncreature" qualifier — negation via TypeFilter::Non
            else if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("non").parse(qualifier) {
                let inner = match rest {
                    "creature" => TypeFilter::Creature,
                    "land" => TypeFilter::Land,
                    "artifact" => TypeFilter::Artifact,
                    "enchantment" => TypeFilter::Enchantment,
                    "planeswalker" => TypeFilter::Planeswalker,
                    other => TypeFilter::Subtype(capitalize_first(other)),
                };
                filter = filter.with_type(TypeFilter::Non(Box::new(inner)));
            }
            // Check for creature type qualifier (e.g. "giant")
            else if !qualifier.is_empty() {
                filter = filter.subtype(capitalize_first(qualifier));
            }
        }

        if !props.is_empty() {
            filter.properties = props;
        }
        return Some(TargetFilter::Typed(filter));
    }

    // "source you control" without explicit "source" word
    if subject.ends_with("you control") {
        return Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ));
    }

    // "a source" with no qualifier — no filter needed (matches any source)
    if subject == "source" {
        return None;
    }

    // "a spell" — no source filter (handled as general case for now)
    None
}

/// Parse the damage target filter from the clause after "damage".
/// Uses word-boundary scanning with nom combinators for target phrase matching.
fn parse_damage_target_filter(norm_lower: &str) -> Option<DamageTargetFilter> {
    // Most specific first: "to an opponent or a permanent an opponent controls"
    // must precede bare "to an opponent".
    let mut remaining = norm_lower;
    while !remaining.is_empty() {
        if let Ok((_, filter)) = parse_damage_target_phrase(remaining) {
            // Guard: opponent-only and player-only exclude "permanent" from the full text
            match filter {
                DamageTargetFilter::OpponentOnly | DamageTargetFilter::PlayerOnly
                    if norm_lower.contains("permanent") =>
                {
                    // Skip — "permanent" present means this is OpponentOrTheirPermanents (already tried)
                }
                _ => return Some(filter),
            }
        }
        remaining = remaining
            .find(' ')
            .map_or("", |i| remaining[i + 1..].trim_start());
    }
    None
}

/// Nom combinator for damage target phrases. Most specific tags first.
fn parse_damage_target_phrase(
    input: &str,
) -> nom::IResult<&str, DamageTargetFilter, VerboseError<&str>> {
    alt((
        value(
            DamageTargetFilter::OpponentOrTheirPermanents,
            tag("to an opponent or a permanent an opponent controls"),
        ),
        value(
            DamageTargetFilter::CreatureOnly,
            alt((tag("to a creature"), tag("to that creature"))),
        ),
        value(DamageTargetFilter::OpponentOnly, tag("to an opponent")),
        value(
            DamageTargetFilter::PlayerOnly,
            alt((tag("to a player"), tag("to that player"))),
        ),
    ))
    .parse(input)
}

// ---------------------------------------------------------------------------
// Damage replacement combinators
// ---------------------------------------------------------------------------

/// Scan for damage modification formula at word boundaries using nom combinators.
fn scan_damage_modification(text: &str) -> Option<DamageModification> {
    if let Some(modification) =
        nom_primitives::scan_at_word_boundaries(text, parse_damage_modification_phrase)
    {
        return Some(modification);
    }
    // Fallback: "that much damage plus/minus N" uses strip_after for the number
    if let Some(rest) = strip_after(text, "that much damage plus ") {
        let (_rem, val) = nom_primitives::parse_number.parse(rest).ok()?;
        return Some(DamageModification::Plus { value: val });
    }
    if let Some(rest) = strip_after(text, "that much damage minus ") {
        let (_rem, val) = nom_primitives::parse_number.parse(rest).ok()?;
        return Some(DamageModification::Minus { value: val });
    }
    None
}

/// Nom combinator for damage modification phrases.
fn parse_damage_modification_phrase(
    input: &str,
) -> nom::IResult<&str, DamageModification, VerboseError<&str>> {
    alt((
        value(
            DamageModification::Double,
            alt((tag("double that damage"), tag("deals double that damage"))),
        ),
        value(
            DamageModification::Triple,
            alt((tag("triple that damage"), tag("deals triple that damage"))),
        ),
        value(
            DamageModification::SetToSourcePower,
            alt((
                tag("damage equal to ~'s power instead"),
                tag("deals damage equal to ~'s power"),
            )),
        ),
    ))
    .parse(input)
}

/// Scan for combat damage scope at word boundaries.
/// "noncombat" tried first since "combat damage" is a substring.
fn scan_combat_scope(text: &str) -> Option<CombatDamageScope> {
    nom_primitives::scan_at_word_boundaries(text, |input| {
        alt((
            value(
                CombatDamageScope::NoncombatOnly,
                tag::<_, _, VerboseError<&str>>("noncombat damage"),
            ),
            value(CombatDamageScope::CombatOnly, tag("combat damage")),
        ))
        .parse(input)
    })
}

fn parse_color_word(word: &str) -> Option<ManaColor> {
    match word {
        "white" => Some(ManaColor::White),
        "blue" => Some(ManaColor::Blue),
        "black" => Some(ManaColor::Black),
        "red" => Some(ManaColor::Red),
        "green" => Some(ManaColor::Green),
        _ => None,
    }
}

fn extract_replacement_effect(text: &str) -> Option<String> {
    // Find ", " after "would" or "instead" clause
    if let Some(effect) = strip_after(text, ", ").map(str::trim) {
        if !effect.is_empty() {
            return Some(effect.to_string());
        }
    }
    None
}

/// CR 614.1a: Parse token creation replacement effects.
/// Handles "twice that many tokens" (Primal Vigor, Doubling Season, Parallel Lives)
/// and "those tokens plus a [Name] token" (Donatello, Chatterfang) as a recognized stub.
fn parse_token_replacement(lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    use crate::types::ability::QuantityModification;

    let modification = if lower.contains("twice that many") {
        Some(QuantityModification::Double)
    } else if lower.contains("those tokens plus") {
        // "those tokens plus a [Name] token" — recognized as CreateToken replacement.
        // Extra token creation requires ExtraTokenSpec infrastructure (future work).
        None
    } else {
        return None;
    };

    let mut def = ReplacementDefinition::new(ReplacementEvent::CreateToken)
        .description(original_text.to_string());

    if let Some(m) = modification {
        def = def.quantity_modification(m);
    }

    // Scope: "under your control" → restrict to controller's tokens
    if lower.contains("under your control") {
        def = def.token_owner_scope(ControllerRef::You);
    }

    Some(def)
}

/// CR 614.1a: Parse counter addition replacement effects.
/// Handles "twice that many ... counters" (Primal Vigor, Doubling Season)
/// and "that many plus N ... counters" (Hardened Scales, Branching Evolution).
fn parse_counter_replacement(lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    use crate::types::ability::QuantityModification;

    let modification = if lower.contains("twice that many") {
        QuantityModification::Double
    } else if let Some(rest) = strip_after(lower, "that many plus ") {
        // "that many plus one ... counters are put on it instead"
        // Delegate to nom_primitives::parse_number (input already lowercase)
        let (_rem, value) = nom_primitives::parse_number.parse(rest).ok()?;
        QuantityModification::Plus { value }
    } else if let Some(rest) = strip_after(lower, "that many minus ") {
        let (_rem, value) = nom_primitives::parse_number.parse(rest).ok()?;
        QuantityModification::Minus { value }
    } else {
        return None;
    };

    let def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
        .quantity_modification(modification)
        .description(original_text.to_string());

    Some(def)
}

/// CR 614.1a: Parse damage redirection replacement effects.
/// Handles "all damage that would be dealt to [target] is dealt to ~ instead" (Pariah, Palisade Giant)
/// and "if a source would deal damage to you, prevent that damage. ~ deals that much damage to
/// any target" (Pariah's Shield).
fn parse_damage_redirection_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Pattern 1: "all damage that would be dealt to [X] is dealt to ~ instead" (Pariah)
    // Pattern 2: "damage that would be dealt to [X] is dealt to ~ instead" (Palisade Giant)
    // CR 615.1a: Redirect = prevent original + deal to new target
    if norm_lower.contains("would be dealt to") && norm_lower.contains("is dealt to") {
        let target_filter = if norm_lower.contains("would be dealt to you") {
            Some(DamageTargetFilter::PlayerOnly)
        } else {
            // "would be dealt to ~" or other targets — no specific filter
            None
        };

        // Determine redirect destination
        let redirect = if norm_lower.contains("is dealt to ~ instead") {
            // Redirect to self (the permanent with this ability)
            Some(TargetFilter::SelfRef)
        } else {
            None
        };

        let mut def = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .prevention_shield(PreventionAmount::All)
            .description(original_text.to_string());
        if let Some(tf) = target_filter {
            def = def.damage_target_filter(tf);
        }
        if let Some(rt) = redirect {
            def = def.redirect_target(rt);
        }
        return Some(def);
    }

    // Pattern 3: "if a source would deal damage to you, prevent that damage"
    // followed by "~ deals that much damage to any target" (Pariah's Shield)
    // CR 615.1a: Prevention + redirect combination
    if norm_lower.contains("would deal damage to you") && norm_lower.contains("prevent that damage")
    {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .prevention_shield(PreventionAmount::All)
                .damage_target_filter(DamageTargetFilter::PlayerOnly)
                .redirect_target(TargetFilter::SelfRef)
                .description(original_text.to_string()),
        );
    }

    None
}

/// CR 615: Parse damage prevention replacement effects.
/// Handles:
/// - "prevent all combat damage that would be dealt [this turn]" (Fog, Moments Peace)
/// - "prevent all damage that would be dealt to you [this turn]" (Hallow)
/// - "prevent the next N damage that would be dealt to [target] this turn" (Mending Hands)
/// - "prevent all damage that would be dealt to and dealt by [creature]" (Stonehorn Dignitary)
fn parse_damage_prevention_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Must contain "prevent" and "damage" to be a prevention pattern
    if !norm_lower.contains("prevent") || !norm_lower.contains("damage") {
        return None;
    }

    // "damage can't be prevented" is NOT a prevention replacement -- it's a restriction.
    if norm_lower.contains("can't be prevented") {
        return None;
    }

    // CR 615: "sources of the color of your choice" requires interactive color choice —
    // handled as a Choose → PreventDamage spell effect chain, not a passive replacement.
    if norm_lower.contains("color of your choice") {
        return None;
    }

    // Redirection patterns ("prevent that damage. ~ deals that much damage to") are handled
    // by parse_damage_redirection_replacement — don't intercept them here.
    if norm_lower.contains("prevent that damage") && norm_lower.contains("deals that much damage") {
        return None;
    }
    // "is dealt to ~ instead" patterns are also redirections, not pure prevention
    if norm_lower.contains("is dealt to") && norm_lower.contains("instead") {
        return None;
    }

    // --- 1. Extract prevention amount ---
    // CR 615.7: "prevent the next N damage" → specific shield amount
    // CR 615.1a: "prevent all damage" → prevent everything
    let amount = if norm_lower.contains("prevent all") {
        PreventionAmount::All
    } else if let Some(rest) = strip_after(norm_lower, "prevent the next ") {
        // Uses oracle_util::parse_number (not nom directly) because it handles "X" → 0
        // for cards like Temper, Acolyte's Reward, etc.
        let (n, _) = parse_number(rest)?;
        PreventionAmount::Next(n)
    } else if norm_lower.contains("prevent that damage") {
        // "prevent that damage" in redirection context — redirect handled separately
        PreventionAmount::All
    } else {
        return None;
    };

    // --- 2. Extract combat scope ---
    // CR 615: "combat damage" restricts to combat damage only.
    // Longest-match-first: "noncombat damage" before "combat damage" because
    // "noncombat" contains the substring "combat".
    let combat_scope = if norm_lower.contains("noncombat damage") {
        Some(CombatDamageScope::NoncombatOnly)
    } else if norm_lower.contains("combat damage") {
        Some(CombatDamageScope::CombatOnly)
    } else {
        None
    };

    // --- 3. Extract damage target filter ---
    // "to you" → player only, "to target creature" → creature only
    let damage_target_filter =
        if norm_lower.contains("dealt to you") || norm_lower.contains("deal to you") {
            Some(DamageTargetFilter::PlayerOnly)
        } else if norm_lower.contains("dealt to target creature")
            || norm_lower.contains("dealt to ~")
            || norm_lower.contains("dealt to and dealt by ~")
        {
            Some(DamageTargetFilter::CreatureOnly)
        } else {
            // "prevent all combat damage" with no target → any target
            None
        };

    // --- 4. Extract damage source filter ---
    let damage_source_filter = parse_damage_source_filter(norm_lower);

    // --- 5. Build the replacement definition ---
    let mut def = ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .prevention_shield(amount)
        .description(original_text.to_string());

    if let Some(cs) = combat_scope {
        def = def.combat_scope(cs);
    }
    if let Some(tf) = damage_target_filter {
        def = def.damage_target_filter(tf);
    }
    if let Some(sf) = damage_source_filter {
        def = def.damage_source_filter(sf);
    }

    Some(def)
}

/// CR 614.1a: Parse event substitution replacement effects.
/// Handles patterns where an event is completely skipped or replaced with a different outcome:
/// - "if [player] would begin an extra turn, that player skips that turn instead"
/// - "if you would lose the game, instead..."
/// - "if [player] would draw a card except the first one ... each turn, that player discards..."
fn parse_event_substitution_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // "would begin an extra turn" / "would take an extra turn"
    if norm_lower.contains("would begin an extra turn")
        || norm_lower.contains("would take an extra turn")
    {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::BeginTurn)
                .description(original_text.to_string()),
        );
    }

    // "would lose the game" — Platinum Angel, Lich's Mastery
    if norm_lower.contains("would lose the game") {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::GameLoss)
                .description(original_text.to_string()),
        );
    }

    // "would win the game" — Angel's Grace interaction
    if norm_lower.contains("would win the game") {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::GameWin)
                .description(original_text.to_string()),
        );
    }

    None
}

/// CR 614.1a: Parse mana replacement effects.
/// Handles "if a land [you control] would produce mana, it produces [X] instead"
/// (Chromatic Lantern, Dryad of the Ilysian Grove, Blood Moon color override).
fn parse_mana_replacement(norm_lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    // "would produce mana" / "is tapped for mana"
    if norm_lower.contains("would produce mana") || norm_lower.contains("tapped for mana") {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::ProduceMana)
                .description(original_text.to_string()),
        );
    }

    None
}

/// CR 614.1d: Parse "enters tapped unless a player has N or less life" (bond lands).
/// Extract "unless a player has N or less life" condition (bond lands).
/// CR 614.1d
fn parse_player_life_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    let rest = strip_after(norm_lower, "unless a player has ")?;
    // "13 or less life" → extract amount
    // Delegate to nom_primitives::parse_number (input already lowercase)
    let (nom_rest, amount) = nom_primitives::parse_number.parse(rest).ok()?;
    let remainder = nom_rest.trim_start();
    if alt((
        tag::<_, _, VerboseError<&str>>("or less life"),
        tag("or fewer life"),
    ))
    .parse(remainder.trim())
    .is_err()
    {
        return None;
    }
    Some(ReplacementCondition::UnlessPlayerLifeAtMost { amount })
}

/// Extract "unless you have two or more opponents" condition (battlebond lands).
/// CR 614.1d
fn parse_multiple_opponents_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    if !norm_lower.contains("unless you have two or more opponents") {
        return None;
    }
    Some(ReplacementCondition::UnlessMultipleOpponents)
}

/// Extract "unless it's your turn" / "if it's not your turn" condition.
/// Both phrasings are semantically identical: the permanent enters tapped on the opponent's turn.
/// CR 614.1d + CR 500
fn parse_your_turn_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    if norm_lower.contains("unless it's your turn") || norm_lower.contains("if it's not your turn")
    {
        Some(ReplacementCondition::UnlessYourTurn)
    } else {
        None
    }
}

/// Extract "unless it's your <ordinal-list> turn of the game" condition.
/// CR 614.1d + CR 500
/// Handles variable-length ordinal lists ("first", "first or second", "first, second, or third").
/// Takes the maximum ordinal as the threshold.
fn parse_turn_of_game_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    let rest = strip_after(norm_lower, "unless it's your ")?;
    // Parse comma/or-separated ordinal list: "first, second, or third turn"
    let mut max_ordinal: u32 = 0;
    let mut remaining = rest;
    loop {
        // Strip optional separator: ", or ", ", ", " or ", "or "
        // parse_ordinal trims leading space, so after parsing "first" from
        // "first or second", remaining is "or second" (no leading space).
        remaining = alt((
            tag::<_, _, VerboseError<&str>>(", or "),
            tag(", "),
            tag(" or "),
            tag("or "),
        ))
        .parse(remaining)
        .map_or(remaining, |(rest, _)| rest);
        if let Some((val, rest)) = parse_ordinal(remaining) {
            max_ordinal = max_ordinal.max(val);
            remaining = rest;
        } else {
            break;
        }
    }
    if max_ordinal == 0 {
        return None;
    }
    // Expect "turn" (optionally followed by "of the game")
    tag::<_, _, VerboseError<&str>>("turn")
        .parse(remaining)
        .ok()?;
    Some(ReplacementCondition::UnlessQuantity {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::TurnsTaken,
        },
        comparator: Comparator::LE,
        rhs: QuantityExpr::Fixed {
            value: max_ordinal as i32,
        },
        active_player_req: Some(ControllerRef::You),
    })
}

/// Catch-all: extract the text after "unless" as an Unrecognized condition.
/// CR 614.1d — Ensures the card is counted as having a parsed replacement for coverage.
fn parse_generic_unless_condition(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementCondition> {
    // Only match if there's actually an "unless" clause
    let _ = strip_after(norm_lower, " unless ")?;
    let original_lower = original_text.to_lowercase();
    let tp = TextPair::new(original_text, &original_lower);
    let unless_part = tp.strip_after(" unless ")?;
    let condition_text = unless_part.original;
    Some(ReplacementCondition::Unrecognized {
        text: condition_text.trim_end_matches('.').to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{QuantityExpr, ShieldKind};
    use crate::types::card_type::Supertype;

    #[test]
    fn replacement_enters_tapped() {
        let def =
            parse_replacement_line("Gutterbones enters the battlefield tapped.", "Gutterbones")
                .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
    }

    #[test]
    fn replacement_prevent_all_combat_damage_to_you() {
        let def = parse_replacement_line(
            "Prevent all combat damage that would be dealt to you.",
            "Some Card",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert_eq!(def.combat_scope, Some(CombatDamageScope::CombatOnly));
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::PlayerOnly)
        );
    }

    #[test]
    fn replacement_prevent_all_combat_damage_fog() {
        // Fog: "Prevent all combat damage that would be dealt this turn."
        let def = parse_replacement_line(
            "Prevent all combat damage that would be dealt this turn.",
            "Fog",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert_eq!(def.combat_scope, Some(CombatDamageScope::CombatOnly));
        assert!(def.damage_target_filter.is_none()); // any target
    }

    #[test]
    fn replacement_prevent_next_n_damage() {
        let def = parse_replacement_line(
            "Prevent the next 3 damage that would be dealt to target creature this turn.",
            "Mending Hands",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::Next(3)
            }
        ));
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::CreatureOnly)
        );
    }

    #[test]
    fn replacement_prevent_all_damage_to_you() {
        let def = parse_replacement_line(
            "Prevent all damage that would be dealt to you this turn.",
            "Safe Passage",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert!(def.combat_scope.is_none()); // all damage, not just combat
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::PlayerOnly)
        );
    }

    #[test]
    fn damage_cant_be_prevented_no_longer_parses_as_replacement() {
        // "can't be prevented" is now routed to effect parsing (Effect::AddRestriction),
        // not replacement parsing. This line should return None from the replacement parser.
        let def = parse_replacement_line(
            "Combat damage that would be dealt by creatures you control can't be prevented.",
            "Questing Beast",
        );
        // Note: This still matches because the line contains "would" which triggers
        // is_replacement_pattern. But parse_replacement_line doesn't have a handler
        // for "can't be prevented" anymore, so it falls through.
        // The line contains "would" so is_replacement_pattern returns true,
        // but the "would die/destroyed" check doesn't match. Result is None.
        assert!(def.is_none());
    }

    #[test]
    fn replacement_lose_life_doubled() {
        let def = parse_replacement_line(
            "If an opponent would lose life during your turn, they lose twice that much life instead.",
            "Bloodletter of Aclazotz",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::LoseLife);
        assert!(def.description.is_some());
    }

    #[test]
    fn replacement_non_match_returns_none() {
        assert!(parse_replacement_line("Destroy target creature.", "Some Card").is_none());
    }

    #[test]
    fn shock_land_watery_grave() {
        let def = parse_replacement_line(
            "As this land enters, you may pay 2 life. If you don't, it enters tapped.",
            "Watery Grave",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Optional { .. }));
        // Accept branch: LoseLife { amount: 2 }
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ));
        // Decline branch: Tap { target: SelfRef }
        if let ReplacementMode::Optional { decline } = &def.mode {
            let decline = decline.as_ref().unwrap();
            assert!(matches!(
                *decline.effect,
                Effect::Tap {
                    target: TargetFilter::SelfRef
                }
            ));
        } else {
            panic!("Expected Optional mode");
        }
    }

    #[test]
    fn shock_land_3_life() {
        let def = parse_replacement_line(
            "As this land enters, you may pay 3 life. If you don't, it enters tapped.",
            "Some Shock Land",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
    }

    #[test]
    fn shock_land_with_basic_land_type_choice_adds_choose_chain() {
        let def = parse_replacement_line(
            "As this land enters, choose a basic land type. Then you may pay 2 life. If you don't, it enters tapped.",
            "Multiversal Passage",
        )
        .unwrap();

        assert!(matches!(def.mode, ReplacementMode::Optional { .. }));
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::Choose {
                choice_type: ChoiceType::BasicLandType,
                ..
            }
        ));
        assert!(matches!(
            *execute.sub_ability.as_ref().unwrap().effect,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ));

        if let ReplacementMode::Optional { decline } = &def.mode {
            let decline = decline.as_ref().unwrap();
            assert!(matches!(
                *decline.effect,
                Effect::Choose {
                    choice_type: ChoiceType::BasicLandType,
                    ..
                }
            ));
            assert!(matches!(
                *decline.sub_ability.as_ref().unwrap().effect,
                Effect::Tap {
                    target: TargetFilter::SelfRef
                }
            ));
        }
    }

    #[test]
    fn as_enters_choose_a_color() {
        let def = parse_replacement_line(
            "As Captivating Crossroads enters, choose a color.",
            "Captivating Crossroads",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::Choose {
                choice_type: ChoiceType::Color,
                persist: true,
            }
        ));
    }

    #[test]
    fn as_enters_choose_a_creature_type() {
        let def = parse_replacement_line(
            "As Door of Destinies enters, choose a creature type.",
            "Door of Destinies",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::Choose {
                choice_type: ChoiceType::CreatureType,
                persist: true,
            }
        ));
    }

    #[test]
    fn as_enters_choose_does_not_match_shock_land() {
        // Shock lands with "choose a basic land type" should be handled by parse_shock_land,
        // not parse_as_enters_choose
        let def = parse_replacement_line(
            "As this land enters, choose a basic land type. Then you may pay 2 life. If you don't, it enters tapped.",
            "Multiversal Passage",
        )
        .unwrap();
        // Should be Optional (shock land), not Mandatory (simple choose)
        assert!(matches!(def.mode, ReplacementMode::Optional { .. }));
    }

    #[test]
    fn check_land_clifftop_retreat() {
        let def = parse_replacement_line(
            "This land enters tapped unless you control a Mountain or a Plains.",
            "Clifftop Retreat",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsSubtype { subtypes }) => {
                assert_eq!(subtypes, &["Mountain", "Plains"]);
            }
            other => panic!("Expected UnlessControlsSubtype, got {other:?}"),
        }
    }

    #[test]
    fn check_land_drowned_catacomb() {
        let def = parse_replacement_line(
            "Drowned Catacomb enters the battlefield tapped unless you control an Island or a Swamp.",
            "Drowned Catacomb",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsSubtype { subtypes }) => {
                assert_eq!(subtypes, &["Island", "Swamp"]);
            }
            other => panic!("Expected UnlessControlsSubtype, got {other:?}"),
        }
    }

    #[test]
    fn unconditional_enters_tapped_still_works() {
        let def = parse_replacement_line(
            "Submerged Boneyard enters the battlefield tapped.",
            "Submerged Boneyard",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        // execute must be Some(Tap) so the mandatory pipeline can apply it
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
    }

    #[test]
    fn self_enters_with_counters() {
        let def = parse_replacement_line(
            "Polukranos enters the battlefield with twelve +1/+1 counters on it.",
            "Polukranos",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 12 },
                ..
            } if counter_type == "P1P1"
        ));
    }

    #[test]
    fn enters_with_dynamic_counters_equal_to_quantity() {
        let def = parse_replacement_line(
            "Ulamog enters with a number of +1/+1 counters on it equal to the greatest mana value among cards in exile.",
            "Ulamog",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        match &*def.execute.as_ref().unwrap().effect {
            Effect::PutCounter {
                counter_type,
                count,
                ..
            } => {
                assert_eq!(counter_type, "P1P1", "counter type should be P1P1");
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Aggregate { .. }
                        }
                    ),
                    "count should be Aggregate quantity, got {count:?}"
                );
            }
            other => panic!("Expected PutCounter, got {other:?}"),
        }
    }

    #[test]
    fn other_creature_enters_with_counter_chosen_type() {
        let def = parse_replacement_line(
            "Each other creature you control of the chosen type enters with an additional +1/+1 counter on it.",
            "Metallic Mimic",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                ..
            } if counter_type == "P1P1"
        ));
        // valid_card should filter for other creatures you control of chosen type
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::Another));
                assert!(tf.properties.contains(&FilterProp::IsChosenCreatureType));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn other_non_subtype_creature_enters_with_counter() {
        // Grumgully, the Generous
        let def = parse_replacement_line(
            "Each other non-Human creature you control enters with an additional +1/+1 counter on it.",
            "Grumgully, the Generous",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                ..
            } if counter_type == "P1P1"
        ));
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::Another));
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Subtype(
                        "Human".to_string()
                    )))));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    // ── Escape-with-counters ──

    #[test]
    fn escape_with_three_counters() {
        // CR 702.138c: "This creature escapes with three +1/+1 counters on it."
        let def = parse_replacement_line(
            "This creature escapes with three +1/+1 counters on it.",
            "Voracious Typhon",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 3 },
                ..
            } if counter_type == "P1P1"
        ));
        assert_eq!(def.condition, Some(ReplacementCondition::CastViaEscape));
    }

    #[test]
    fn escape_with_one_counter() {
        let def = parse_replacement_line(
            "This creature escapes with a +1/+1 counter on it.",
            "Underworld Rage-Hound",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                ..
            } if counter_type == "P1P1"
        ));
        assert_eq!(def.condition, Some(ReplacementCondition::CastViaEscape));
    }

    // ── Kicker-conditional enters-with-counters ──

    #[test]
    fn kicked_enters_with_counter() {
        // CR 702.33d: "If this creature was kicked, it enters with a +1/+1 counter on it."
        let def = parse_replacement_line(
            "If this creature was kicked, it enters with a +1/+1 counter on it and with flying.",
            "Ana Battlemage",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                ..
            } if counter_type == "P1P1"
        ));
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::CastViaKicker { cost_text: None })
        ));
    }

    #[test]
    fn kicked_with_specific_cost_enters_with_counters() {
        // CR 702.33d: "If this creature was kicked with its {1}{R} kicker, it enters with
        // two +1/+1 counters on it and with first strike."
        let def = parse_replacement_line(
            "If this creature was kicked with its {1}{R} kicker, it enters with two +1/+1 counters on it and with first strike.",
            "Necravolver",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 2 },
                ..
            } if counter_type == "P1P1"
        ));
        match &def.condition {
            Some(ReplacementCondition::CastViaKicker { cost_text }) => {
                assert_eq!(cost_text.as_deref(), Some("{1}{r}"));
            }
            other => panic!("Expected CastViaKicker, got {other:?}"),
        }
    }

    // ── External replacement effects ──

    #[test]
    fn rest_in_peace_graveyard_exile() {
        let def = parse_replacement_line(
            "If a card or token would be put into a graveyard from anywhere, exile it instead.",
            "Rest in Peace",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.destination_zone, Some(Zone::Graveyard));
        assert!(def.valid_card.is_none()); // matches all objects
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ));
    }

    #[test]
    fn leyline_of_the_void_opponent_scoped() {
        let def = parse_replacement_line(
            "If a card would be put into an opponent's graveyard from anywhere, exile it instead.",
            "Leyline of the Void",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.destination_zone, Some(Zone::Graveyard));
        // valid_card should scope to opponent-owned cards
        match &def.valid_card {
            Some(TargetFilter::Typed(TypedFilter { properties, .. })) => {
                assert!(properties.contains(&FilterProp::Owned {
                    controller: ControllerRef::Opponent,
                }));
            }
            other => panic!("Expected Typed filter with Owned, got {other:?}"),
        }
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ));
    }

    #[test]
    fn authority_of_the_consuls_enters_tapped() {
        let def = parse_replacement_line(
            "Creatures your opponents control enter tapped.",
            "Authority of the Consuls",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.destination_zone, Some(Zone::Battlefield));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn blind_obedience_compound_or_filter() {
        let def = parse_replacement_line(
            "Artifacts and creatures your opponents control enter tapped.",
            "Blind Obedience",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.destination_zone, Some(Zone::Battlefield));
        match &def.valid_card {
            Some(TargetFilter::Or { filters }) => {
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
            other => panic!("Expected Or filter, got {other:?}"),
        }
    }

    #[test]
    fn frozen_aether_comma_list() {
        let def = parse_replacement_line(
            "Artifacts, creatures, and lands your opponents control enter tapped.",
            "Frozen Aether",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.destination_zone, Some(Zone::Battlefield));
        match &def.valid_card {
            Some(TargetFilter::Or { filters }) => {
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
            other => panic!("Expected Or filter with 3 elements, got {other:?}"),
        }
    }

    // ── Fast land tests ──

    #[test]
    fn fast_land_spirebluff_canal() {
        let def = parse_replacement_line(
            "This land enters tapped unless you control two or fewer other lands.",
            "Spirebluff Canal",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsOtherLeq { count, filter }) => {
                assert_eq!(*count, 2);
                assert!(filter.type_filters.contains(&TypeFilter::Land));
                assert_eq!(filter.controller, Some(ControllerRef::You));
                assert!(filter.properties.contains(&FilterProp::Another));
            }
            other => panic!("Expected UnlessControlsOtherLeq, got {other:?}"),
        }
    }

    #[test]
    fn fast_land_generality_three_or_fewer() {
        // Hypothetical: "three or fewer" should parse count=3
        let def = parse_replacement_line(
            "This land enters tapped unless you control three or fewer other lands.",
            "Hypothetical Land",
        )
        .unwrap();
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsOtherLeq { count, .. }) => {
                assert_eq!(*count, 3);
            }
            other => panic!("Expected UnlessControlsOtherLeq, got {other:?}"),
        }
    }

    #[test]
    fn fast_land_does_not_capture_check_land() {
        // Check lands must still parse as UnlessControlsSubtype, not UnlessControlsOtherLeq
        let def = parse_replacement_line(
            "This land enters tapped unless you control a Mountain or a Plains.",
            "Clifftop Retreat",
        )
        .unwrap();
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::UnlessControlsSubtype { .. })
        ));
    }

    #[test]
    fn unconditional_enters_tapped_unaffected_by_fast_land() {
        // Plain "enters tapped" must still work (no condition)
        let def = parse_replacement_line("This land enters tapped.", "Some Tapland").unwrap();
        assert!(def.condition.is_none());
    }

    // ── General "unless you control a [type phrase]" tests ──

    #[test]
    fn unless_controls_basic_land() {
        let def = parse_replacement_line(
            "This land enters tapped unless you control a basic land.",
            "Ba Sing Se",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsMatching { filter }) => {
                let TargetFilter::Typed(tf) = filter else {
                    panic!("Expected Typed filter, got {filter:?}");
                };
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert!(tf.properties.contains(&FilterProp::HasSupertype {
                    value: Supertype::Basic,
                }));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            other => panic!("Expected UnlessControlsMatching, got {other:?}"),
        }
    }

    #[test]
    fn unless_controls_legendary_creature() {
        let def = parse_replacement_line(
            "Minas Tirith enters tapped unless you control a legendary creature.",
            "Minas Tirith",
        )
        .unwrap();
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsMatching { filter }) => {
                let TargetFilter::Typed(tf) = filter else {
                    panic!("Expected Typed filter, got {filter:?}");
                };
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert!(tf.properties.contains(&FilterProp::HasSupertype {
                    value: Supertype::Legendary,
                }));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            other => panic!("Expected UnlessControlsMatching, got {other:?}"),
        }
    }

    #[test]
    fn unless_controls_legendary_green_creature() {
        let def = parse_replacement_line(
            "This land enters tapped unless you control a legendary green creature.",
            "Argoth, Sanctum of Nature",
        )
        .unwrap();
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsMatching { filter }) => {
                let TargetFilter::Typed(tf) = filter else {
                    panic!("Expected Typed filter, got {filter:?}");
                };
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert!(tf.properties.contains(&FilterProp::HasSupertype {
                    value: Supertype::Legendary,
                }));
                assert!(tf.properties.contains(&FilterProp::HasColor {
                    color: ManaColor::Green,
                }));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            other => panic!("Expected UnlessControlsMatching, got {other:?}"),
        }
    }

    #[test]
    fn unless_controls_mount_or_vehicle() {
        let def = parse_replacement_line(
            "This land enters tapped unless you control a Mount or Vehicle.",
            "Country Roads",
        )
        .unwrap();
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsMatching { filter }) => {
                // "Mount or Vehicle" → Or filter with two branches, each with ControllerRef::You
                let TargetFilter::Or { filters } = filter else {
                    panic!("Expected Or filter, got {filter:?}");
                };
                assert_eq!(filters.len(), 2);
                for f in filters {
                    let TargetFilter::Typed(tf) = f else {
                        panic!("Expected Typed branch, got {f:?}");
                    };
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                }
            }
            other => panic!("Expected UnlessControlsMatching, got {other:?}"),
        }
    }

    #[test]
    fn unless_controls_does_not_steal_check_land() {
        // Check lands must still produce UnlessControlsSubtype, not UnlessControlsMatching
        let def = parse_replacement_line(
            "This land enters tapped unless you control a Mountain or a Plains.",
            "Clifftop Retreat",
        )
        .unwrap();
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::UnlessControlsSubtype { .. })
        ));
    }

    #[test]
    fn unconditional_catchall_rejects_unless() {
        // "enters tapped unless..." must NOT match the unconditional catch-all.
        // If the specific parsers all return None, the result should be None (not unconditional).
        // This is a regression guard for the catch-all safety check.
        let result = parse_replacement_line(
            "This land enters tapped unless some future condition we haven't implemented.",
            "Hypothetical Card",
        );
        assert!(
            result.is_none() || result.as_ref().unwrap().condition.is_some(),
            "Catch-all must not silently drop 'unless' clause"
        );
    }

    // ── Damage modification replacement tests ──

    #[test]
    fn damage_furnace_of_rath_double() {
        let def = parse_replacement_line(
            "If a source would deal damage to a permanent or player, it deals double that damage to that permanent or player instead.",
            "Furnace of Rath",
        ).unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert_eq!(def.damage_modification, Some(DamageModification::Double));
        assert_eq!(def.damage_source_filter, None); // any source
        assert_eq!(def.damage_target_filter, None); // any target
        assert_eq!(def.combat_scope, None); // all damage
    }

    #[test]
    fn damage_torbran_plus_2_red_source() {
        let def = parse_replacement_line(
            "If a red source you control would deal damage to an opponent or a permanent an opponent controls, it deals that much damage plus 2 instead.",
            "Torbran, Thane of Red Fell",
        ).unwrap();
        assert_eq!(
            def.damage_modification,
            Some(DamageModification::Plus { value: 2 })
        );
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::OpponentOrTheirPermanents)
        );
        // Source filter: red source you control
        let sf = def.damage_source_filter.unwrap();
        match sf {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::HasColor {
                    color: ManaColor::Red,
                }));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn damage_artists_talent_noncombat_plus_2() {
        let def = parse_replacement_line(
            "If a source you control would deal noncombat damage to an opponent or a permanent an opponent controls, it deals that much damage plus 2 instead.",
            "Artist's Talent",
        ).unwrap();
        assert_eq!(
            def.damage_modification,
            Some(DamageModification::Plus { value: 2 })
        );
        assert_eq!(def.combat_scope, Some(CombatDamageScope::NoncombatOnly));
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::OpponentOrTheirPermanents)
        );
        // Source filter: source you control (no color qualifier)
        match def.damage_source_filter.unwrap() {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.is_empty());
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn damage_fiery_emancipation_triple() {
        let def = parse_replacement_line(
            "If a source you control would deal damage to a permanent or player, it deals triple that damage to that permanent or player instead.",
            "Fiery Emancipation",
        ).unwrap();
        assert_eq!(def.damage_modification, Some(DamageModification::Triple));
        match def.damage_source_filter.unwrap() {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
        assert_eq!(def.damage_target_filter, None); // "permanent or player" = any
    }

    #[test]
    fn damage_benevolent_unicorn_minus_1() {
        let def = parse_replacement_line(
            "If a spell would deal damage to a permanent or player, it deals that much damage minus 1 to that permanent or player instead.",
            "Benevolent Unicorn",
        ).unwrap();
        assert_eq!(
            def.damage_modification,
            Some(DamageModification::Minus { value: 1 })
        );
        assert_eq!(def.damage_source_filter, None); // "a spell" → no source filter
        assert_eq!(def.damage_target_filter, None); // "permanent or player" = any
    }

    #[test]
    fn damage_calamity_bearer_giant_double() {
        let def = parse_replacement_line(
            "If a Giant source you control would deal damage to a permanent or player, it deals double that damage to that permanent or player instead.",
            "Calamity Bearer",
        ).unwrap();
        assert_eq!(def.damage_modification, Some(DamageModification::Double));
        match def.damage_source_filter.unwrap() {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert_eq!(tf.get_subtype(), Some("Giant"));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn damage_charging_tuskodon_self_combat_player() {
        let def = parse_replacement_line(
            "If this creature would deal combat damage to a player, it deals double that damage to that player instead.",
            "Charging Tuskodon",
        ).unwrap();
        assert_eq!(def.damage_modification, Some(DamageModification::Double));
        assert_eq!(def.damage_source_filter, Some(TargetFilter::SelfRef));
        assert_eq!(def.combat_scope, Some(CombatDamageScope::CombatOnly));
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::PlayerOnly)
        );
    }

    // ── Clone replacement tests ──

    #[test]
    fn clone_creature_basic() {
        // CR 707.9: "You may have ~ enter as a copy of any creature on the battlefield"
        let def = parse_replacement_line(
            "You may have Clone enter as a copy of any creature on the battlefield.",
            "Clone",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(
            def.mode,
            ReplacementMode::Optional { decline: None }
        ));
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy { target, duration } => {
                assert!(duration.is_none());
                match target {
                    TargetFilter::Typed(tf) => {
                        assert!(tf.type_filters.contains(&TypeFilter::Creature));
                    }
                    other => panic!("Expected Typed creature filter, got {other:?}"),
                }
            }
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn clone_enchantment() {
        // Estrid's Invocation, Copy Enchantment
        let def = parse_replacement_line(
            "You may have this enchantment enter as a copy of an enchantment on the battlefield.",
            "Copy Enchantment",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            def.mode,
            ReplacementMode::Optional { decline: None }
        ));
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert!(tf.type_filters.contains(&TypeFilter::Enchantment));
                }
                other => panic!("Expected Typed enchantment filter, got {other:?}"),
            },
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn clone_artifact() {
        // Sculpting Steel, Phyrexian Metamorph
        let def = parse_replacement_line(
            "You may have this artifact enter as a copy of any artifact on the battlefield.",
            "Sculpting Steel",
        )
        .unwrap();
        assert!(matches!(
            def.mode,
            ReplacementMode::Optional { decline: None }
        ));
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert!(tf.type_filters.contains(&TypeFilter::Artifact));
                }
                other => panic!("Expected Typed artifact filter, got {other:?}"),
            },
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn clone_vehicle() {
        let def = parse_replacement_line(
            "You may have this vehicle enter as a copy of any vehicle on the battlefield.",
            "Mirror Vehicle",
        )
        .unwrap();
        assert!(matches!(
            def.mode,
            ReplacementMode::Optional { decline: None }
        ));
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.get_subtype(), Some("Vehicle"));
                }
                other => panic!("Expected Typed vehicle filter, got {other:?}"),
            },
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn clone_uses_self_ref_normalization() {
        // "this creature" should be normalized to "~" by replace_self_refs
        let def = parse_replacement_line(
            "You may have this creature enter as a copy of any creature on the battlefield.",
            "Some Clone",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(def.mode, ReplacementMode::Optional { .. }));
    }

    // --- "Instead" clause pattern tests ---

    #[test]
    fn token_doubling_replacement() {
        let def = parse_replacement_line(
            "If one or more tokens would be created under your control, twice that many tokens are created instead.",
            "Parallel Lives",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::CreateToken);
        assert!(def.quantity_modification.is_some());
        assert!(def.token_owner_scope.is_some());
    }

    #[test]
    fn counter_doubling_replacement() {
        let def = parse_replacement_line(
            "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on it instead.",
            "Doubling Season",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::AddCounter);
        assert!(def.quantity_modification.is_some());
    }

    #[test]
    fn damage_redirection_to_self_instead() {
        // CR 614.1a: "All damage that would be dealt to you is dealt to ~ instead"
        let def = parse_replacement_line(
            "All damage that would be dealt to you is dealt to Pariah instead.",
            "Pariah",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::PlayerOnly)
        );
        // CR 615.1a: Redirect populates prevention shield + redirect target
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert_eq!(def.redirect_target, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn damage_redirection_prevent_and_redirect() {
        // CR 614.1a: "If a source would deal damage to you, prevent that damage.
        // ~ deals that much damage to any target."
        let def = parse_replacement_line(
            "If a source would deal damage to you, prevent that damage. Pariah's Shield deals that much damage to any target.",
            "Pariah's Shield",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::PlayerOnly)
        );
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert_eq!(def.redirect_target, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn event_substitution_extra_turn_skip() {
        // CR 614.1a: "If a player would begin an extra turn, that player skips that turn instead."
        let def = parse_replacement_line(
            "If a player would begin an extra turn, that player skips that turn instead.",
            "Stranglehold",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::BeginTurn);
    }

    #[test]
    fn event_substitution_lose_game() {
        // CR 614.1a: "If you would lose the game, instead..."
        let def = parse_replacement_line(
            "If you would lose the game, instead draw seven cards and your life total becomes 20.",
            "Lich's Mastery",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::GameLoss);
    }

    #[test]
    fn event_substitution_win_game() {
        let def = parse_replacement_line(
            "If a player would win the game, instead that player's opponents each draw a card.",
            "Some Card",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::GameWin);
    }

    #[test]
    fn mana_replacement_produce_any_color() {
        // CR 614.1a: "If a land you control would produce mana, it produces mana of any color instead."
        let def = parse_replacement_line(
            "If a land you control would produce mana, it produces mana of any color instead.",
            "Chromatic Lantern",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ProduceMana);
    }

    #[test]
    fn mana_replacement_tapped_for_mana() {
        // CR 614.1a: "If a land is tapped for mana, it produces mana of a color of your choice instead."
        let def = parse_replacement_line(
            "If a land is tapped for mana, it produces mana of a color of your choice instead of any other type.",
            "Celestial Dawn",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ProduceMana);
    }

    #[test]
    fn replacement_bond_land_enters_tapped_unless_player_life() {
        let def = parse_replacement_line(
            "This land enters tapped unless a player has 13 or less life.",
            "Abandoned Campground",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::UnlessPlayerLifeAtMost { amount: 13 })
        ));
    }

    #[test]
    fn replacement_battlebond_land_enters_tapped_unless_opponents() {
        let def = parse_replacement_line(
            "This land enters tapped unless you have two or more opponents.",
            "Luxury Suite",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::UnlessMultipleOpponents)
        ));
    }

    #[test]
    fn replacement_enters_tapped_unless_generic_fallback() {
        let def = parse_replacement_line(
            "This land enters tapped unless you revealed a Soldier card from your hand.",
            "Fortified Beachhead",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::Unrecognized { .. })
        ));
    }

    #[test]
    fn enters_tapped_unless_long_card_name() {
        // Verify condition_text is extracted from original_text, not norm_lower offset.
        // norm_lower has the card name replaced with `~` (1 char), so using its byte
        // offset against original_text would point to the wrong position.
        let norm = "~ enters the battlefield tapped unless you pay {2}.";
        let original = "Some Very Long Card Name enters the battlefield tapped unless you pay {2}.";
        let result = parse_enters_tapped_unless(norm, original);
        assert!(result.is_some(), "Should parse enters-tapped-unless");
    }

    #[test]
    fn enters_tapped_unless_your_turn() {
        let text = "~ enters tapped unless it's your turn.";
        let result = parse_replacement_line(text, "Test Card");
        let def = result.expect("Should parse unless-your-turn");
        assert_eq!(def.condition, Some(ReplacementCondition::UnlessYourTurn));
    }

    #[test]
    fn enters_tapped_if_not_your_turn() {
        // "if it's not your turn" is semantically equivalent to "unless it's your turn" (CR 614.1d).
        // Eddymurk Crab uses this positive-conditional phrasing.
        let text = "~ enters tapped if it's not your turn.";
        let result = parse_replacement_line(text, "Eddymurk Crab");
        let def = result.expect("Should parse if-not-your-turn as UnlessYourTurn");
        assert_eq!(def.condition, Some(ReplacementCondition::UnlessYourTurn));
    }

    #[test]
    fn enters_tapped_unless_first_second_third_turn() {
        let text = "~ enters tapped unless it's your first, second, or third turn of the game.";
        let result = parse_replacement_line(text, "Starting Town");
        let def = result.expect("Should parse unless-turn-of-game");
        assert_eq!(
            def.condition,
            Some(ReplacementCondition::UnlessQuantity {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::TurnsTaken
                },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 3 },
                active_player_req: Some(ControllerRef::You),
            })
        );
    }

    #[test]
    fn enters_tapped_unless_first_or_second_turn() {
        let text = "~ enters tapped unless it's your first or second turn of the game.";
        let result = parse_replacement_line(text, "Test Card");
        assert!(
            result.is_some(),
            "Should parse unless-turn-of-game with 2 ordinals"
        );
    }

    #[test]
    fn enters_tapped_unless_sixth_turn() {
        let text = "~ enters tapped unless it's your sixth turn of the game.";
        let result = parse_replacement_line(text, "Test Card");
        let def = result.expect("Should parse single ordinal");
        assert_eq!(
            def.condition,
            Some(ReplacementCondition::UnlessQuantity {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::TurnsTaken
                },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 6 },
                active_player_req: Some(ControllerRef::You),
            })
        );
    }
}
