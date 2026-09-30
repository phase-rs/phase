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
//! (`card_face_gaps`), with a paired control that the SINGULAR form of the same
//! sentence is never refused as plural (it reports only the gain-control
//! clause's `attached_to_qualifier` gap) — so the plural gap cannot come from
//! the sentence's other clauses.

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
    // Reach-guard: the chain still reaches the gain-control clause (an honest
    // `attached_to_qualifier` gap, not a collapsed chain), so the gap below
    // cannot come from the chain having been dropped.
    fn chain_has_attached_to_gap(def: &engine::types::ability::AbilityDefinition) -> bool {
        matches!(
            &*def.effect,
            engine::types::ability::Effect::Unimplemented { name, .. }
                if name == "attached_to_qualifier"
        ) || def
            .sub_ability
            .as_deref()
            .is_some_and(chain_has_attached_to_gap)
    }
    assert!(
        face.abilities.iter().any(chain_has_attached_to_gap),
        "reach-guard: the gain-control clause must carry the attached_to_qualifier gap, got {:?}",
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

/// Paired control: the SINGULAR form of the same sentence reports only the
/// gain-control clause's `attached_to_qualifier` gap and never the plural
/// anaphor gap. Without this row the plural gap above could come from the
/// sentence's other clauses rather than from plurality being refused.
#[test]
fn singular_attachment_anaphor_is_not_refused_as_plural() {
    let face = spell_face("Fumble (singular fixture)", FUMBLE_SINGULAR);
    // Reach-guard: the singular attach clause parses as its own Attach, so the
    // plural-refusal negative below reads a clause that reached the parser.
    fn chain_has_attach(def: &engine::types::ability::AbilityDefinition) -> bool {
        matches!(&*def.effect, engine::types::ability::Effect::Attach { .. })
            || def.sub_ability.as_deref().is_some_and(chain_has_attach)
    }
    assert!(
        face.abilities.iter().any(chain_has_attach),
        "reach-guard: the singular attach clause must parse to an Attach, got {:?}",
        face.abilities
            .iter()
            .map(|d| format!("{:?}", d.effect))
            .collect::<Vec<_>>()
    );
    let gaps = card_face_gaps(&face);
    assert!(
        !gaps.is_empty() && gaps.iter().any(|gap| gap.contains("attached_to_qualifier")),
        "reach-guard: the gain-control clause is an unsupported attachment qualifier, got {gaps:?}"
    );
    assert!(
        !gaps
            .iter()
            .any(|gap| gap.contains("plural_attachment_anaphor")),
        "the singular attachment anaphor must not be refused as plural, got {gaps:?}"
    );
}
