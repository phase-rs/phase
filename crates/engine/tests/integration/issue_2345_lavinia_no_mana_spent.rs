//! Regression for issue #2345: Lavinia, Azorius Renegade must only counter
//! spells cast without spending mana from the mana pool.
//!
//! https://github.com/phase-rs/phase/issues/2345

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::TriggerCondition;
use engine::types::triggers::TriggerMode;

const LAVINIA_TRIGGER: &str =
    "Whenever an opponent casts a spell, if no mana was spent to cast it, counter that spell.";

#[test]
fn lavinia_spell_cast_trigger_parses_no_mana_spent_intervening_if() {
    let parsed = parse_oracle_text(
        LAVINIA_TRIGGER,
        "Lavinia, Azorius Renegade",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Soldier".to_string()],
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("Lavinia must have a spell-cast trigger");
    assert_eq!(trigger.mode, TriggerMode::SpellCast);
    assert!(matches!(
        trigger.condition.as_ref(),
        Some(TriggerCondition::ManaSpentCondition { text })
            if text.contains("no mana was spent")
    ));
}
