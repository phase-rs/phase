//! Parser-only ownership of block-history clauses. Nothing in this module is serialized.
//!
//! Expected keys are read while the clause's binding context exists. A locator is
//! only an address: the audit must still inspect that instruction's own operands.

use super::ast::ParsedEffectClause;
use super::context::ParseContext;
use super::effect_chain::ClauseId;
use crate::parser::oracle_nom::primitives::scan_preceded;
use crate::parser::oracle_target::{
    history_clause_subject, parse_block_history_clause, BlockHistoryClause, BlockHistoryKey,
    HistoryPartner,
};
use crate::types::ability::{AbilityDefinition, Effect};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstructionStep {
    SubAbility,
    ElseAbility,
    DelayedPayload,
    Mode(usize),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InstructionPath(pub(crate) Vec<InstructionStep>);

impl InstructionPath {
    pub(crate) fn resolve<'a>(
        &self,
        mut def: &'a AbilityDefinition,
    ) -> Option<&'a AbilityDefinition> {
        for step in &self.0 {
            def = match step {
                InstructionStep::SubAbility => def.sub_ability.as_deref()?,
                InstructionStep::ElseAbility => def.else_ability.as_deref()?,
                InstructionStep::DelayedPayload => match def.effect.as_ref() {
                    Effect::CreateDelayedTrigger { effect, .. } => effect,
                    _ => return None,
                },
                InstructionStep::Mode(index) => def.mode_abilities.get(*index)?,
            };
        }
        Some(def)
    }

    pub(crate) fn prepend(&mut self, step: InstructionStep) {
        self.0.insert(0, step);
    }
}

/// A distinct occurrence within the instruction's own printed fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryOccurrence {
    pub(crate) clause: BlockHistoryClause,
    pub(crate) fragment: String,
    /// Exact owner-local offsets; the envelope below names its committed clause.
    pub(crate) owner_text: String,
    pub(crate) source: Option<super::doc::OracleUnitSource>,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingHistory {
    pub(crate) occurrence: HistoryOccurrence,
    pub(crate) expected: Option<BlockHistoryKey>,
    pub(crate) instruction: InstructionPath,
    pub(crate) lineage: Vec<ClauseId>,
    pub(crate) locator: Option<InstructionPath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstructionIdentity {
    pub(crate) clause: ClauseId,
    pub(crate) lineage: Vec<ClauseId>,
    pub(crate) instruction: InstructionPath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryProvenance {
    pub(crate) occurrence: HistoryOccurrence,
    pub(crate) expected: Option<BlockHistoryKey>,
    pub(crate) producer: InstructionIdentity,
    pub(crate) locator: Option<InstructionPath>,
}

#[derive(Debug, Clone)]
pub(crate) struct Lowered<T> {
    pub(crate) value: T,
    pub(crate) history: Vec<HistoryProvenance>,
}

/// Capture only the head instruction. Compound parsers transport their followers'
/// records separately; a parent never rebinds a follower's pronoun.
pub(crate) fn capture_own_history(clause: &mut ParsedEffectClause, text: &str, ctx: &ParseContext) {
    let lower = clause
        .head_text
        .as_deref()
        .unwrap_or(text)
        .to_ascii_lowercase();
    clause
        .history
        .retain(|record| !record.instruction.0.is_empty());
    let mut own = Vec::new();
    let mut remaining = lower.as_str();
    while let Some((before, parsed, rest)) = scan_preceded(remaining, parse_block_history_clause) {
        let start = lower.len() - remaining.len() + before.len();
        let end = lower.len() - rest.len();
        let expected = match parsed.partner {
            HistoryPartner::Subject(word) => {
                history_clause_subject(word, ctx).map(|subject| BlockHistoryKey {
                    direction: parsed.direction,
                    scope: parsed.scope,
                    subject,
                })
            }
            HistoryPartner::Filter | HistoryPartner::Unbound => None,
        };
        own.push(PendingHistory {
            occurrence: HistoryOccurrence {
                clause: parsed,
                fragment: lower[start..end].to_string(),
                owner_text: lower.clone(),
                source: None,
                start,
                end,
            },
            expected,
            instruction: InstructionPath::default(),
            lineage: Vec::new(),
            locator: Some(InstructionPath::default()),
        });
        remaining = rest;
    }
    own.append(&mut clause.history);
    clause.history = own;
}

/// Assembly-local allocation addresses transport identity through moves, wrapping,
/// and spine linking. They are never dereferenced or published. At the boundary
/// they become typed paths; a node that was removed has no locator. No semantic
/// tree search can substitute a different instruction for a missing producer.
#[derive(Default)]
pub(crate) struct HistoryTransport {
    records: Vec<(HistoryProvenance, Option<usize>)>,
}

fn address(def: &AbilityDefinition) -> usize {
    std::ptr::from_ref(def.effect.as_ref()) as usize
}

fn walk(
    def: &AbilityDefinition,
    path: &mut InstructionPath,
    out: &mut Vec<(usize, InstructionPath)>,
) {
    out.push((address(def), path.clone()));
    let mut child = |step, def: &AbilityDefinition| {
        path.0.push(step);
        walk(def, path, out);
        path.0.pop();
    };
    if let Some(sub) = def.sub_ability.as_deref() {
        child(InstructionStep::SubAbility, sub);
    }
    if let Some(otherwise) = def.else_ability.as_deref() {
        child(InstructionStep::ElseAbility, otherwise);
    }
    if let Effect::CreateDelayedTrigger { effect, .. } = def.effect.as_ref() {
        child(InstructionStep::DelayedPayload, effect);
    }
    for (index, mode) in def.mode_abilities.iter().enumerate() {
        child(InstructionStep::Mode(index), mode);
    }
}

impl HistoryTransport {
    pub(crate) fn capture(&mut self, record: HistoryProvenance, def: Option<&AbilityDefinition>) {
        let handle = def.map(address);
        if let Some((existing, position)) = self.records.iter_mut().find(|(existing, _)| {
            existing.producer == record.producer && existing.occurrence == record.occurrence
        }) {
            *existing = record;
            *position = handle;
        } else {
            self.records.push((record, handle));
        }
    }

    pub(crate) fn from_lowered(value: &AbilityDefinition, history: Vec<HistoryProvenance>) -> Self {
        let mut transport = Self::default();
        for record in history {
            let def = record.locator.as_ref().and_then(|path| path.resolve(value));
            let handle = def.map(address);
            transport.records.push((record, handle));
        }
        transport
    }

    pub(crate) fn finish(self, value: AbilityDefinition) -> Lowered<AbilityDefinition> {
        let history = self.finish_history(Some(&value));
        Lowered { value, history }
    }

    pub(crate) fn finish_history(
        self,
        value: Option<&AbilityDefinition>,
    ) -> Vec<HistoryProvenance> {
        if self.records.is_empty() {
            return Vec::new();
        }
        let mut positions = Vec::new();
        if let Some(value) = value {
            walk(value, &mut InstructionPath::default(), &mut positions);
        }
        self.records
            .into_iter()
            .map(|(mut record, handle)| {
                record.locator = handle.and_then(|handle| {
                    positions
                        .iter()
                        .find_map(|(candidate, path)| (*candidate == handle).then(|| path.clone()))
                });
                record
            })
            .collect()
    }
}

/// A document item's root is resolved through the category's item-id track,
/// rather than keeping an index that a document relation can invalidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryRoot {
    Ability,
    TriggerExecute,
}

#[derive(Debug, Clone)]
pub(crate) struct DocumentHistory {
    pub(crate) item: super::doc::OracleItemId,
    pub(crate) root: HistoryRoot,
    pub(crate) record: HistoryProvenance,
}

/// Provenance and the explicitly located typed instruction. Absence is an audit
/// failure; it never authorizes a search elsewhere in the unit.
pub(crate) struct HistoryAudit<'a> {
    pub(crate) record: &'a HistoryProvenance,
    pub(crate) instruction: Option<&'a AbilityDefinition>,
}

#[cfg(test)]
pub(crate) fn audit_history<'a>(
    history: &'a [HistoryProvenance],
    definition: &'a AbilityDefinition,
) -> Vec<HistoryAudit<'a>> {
    history
        .iter()
        .map(|record| HistoryAudit {
            record,
            instruction: record
                .locator
                .as_ref()
                .and_then(|path| path.resolve(definition)),
        })
        .collect()
}

impl PendingHistory {
    pub(crate) fn nested(record: HistoryProvenance, step: InstructionStep) -> Self {
        let mut instruction = record.producer.instruction;
        instruction.prepend(step.clone());
        let mut locator = record.locator;
        if let Some(path) = &mut locator {
            path.prepend(step);
        }
        let mut lineage = record.producer.lineage;
        lineage.insert(0, record.producer.clause);
        Self {
            occurrence: record.occurrence,
            expected: record.expected,
            instruction,
            lineage,
            locator,
        }
    }

    pub(crate) fn prepend(&mut self, step: InstructionStep) {
        self.instruction.prepend(step.clone());
        if let Some(path) = &mut self.locator {
            path.prepend(step);
        }
    }
}

impl PendingHistory {
    pub(crate) fn committed(&self, clause: ClauseId) -> HistoryProvenance {
        HistoryProvenance {
            occurrence: self.occurrence.clone(),
            expected: self.expected,
            producer: InstructionIdentity {
                clause,
                lineage: self.lineage.clone(),
                instruction: self.instruction.clone(),
            },
            locator: None,
        }
    }
}

impl HistoryProvenance {
    pub(crate) fn prepend(&mut self, step: InstructionStep) {
        self.producer.instruction.prepend(step.clone());
        if let Some(path) = &mut self.locator {
            path.prepend(step);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_effect::{
        lower_ability_ir_with_provenance, parse_ability_ir_standalone,
        parse_ability_ir_with_context,
    };
    use crate::parser::swallow_check::detect_combat_history_relation;
    use crate::types::ability::{
        AbilityKind, BlockPairDirection, CombatHistoryScope, CombatRelation, CombatRelationSubject,
        FilterProp, TargetFilter, TypedFilter,
    };

    fn source_context() -> ParseContext {
        ParseContext {
            subject: Some(TargetFilter::SelfRef),
            ..Default::default()
        }
    }

    fn lower(text: &str, mut ctx: ParseContext) -> Lowered<AbilityDefinition> {
        lower_ability_ir_with_provenance(&parse_ability_ir_with_context(
            text,
            AbilityKind::Spell,
            &mut ctx,
        ))
    }

    fn flagged(text: &str, lowered: &Lowered<AbilityDefinition>) -> bool {
        let evidence = audit_history(&lowered.history, &lowered.value);
        let mut diagnostics = Vec::new();
        detect_combat_history_relation(
            &text.to_ascii_lowercase(),
            text,
            &evidence,
            &mut diagnostics,
        );
        !diagnostics.is_empty()
    }

    fn population(subject: CombatRelationSubject) -> TargetFilter {
        TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::CombatRelation {
                relation: CombatRelation::BlockPairing {
                    direction: BlockPairDirection::SubjectIsBlocker,
                    scope: Some(CombatHistoryScope::ThisTurn),
                },
                subject,
            }]),
        )
    }

    fn bare() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::creature())
    }

    fn resolve_mut<'a>(
        path: &InstructionPath,
        mut def: &'a mut AbilityDefinition,
    ) -> &'a mut AbilityDefinition {
        for step in &path.0 {
            def = match step {
                InstructionStep::SubAbility => {
                    def.sub_ability.as_deref_mut().expect("sub instruction")
                }
                InstructionStep::ElseAbility => {
                    def.else_ability.as_deref_mut().expect("else instruction")
                }
                InstructionStep::Mode(index) => def.mode_abilities.get_mut(*index).expect("mode"),
                InstructionStep::DelayedPayload => match def.effect.as_mut() {
                    Effect::CreateDelayedTrigger { effect, .. } => effect,
                    other => panic!("delayed payload, got {other:?}"),
                },
            };
        }
        def
    }

    fn replace_population(def: &mut AbilityDefinition, replacement: TargetFilter) {
        match def.effect.as_mut() {
            Effect::SetTapState { target, .. }
            | Effect::Destroy { target, .. }
            | Effect::DestroyAll { target, .. } => *target = replacement,
            Effect::GenericEffect {
                target: None,
                static_abilities,
                ..
            } => {
                let carrier = static_abilities
                    .iter_mut()
                    .find(|def| def.affected.is_some())
                    .expect("broadcast static");
                carrier.affected = Some(replacement);
            }
            other => panic!("population carrier, got {other:?}"),
        }
    }

    #[test]
    fn audit_keeps_the_tap_producer_when_the_key_moves_to_untap() {
        let text = "Tap all creatures it blocked this turn and untap all creatures.";
        let mut lowered = lower(text, source_context());
        assert_eq!(lowered.history.len(), 1, "{lowered:?}");
        assert!(
            !flagged(text, &lowered),
            "positive producer witness: {lowered:?}"
        );
        let path = lowered.history[0].locator.clone().expect("tap locator");
        let tap = resolve_mut(&path, &mut lowered.value);
        assert!(matches!(
            *tap.effect,
            Effect::SetTapState {
                state: crate::types::ability::TapStateChange::Tap,
                ..
            }
        ));
        replace_population(tap, bare());
        let untap = lowered
            .value
            .sub_ability
            .as_deref_mut()
            .expect("untap continuation");
        replace_population(untap, population(CombatRelationSubject::Source));
        assert!(
            flagged(text, &lowered),
            "the Untap restriction cannot vouch for Tap"
        );
    }

    #[test]
    fn audit_keeps_the_resolved_subject_for_it() {
        for (text, ctx, expected, wrong) in [
            (
                "Tap all creatures it blocked this turn.",
                source_context(),
                CombatRelationSubject::Source,
                CombatRelationSubject::AnaphoricTarget,
            ),
            (
                "Destroy target creature. Tap all creatures it blocked this turn.",
                ParseContext::default(),
                CombatRelationSubject::AnaphoricTarget,
                CombatRelationSubject::Source,
            ),
        ] {
            let mut lowered = lower(text, ctx);
            assert_eq!(lowered.history.len(), 1, "{lowered:?}");
            assert_eq!(
                lowered.history[0]
                    .expected
                    .expect("resolved subject")
                    .subject,
                expected
            );
            assert!(
                !flagged(text, &lowered),
                "positive subject witness: {lowered:?}"
            );
            let path = lowered.history[0]
                .locator
                .clone()
                .expect("producer locator");
            replace_population(resolve_mut(&path, &mut lowered.value), population(wrong));
            assert!(
                flagged(text, &lowered),
                "the other possible reading of it cannot vouch"
            );
        }
    }

    #[test]
    fn identical_occurrences_keep_distinct_producers() {
        let text = "Tap all creatures ~ blocked this turn. Tap all creatures ~ blocked this turn.";
        let mut lowered = lower(text, source_context());
        assert_eq!(lowered.history.len(), 2, "{lowered:?}");
        assert_ne!(lowered.history[0].producer, lowered.history[1].producer);
        assert!(
            !flagged(text, &lowered),
            "each occurrence has its own restriction"
        );
        let path = lowered.history[1].locator.clone().expect("second producer");
        replace_population(resolve_mut(&path, &mut lowered.value), bare());
        assert!(
            flagged(text, &lowered),
            "the first producer cannot cover the repeated clause"
        );
    }

    #[test]
    fn compound_continuation_transports_its_own_producer() {
        let text = "Destroy target creature and all creatures ~ blocked this turn.";
        let mut lowered = lower(text, ParseContext::default());
        assert_eq!(lowered.history.len(), 1, "{lowered:?}");
        let path = lowered.history[0]
            .locator
            .clone()
            .expect("compound locator");
        assert_eq!(path.0.first(), Some(&InstructionStep::SubAbility));
        assert!(
            !flagged(text, &lowered),
            "positive compound witness: {lowered:?}"
        );
        replace_population(resolve_mut(&path, &mut lowered.value), bare());
        replace_population(
            &mut lowered.value,
            population(CombatRelationSubject::Source),
        );
        assert!(
            flagged(text, &lowered),
            "the head cannot vouch for the continuation"
        );
    }

    #[test]
    fn delayed_payloads_keep_the_history_producer_inside_the_payload() {
        for text in [
            "Regenerate target creature. At this turn's next end of combat, destroy all creatures that blocked or were blocked by it this turn.",
            "Choose target creature. At this turn's next end of combat, destroy all creatures that blocked or were blocked by it this turn.",
        ] {
            let mut lowered = lower_ability_ir_with_provenance(&parse_ability_ir_standalone(text, AbilityKind::Spell));
            assert_eq!(lowered.history.len(), 1, "{lowered:?}");
            let path = lowered.history[0].locator.clone().expect("delayed producer locator");
            assert!(path.0.contains(&InstructionStep::DelayedPayload));
            assert_eq!(lowered.history[0].expected.expect("delayed subject").subject, CombatRelationSubject::AnaphoricTarget);
            assert!(!flagged(text, &lowered), "positive delayed witness: {lowered:?}");
            let instruction = resolve_mut(&path, &mut lowered.value);
            let original = crate::parser::oracle_effect::own_instruction_populations(&instruction.effect)[0].clone();
            replace_population(instruction, bare());
            *lowered.value.effect.target_filter_mut().expect("earlier target operand") = original;
            assert!(flagged(text, &lowered), "the creating resolution cannot vouch for its delayed payload");
        }
    }

    #[test]
    fn broadcast_static_is_owned_by_its_grant_instruction() {
        let text = "Destroy target blocking creature. Creatures that were blocked by that creature this combat gain trample until end of turn.";
        let mut lowered = lower_ability_ir_with_provenance(&parse_ability_ir_standalone(
            text,
            AbilityKind::Spell,
        ));
        assert_eq!(lowered.history.len(), 1, "{lowered:?}");
        let path = lowered.history[0]
            .locator
            .clone()
            .expect("broadcast locator");
        assert!(
            !flagged(text, &lowered),
            "positive broadcast witness: {lowered:?}"
        );
        let instruction = resolve_mut(&path, &mut lowered.value);
        assert!(matches!(
            *instruction.effect,
            Effect::GenericEffect { target: None, .. }
        ));
        let original =
            crate::parser::oracle_effect::own_instruction_populations(&instruction.effect)[0]
                .clone();
        replace_population(instruction, bare());
        replace_population(&mut lowered.value, original);
        assert!(
            flagged(text, &lowered),
            "the destroy operand cannot vouch for the broadcast grant"
        );
    }

    #[test]
    fn missing_records_and_dangling_locators_are_reported() {
        let text = "Tap all creatures ~ blocked this turn.";
        let mut lowered = lower(text, source_context());
        assert_eq!(lowered.history.len(), 1);
        assert!(!flagged(text, &lowered));
        let saved = lowered.history.clone();
        lowered.history.clear();
        assert!(
            flagged(text, &lowered),
            "a recognized occurrence with no record"
        );
        lowered.history = saved;
        lowered.history[0].locator = Some(InstructionPath(vec![InstructionStep::ElseAbility]));
        assert!(
            flagged(text, &lowered),
            "a dangling locator cannot use another instruction"
        );
        lowered.history[0].locator = None;
        assert!(
            flagged(text, &lowered),
            "an unresolved locator is also dangling"
        );
    }

    #[test]
    fn transport_never_substitutes_a_shape_identical_clone() {
        let text = "Tap all creatures ~ blocked this turn.";
        let lowered = lower(text, source_context());
        assert_eq!(lowered.history.len(), 1);
        assert!(!flagged(text, &lowered));
        let transport = HistoryTransport::from_lowered(&lowered.value, lowered.history.clone());
        let cloned = lowered.value.clone();
        let finished = transport.finish(cloned);
        assert_eq!(
            finished.history[0].locator, None,
            "a clone has no transported producer identity"
        );
        assert!(flagged(text, &finished));
        // Keep the original allocation live through transport sealing.
        assert!(!flagged(text, &lowered));
    }

    #[test]
    fn unresolved_expectation_cannot_be_inferred_from_a_typed_relation() {
        let text = "Tap all creatures ~ blocked this turn.";
        let mut lowered = lower(text, source_context());
        assert_eq!(lowered.history.len(), 1);
        assert!(!flagged(text, &lowered));
        lowered.history[0].expected = None;
        assert!(flagged(text, &lowered));
    }

    #[test]
    fn unresolved_subjects_still_have_occurrence_records() {
        let text = "Tap all creatures it blocked this turn.";
        let mut clause = crate::parser::oracle_ir::ast::parsed_clause(Effect::DestroyAll {
            target: bare(),
            cant_regenerate: false,
        });
        capture_own_history(&mut clause, text, &ParseContext::default());
        assert_eq!(clause.history.len(), 1);
        assert_eq!(clause.history[0].expected, None);
    }

    #[test]
    fn parser_trace_serialization_omits_history_provenance() {
        let text = "Tap all creatures ~ blocked this turn.";
        let mut ir = parse_ability_ir_with_context(text, AbilityKind::Spell, &mut source_context());
        assert!(ir
            .body
            .clauses
            .iter()
            .any(|clause| !clause.parsed.history.is_empty()));
        let with_history = serde_json::to_vec(&ir).expect("IR trace");
        for clause in &mut ir.body.clauses {
            clause.parsed.history.clear();
        }
        assert_eq!(
            with_history,
            serde_json::to_vec(&ir).expect("IR trace without metadata")
        );
    }
}
