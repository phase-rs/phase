use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, TargetFilter};

use super::oracle::has_unimplemented;
use super::oracle_classifier::{
    has_trigger_prefix, is_damage_prevention_pattern, is_effect_sentence_candidate,
    is_replacement_pattern, is_static_pattern,
};
use super::oracle_effect::parse_effect_chain_with_context;
use super::oracle_ir::context::ParseContext;

/// CR 303.4 + CR 702.103: `host_self_reference` carries the enclosing card's
/// typed attachment-host self-reference (set by `parse_oracle_ir` for
/// Aura/bestow cards) so a `"that creature"` copy-token anaphor dispatched
/// through this nom path remaps to the enchanted host. `None` for non-Aura
/// cards leaves `ParentTarget` semantics intact.
///
/// Returns the full `AbilityDefinition` so that fields beyond `effect`
/// (e.g. `distribute`, `multi_target`) survive to the calling `parse_oracle_ir`
/// loop. Callers that previously wrapped the result in `AbilityDefinition::new`
/// must use the returned def directly.
pub(super) fn dispatch_line_nom(
    line: &str,
    card_name: &str,
    host_self_reference: Option<TargetFilter>,
) -> AbilityDefinition {
    let lower = line.to_lowercase();
    let mut ctx = ParseContext {
        subject: None,
        card_name: Some(card_name.to_string()),
        actor: None,
        host_self_reference,
        ..Default::default()
    };

    if is_effect_sentence_candidate(&lower) || is_damage_prevention_pattern(&lower) {
        let def = parse_effect_chain_with_context(line, AbilityKind::Spell, &mut ctx);
        if !has_unimplemented(&def) {
            // Return the full AbilityDefinition so callers retain distribute,
            // multi_target, and any other parsed metadata.
            return def;
        }
    }

    let lower_trimmed = lower.trim_start();
    if has_trigger_prefix(lower_trimmed) {
        return AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented(
                "trigger_structure",
                format!("Trigger prefix matched but line failed trigger parser: {line}"),
            ),
        )
        .description(line.to_string());
    }

    if is_static_pattern(&lower) {
        return AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented(
                "static_structure",
                format!("Static pattern matched but line failed static parser: {line}"),
            ),
        )
        .description(line.to_string());
    }

    if is_replacement_pattern(&lower) {
        return AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented(
                "replacement_structure",
                format!("Replacement pattern matched but line failed replacement parser: {line}"),
            ),
        )
        .description(line.to_string());
    }

    if is_effect_sentence_candidate(&lower) {
        return AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented(
                "effect_structure",
                format!("Effect sentence candidate but line failed effect parser: {line}"),
            ),
        )
        .description(line.to_string());
    }

    AbilityDefinition::new(AbilityKind::Spell, Effect::unimplemented("unknown", line))
        .description(line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::MultiTargetSpec;
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
        let def = dispatch_line_nom(
            "~ deals 2 damage divided as you choose among one or two targets.",
            "Forked Bolt",
            None,
        );
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
}
