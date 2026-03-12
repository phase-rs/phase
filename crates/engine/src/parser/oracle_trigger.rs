use super::oracle_effect::parse_effect_chain;
use super::oracle_target::parse_type_phrase;
use super::oracle_util::strip_reminder_text;
use crate::types::ability::{
    AbilityKind, FilterProp, TargetFilter, TriggerConstraint, TriggerDefinition,
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

    // Parse the effect
    let execute = if !effect_clean.is_empty() {
        Some(Box::new(parse_effect_chain(
            &effect_clean,
            AbilityKind::Spell,
        )))
    } else {
        None
    };

    // Parse the condition
    let (_, mut def) = parse_trigger_condition(&condition_text);
    def.execute = execute;
    def.optional = optional;

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

fn normalize_self_refs(text: &str, card_name: &str) -> String {
    text.replace(card_name, "~")
        .replace("this creature", "~")
        .replace("this enchantment", "~")
        .replace("this artifact", "~")
        .replace("This creature", "~")
        .replace("This enchantment", "~")
        .replace("This artifact", "~")
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
    TriggerDefinition {
        mode: TriggerMode::Unknown("unknown".to_string()),
        execute: None,
        valid_card: None,
        origin: None,
        destination: None,
        trigger_zones: vec![Zone::Battlefield],
        phase: None,
        optional: false,
        combat_damage: false,
        secondary: false,
        valid_target: None,
        valid_source: None,
        description: None,
        constraint: None,
    }
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
fn parse_trigger_subject(text: &str) -> (TargetFilter, &str) {
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
    let after_article = text
        .strip_prefix("a ")
        .or_else(|| text.strip_prefix("an "));
    if let Some(after) = after_article {
        let (filter, rest) = parse_type_phrase(after);
        return (filter, rest);
    }

    // Fallback: no subject parsed, return Any
    (TargetFilter::Any, text)
}

/// Add FilterProp::Another to a TargetFilter. If it's already Typed, append to properties.
/// Otherwise, wrap in a Typed filter with just Another.
fn add_another_prop(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed {
            card_type,
            subtype,
            controller,
            mut properties,
        } => {
            properties.push(FilterProp::Another);
            TargetFilter::Typed {
                card_type,
                subtype,
                controller,
                properties,
            }
        }
        _ => TargetFilter::Typed {
            card_type: None,
            subtype: None,
            controller: None,
            properties: vec![FilterProp::Another],
        },
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

    if lower.contains("you draw a card") {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        return Some((TriggerMode::Drawn, def));
    }

    None
}

/// Parse counter-placement triggers: "a +1/+1 counter is put on ~",
/// "one or more counters are put on ~"
fn try_parse_counter_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // "one or more counters are put on ~" / "a counter is put on ~"
    if lower.contains("counter is put on ~")
        || lower.contains("counters are put on ~")
        || lower.contains("counter is placed on ~")
    {
        let mut def = make_base();
        def.mode = TriggerMode::CounterAdded;
        def.valid_card = Some(TargetFilter::SelfRef);
        return Some((TriggerMode::CounterAdded, def));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Some(TargetFilter::Typed {
                card_type: Some(crate::types::ability::TypeFilter::Creature),
                subtype: None,
                controller: Some(crate::types::ability::ControllerRef::You),
                properties: vec![FilterProp::Another],
            })
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
            Some(TargetFilter::Typed { properties, .. }) => {
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
            Some(TargetFilter::Typed {
                card_type: Some(crate::types::ability::TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![],
            })
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
}
