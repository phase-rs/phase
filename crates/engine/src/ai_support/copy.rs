use crate::types::ability::{
    AbilityDefinition, ContinuousModification, CopyManaValueLimit, Effect, TargetFilter,
};
use crate::types::game_state::PendingCast;
use crate::types::keywords::Keyword;

pub fn copy_target_filter(effect_def: &AbilityDefinition) -> Option<&TargetFilter> {
    match &*effect_def.effect {
        Effect::BecomeCopy { target, .. } => Some(target),
        _ => None,
    }
}

pub fn copy_target_mana_value_ceiling(
    actual_mana_spent: u32,
    effect_def: &AbilityDefinition,
) -> Option<u32> {
    match &*effect_def.effect {
        Effect::BecomeCopy {
            mana_value_limit: Some(CopyManaValueLimit::AmountSpentToCastSource),
            ..
        } => Some(actual_mana_spent),
        Effect::BecomeCopy { .. } => None,
        _ => None,
    }
}

pub fn copy_effect_adds_flying(effect_def: &AbilityDefinition) -> bool {
    match &*effect_def.effect {
        Effect::BecomeCopy {
            additional_modifications,
            ..
        } => additional_modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying,
                }
            )
        }),
        _ => false,
    }
}

pub fn project_copy_mana_spent_for_x(pending_cast: &PendingCast, x_value: u32) -> u32 {
    let mut cost = pending_cast.cost.clone();
    cost.concretize_x(x_value);
    cost.mana_value()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, Duration};

    fn copy_effect(description: &str) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration: Some(Duration::Permanent),
                mana_value_limit: None,
                additional_modifications: Vec::new(),
            },
        )
        .description(description.to_string())
    }

    #[test]
    fn typed_copy_limit_sets_copy_ceiling() {
        let effect = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration: Some(Duration::Permanent),
                mana_value_limit: Some(CopyManaValueLimit::AmountSpentToCastSource),
                additional_modifications: vec![
                    ContinuousModification::AddSubtype {
                        subtype: "Bird".to_string(),
                    },
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Flying,
                    },
                ],
            },
        );

        assert_eq!(copy_target_mana_value_ceiling(4, &effect), Some(4));
    }

    #[test]
    fn generic_clone_text_has_no_mana_ceiling() {
        let effect = copy_effect(
            "You may have this creature enter as a copy of any creature on the battlefield.",
        );

        assert_eq!(copy_target_mana_value_ceiling(4, &effect), None);
    }

    #[test]
    fn detects_copy_effects_that_add_flying() {
        let effect = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration: Some(Duration::Permanent),
                mana_value_limit: None,
                additional_modifications: vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying,
                }],
            },
        );
        assert!(copy_effect_adds_flying(&effect));
    }
}
