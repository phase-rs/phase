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
use engine::types::ability::{
    AbilityDefinition, ContinuousModification, ControllerRef, Effect, FilterProp, TargetFilter,
};

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
/// it, whether or not that ability carried a definition to stamp.
///
/// `lower_oracle_ir` resolves every "…except it has this ability" clause while
/// bucketing items into the per-category vectors: the slot IS
/// `result.abilities.len()` at the moment of the push, so an IR-native spell
/// consumes its slot by being pushed, exactly like a pre-lowered one. (Plan 05b
/// T9b moved this off `OracleDocBuilder::finish()`, which ran before lowering
/// and so could only stamp the pre-lowered shapes.)
///
/// DISCRIMINATING: drop the stamp and this reads `0` — the copy grafts the FIRST
/// printed ability, the prevention spell, instead of itself. Verified red.
///
/// Line 1 reaches the instant/sorcery prevention recognizer, an IR-native
/// (`OracleNodeIr::Spell`) producer. Line 2 is a synthetic activated ability;
/// `parse_activated_ability_definition` is the only writer of
/// `ParseContext::current_ability_index` and therefore the only source of
/// `RetainPrintedAbilityFromSource`, and it is deliberately still pre-lowered.
/// No printing pairs the two today — which is why this defect had no corpus
/// witness and why the full-pool byte gate is silent on it.
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

// ---------------------------------------------------------------------------
// CR 607.2d document relations and the `item_ability` reader.
//
// `item_ability` is the ability side of cross-item relation discovery. It used
// to return `Option<&AbilityDefinition>` and recognize exactly one spell node
// shape; it now returns `Option<Cow<'_, AbilityDefinition>>` and recognizes
// both, lowering the IR-native shape on demand because that shape owns no
// definition to borrow.
//
// A relation that stops being discovered fails SILENTLY — no error, no panic,
// just a card that parses to its unrelated line-local shape, with the symptom
// surfacing on a different card from the one that was edited. The borrowed path
// therefore needs an explicit non-regression witness across the signature
// change.
//
// SCOPE, stated honestly: this witnesses the BORROWED arm. The IR-native arm is
// exercised by nine producers as of Plan 05b T9b, but by no *relation predicate*
// — the relations these tests drive pair ability items whose text reaches the
// still-pre-lowered fallbacks, and no relation predicate matches a prevention
// chain or a keyword-activated body. So the IR-native arm remains covered by
// construction (`item_ability` lowers through the same `lower_ability_ir` the
// `Spell` bucketing arm calls) rather than by a relation witness.
// ---------------------------------------------------------------------------

/// Verified against Scryfall 2026-07-27 (`cards/named?exact=Siren's Call`).
const SIRENS_CALL: &str = "Cast this spell only during an opponent's turn, before attackers are declared.\nCreatures the active player controls attack this turn if able.\nAt the beginning of the next end step, destroy all non-Wall creatures that player controls that didn't attack this turn. Ignore this effect for each creature the player didn't control continuously since the beginning of the turn.";

/// The delayed punisher's destroyed set, plus whether its exemption sibling is
/// still hanging off the delayed ability.
fn punisher_destroy_target(parsed: &ParsedAbilities) -> (TargetFilter, bool) {
    for ability in &parsed.abilities {
        if let Effect::CreateDelayedTrigger { effect, .. } = ability.effect.as_ref() {
            if let Effect::DestroyAll { target, .. } = effect.effect.as_ref() {
                return (target.clone(), effect.sub_ability.is_some());
            }
        }
    }
    panic!("expected a delayed DestroyAll punisher ability, got {parsed:?}");
}

fn typed_controllers(filter: &TargetFilter, out: &mut Vec<Option<ControllerRef>>) {
    match filter {
        TargetFilter::Typed(tf) => out.push(tf.controller.clone()),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().for_each(|f| typed_controllers(f, out))
        }
        TargetFilter::Not { filter } => typed_controllers(filter, out),
        _ => {}
    }
}

fn typed_props(filter: &TargetFilter, out: &mut Vec<FilterProp>) {
    match filter {
        TargetFilter::Typed(tf) => out.extend(tf.properties.iter().cloned()),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().for_each(|f| typed_props(f, out))
        }
        TargetFilter::Not { filter } => typed_props(filter, out),
        _ => {}
    }
}

/// CR 102.1 + CR 603.7c + CR 608.2c: the `ActivePlayerPunisher` relation pairs
/// the mass-attack coerce clause with its sibling delayed punisher, then
/// rebinds the punisher's "that player controls" anaphor from the line-local
/// `You` default to `ActivePlayer`.
///
/// Siren's Call is the witness because BOTH sides of the relation are ability
/// items — two printed abilities on one instant — so a single card drives
/// `item_ability` twice, once per predicate.
///
/// DISCRIMINATING: the rebind is reachable ONLY through the relation, and the
/// relation is discovered only if `item_ability` returns a definition for both
/// participating items. Blind the reader on either side and this sees the
/// line-local `You` — the wrong player, which destroys the caster's own
/// creatures.
#[test]
fn active_player_punisher_relation_survives_the_item_ability_reader() {
    let parsed = parse_oracle_text(
        SIRENS_CALL,
        "Siren's Call",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let (target, exemption_sibling_present) = punisher_destroy_target(&parsed);

    let mut controllers = Vec::new();
    typed_controllers(&target, &mut controllers);
    assert!(
        !controllers.is_empty(),
        "reach-guard: the destroyed set must carry at least one typed node to rebind, got {target:?}"
    );
    assert!(
        controllers
            .iter()
            .all(|c| *c == Some(ControllerRef::ActivePlayer)),
        "the punisher's destroyed set must be rebound to ActivePlayer by the CR 607.2d relation; \
         a `You` here means the relation was not discovered, got {controllers:?}"
    );

    // CR 302.6 + CR 508.1a: the same relation folds the continuous-control
    // exemption into the destroyed set and CONSUMES the redundant sibling. Both
    // halves are asserted so a partial application cannot pass.
    let mut props = Vec::new();
    typed_props(&target, &mut props);
    assert!(
        props.contains(&FilterProp::ControlledContinuouslySinceTurnBegan),
        "the exemption must be folded into the destroyed set as a filter prop, got {props:?}"
    );
    assert!(
        !exemption_sibling_present,
        "the redundant exemption sibling must be consumed once folded"
    );
}

/// Reach-guard for the assertion above: `ActivePlayer` must not be something
/// the punisher line parses to on its own. With no sibling coerce clause there
/// is no relation to discover, so the anaphor keeps its line-local `You`.
///
/// Without this, the test above would still pass against an `item_ability` that
/// returned `Some` for every item, or against a parser that hardcoded
/// `ActivePlayer` into the punisher line.
#[test]
fn the_punisher_line_alone_keeps_its_line_local_controller() {
    let parsed = parse_oracle_text(
        "At the beginning of the next end step, destroy all non-Wall creatures that player controls that didn't attack this turn.",
        "Probe",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let (target, _) = punisher_destroy_target(&parsed);
    let mut controllers = Vec::new();
    typed_controllers(&target, &mut controllers);
    assert!(
        !controllers.contains(&Some(ControllerRef::ActivePlayer)),
        "with no coerce sibling there is no relation, so the anaphor must NOT be \
         rebound to ActivePlayer, got {controllers:?}"
    );
}
