use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ModalChoice, ModalSelectionConstraint,
};

use super::oracle::find_activated_colon;
use super::oracle_effect::parse_effect_chain;
use super::oracle_util::{parse_mana_symbols, strip_reminder_text};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OracleBlockAst {
    ActivatedModal {
        cost_text: String,
        header: ModalHeaderAst,
        modes: Vec<ModeAst>,
    },
    Modal {
        header: ModalHeaderAst,
        modes: Vec<ModeAst>,
    },
    TriggeredModal {
        trigger_line: String,
        header: ModalHeaderAst,
        modes: Vec<ModeAst>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModeAst {
    pub(crate) raw: String,
    pub(crate) label: Option<String>,
    pub(crate) body: String,
    /// Per-mode additional cost (Spree). None for standard `•` modes.
    pub(crate) mode_cost: Option<crate::types::mana::ManaCost>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModalHeaderAst {
    pub(crate) raw: String,
    pub(crate) min_choices: usize,
    pub(crate) max_choices: usize,
    pub(crate) allow_repeat_modes: bool,
    pub(crate) constraints: Vec<ModalSelectionConstraint>,
}

pub(crate) fn parse_oracle_block(lines: &[&str], start: usize) -> Option<(OracleBlockAst, usize)> {
    let line = strip_reminder_text(lines.get(start)?.trim());
    if line.is_empty() {
        return None;
    }

    let modes = collect_mode_asts(lines, start + 1);
    if modes.is_empty() {
        return None;
    }

    let next = start + 1 + modes.len();

    if let Some(colon_pos) = find_activated_colon(&line) {
        let cost_text = line[..colon_pos].trim();
        let effect_text = line[colon_pos + 1..].trim();
        if let Some(header) = parse_modal_header_ast(effect_text) {
            return Some((
                OracleBlockAst::ActivatedModal {
                    cost_text: cost_text.to_string(),
                    header,
                    modes,
                },
                next,
            ));
        }
    }

    let candidate = strip_ability_word(&line).unwrap_or_else(|| line.clone());
    let lower = candidate.to_lowercase();

    if let Some(header) = parse_modal_header_ast(&candidate) {
        if !lower.starts_with("when ")
            && !lower.starts_with("whenever ")
            && !lower.starts_with("at ")
        {
            return Some((OracleBlockAst::Modal { header, modes }, next));
        }
    }

    if let Some((trigger_line, header)) = split_triggered_modal_header(&candidate) {
        if let Some(header) = parse_modal_header_ast(&header) {
            return Some((
                OracleBlockAst::TriggeredModal {
                    trigger_line,
                    header,
                    modes,
                },
                next,
            ));
        }
    }

    // CR 702.172: Spree keyword line + all modes have per-mode costs
    if line.eq_ignore_ascii_case("spree")
        && !modes.is_empty()
        && modes.iter().all(|m| m.mode_cost.is_some())
    {
        let header = ModalHeaderAst {
            raw: line.to_string(),
            min_choices: 1,
            max_choices: modes.len(),
            allow_repeat_modes: false,
            constraints: vec![],
        };
        return Some((OracleBlockAst::Modal { header, modes }, next));
    }

    None
}

pub(crate) fn collect_mode_asts(lines: &[&str], start: usize) -> Vec<ModeAst> {
    let mut modes = Vec::new();

    for raw in lines.iter().skip(start) {
        let line = strip_reminder_text(raw.trim());
        if let Some(stripped) = line.strip_prefix('•') {
            modes.push(parse_mode_ast(stripped.trim()));
        } else if let Some(stripped) = line.strip_prefix('+') {
            // CR 702.172: Spree mode lines use `+ {cost} — effect` format
            let stripped = stripped.trim();
            if let Some((cost, rest)) = parse_mana_symbols(stripped) {
                // Strip " — " or " – " separator between cost and effect text
                let body = rest
                    .trim()
                    .strip_prefix('—')
                    .or_else(|| rest.trim().strip_prefix('–'))
                    .unwrap_or(rest)
                    .trim();
                modes.push(ModeAst {
                    raw: body.to_string(),
                    label: None,
                    body: body.to_string(),
                    mode_cost: Some(cost),
                });
            } else {
                break; // Cost parse failure → stop collecting modes
            }
        } else {
            break;
        }
    }

    modes
}

fn parse_mode_ast(text: &str) -> ModeAst {
    if let Some((label, body)) = split_short_label_prefix(text, 4) {
        return ModeAst {
            raw: text.to_string(),
            label: Some(label.to_string()),
            body: body.to_string(),
            mode_cost: None,
        };
    }

    ModeAst {
        raw: text.to_string(),
        label: None,
        body: text.to_string(),
        mode_cost: None,
    }
}

pub(super) fn split_short_label_prefix(text: &str, max_words: usize) -> Option<(&str, &str)> {
    for sep in [" — ", " – ", " - "] {
        if let Some(pos) = text.find(sep) {
            let prefix = text[..pos].trim();
            let rest = text[pos + sep.len()..].trim();
            let word_count = prefix.split_whitespace().count();
            if (1..=max_words).contains(&word_count)
                && !prefix.contains('{')
                && !prefix.contains(':')
                && !rest.is_empty()
            {
                return Some((prefix, rest));
            }
        }
    }

    None
}

fn is_modal_header_text(lower: &str) -> bool {
    let lower = lower.trim();
    lower.starts_with("choose ")
        || lower.starts_with("you may choose ")
        || (lower.starts_with("if ") && lower.contains("choose "))
}

pub(crate) fn parse_modal_header_ast(text: &str) -> Option<ModalHeaderAst> {
    let sentences: Vec<&str> = text
        .split('.')
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        .collect();
    let header_text = sentences.first().copied().unwrap_or(text).trim();
    let header_lower = header_text.to_lowercase();
    if !is_modal_header_text(&header_lower) {
        return None;
    }

    let (min_choices, max_choices) = parse_modal_choose_count(&text.to_lowercase());
    let mut allow_repeat_modes = false;
    let mut constraints = Vec::new();

    // CR 700.2: Detect cross-resolution mode restrictions from Oracle text.
    // The constraint phrase is part of the header sentence, not a period-delimited sub-sentence.
    // Order matters — "this turn" is the more specific substring.
    if header_lower.contains("that hasn't been chosen this turn") {
        constraints.push(ModalSelectionConstraint::NoRepeatThisTurn);
    } else if header_lower.contains("that hasn't been chosen") {
        constraints.push(ModalSelectionConstraint::NoRepeatThisGame);
    }

    for sentence in sentences.iter().skip(1) {
        let lower = sentence.to_lowercase();
        if lower == "you may choose the same mode more than once" {
            allow_repeat_modes = true;
            continue;
        }
        if lower == "each mode must target a different player" {
            constraints.push(ModalSelectionConstraint::DifferentTargetPlayers);
        }
    }

    Some(ModalHeaderAst {
        raw: text.to_string(),
        min_choices,
        max_choices,
        allow_repeat_modes,
        constraints,
    })
}

fn split_triggered_modal_header(line: &str) -> Option<(String, String)> {
    for (comma_pos, _) in line.match_indices(", ") {
        let trigger_line = line[..comma_pos].trim();
        let header = line[comma_pos + 2..].trim();
        if is_modal_header_text(&header.to_lowercase()) {
            return Some((trigger_line.to_string(), header.to_string()));
        }
    }

    None
}

pub(crate) fn lower_oracle_block(
    block: OracleBlockAst,
    card_name: &str,
    result: &mut super::oracle::ParsedAbilities,
) {
    use super::oracle_cost::parse_oracle_cost;
    use super::oracle_trigger::parse_trigger_line;

    match block {
        OracleBlockAst::ActivatedModal {
            cost_text,
            header,
            modes,
        } => {
            let def = build_modal_ability(AbilityKind::Activated, &header, &modes)
                .cost(parse_oracle_cost(&cost_text));
            result.abilities.push(def);
        }
        OracleBlockAst::Modal { header, modes } => {
            let modal = build_modal_choice(&header, &modes);
            let mode_abilities = lower_mode_abilities(&modes, AbilityKind::Spell);
            result.abilities.extend(mode_abilities);
            result.modal = Some(modal);
        }
        OracleBlockAst::TriggeredModal {
            trigger_line,
            header,
            modes,
        } => {
            let mut trigger = parse_trigger_line(&trigger_line, card_name);
            trigger.execute = Some(Box::new(build_modal_ability(
                AbilityKind::Spell,
                &header,
                &modes,
            )));
            result.triggers.push(trigger);
        }
    }
}

pub(crate) fn build_modal_ability(
    kind: AbilityKind,
    header: &ModalHeaderAst,
    modes: &[ModeAst],
) -> AbilityDefinition {
    AbilityDefinition::new(kind, modal_marker_effect(header)).with_modal(
        build_modal_choice(header, modes),
        lower_mode_abilities(modes, kind),
    )
}

fn modal_marker_effect(_header: &ModalHeaderAst) -> Effect {
    Effect::GenericEffect {
        static_abilities: vec![],
        duration: None,
        target: None,
    }
}

fn build_modal_choice(header: &ModalHeaderAst, modes: &[ModeAst]) -> ModalChoice {
    ModalChoice {
        min_choices: header.min_choices,
        max_choices: header.max_choices.min(modes.len()),
        mode_count: modes.len(),
        mode_descriptions: modes.iter().map(|mode| mode.raw.clone()).collect(),
        allow_repeat_modes: header.allow_repeat_modes,
        constraints: header.constraints.clone(),
        mode_costs: modes.iter().filter_map(|m| m.mode_cost.clone()).collect(),
    }
}

fn lower_mode_abilities(modes: &[ModeAst], kind: AbilityKind) -> Vec<AbilityDefinition> {
    modes
        .iter()
        .map(|mode| parse_effect_chain(&mode.body, kind))
        .collect()
}

/// Parse the "choose N" count from the modal header line.
///
/// Returns (min_choices, max_choices). Examples:
/// - "choose one —" → (1, 1)
/// - "choose two —" → (2, 2)
/// - "choose one or both —" → (1, 2)
/// - "choose one or more —" → (1, usize::MAX) (capped to mode_count at construction)
/// - "choose any number of —" → (1, usize::MAX)
pub(crate) fn parse_modal_choose_count(lower: &str) -> (usize, usize) {
    let lower = lower.trim();
    let lower = lower.strip_prefix("you may ").unwrap_or(lower).trim_start();

    if lower.contains("choose any number instead") {
        return (1, usize::MAX);
    }
    if lower.contains("choose both instead") {
        return (1, 2);
    }
    if lower.contains("choose two instead") {
        return (1, 2);
    }
    if lower.contains("choose three instead") {
        return (1, 3);
    }
    if lower.contains("one or both") {
        return (1, 2);
    }
    if lower.contains("one or more") || lower.contains("any number") {
        return (1, usize::MAX);
    }
    // Extract the number word after "choose "
    if let Some(rest) = lower.strip_prefix("choose ") {
        if let Some((n, _)) = super::oracle_util::parse_number(rest) {
            return (n as usize, n as usize);
        }
    }
    // Default fallback
    (1, 1)
}

/// Strip an "ability word — " prefix from a line.
/// Ability words are italicized flavor prefixes before an em dash, e.g.:
/// "Landfall — Whenever a land enters..." → "Whenever a land enters..."
/// "Spell mastery — If there are two or more..." → "If there are two or more..."
pub(super) fn strip_ability_word(line: &str) -> Option<String> {
    split_short_label_prefix(line, 4).map(|(_, rest)| rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_modal_choose_count_variants() {
        assert_eq!(parse_modal_choose_count("choose one —"), (1, 1));
        assert_eq!(parse_modal_choose_count("choose two —"), (2, 2));
        assert_eq!(parse_modal_choose_count("you may choose two."), (2, 2));
        assert_eq!(parse_modal_choose_count("choose three —"), (3, 3));
        assert_eq!(parse_modal_choose_count("choose one or both —"), (1, 2));
        assert_eq!(
            parse_modal_choose_count("choose one or more —"),
            (1, usize::MAX)
        );
        assert_eq!(
            parse_modal_choose_count("choose any number of —"),
            (1, usize::MAX)
        );
    }

    #[test]
    fn modal_header_tracks_repeatable_modes() {
        let header = parse_modal_header_ast(
            "Choose up to five {P} worth of modes. You may choose the same mode more than once.",
        )
        .expect("header should parse");
        assert!(header.allow_repeat_modes);
    }

    #[test]
    fn modal_header_detects_no_repeat_this_turn_constraint() {
        let header = parse_modal_header_ast("choose one that hasn't been chosen this turn —")
            .expect("header should parse");
        assert_eq!(
            header.constraints,
            vec![ModalSelectionConstraint::NoRepeatThisTurn]
        );
    }

    #[test]
    fn modal_header_detects_no_repeat_this_game_constraint() {
        let header = parse_modal_header_ast("choose one that hasn't been chosen —")
            .expect("header should parse");
        assert_eq!(
            header.constraints,
            vec![ModalSelectionConstraint::NoRepeatThisGame]
        );
    }

    #[test]
    fn collect_mode_asts_plus_prefix_extracts_cost_and_body() {
        let lines = vec![
            "Spree",
            "+ {2} — Draw a card.",
            "+ {R} — Deal 3 damage to target creature.",
        ];
        let modes = collect_mode_asts(&lines, 1);
        assert_eq!(modes.len(), 2);
        assert!(modes[0].mode_cost.is_some());
        assert_eq!(modes[0].body, "Draw a card.");
        assert!(modes[1].mode_cost.is_some());
    }

    #[test]
    fn collect_mode_asts_standard_bullet_has_no_mode_cost() {
        let lines = vec!["Choose one —", "• Draw a card.", "• Gain 3 life."];
        let modes = collect_mode_asts(&lines, 1);
        assert_eq!(modes.len(), 2);
        assert!(modes[0].mode_cost.is_none());
        assert!(modes[1].mode_cost.is_none());
    }

    #[test]
    fn collect_mode_asts_malformed_plus_line_stops_collection() {
        // A `+` line without valid mana cost should break mode collection
        let lines = vec![
            "Spree",
            "+ Draw a card.", // no mana cost after +
        ];
        let modes = collect_mode_asts(&lines, 1);
        assert!(modes.is_empty());
    }
}
