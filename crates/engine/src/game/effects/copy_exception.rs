use std::sync::Arc;

use crate::types::ability::{ContinuousModification, StaticDefinition};

/// The copiable characteristic axes an "except" clause supplies while copying.
///
/// CR 707.9d excludes a copied characteristic-defining ability when a copy
/// exception does not copy, retains, or supplies values for that characteristic.
/// These axes deliberately describe only the characteristics this engine's
/// typed copy-exception vocabulary can override.
#[derive(Clone, Copy, Default)]
struct CopyExceptionOverrides {
    card_types: bool,
    color: bool,
    power: bool,
    toughness: bool,
}

impl CopyExceptionOverrides {
    fn from_modifications(modifications: &[ContinuousModification]) -> Self {
        let mut overrides = Self::default();
        for modification in modifications {
            match modification {
                ContinuousModification::SetCardTypes { .. }
                | ContinuousModification::RemoveAllSubtypes { .. } => overrides.card_types = true,
                // CR 707.9d applies when an exception adds a color in addition
                // to the copied object's other colors, too (Lazotep Convert).
                ContinuousModification::AddColor { .. }
                | ContinuousModification::SetColor { .. } => overrides.color = true,
                ContinuousModification::SetPower { .. }
                | ContinuousModification::SetPowerDynamic { .. } => overrides.power = true,
                ContinuousModification::SetToughness { .. }
                | ContinuousModification::SetToughnessDynamic { .. } => overrides.toughness = true,
                _ => {}
            }
        }
        overrides
    }

    fn is_empty(self) -> bool {
        !self.card_types && !self.color && !self.power && !self.toughness
    }
}

/// Source CDA pruning for a copy exception.
///
/// `definitions` retains every source definition that this typed vocabulary
/// cannot classify. `all_definitions_classified` is separate because a
/// permanent-copy snapshot must retain its legacy layered representation when
/// any source CDA could define an unknown characteristic.
pub(crate) struct CopyExceptionCdaPruning {
    pub(crate) definitions: Vec<StaticDefinition>,
    pub(crate) all_definitions_classified: bool,
}

/// Prunes source CDAs whose defined characteristic is overridden by a copy
/// exception, retaining only an individual CDA whose shape is outside the
/// typed vocabulary.
///
/// CR 707.9d: this is shared by permanent-copy snapshots and copy-token body
/// materialization, because both paths establish the copied object's copiable
/// values. An unclassified CDA cannot justify its own deletion, but must not
/// stop a separate, classified CDA from being pruned.
pub(crate) fn prune_copy_exception_overridden_cdas(
    definitions: &Arc<Vec<StaticDefinition>>,
    modifications: &[ContinuousModification],
) -> CopyExceptionCdaPruning {
    let overrides = CopyExceptionOverrides::from_modifications(modifications);
    if overrides.is_empty() {
        return CopyExceptionCdaPruning {
            definitions: definitions.as_ref().clone(),
            all_definitions_classified: true,
        };
    }

    let mut retained = Vec::with_capacity(definitions.len());
    let mut all_definitions_classified = true;
    for definition in definitions.iter() {
        if !definition.characteristic_defining {
            retained.push(definition.clone());
            continue;
        }

        let Some(axes) = cda_defined_axes(definition) else {
            all_definitions_classified = false;
            retained.push(definition.clone());
            continue;
        };
        let overridden = (axes.card_types && overrides.card_types)
            || (axes.color && overrides.color)
            || (axes.power && overrides.power)
            || (axes.toughness && overrides.toughness);
        if !overridden {
            retained.push(definition.clone());
        }
    }
    CopyExceptionCdaPruning {
        definitions: retained,
        all_definitions_classified,
    }
}

/// A CDA definition is removable as a whole only when each of its modifications
/// has a known characteristic axis. A fixed P/T pair defines both axes.
fn cda_defined_axes(definition: &StaticDefinition) -> Option<CopyExceptionOverrides> {
    let mut axes = CopyExceptionOverrides::default();
    for modification in &definition.modifications {
        match modification {
            ContinuousModification::AddAllCreatureTypes => axes.card_types = true,
            ContinuousModification::SetDynamicPower { .. }
            | ContinuousModification::SetPower { .. } => axes.power = true,
            ContinuousModification::SetDynamicToughness { .. }
            | ContinuousModification::SetToughness { .. } => axes.toughness = true,
            ContinuousModification::SetColor { .. } => axes.color = true,
            _ => return None,
        }
    }
    Some(axes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{QuantityExpr, TargetFilter};

    /// CR 707.9d + CR 613.4a/b: Saw in Half-style dynamic base P/T exceptions
    /// replace the corresponding value of a copied P/T CDA.
    #[test]
    fn dynamic_pt_copy_exception_prunes_corresponding_cda_axes() {
        let definitions = Arc::new(vec![
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .cda()
                .modifications(vec![ContinuousModification::SetDynamicPower {
                    value: QuantityExpr::Fixed { value: 2 },
                }]),
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .cda()
                .modifications(vec![ContinuousModification::SetDynamicToughness {
                    value: QuantityExpr::Fixed { value: 3 },
                }]),
        ]);

        let pruned = prune_copy_exception_overridden_cdas(
            &definitions,
            &[
                ContinuousModification::SetPowerDynamic {
                    value: QuantityExpr::Fixed { value: 1 },
                },
                ContinuousModification::SetToughnessDynamic {
                    value: QuantityExpr::Fixed { value: 1 },
                },
            ],
        );

        assert!(pruned.all_definitions_classified);
        assert!(pruned.definitions.is_empty());
    }
}
