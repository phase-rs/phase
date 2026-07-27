//! Readers of an IR-native spell item (`OracleNodeIr::Spell`) — Plan 05b T7.
//!
//! That node shape has a live producer today (the instant/sorcery
//! prevent-damage recognizer in `parser/oracle.rs`), but several of its readers
//! were written when it had none, and each assumed the pre-lowered shape. The
//! defects they carried are silent by construction: a mis-stamped printed slot
//! and a missed document relation both produce a card that parses successfully
//! into the wrong thing, so full-pool byte-identity cannot see them.
//!
//! These tests drive the real `parse_oracle_text` pipeline and assert on the
//! parsed AST, which is the layer the defects live in.

use engine::parser::oracle::ParsedAbilities;
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, ContinuousModification, Effect};

/// Every CR 707.9a printed-ability slot a card's copy-except clauses resolve to.
fn retained_ability_slots(parsed: &ParsedAbilities) -> Vec<usize> {
    fn from_mods(mods: &[ContinuousModification], out: &mut Vec<usize>) {
        for m in mods {
            if let ContinuousModification::RetainPrintedAbilityFromSource {
                source_ability_index,
            } = m
            {
                out.push(*source_ability_index);
            }
        }
    }
    fn walk(def: &AbilityDefinition, out: &mut Vec<usize>) {
        match def.effect.as_ref() {
            Effect::CopySpell {
                additional_modifications,
                ..
            }
            | Effect::CopyTokenOf {
                additional_modifications,
                ..
            }
            | Effect::BecomeCopy {
                additional_modifications,
                ..
            } => from_mods(additional_modifications, out),
            Effect::AddPendingEntersModifications { modifications } => {
                from_mods(modifications, out)
            }
            _ => {}
        }
        if let Some(sub) = def.sub_ability.as_deref() {
            walk(sub, out);
        }
    }
    let mut out = Vec::new();
    for ability in &parsed.abilities {
        walk(ability, &mut out);
    }
    out
}

/// CR 707.9a: a printed slot is consumed by the printed ability that occupies
/// it, whether or not that ability has a definition yet to stamp.
///
/// `OracleDocBuilder::finish()` resolves every "…except it has this ability"
/// clause by walking items in source order and counting each category
/// separately. An IR-native spell node holds only an effect chain, so there is
/// nothing to stamp into — but it still occupies an ability slot, because
/// `lower_oracle_ir` pushes it into `result.abilities` exactly like a
/// pre-lowered one does.
///
/// DISCRIMINATING: with the `Spell` arm parked in `finish()`'s no-op list this
/// reads `0`, and the copy grafts the FIRST printed ability — the prevention
/// spell — instead of itself.
///
/// Line 1 is the shape the only live `OracleNodeIr::Spell` producer emits: an
/// instant/sorcery prevention line. Line 2 is a synthetic activated ability,
/// which is the sole source of `RetainPrintedAbilityFromSource`. No printing
/// pairs the two today — which is exactly why this defect had no corpus witness
/// and why the full-pool byte gate is silent on it.
#[test]
fn an_ir_native_spell_consumes_the_printed_ability_slot_it_occupies() {
    let parsed = parse_oracle_text(
        "Prevent all damage that would be dealt to you this turn.\n{2}: Create a token that's a copy of target creature, except it has this ability.",
        "Probe",
        &[],
        &["Instant".to_string()],
        &[],
    );

    // Reach-guard: both printed abilities must be present, or the slot
    // assertion below is vacuous.
    assert_eq!(
        parsed.abilities.len(),
        2,
        "expected the prevention spell and the copy ability, got {:?}",
        parsed.abilities
    );
    assert!(
        matches!(
            parsed.abilities[0].effect.as_ref(),
            Effect::PreventDamage { .. }
        ),
        "the first printed ability must be the IR-native prevention spell, got {:?}",
        parsed.abilities[0].effect
    );

    assert_eq!(
        retained_ability_slots(&parsed),
        vec![1],
        "the copy-except clause must resolve to printed slot 1 — the IR-native \
         prevention spell consumed slot 0 despite carrying no definition to stamp"
    );
}
