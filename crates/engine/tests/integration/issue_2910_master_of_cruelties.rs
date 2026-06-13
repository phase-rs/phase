//! Regression for issue #2910: Master of Cruelties attack-alone restriction.
//!
//! https://github.com/phase-rs/phase/issues/2910
//!
//! The "can only attack alone" line must parse as a static CombatAlone restriction,
//! not fall through to Unimplemented with empty static_abilities.

use engine::parser::oracle::parse_oracle_text;
use engine::types::statics::{CombatAloneAction, CombatAloneRequirement, StaticMode};

const MASTER_OF_CRUELTIES_ORACLE: &str = "\
First strike, deathtouch\n\
This creature can only attack alone.\n\
Whenever this creature attacks a player and isn't blocked, that player's life total becomes 1. This creature assigns no combat damage this combat.";

#[test]
fn master_of_cruelties_parses_attack_alone_static() {
    let parsed = parse_oracle_text(
        MASTER_OF_CRUELTIES_ORACLE,
        "Master of Cruelties",
        &["First strike".to_string(), "Deathtouch".to_string()],
        &["Creature".to_string()],
        &["Demon".to_string()],
    );
    assert!(
        parsed.statics.iter().any(|sd| {
            matches!(
                sd.mode,
                StaticMode::CombatAlone {
                    action: CombatAloneAction::Attack,
                    requirement: CombatAloneRequirement::MustBeSole,
                }
            )
        }),
        "expected CombatAlone(Attack, MustBeSole) static, got {:?}",
        parsed.statics
    );
}

#[test]
fn master_of_cruelties_has_no_unimplemented_attack_alone_leak() {
    let parsed = parse_oracle_text(
        "This creature can only attack alone.",
        "Master of Cruelties",
        &[],
        &["Creature".to_string()],
        &["Demon".to_string()],
    );
    assert!(
        !parsed.statics.is_empty(),
        "attack-alone line must produce static abilities"
    );
    assert!(
        parsed
            .statics
            .iter()
            .all(|sd| { !matches!(&sd.mode, StaticMode::Other(name) if name == "Unimplemented") }),
        "static line must not leak Unimplemented: {:?}",
        parsed.statics
    );
}
