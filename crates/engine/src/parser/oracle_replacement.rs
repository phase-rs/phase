use super::oracle_effect::{parse_effect_chain, try_parse_named_choice};
use super::oracle_quantity::capitalize_first;
use super::oracle_target::parse_type_phrase;
use super::oracle_util::{
    normalize_card_name_refs, parse_number, strip_after, strip_reminder_text, TextPair,
};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ChoiceType, CombatDamageScope, ControllerRef,
    DamageModification, DamageTargetFilter, Effect, FilterProp, QuantityExpr, ReplacementCondition,
    ReplacementDefinition, ReplacementMode, TargetFilter, TypeFilter, TypedFilter,
};
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

    // --- Fast lands: "enters tapped unless you control N or fewer other [type]" ---
    // Must be checked BEFORE check lands (both match "unless you control").
    if let Some(def) = parse_fast_land(&norm_lower, &text) {
        return Some(def);
    }

    // --- Check lands: "enters tapped unless you control a [LandType] or a [LandType]" ---
    // Must be checked BEFORE the generic "enters tapped" pattern.
    if let Some(def) = parse_check_land(&norm_lower, &text) {
        return Some(def);
    }

    // --- General "enters tapped unless you control a [type phrase]" ---
    // CR 614.1d — Catches basic lands, legendary creatures, Mount/Vehicle, etc.
    // Must be checked AFTER check lands (which produce the more specific UnlessControlsSubtype)
    // but BEFORE the unconditional "enters tapped" catch-all.
    if let Some(def) = parse_unless_controls_typed(&norm_lower, &text) {
        return Some(def);
    }

    // --- "You may have ~ enter as a copy of [filter]" (clone replacement) ---
    // CR 707.9: "Enter as a copy" is a replacement effect modifying the ETB event.
    if let Some(def) = parse_clone_replacement(&norm_lower, &text) {
        return Some(def);
    }

    // --- "enters tapped unless a player has N or less life" (bond lands) ---
    // CR 614.1d — Must be checked BEFORE external and unconditional patterns.
    if let Some(def) = parse_unless_player_life(&norm_lower, &text) {
        return Some(def);
    }

    // --- "enters tapped unless you have two or more opponents" (battlebond lands) ---
    if let Some(def) = parse_unless_multiple_opponents(&norm_lower, &text) {
        return Some(def);
    }

    // --- Catch-all: "enters tapped unless [unrecognized condition]" ---
    // CR 614.1d — Recognize the replacement for coverage even when the condition is exotic.
    if let Some(def) = parse_enters_tapped_unless_generic(&norm_lower, &text) {
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

    // --- "Prevent all combat damage" / "damage ... can't be prevented" ---
    if lower.contains("prevent all") && lower.contains("damage") {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::DamageDone).description(text.to_string()),
        );
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

    // --- "If you would gain life, {effect}" ---
    if lower.contains("you would gain life") {
        let effect_text = extract_replacement_effect(&normalized);
        let mut def =
            ReplacementDefinition::new(ReplacementEvent::GainLife).description(text.to_string());
        if let Some(e) = effect_text {
            def = def.execute(parse_effect_chain(&e, AbilityKind::Spell));
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

    // --- "[Subject] enters with N [type] counter(s)" ---
    if lower.contains("enters") && lower.contains("counter") {
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
    let type_text = after_copy
        .strip_prefix("any ")
        .or_else(|| after_copy.strip_prefix("a "))
        .or_else(|| after_copy.strip_prefix("an "))
        .unwrap_or(after_copy);

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
fn parse_check_land(norm_lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    if !norm_lower.contains("enters tapped")
        && !norm_lower.contains("enters the battlefield tapped")
    {
        return None;
    }

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
            .condition(ReplacementCondition::UnlessControlsSubtype { subtypes }),
    )
}

/// Parse fast land pattern: "enters tapped unless you control N or fewer other [type]"
/// Returns Mandatory ReplacementDefinition with an UnlessControlsOtherLeq condition.
/// CR 305.7 + CR 614.1c — fast lands (Spirebluff Canal, Blackcleave Cliffs, etc.).
fn parse_fast_land(norm_lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    if !norm_lower.contains("enters tapped")
        && !norm_lower.contains("enters the battlefield tapped")
    {
        return None;
    }

    let rest = strip_after(norm_lower, "unless you control ")?;

    // Parse "two or fewer other lands." → count=2, remainder="or fewer other lands."
    let (count, after_number) = parse_number(rest)?;
    let after_or_fewer = after_number.trim_start().strip_prefix("or fewer ")?;
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
            .condition(ReplacementCondition::UnlessControlsOtherLeq {
                count,
                filter: typed_filter,
            }),
    )
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

/// Parse general "enters tapped unless you control a [type phrase]" pattern.
/// CR 614.1d — Handles basic lands, legendary creatures, Mount/Vehicle, etc.
/// Uses `parse_type_phrase` to parse the type expression after article stripping.
fn parse_unless_controls_typed(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    if !norm_lower.contains("enters tapped")
        && !norm_lower.contains("enters the battlefield tapped")
    {
        return None;
    }

    let rest = strip_after(norm_lower, "unless you control ")?;
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
    // CR 614.1d — consistent with parse_fast_land controller injection pattern
    let filter = inject_controller_you(filter);

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
            .condition(ReplacementCondition::UnlessControlsMatching { filter }),
    )
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

/// Parse "enters with N [type] counter(s)" patterns into a Moved replacement.
/// Handles both self ("~ enters with") and other ("each other creature ... enters with").
fn parse_enters_with_counters(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Find "with [N] [type] counter" to extract count and counter type
    let after_with = strip_after(norm_lower, "with ")?;
    // Skip "an additional" if present
    let after_additional = after_with
        .strip_prefix("an additional ")
        .or_else(|| after_with.strip_prefix("additional "))
        .unwrap_or(after_with);
    let (count, rest) = parse_number(after_additional).unwrap_or((1, after_additional));
    // Next word(s) before "counter" are the counter type
    let counter_pos = rest.find("counter")?;
    let counter_type_raw = rest[..counter_pos].trim();
    let counter_type = match counter_type_raw {
        "+1/+1" => "P1P1".to_string(),
        "-1/-1" => "M1M1".to_string(),
        other => other.to_uppercase(),
    };

    let put_counter = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type,
            count: count as i32,
            target: TargetFilter::SelfRef,
        },
    );

    // Determine valid_card filter: self vs other creatures
    // Strip "each other " or "other " prefix, then delegate to parse_type_phrase
    // which handles non-X, controller, "of the chosen type", etc.
    let subject = norm_lower
        .strip_prefix("each other ")
        .or_else(|| norm_lower.strip_prefix("other "))
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
    Some(def)
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
    let modification = if norm_lower.contains("double that damage")
        || norm_lower.contains("deals double that damage")
    {
        DamageModification::Double
    } else if norm_lower.contains("triple that damage")
        || norm_lower.contains("deals triple that damage")
    {
        DamageModification::Triple
    } else if let Some(rest) = strip_after(norm_lower, "that much damage plus ") {
        let (value, _) = parse_number(rest)?;
        DamageModification::Plus { value }
    } else if let Some(rest) = strip_after(norm_lower, "that much damage minus ") {
        let (value, _) = parse_number(rest)?;
        DamageModification::Minus { value }
    } else {
        return None; // Exotic pattern — fall through to stub
    };

    // --- 2. Extract source filter from the subject clause (before "would deal") ---
    let source_filter = parse_damage_source_filter(norm_lower);

    // --- 3. Extract combat scope ---
    let combat_scope = if norm_lower.contains("would deal noncombat damage") {
        Some(CombatDamageScope::NoncombatOnly)
    } else if norm_lower.contains("would deal combat damage") {
        Some(CombatDamageScope::CombatOnly)
    } else {
        None
    };

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
    let subject = subject
        .strip_prefix("a ")
        .or_else(|| subject.strip_prefix("an "))
        .unwrap_or(subject)
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
            } else if let Some(rest) = prefix.strip_prefix("another ") {
                props.push(FilterProp::Another);
                rest.trim()
            } else {
                prefix
            };

            // Check for color qualifier (e.g. "red")
            if is_color_word(qualifier) {
                props.push(FilterProp::HasColor {
                    color: capitalize_first(qualifier),
                });
            }
            // CR 205.4b: "noncreature" qualifier — negation via TypeFilter::Non
            else if let Some(rest) = qualifier.strip_prefix("non") {
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
fn parse_damage_target_filter(norm_lower: &str) -> Option<DamageTargetFilter> {
    if norm_lower.contains("to an opponent or a permanent an opponent controls") {
        return Some(DamageTargetFilter::OpponentOrTheirPermanents);
    }
    if norm_lower.contains("to a creature") || norm_lower.contains("to that creature") {
        return Some(DamageTargetFilter::CreatureOnly);
    }
    if (norm_lower.contains("to a player") || norm_lower.contains("to that player"))
        && !norm_lower.contains("permanent")
    {
        return Some(DamageTargetFilter::PlayerOnly);
    }
    None
}

fn is_color_word(word: &str) -> bool {
    matches!(word, "white" | "blue" | "black" | "red" | "green")
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
/// Handles "twice that many ... counters" (Primal Vigor, Doubling Season).
fn parse_counter_replacement(lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    use crate::types::ability::QuantityModification;

    let modification = if lower.contains("twice that many") {
        QuantityModification::Double
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
    // Pattern 1: "all damage that would be dealt to [X] is dealt to ~ instead"
    // Pattern 2: "damage that would be dealt to [X] is dealt to ~ instead"
    if norm_lower.contains("would be dealt to") && norm_lower.contains("is dealt to") {
        let target_filter = if norm_lower.contains("would be dealt to you") {
            Some(DamageTargetFilter::PlayerOnly)
        } else if norm_lower.contains("would be dealt to ~") {
            // Self-redirect (unusual, but handle for completeness)
            None
        } else {
            None
        };

        let mut def = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .description(original_text.to_string());
        if let Some(tf) = target_filter {
            def = def.damage_target_filter(tf);
        }
        return Some(def);
    }

    // Pattern 3: "if a source would deal damage to you, prevent that damage"
    // followed by "~ deals that much damage to any target"
    if norm_lower.contains("would deal damage to you") && norm_lower.contains("prevent that damage")
    {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::DealtDamage)
                .damage_target_filter(DamageTargetFilter::PlayerOnly)
                .description(original_text.to_string()),
        );
    }

    None
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
fn parse_unless_player_life(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    if !norm_lower.contains("enters tapped")
        && !norm_lower.contains("enters the battlefield tapped")
    {
        return None;
    }
    let rest = strip_after(norm_lower, "unless a player has ")?;
    // "13 or less life" → extract amount
    let (amount, remainder) = parse_number(rest)?;
    if !remainder.trim().starts_with("or less life")
        && !remainder.trim().starts_with("or fewer life")
    {
        return None;
    }
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
            .condition(ReplacementCondition::UnlessPlayerLifeAtMost { amount }),
    )
}

/// CR 614.1d: Parse "enters tapped unless you have two or more opponents" (battlebond lands).
fn parse_unless_multiple_opponents(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    if !norm_lower.contains("enters tapped")
        && !norm_lower.contains("enters the battlefield tapped")
    {
        return None;
    }
    if !norm_lower.contains("unless you have two or more opponents") {
        return None;
    }
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
            .condition(ReplacementCondition::UnlessMultipleOpponents),
    )
}

/// CR 614.1d: Catch-all "enters tapped unless [unrecognized condition]".
/// Recognizes the replacement as a Moved event with an unrecognized condition.
/// This ensures the card is counted as having a parsed replacement for coverage.
fn parse_enters_tapped_unless_generic(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    if !norm_lower.contains("enters tapped")
        && !norm_lower.contains("enters the battlefield tapped")
    {
        return None;
    }
    let original_lower = original_text.to_lowercase();
    let tp = TextPair::new(original_text, &original_lower);
    let unless_part = tp.strip_after(" unless ")?;
    let condition_text = unless_part.original;
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
            .condition(ReplacementCondition::Unrecognized {
                text: condition_text.trim_end_matches('.').to_string(),
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn replacement_prevent_all_combat_damage() {
        let def = parse_replacement_line(
            "Prevent all combat damage that would be dealt to you.",
            "Some Card",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
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
                amount: QuantityExpr::Fixed { value: 2 }
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
                amount: QuantityExpr::Fixed { value: 3 }
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
                amount: QuantityExpr::Fixed { value: 2 }
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
                count: 12,
                ..
            } if counter_type == "P1P1"
        ));
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
                count: 1,
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
                count: 1,
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
                    value: "Basic".to_string(),
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
                    value: "Legendary".to_string(),
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
                    value: "Legendary".to_string(),
                }));
                assert!(tf.properties.contains(&FilterProp::HasColor {
                    color: "Green".to_string(),
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
                    color: "Red".to_string()
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
        assert_eq!(def.event, ReplacementEvent::DealtDamage);
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::PlayerOnly)
        );
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
        let result = parse_enters_tapped_unless_generic(norm, original);
        assert!(result.is_some(), "Should parse enters-tapped-unless");
    }
}
