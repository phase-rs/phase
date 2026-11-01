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
//! sentence is never refused as plural — the gain-control clause in front of it
//! is modelled (`GainControlAll` with a `DeclaredTarget` attachment referent),
//! so the singular fixture is gap-free and the plural gap cannot come from the
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
    // Reach-guard: the chain still reaches the MODELLED gain-control clause
    // (not a collapsed chain), so the plural gap below cannot come from the
    // chain having been dropped.
    fn chain_has_gain_control_all(def: &engine::types::ability::AbilityDefinition) -> bool {
        matches!(
            &*def.effect,
            engine::types::ability::Effect::GainControlAll { .. }
        ) || def
            .sub_ability
            .as_deref()
            .is_some_and(chain_has_gain_control_all)
    }
    assert!(
        face.abilities.iter().any(chain_has_gain_control_all),
        "reach-guard: the gain-control clause must lower to GainControlAll, got {:?}",
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
    assert!(
        !gaps.iter().any(|gap| gap.contains("attached_to_qualifier")),
        "the modelled gain-control clause must not report the old gap, got {gaps:?}"
    );
}

/// Paired control: the SINGULAR form of the same sentence is fully modelled —
/// `GainControlAll` (with a `DeclaredTarget` attachment referent) and a singular
/// `Attach` — so it is gap-free and never reports the plural anaphor gap.
/// Without this row the plural gap above could come from the sentence's other
/// clauses rather than from plurality being refused.
#[test]
fn singular_attachment_anaphor_is_not_refused_as_plural() {
    let face = spell_face("Fumble (singular fixture)", FUMBLE_SINGULAR);
    // Reach-guard: BOTH modelled clauses parse, so the plural-refusal negative
    // below reads clauses that reached the parser.
    fn chain_has(def: &engine::types::ability::AbilityDefinition) -> (bool, bool) {
        let (mut attach, mut gain) = match &*def.effect {
            engine::types::ability::Effect::Attach { .. } => (true, false),
            engine::types::ability::Effect::GainControlAll { .. } => (false, true),
            _ => (false, false),
        };
        if let Some(sub) = def.sub_ability.as_deref() {
            let (sub_attach, sub_gain) = chain_has(sub);
            attach |= sub_attach;
            gain |= sub_gain;
        }
        (attach, gain)
    }
    let (attach, gain) = face
        .abilities
        .iter()
        .map(chain_has)
        .fold((false, false), |(a, g), (sa, sg)| (a || sa, g || sg));
    assert!(
        attach && gain,
        "reach-guard: the singular fixture lowers Attach + GainControlAll, got {:?}",
        face.abilities
            .iter()
            .map(|d| format!("{:?}", d.effect))
            .collect::<Vec<_>>()
    );
    let gaps = card_face_gaps(&face);
    assert!(
        gaps.is_empty(),
        "the singular fixture is fully modelled, got {gaps:?}"
    );
    assert!(
        !gaps
            .iter()
            .any(|gap| gap.contains("plural_attachment_anaphor")),
        "the singular attachment anaphor must not be refused as plural, got {gaps:?}"
    );
}
