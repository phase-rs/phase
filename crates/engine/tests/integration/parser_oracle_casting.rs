//! Tests relocated from `src/parser/oracle_casting.rs`'s test module.

use engine::types::ability::Effect;

/// CR 601.3: Even the Odds (verbatim). Its "Cast this spell only if …" condition is
/// not representable as a restriction, so the line must be an honest coverage gap —
/// never a conditional self-recast (`CastFromZone`) that drops the restriction —
/// while the spell's own instruction still parses on its own.
#[test]
fn unrepresentable_cast_only_condition_is_a_gap_not_a_self_recast() {
    let text = "Cast this spell only if you control fewer creatures than each opponent.\nCreate three 1/1 white Soldier creature tokens.";
    let parsed = engine::parser::oracle::parse_oracle_text(
        text,
        "Even the Odds",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let mut effects = Vec::new();
    for ability in &parsed.abilities {
        let _ = engine::types::ability_visit::visit_ability_def(ability, &mut |effect| {
            effects.push(effect.clone());
            std::ops::ControlFlow::Continue(())
        });
    }
    // Reach-guard: the resolution instruction is still parsed.
    assert!(
        effects.iter().any(|e| matches!(e, Effect::Token { .. })),
        "{effects:?}"
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::CastFromZone { .. })),
        "{effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::Unimplemented { name, .. } if name == "casting_restriction"
        )),
        "{effects:?}"
    );
    assert!(parsed.casting_restrictions.is_empty());
    let face = engine::types::card::CardFace {
        name: "Even the Odds".to_string(),
        oracle_text: Some(text.to_string()),
        abilities: parsed.abilities,
        ..Default::default()
    };
    assert!(!engine::game::coverage::card_face_gaps(&face).is_empty());
}
