//! CR 608.2c (rules of English — number agreement) + CR 400.7: an Attach
//! instruction whose ATTACHMENT operand is a plural anaphor ("attach them …")
//! names a SET, and this engine has no set-valued attachment operand: the
//! antecedent producers (`GainControlAll`, conjure) publish no typed
//! provenance, so the singular `ParentTarget` fallback would bind the wrong
//! object (Fumble: the bounced creature, which CR 400.7 makes a new object).
//!
//! The clause must therefore report the card as UNSUPPORTED rather than
//! silently claiming support while its printed instruction does nothing. This
//! file pins the honesty through the PUBLIC coverage authority
//! (`card_face_gaps`), with a paired positive control that the SINGULAR form of
//! the same sentence stays supported — so the gap cannot come from the
//! sentence's other clauses.

use engine::game::coverage::card_face_gaps;
use engine::parser::parse_oracle_text;
use engine::types::card::CardFace;

const FUMBLE: &str = "Return target creature to its owner's hand. Gain control of all Auras and Equipment that were attached to it, then attach them to another creature.";

/// The same sentence with a SINGULAR attachment anaphor ("attach it …") — the
/// form the engine models (the attachment is the source/prior referent, chosen
/// through the ordinary cascade).
const FUMBLE_SINGULAR: &str = "Return target creature to its owner's hand. Gain control of all Auras and Equipment that were attached to it, then attach it to another creature.";

fn spell_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Sorcery".to_string()], &[]);
    CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    }
}

/// The honesty regression: Fumble carries a coverage gap naming the refused
/// plural-anaphor clause.
#[test]
fn plural_anaphor_attachment_reports_a_coverage_gap() {
    let face = spell_face("Fumble", FUMBLE);
    // Reach-guard: the two modelled clauses in front of the attach clause are
    // still on the face, so the gap below cannot come from a collapsed chain.
    fn chain_has_gain_control(def: &engine::types::ability::AbilityDefinition) -> bool {
        matches!(
            &*def.effect,
            engine::types::ability::Effect::GainControlAll { .. }
        ) || def
            .sub_ability
            .as_deref()
            .is_some_and(chain_has_gain_control)
    }
    assert!(
        face.abilities.iter().any(chain_has_gain_control),
        "reach-guard: the gain-control clause must still parse, got {:?}",
        face.abilities
            .iter()
            .map(|d| format!("{:?}", d.effect))
            .collect::<Vec<_>>()
    );

    let gaps = card_face_gaps(&face);
    assert!(
        !gaps.is_empty(),
        "the unresolved plural-anaphor attachment must surface a coverage gap"
    );
    assert!(
        gaps.iter()
            .any(|gap| gap.contains("plural_attachment_anaphor")),
        "the gap must name the pattern class, got {gaps:?}"
    );
}

/// Paired positive control: the SINGULAR form of the same sentence still parses
/// to a fully supported card. Without this row the gap above could pass because
/// the sentence's other clauses regressed, not because plurality is refused.
#[test]
fn singular_attachment_anaphor_stays_supported() {
    let face = spell_face("Fumble (singular fixture)", FUMBLE_SINGULAR);
    let gaps = card_face_gaps(&face);
    assert!(
        gaps.is_empty(),
        "the singular attachment anaphor is modelled and must stay supported, got {gaps:?}"
    );
}
