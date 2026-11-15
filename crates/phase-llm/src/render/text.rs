//! Small display formatters shared by the game and draft renderers.

use engine::types::card_type::CardType;
use engine::types::mana::ManaCost;

/// Printed-style mana cost, e.g. `{2}{U}{U}`. `{0}` for a zero cost, and an
/// empty string for a cost that is not a concrete symbol run (a land, or an
/// unresolved self-referential alternative cost) so callers can omit the field.
pub fn mana_cost_text(cost: &ManaCost) -> String {
    match cost {
        ManaCost::Cost { shards, generic } => {
            if shards.is_empty() && *generic == 0 {
                return "{0}".to_string();
            }
            let mut out = String::new();
            if *generic > 0 {
                out.push('{');
                out.push_str(&generic.to_string());
                out.push('}');
            }
            for shard in shards {
                out.push('{');
                out.push_str(shard.symbol());
                out.push('}');
            }
            out
        }
        // CR 202: these are pre-resolution placeholders, not printed costs.
        ManaCost::NoCost
        | ManaCost::SelfManaCost
        | ManaCost::SelfManaValue
        | ManaCost::SelfManaCostReduced { .. } => String::new(),
    }
}

/// CR 205.1: the type line, `Legendary Creature — Human Wizard`.
pub fn type_line_text(card_type: &CardType) -> String {
    let mut line = String::new();
    for supertype in &card_type.supertypes {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&format!("{supertype:?}"));
    }
    for core in &card_type.core_types {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&format!("{core:?}"));
    }
    if !card_type.subtypes.is_empty() {
        // CR 205.3b: subtypes are listed after a long dash.
        line.push_str(" — ");
        line.push_str(&card_type.subtypes.join(" "));
    }
    line
}

/// Collapse a card's Oracle text onto one line so a card entry stays a single
/// row in the rendered board. Reminder text is left intact: it is short and it
/// tells a model what an unfamiliar keyword does.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to `max` characters on a word boundary, appending an ellipsis when
/// anything was dropped. Keeps a long Oracle text from crowding out the board.
pub fn clamp_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let clipped: String = text.chars().take(max).collect();
    let cut = clipped.rfind(' ').unwrap_or(clipped.len());
    format!("{}…", &clipped[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::card_type::{CoreType, Supertype};
    use engine::types::mana::ManaCostShard;

    #[test]
    fn mana_costs_print_generic_first() {
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 2,
        };
        assert_eq!(mana_cost_text(&cost), "{2}{U}{U}");
    }

    #[test]
    fn a_zero_cost_prints_as_a_symbol_and_a_placeholder_prints_as_nothing() {
        assert_eq!(mana_cost_text(&ManaCost::zero()), "{0}");
        assert_eq!(mana_cost_text(&ManaCost::SelfManaCost), "");
    }

    #[test]
    fn type_lines_use_the_em_dash_only_when_there_are_subtypes() {
        let with_subtypes = CardType {
            supertypes: vec![Supertype::Legendary],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Human".to_string(), "Wizard".to_string()],
        };
        assert_eq!(
            type_line_text(&with_subtypes),
            "Legendary Creature — Human Wizard"
        );
        let without = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Instant],
            subtypes: Vec::new(),
        };
        assert_eq!(type_line_text(&without), "Instant");
    }

    #[test]
    fn clamping_breaks_on_a_word_boundary() {
        assert_eq!(clamp_text("draw a card then discard", 10), "draw a…");
        assert_eq!(clamp_text("short", 10), "short");
    }

    #[test]
    fn one_line_collapses_oracle_newlines() {
        assert_eq!(one_line("Flying\nVigilance"), "Flying Vigilance");
    }
}
