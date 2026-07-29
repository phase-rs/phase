use crate::types::ability::{AbilityKind, TargetFilter};

use super::oracle::has_unimplemented;
use super::oracle_classifier::{
    has_trigger_prefix, is_damage_prevention_pattern, is_effect_sentence_candidate,
    is_replacement_pattern, is_static_pattern,
};
use super::oracle_effect::{lower_ability_ir, parse_ability_ir_with_context};
use super::oracle_ir::context::ParseContext;
use super::oracle_ir::doc::{UnsupportedAbilityCategory, UnsupportedAbilityIr};
use super::oracle_ir::effect_chain::AbilityIr;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[allow(clippy::large_enum_variant)] // Intentional: successful dispatch retains parser IR without an extra allocation.
pub(super) enum NomDispatchIr {
    Spell(AbilityIr),
    Unsupported(UnsupportedAbilityIr),
}

/// CR 303.4 + CR 702.103: `host_self_reference` carries the enclosing card's
/// typed attachment-host self-reference (set by `parse_oracle_ir` for
/// Aura/bestow cards) so a `"that creature"` copy-token anaphor dispatched
/// through this nom path remaps to the enchanted host. `None` for non-Aura
/// cards leaves `ParentTarget` semantics intact.
///
pub(super) fn dispatch_line_nom(
    line: &str,
    card_name: &str,
    host_self_reference: Option<TargetFilter>,
) -> NomDispatchIr {
    let lower = line.to_lowercase();
    let mut ctx = ParseContext {
        subject: None,
        card_name: Some(card_name.to_string()),
        actor: None,
        host_self_reference,
        ..Default::default()
    };

    if is_effect_sentence_candidate(&lower) || is_damage_prevention_pattern(&lower) {
        let ir = parse_ability_ir_with_context(line, AbilityKind::Spell, &mut ctx);
        if !has_unimplemented(&lower_ability_ir(&ir)) {
            return NomDispatchIr::Spell(ir);
        }
    }

    let lower_trimmed = lower.trim_start();
    if has_trigger_prefix(lower_trimmed) {
        return NomDispatchIr::Unsupported(UnsupportedAbilityIr::new(
            UnsupportedAbilityCategory::TriggerStructure,
            format!("Trigger prefix matched but line failed trigger parser: {line}"),
            line,
        ));
    }

    if is_static_pattern(&lower) {
        return NomDispatchIr::Unsupported(UnsupportedAbilityIr::new(
            UnsupportedAbilityCategory::StaticStructure,
            format!("Static pattern matched but line failed static parser: {line}"),
            line,
        ));
    }

    if is_replacement_pattern(&lower) {
        return NomDispatchIr::Unsupported(UnsupportedAbilityIr::new(
            UnsupportedAbilityCategory::ReplacementStructure,
            format!("Replacement pattern matched but line failed replacement parser: {line}"),
            line,
        ));
    }

    if is_effect_sentence_candidate(&lower) {
        return NomDispatchIr::Unsupported(UnsupportedAbilityIr::new(
            UnsupportedAbilityCategory::EffectStructure,
            format!("Effect sentence candidate but line failed effect parser: {line}"),
            line,
        ));
    }

    NomDispatchIr::Unsupported(UnsupportedAbilityIr::unknown(line))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Effect, MultiTargetSpec};
    use crate::types::game_state::DistributionUnit;

    /// Issue #4266 regression: `dispatch_line_nom` was returning `*def.effect`,
    /// discarding `distribute` and `multi_target` from the parsed
    /// `AbilityDefinition`. The caller then wrapped the bare `Effect` in a new
    /// `AbilityDefinition::new(...)`, losing those fields permanently. Forked
    /// Bolt therefore never reached `WaitingFor::DistributeAmong` and instead
    /// dealt the full 2 damage to every selected target.
    ///
    /// This test is fail-on-revert: if the return type reverts to `-> Effect`,
    /// the `.distribute` / `.multi_target` field accesses below will not compile.
    #[test]
    fn dispatch_line_nom_preserves_distribute_and_multi_target_for_divided_damage() {
        let NomDispatchIr::Spell(ir) = dispatch_line_nom(
            "~ deals 2 damage divided as you choose among one or two targets.",
            "Forked Bolt",
            None,
        ) else {
            panic!("Forked Bolt must dispatch as an IR-native spell");
        };
        let def = lower_ability_ir(&ir);
        assert_eq!(
            def.distribute,
            Some(DistributionUnit::Damage),
            "distribute lost by dispatch_line_nom"
        );
        assert_eq!(
            def.multi_target,
            Some(MultiTargetSpec::fixed(1, 2)),
            "multi_target lost by dispatch_line_nom"
        );
    }

    #[test]
    fn dispatch_line_nom_preserves_structural_residual_payload() {
        let line = "Whenever unsupported trigger structure";
        let NomDispatchIr::Unsupported(unsupported) = dispatch_line_nom(line, "Test Card", None)
        else {
            panic!("unsupported trigger must retain its structural category");
        };
        assert_eq!(
            unsupported.category,
            UnsupportedAbilityCategory::TriggerStructure
        );
        let def = crate::parser::oracle::lower_unsupported_node(&unsupported, 0);
        let Effect::Unimplemented { name, description } = def.effect.as_ref() else {
            panic!("expected unimplemented residual: {def:?}");
        };
        assert_eq!(name, "trigger_structure");
        assert_eq!(
            description.as_deref(),
            Some("Trigger prefix matched but line failed trigger parser: Whenever unsupported trigger structure")
        );
        assert_eq!(def.description.as_deref(), Some(line));
    }
}
