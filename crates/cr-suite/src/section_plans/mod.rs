//! Per-section coverage plans — structural guidance for promoting skeletons.
//!
//! Each submodule documents which CR rules in that section are good first
//! executable candidates and which assertion kinds apply. Agents extend
//! fixtures using this map rather than inventing one-off card tests.

pub mod card_types;
pub mod casting;
pub mod combat;
pub mod keywords;
pub mod keywords_702;
pub mod layers;
pub mod life_damage;
pub mod multiplayer;
pub mod priority;
pub mod replacement;
pub mod sba;
pub mod stack_resolution;
pub mod turn_structure;
pub mod zones;

/// A planned executable promotion for a CR rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoveragePlanEntry {
    pub rule: &'static str,
    pub section: u32,
    pub priority: CoveragePriority,
    pub suggested_assertions: &'static [&'static str],
    pub notes: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoveragePriority {
    High,
    Medium,
    Low,
    NotApplicable,
}

/// Aggregate all section coverage plans.
pub fn all_coverage_plans() -> Vec<&'static CoveragePlanEntry> {
    let mut out = Vec::new();
    out.extend(life_damage::PLAN);
    out.extend(sba::PLAN);
    out.extend(zones::PLAN);
    out.extend(turn_structure::PLAN);
    out.extend(priority::PLAN);
    out.extend(combat::PLAN);
    out.extend(keywords::PLAN);
    out.extend(keywords_702::PLAN);
    out.extend(casting::PLAN);
    out.extend(stack_resolution::PLAN);
    out.extend(layers::PLAN);
    out.extend(replacement::PLAN);
    out.extend(card_types::PLAN);
    out.extend(multiplayer::PLAN);
    out
}

/// Plans for a single section.
pub fn plans_for_section(section: u32) -> Vec<&'static CoveragePlanEntry> {
    all_coverage_plans()
        .into_iter()
        .filter(|p| p.section == section)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_cover_core_sections() {
        let plans = all_coverage_plans();
        assert!(plans.len() >= 20);
        let sections: std::collections::BTreeSet<_> = plans.iter().map(|p| p.section).collect();
        assert!(sections.contains(&119));
        assert!(sections.contains(&704));
        assert!(sections.contains(&403));
    }

    #[test]
    fn plans_cover_expanded_sections() {
        let sections: std::collections::BTreeSet<_> =
            all_coverage_plans().iter().map(|p| p.section).collect();
        for expected in [601, 608, 613, 614, 615, 702, 300, 302, 800, 903] {
            assert!(
                sections.contains(&expected),
                "coverage plans missing section {expected}"
            );
        }
    }

    #[test]
    fn every_planned_section_is_included_in_catalog() {
        // A plan for a section the catalog excludes is a bug (summary would drop it).
        for plan in all_coverage_plans() {
            assert!(
                crate::catalog::is_included_section(plan.section),
                "plan for CR {} targets excluded section {}",
                plan.rule,
                plan.section
            );
        }
    }
}
