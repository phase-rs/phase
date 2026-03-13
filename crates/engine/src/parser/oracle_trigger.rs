use super::oracle_effect::parse_effect_chain;
use super::oracle_target::parse_type_phrase;
use super::oracle_util::{parse_number, strip_reminder_text};
use crate::types::ability::{
    AbilityKind, ControllerRef, FilterProp, TargetFilter, TypedFilter, TriggerCondition, TriggerConstraint,
    TriggerDefinition,
};
use crate::types::phase::Phase;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

/// Parse a full trigger line into a TriggerDefinition.
/// Input: a line starting with "When", "Whenever", or "At".
/// The card_name is used for self-reference substitution.
pub fn parse_trigger_line(text: &str, card_name: &str) -> TriggerDefinition {
    let text = strip_reminder_text(text);
    // Replace self-references: "this creature", "this enchantment", card name → ~
    let normalized = normalize_self_refs(&text, card_name);
    let lower = normalized.to_lowercase();

    // Split condition from effect at first ", " after the trigger phrase
    let (condition_text, effect_text) = split_trigger(&lower, &normalized);

    // Check for "you may" optional
    let optional = effect_text.to_lowercase().contains("you may ");
    let effect_clean = effect_text
        .to_lowercase()
        .replace("you may ", "")
        .trim()
        .to_string();

    // Extract intervening-if condition from effect text
    let (effect_without_if, if_condition) = extract_if_condition(&effect_clean);

    // Strip constraint sentences so they don't leak into effect parsing as sub-abilities
    let effect_final = strip_constraint_sentences(&effect_without_if);

    // Parse the effect
    let has_up_to = effect_final.contains("up to one");
    let execute = if !effect_final.is_empty() {
        let mut ability = parse_effect_chain(&effect_final, AbilityKind::Spell);
        if has_up_to {
            ability.optional_targeting = true;
        }
        Some(Box::new(ability))
    } else {
        None
    };

    // Parse the condition
    let (_, mut def) = parse_trigger_condition(&condition_text);
    def.execute = execute;
    def.optional = optional;
    def.condition = if_condition;

    // Check for constraint phrases in the full text
    def.constraint = parse_trigger_constraint(&lower);

    def
}

/// Parse trigger constraint from the full trigger text.
fn parse_trigger_constraint(lower: &str) -> Option<TriggerConstraint> {
    if lower.contains("this ability triggers only once each turn")
        || lower.contains("triggers only once each turn")
    {
        return Some(TriggerConstraint::OncePerTurn);
    }
    if lower.contains("this ability triggers only once") {
        return Some(TriggerConstraint::OncePerGame);
    }
    if lower.contains("only during your turn") {
        return Some(TriggerConstraint::OnlyDuringYourTurn);
    }
    None
}

/// Strip constraint sentences from effect text so they don't produce spurious sub-abilities.
/// The constraint itself is already extracted by `parse_trigger_constraint` from the full text.
fn strip_constraint_sentences(text: &str) -> String {
    let patterns = [
        "this ability triggers only once each turn.",
        "this ability triggers only once each turn",
        "triggers only once each turn.",
        "triggers only once each turn",
        "this ability triggers only once.",
        "this ability triggers only once",
        "this ability triggers only during your turn.",
        "this ability triggers only during your turn",
    ];
    let mut result = text.to_string();
    for pattern in &patterns {
        result = result.replace(pattern, "");
    }
    let result = result.trim().to_string();
    if result.ends_with('.') {
        result[..result.len() - 1].trim().to_string()
    } else {
        result
    }
}

/// Extract an "if you've gained N or more life this turn" condition from effect text.
/// Returns (cleaned effect text, optional condition).
fn extract_if_condition(text: &str) -> (String, Option<TriggerCondition>) {
    let lower = text.to_lowercase();

    // Patterns: "if you've gained N or more life this turn" / "if you gained N or more life this turn"
    // Also: "if you've gained life this turn" (minimum = 1, no number)
    let if_patterns = [
        "if you've gained ",
        "if you gained ",
        "if you've gained life this turn",
        "if you gained life this turn",
    ];

    for pattern in &if_patterns {
        if let Some(pos) = lower.find(pattern) {
            let after = &lower[pos + pattern.len()..];

            // "if you've gained life this turn" (no number → minimum 1)
            if pattern.ends_with("life this turn") {
                let cleaned = text[..pos].trim_end().to_string();
                return (
                    cleaned,
                    Some(TriggerCondition::LifeGainedThisTurn { minimum: 1 }),
                );
            }

            // Try to parse "N or more life this turn"
            if let Some(minimum) = parse_life_threshold(after) {
                // Strip the entire "if..." clause from the effect text
                let cleaned = text[..pos].trim_end().to_string();
                return (
                    cleaned,
                    Some(TriggerCondition::LifeGainedThisTurn { minimum }),
                );
            }

            // "life this turn" without a number → minimum 1
            if after.starts_with("life this turn") {
                let cleaned = text[..pos].trim_end().to_string();
                return (
                    cleaned,
                    Some(TriggerCondition::LifeGainedThisTurn { minimum: 1 }),
                );
            }
        }
    }

    // Pattern: "if you control N or more creatures, {effect}"
    if let Some((condition, end_pos)) = parse_control_count_condition(&lower) {
        // Strip the "if you control N or more creatures" clause and keep the rest.
        // The text before the clause + the text after the clause = the effect.
        let before = text[..lower.find("if you control ").unwrap()]
            .trim_end()
            .trim_end_matches(',');
        let after = text[end_pos..].trim_start_matches(',').trim_start();
        let cleaned = if before.is_empty() {
            after.to_string()
        } else if after.is_empty() {
            before.to_string()
        } else {
            format!("{before} {after}")
        };
        return (cleaned, Some(condition));
    }

    (text.to_string(), None)
}

/// Parse "if you control N or more creatures" → (condition, end_byte_offset)
fn parse_control_count_condition(lower: &str) -> Option<(TriggerCondition, usize)> {
    let start = lower.find("if you control ")?;
    let after_prefix = &lower[start + "if you control ".len()..];
    let (n, rest) = parse_number(after_prefix)?;
    if rest.starts_with("or more creatures") {
        let end = start
            + "if you control ".len()
            + (after_prefix.len() - rest.len())
            + "or more creatures".len();
        return Some((TriggerCondition::ControlCreatureCount { minimum: n }, end));
    }
    None
}

/// Parse "N or more life this turn" → N, or "life this turn" → 1
fn parse_life_threshold(text: &str) -> Option<u32> {
    let text = text.trim_start();
    // "3 or more life this turn"
    if let Some(space) = text.find(' ') {
        if let Ok(n) = text[..space].parse::<u32>() {
            return Some(n);
        }
    }
    None
}

fn normalize_self_refs(text: &str, card_name: &str) -> String {
    let mut result = text.replace(card_name, "~");

    // Legendary short name: "Haliya, Guided by Light" → also match "Haliya"
    if let Some(comma_pos) = card_name.find(", ") {
        let short_name = &card_name[..comma_pos];
        if short_name.len() >= 3 {
            result = result.replace(short_name, "~");
        }
    }

    result
        .replace("this creature", "~")
        .replace("this enchantment", "~")
        .replace("this artifact", "~")
        .replace("this land", "~")
        .replace("this Equipment", "~")
        .replace("this equipment", "~")
        .replace("this Aura", "~")
        .replace("this aura", "~")
        .replace("this Vehicle", "~")
        .replace("this vehicle", "~")
        .replace("This creature", "~")
        .replace("This enchantment", "~")
        .replace("This artifact", "~")
        .replace("This land", "~")
        .replace("This Equipment", "~")
        .replace("This Aura", "~")
        .replace("This Vehicle", "~")
}

fn split_trigger(lower: &str, original: &str) -> (String, String) {
    if let Some(comma_pos) = find_effect_boundary(lower) {
        let condition = original[..comma_pos].trim().to_string();
        let effect = original[comma_pos + 2..].trim().to_string();
        (condition, effect)
    } else {
        (original.to_string(), String::new())
    }
}

fn find_effect_boundary(lower: &str) -> Option<usize> {
    lower.find(", ")
}

fn make_base() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Unknown("unknown".to_string()))
        .trigger_zones(vec![Zone::Battlefield])
}

fn parse_trigger_condition(condition: &str) -> (TriggerMode, TriggerDefinition) {
    let lower = condition.to_lowercase();

    // --- Phase triggers: "At the beginning of..." ---
    if let Some(result) = try_parse_phase_trigger(&lower) {
        return result;
    }

    // --- Player triggers: "you gain life", "you cast a spell", "you draw a card" ---
    if let Some(result) = try_parse_player_trigger(&lower) {
        return result;
    }

    // --- Subject + event decomposition ---
    // Strip leading "when"/"whenever"
    let after_keyword = lower
        .strip_prefix("whenever ")
        .or_else(|| lower.strip_prefix("when "))
        .unwrap_or(&lower);

    // Parse the subject ("~", "another creature you control", "a creature", etc.)
    let (subject, rest) = parse_trigger_subject(after_keyword);

    // Parse event verb from the remaining text
    if let Some(result) = try_parse_event(&subject, rest, &lower) {
        return result;
    }

    // --- Fallback ---
    let mut def = make_base();
    let mode = TriggerMode::Unknown(condition.to_string());
    def.mode = mode.clone();
    def.description = Some(condition.to_string());
    (mode, def)
}

// ---------------------------------------------------------------------------
// Subject parsing: extracts the trigger subject filter and remaining text
// ---------------------------------------------------------------------------

/// Parse a trigger subject from the beginning of the condition text (after when/whenever).
/// Returns (TargetFilter for valid_card, remaining text after subject).
///
/// Handles compound subjects joined by "or":
///   "~ or another creature or artifact you control enters"
///   → Or { SelfRef, Typed{Creature, You, [Another]}, Typed{Artifact, You, [Another]} }
///   with remaining text "enters"
fn parse_trigger_subject(text: &str) -> (TargetFilter, &str) {
    let (first, rest) = parse_single_subject(text);

    // Check for "or " combinator to build compound subjects
    let rest_trimmed = rest.trim_start();
    if let Some(after_or) = rest_trimmed.strip_prefix("or ") {
        let (second, final_rest) = parse_trigger_subject(after_or);
        return (merge_or_filters(first, second), final_rest);
    }

    (first, rest)
}

/// Parse a single (non-compound) trigger subject.
fn parse_single_subject(text: &str) -> (TargetFilter, &str) {
    // Self-reference: "~"
    if let Some(rest) = text.strip_prefix("~ ") {
        return (TargetFilter::SelfRef, rest);
    }
    if text == "~" {
        return (TargetFilter::SelfRef, "");
    }

    // "another <type phrase>" — compose with FilterProp::Another
    if let Some(after_another) = text.strip_prefix("another ") {
        let (filter, rest) = parse_type_phrase(after_another);
        let with_another = add_another_prop(filter);
        return (with_another, rest);
    }

    // "a "/"an " + type phrase (general subject)
    let after_article = text.strip_prefix("a ").or_else(|| text.strip_prefix("an "));
    if let Some(after) = after_article {
        let (filter, rest) = parse_type_phrase(after);
        return (filter, rest);
    }

    // Fallback: no subject parsed, return Any
    (TargetFilter::Any, text)
}

/// Merge two filters into an Or, flattening nested Or branches.
fn merge_or_filters(a: TargetFilter, b: TargetFilter) -> TargetFilter {
    let mut filters = Vec::new();
    match a {
        TargetFilter::Or { filters: af } => filters.extend(af),
        other => filters.push(other),
    }
    match b {
        TargetFilter::Or { filters: bf } => filters.extend(bf),
        other => filters.push(other),
    }
    TargetFilter::Or { filters }
}

/// Add FilterProp::Another to a TargetFilter. Distributes into Or branches recursively.
fn add_another_prop(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(TypedFilter {
            card_type,
            subtype,
            controller,
            mut properties,
        }) => {
            properties.push(FilterProp::Another);
            TargetFilter::Typed(TypedFilter {
                card_type,
                subtype,
                controller,
                properties,
            })
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.into_iter().map(add_another_prop).collect(),
        },
        _ => TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Another])),
    }
}

// ---------------------------------------------------------------------------
// Event verb parsing: matches the event after the subject
// ---------------------------------------------------------------------------

/// Try to parse an event verb and build a TriggerDefinition from subject + event.
fn try_parse_event(
    subject: &TargetFilter,
    rest: &str,
    full_lower: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    let rest = rest.trim_start();

    // "enters [the battlefield]"
    if rest.starts_with("enters") {
        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.destination = Some(Zone::Battlefield);
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::ChangesZone, def));
    }

    // "dies"
    if rest.starts_with("dies") {
        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.origin = Some(Zone::Battlefield);
        def.destination = Some(Zone::Graveyard);
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::ChangesZone, def));
    }

    // "deals combat damage"
    if rest.starts_with("deals combat damage") {
        let mut def = make_base();
        def.mode = TriggerMode::DamageDone;
        def.combat_damage = true;
        def.valid_source = Some(subject.clone());
        return Some((TriggerMode::DamageDone, def));
    }

    // "deals damage" (non-combat)
    if rest.starts_with("deals damage") {
        let mut def = make_base();
        def.mode = TriggerMode::DamageDone;
        def.valid_source = Some(subject.clone());
        return Some((TriggerMode::DamageDone, def));
    }

    // "attacks"
    if rest.starts_with("attacks") {
        let mut def = make_base();
        def.mode = TriggerMode::Attacks;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Attacks, def));
    }

    // "leaves the battlefield"
    if rest.starts_with("leaves the battlefield") || rest.starts_with("leaves") {
        let mut def = make_base();
        def.mode = TriggerMode::LeavesBattlefield;
        def.valid_card = Some(subject.clone());
        // LTB triggers fire from the graveyard (object has already moved)
        def.trigger_zones = vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile];
        return Some((TriggerMode::LeavesBattlefield, def));
    }

    // "becomes blocked"
    if rest.starts_with("becomes blocked") {
        let mut def = make_base();
        def.mode = TriggerMode::BecomesBlocked;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::BecomesBlocked, def));
    }

    // "is dealt combat damage" / "is dealt damage"
    if rest.starts_with("is dealt combat damage") {
        let mut def = make_base();
        def.mode = TriggerMode::DamageReceived;
        def.combat_damage = true;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::DamageReceived, def));
    }
    if rest.starts_with("is dealt damage") {
        let mut def = make_base();
        def.mode = TriggerMode::DamageReceived;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::DamageReceived, def));
    }

    // "becomes tapped"
    if rest.starts_with("becomes tapped") {
        let mut def = make_base();
        def.mode = TriggerMode::Taps;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Taps, def));
    }

    // Counter-related events: "a +1/+1 counter is put on ~" / "one or more counters are put on ~"
    if let Some(result) = try_parse_counter_trigger(full_lower) {
        return Some(result);
    }

    None
}

// ---------------------------------------------------------------------------
// Category parsers
// ---------------------------------------------------------------------------

/// Parse phase triggers: "At the beginning of your upkeep/end step/combat/draw step"
fn try_parse_phase_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let stripped = lower.strip_prefix("at the beginning of")?;
    let phase_text = stripped.trim();
    let mut def = make_base();
    def.mode = TriggerMode::Phase;
    if phase_text.contains("upkeep") {
        def.phase = Some(Phase::Upkeep);
    } else if phase_text.contains("end step") {
        def.phase = Some(Phase::End);
    } else if phase_text.contains("combat") {
        def.phase = Some(Phase::BeginCombat);
    } else if phase_text.contains("draw step") {
        def.phase = Some(Phase::Draw);
    }
    Some((TriggerMode::Phase, def))
}

/// Parse player-centric triggers: "you gain life", "you cast a/an ...", "you draw a card"
fn try_parse_player_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    if lower.contains("you gain life") {
        let mut def = make_base();
        def.mode = TriggerMode::LifeGained;
        return Some((TriggerMode::LifeGained, def));
    }

    if lower.contains("you cast a") || lower.contains("you cast an") {
        let mut def = make_base();
        def.mode = TriggerMode::SpellCast;
        return Some((TriggerMode::SpellCast, def));
    }

    // "an opponent casts a [quality] spell" / "a player casts a spell from a graveyard"
    if let Some(casts_pos) = lower.find(" casts a") {
        let who = &lower[..casts_pos];
        let mut def = make_base();
        def.mode = TriggerMode::SpellCast;

        // Determine the caster filter
        if who.contains("opponent") {
            def.valid_target = Some(TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)));
        }

        // Parse the spell quality (e.g., "multicolored spell")
        let after_casts = &lower[casts_pos + " casts a".len()..].trim_start();
        let after_article = after_casts
            .strip_prefix("n ") // "an" → strip the trailing "n "
            .unwrap_or(after_casts)
            .trim_start();
        if after_article.starts_with("multicolored") {
            def.valid_card = Some(TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Multicolored])));
        }

        return Some((TriggerMode::SpellCast, def));
    }

    if lower.contains("you draw a card") {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        return Some((TriggerMode::Drawn, def));
    }

    // "whenever you attack" — player-centric attack trigger
    if lower.contains("whenever you attack") || lower.contains("when you attack") {
        let mut def = make_base();
        def.mode = TriggerMode::YouAttack;
        return Some((TriggerMode::YouAttack, def));
    }

    // "when you cast this spell" — self-cast trigger (fires from stack)
    if lower.contains("when you cast this spell") || lower.contains("when ~ is cast") {
        let mut def = make_base();
        def.mode = TriggerMode::SpellCast;
        def.valid_card = Some(TargetFilter::SelfRef);
        // Cast triggers fire while the spell is on the stack
        def.trigger_zones = vec![Zone::Stack];
        return Some((TriggerMode::SpellCast, def));
    }

    None
}

/// Parse counter-placement triggers from Oracle text.
/// Handles all patterns: passive ("a counter is put on ~"), active ("you put counters on ~"),
/// and with arbitrary subjects ("counters are put on another creature you control").
fn try_parse_counter_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // Must mention both a counter and a placement verb
    if !lower.contains("counter") {
        return None;
    }
    if !lower.contains("put") && !lower.contains("placed") {
        return None;
    }

    // Find "counter(s) ... on SUBJECT" — locate " on " after the counter mention
    let counter_pos = lower.find("counter")?;
    let after_counter = &lower[counter_pos..];
    let on_offset = after_counter.find(" on ")?;
    let subject_start = counter_pos + on_offset + " on ".len();
    let subject_text = lower[subject_start..].trim();

    let mut def = make_base();
    def.mode = TriggerMode::CounterAdded;

    // Parse the subject after "on "
    if subject_text.starts_with('~') {
        def.valid_card = Some(TargetFilter::SelfRef);
    } else {
        let (filter, _) = parse_single_subject(subject_text);
        def.valid_card = Some(filter);
    }

    Some((TriggerMode::CounterAdded, def))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Duration, Effect, LifeAmount, PtValue};

    #[test]
    fn trigger_etb_self() {
        let def = parse_trigger_line(
            "When this creature enters, it deals 1 damage to each opponent.",
            "Goblin Chainwhirler",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_dies() {
        let def = parse_trigger_line(
            "When this creature dies, create a 1/1 white Spirit creature token.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
    }

    #[test]
    fn trigger_combat_damage_to_player() {
        let def = parse_trigger_line(
            "Whenever Eye Collector deals combat damage to a player, each player mills a card.",
            "Eye Collector",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert!(def.combat_damage);
    }

    #[test]
    fn trigger_upkeep() {
        let def = parse_trigger_line(
            "At the beginning of your upkeep, look at the top card of your library.",
            "Delver of Secrets",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
    }

    #[test]
    fn trigger_optional_you_may() {
        let def = parse_trigger_line(
            "When this creature enters, you may draw a card.",
            "Some Card",
        );
        assert!(def.optional);
    }

    #[test]
    fn trigger_attacks() {
        let def = parse_trigger_line(
            "Whenever Goblin Guide attacks, defending player reveals the top card of their library.",
            "Goblin Guide",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
    }

    // --- Subject decomposition tests ---

    #[test]
    fn trigger_another_creature_you_control_enters() {
        let def = parse_trigger_line(
            "Whenever another creature you control enters, put a +1/+1 counter on Hinterland Sanctifier.",
            "Hinterland Sanctifier",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::creature().controller(crate::types::ability::ControllerRef::You).properties(vec![FilterProp::Another])))
        );
    }

    #[test]
    fn trigger_another_creature_enters_no_controller() {
        let def = parse_trigger_line(
            "Whenever another creature enters, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        match &def.valid_card {
            Some(TargetFilter::Typed(TypedFilter { properties, .. })) => {
                assert!(properties.contains(&FilterProp::Another));
            }
            other => panic!("Expected Typed filter with Another, got {:?}", other),
        }
    }

    #[test]
    fn trigger_a_creature_enters() {
        let def = parse_trigger_line(
            "Whenever a creature enters, you gain 1 life.",
            "Soul Warden",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::creature()))
        );
    }

    #[test]
    fn trigger_counter_put_on_self() {
        let def = parse_trigger_line(
            "Whenever a +1/+1 counter is put on ~, draw a card.",
            "Fathom Mage",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_one_or_more_counters_on_self() {
        let def = parse_trigger_line(
            "Whenever one or more counters are put on ~, you gain 1 life.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    // --- Constraint parsing tests ---

    #[test]
    fn trigger_once_each_turn_constraint() {
        let def = parse_trigger_line(
            "Whenever you gain life, put a +1/+1 counter on Exemplar of Light. This ability triggers only once each turn.",
            "Exemplar of Light",
        );
        assert_eq!(def.mode, TriggerMode::LifeGained);
        assert_eq!(
            def.constraint,
            Some(crate::types::ability::TriggerConstraint::OncePerTurn)
        );
    }

    #[test]
    fn trigger_no_constraint_by_default() {
        let def = parse_trigger_line(
            "Whenever you gain life, put a +1/+1 counter on this creature.",
            "Ajani's Pridemate",
        );
        assert_eq!(def.mode, TriggerMode::LifeGained);
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_only_during_your_turn() {
        let def = parse_trigger_line(
            "Whenever a creature enters, draw a card. This ability triggers only during your turn.",
            "Some Card",
        );
        assert_eq!(
            def.constraint,
            Some(crate::types::ability::TriggerConstraint::OnlyDuringYourTurn)
        );
    }

    // --- Compound subject tests ---

    #[test]
    fn trigger_self_or_another_creature_or_artifact_you_control() {
        use crate::types::ability::{ControllerRef, TypeFilter};
        let def = parse_trigger_line(
            "Whenever Haliya or another creature or artifact you control enters, you gain 1 life.",
            "Haliya, Guided by Light",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        match &def.valid_card {
            Some(TargetFilter::Or { filters }) => {
                assert_eq!(filters.len(), 3);
                assert_eq!(filters[0], TargetFilter::SelfRef);
                // Both branches should have Another + You controller
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You).properties(vec![FilterProp::Another]))
                );
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You).properties(vec![FilterProp::Another]))
                );
            }
            other => panic!("Expected Or filter with 3 branches, got {:?}", other),
        }
    }

    #[test]
    fn normalize_legendary_short_name() {
        let result = normalize_self_refs(
            "Whenever Haliya or another creature enters",
            "Haliya, Guided by Light",
        );
        assert_eq!(result, "Whenever ~ or another creature enters");
    }

    #[test]
    fn trigger_self_or_another_creature_enters() {
        let def = parse_trigger_line(
            "Whenever Some Card or another creature enters, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        match &def.valid_card {
            Some(TargetFilter::Or { filters }) => {
                assert_eq!(filters.len(), 2);
                assert_eq!(filters[0], TargetFilter::SelfRef);
                match &filters[1] {
                    TargetFilter::Typed(TypedFilter { properties, .. }) => {
                        assert!(properties.contains(&FilterProp::Another));
                    }
                    other => panic!("Expected Typed with Another, got {:?}", other),
                }
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
    }

    // --- Intervening-if condition tests ---

    #[test]
    fn trigger_haliya_end_step_with_life_condition() {
        let def = parse_trigger_line(
            "At the beginning of your end step, draw a card if you've gained 3 or more life this turn.",
            "Haliya, Guided by Light",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::LifeGainedThisTurn { minimum: 3 })
        );
        // Effect should be just "draw a card" with condition stripped
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_if_gained_life_no_number() {
        let def = parse_trigger_line(
            "At the beginning of your end step, create a Blood token if you gained life this turn.",
            "Some Card",
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::LifeGainedThisTurn { minimum: 1 })
        );
    }

    #[test]
    fn trigger_if_gained_5_or_more_life() {
        let def = parse_trigger_line(
            "At the beginning of each end step, if you gained 5 or more life this turn, create a 4/4 white Angel creature token with flying.",
            "Resplendent Angel",
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::LifeGainedThisTurn { minimum: 5 })
        );
    }

    #[test]
    fn extract_if_strips_condition_from_effect() {
        let (cleaned, cond) =
            extract_if_condition("draw a card if you've gained 3 or more life this turn.");
        assert_eq!(cleaned, "draw a card");
        assert_eq!(
            cond,
            Some(TriggerCondition::LifeGainedThisTurn { minimum: 3 })
        );
    }

    // --- Counter placement with "you put" pattern ---

    #[test]
    fn trigger_you_put_counters_on_self() {
        let def = parse_trigger_line(
            "Whenever you put one or more +1/+1 counters on this creature, draw a card. This ability triggers only once each turn.",
            "Exemplar of Light",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.constraint,
            Some(crate::types::ability::TriggerConstraint::OncePerTurn)
        );
        // Constraint sentence should NOT leak as a sub-ability
        if let Some(ref exec) = def.execute {
            assert!(
                !matches!(
                    exec.effect,
                    crate::types::ability::Effect::Unimplemented { .. }
                ),
                "Effect should be Draw, not Unimplemented"
            );
            assert!(
                exec.sub_ability.is_none(),
                "No spurious sub-ability from constraint text"
            );
        }
    }

    #[test]
    fn trigger_counters_put_on_another_creature_you_control() {
        use crate::types::ability::{ControllerRef, TypeFilter};
        let def = parse_trigger_line(
            "Whenever one or more +1/+1 counters are put on another creature you control, put a +1/+1 counter on this creature.",
            "Enduring Scalelord",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You).properties(vec![FilterProp::Another])))
        );
    }

    #[test]
    fn trigger_you_put_counters_on_creature_you_control() {
        use crate::types::ability::{ControllerRef, TypeFilter};
        let def = parse_trigger_line(
            "Whenever you put one or more +1/+1 counters on a creature you control, draw a card.",
            "The Powerful Dragon",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)))
        );
    }

    #[test]
    fn strip_constraint_does_not_affect_effect() {
        let result =
            strip_constraint_sentences("draw a card. this ability triggers only once each turn.");
        assert_eq!(result, "draw a card");
    }

    #[test]
    fn strip_constraint_preserves_plain_effect() {
        let result = strip_constraint_sentences("put a +1/+1 counter on ~");
        assert_eq!(result, "put a +1/+1 counter on ~");
    }

    // --- Color-filtered trigger subjects ---

    #[test]
    fn trigger_white_creature_you_control_attacks() {
        use crate::types::ability::TypeFilter;
        let def = parse_trigger_line(
            "Whenever a white creature you control attacks, you gain 1 life.",
            "Linden, the Steadfast Queen",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(crate::types::ability::ControllerRef::You)
                    .properties(vec![FilterProp::HasColor { color: "White".to_string() }])
            ))
        );
    }

    // --- New trigger mode tests ---

    #[test]
    fn trigger_land_enters() {
        let def = parse_trigger_line("When this land enters, you gain 1 life.", "Bloodfell Caves");
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_aura_enters() {
        let def = parse_trigger_line(
            "When this Aura enters, tap target creature an opponent controls.",
            "Glaring Aegis",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_equipment_enters() {
        let def = parse_trigger_line(
            "When this Equipment enters, attach it to target creature you control.",
            "Shining Armor",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_vehicle_enters() {
        let def = parse_trigger_line(
            "When this Vehicle enters, create a 1/1 white Pilot creature token.",
            "Some Vehicle",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_leaves_battlefield() {
        let def = parse_trigger_line(
            "When Oblivion Ring leaves the battlefield, return the exiled card to the battlefield.",
            "Oblivion Ring",
        );
        assert_eq!(def.mode, TriggerMode::LeavesBattlefield);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.trigger_zones.contains(&Zone::Graveyard));
        assert!(def.trigger_zones.contains(&Zone::Exile));
    }

    #[test]
    fn trigger_becomes_blocked() {
        let def = parse_trigger_line(
            "Whenever Gustcloak Cavalier becomes blocked, you may untap it and remove it from combat.",
            "Gustcloak Cavalier",
        );
        assert_eq!(def.mode, TriggerMode::BecomesBlocked);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_is_dealt_damage() {
        let def = parse_trigger_line(
            "Whenever Spitemare is dealt damage, it deals that much damage to any target.",
            "Spitemare",
        );
        assert_eq!(def.mode, TriggerMode::DamageReceived);
        assert!(!def.combat_damage);
    }

    #[test]
    fn trigger_is_dealt_combat_damage() {
        let def = parse_trigger_line(
            "Whenever ~ is dealt combat damage, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::DamageReceived);
        assert!(def.combat_damage);
    }

    #[test]
    fn trigger_you_attack() {
        let def = parse_trigger_line(
            "Whenever you attack, create a 1/1 white Soldier creature token.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::YouAttack);
    }

    #[test]
    fn trigger_becomes_tapped() {
        let def = parse_trigger_line(
            "Whenever Night Market Lookout becomes tapped, each opponent loses 1 life and you gain 1 life.",
            "Night Market Lookout",
        );
        assert_eq!(def.mode, TriggerMode::Taps);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_you_cast_this_spell() {
        let def = parse_trigger_line(
            "When you cast this spell, draw cards equal to the greatest power among creatures you control.",
            "Hydroid Krasis",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.trigger_zones.contains(&Zone::Stack));
    }

    #[test]
    fn trigger_opponent_casts_multicolored_spell() {
        let def = parse_trigger_line(
            "Whenever an opponent casts a multicolored spell, you gain 1 life.",
            "Soldier of the Pantheon",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)))
        );
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Multicolored])))
        );
    }

    // --- ControlCreatureCount condition tests ---

    #[test]
    fn trigger_leonin_vanguard_control_creature_count() {
        let def = parse_trigger_line(
            "At the beginning of combat on your turn, if you control three or more creatures, this creature gets +1/+1 until end of turn and you gain 1 life.",
            "Leonin Vanguard",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::BeginCombat));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::ControlCreatureCount { minimum: 3 })
        );
        // Effect: pump self +1/+1 with life gain sub_ability
        let exec = def.execute.as_ref().expect("should have execute");
        assert!(matches!(
            exec.effect,
            Effect::Pump {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                target: TargetFilter::SelfRef,
            }
        ));
        assert_eq!(exec.duration, Some(Duration::UntilEndOfTurn));
        // Sub-ability: gain 1 life
        let sub = exec.sub_ability.as_ref().expect("should have sub_ability");
        assert!(matches!(
            sub.effect,
            Effect::GainLife {
                amount: LifeAmount::Fixed(1),
                ..
            }
        ));
    }

    #[test]
    fn extract_if_control_creature_count() {
        let (cleaned, cond) = extract_if_condition(
            "if you control three or more creatures, ~ gets +1/+1 until end of turn",
        );
        assert_eq!(cleaned, "~ gets +1/+1 until end of turn");
        assert_eq!(
            cond,
            Some(TriggerCondition::ControlCreatureCount { minimum: 3 })
        );
    }
}
