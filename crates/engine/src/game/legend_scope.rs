//! Legend-rule scope — whether same-named legendary permanents interact across
//! controllers, as they did before Magic 2014.
//!
//! CR 704.5j today: "If two or more legendary permanents with the same name are
//! **controlled by the same player**, that player chooses one of them, and the
//! rest are put into their owners' graveyards." Two different players may each
//! keep their own copy, and the one who has several picks which survives.
//!
//! M14 (effective 2013-07-13) wrote that scope in. Every form of the rule
//! before it — from *Legends* in 1994 through 2013 — grouped same-named legends
//! **regardless of controller**, so two players could not both keep a copy.
//! That changes the set of legal board states, which is why this is an axis and
//! not a wording preference: in a mirror where both players resolve the same
//! legend, pre-M14 both die and today each keeps one.
//!
//! **What [`LegendRuleScope::PreM14AnyController`] models, precisely.** The
//! mechanic shifted across pre-M14 eras — early *Legends*-era "first in play
//! trumps", the Sixth Edition (1999) "all of them go to the graveyard" form,
//! and the *Champions of Kamigawa* (2004) nullification variant. This variant
//! is the **Sixth Edition form**: global grouping, no choice, every member of a
//! two-or-more same-name group put into its owner's graveyard. That is the form
//! that changes cross-controller legal board states, and it is deliberately the
//! only one modeled — the era-specific micro-variants are out of scope, and the
//! enum has room for one later if a format ever needs it.
//!
//! This module does not implement current CR; it re-enables a scope the M14
//! update replaced, and only for a custom format that declares it. None of the
//! bundled Eternal Central presets do: their published rulesets list mana burn,
//! damage-on-the-stack and wish reach as their legacy exceptions and never a
//! legend-rule reversion, so all of them resolve to [`LegendRuleScope::Modern`].
//! This ships as the general historical-rules axis, not as preset behavior.

use crate::types::custom_format::LegendRuleScope;
use crate::types::format::FormatConfig;
use crate::types::game_state::GameState;

/// The legend-rule scope a format declares.
///
/// A built-in format carries no `custom_rules` and resolves to the modern
/// default. Mirrors `game::ante::policy_of` and `game::mana_burn::policy_of`.
pub(crate) fn policy_of(format_config: &FormatConfig) -> LegendRuleScope {
    format_config
        .custom_rules
        .as_deref()
        .map(|rules| rules.legality.legacy.legend_rule_scope)
        .unwrap_or_default()
}

/// Whether this game groups same-named legendary permanents across ALL
/// controllers, the pre-M14 way.
///
/// The two scopes differ in more than grouping — the pre-M14 form is also
/// choiceless — so this answers the question the SBA branches on, not a
/// standalone "is grouping global?" that a caller could pair with the wrong
/// resolution. See `sba::check_legend_rule`.
pub(crate) fn groups_across_controllers(state: &GameState) -> bool {
    matches!(
        policy_of(&state.format_config),
        LegendRuleScope::PreM14AnyController
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::custom_format::{test_rules_with_legacy, LegacyRuleSet};

    #[test]
    fn only_the_legacy_scope_groups_across_controllers() {
        let mut legacy = GameState::new_two_player(11);
        legacy.format_config =
            FormatConfig::for_custom_rules(&test_rules_with_legacy(LegacyRuleSet {
                legend_rule_scope: LegendRuleScope::PreM14AnyController,
                ..LegacyRuleSet::default()
            }));
        assert!(groups_across_controllers(&legacy));

        // Paired control: a custom format is not automatically a legacy format.
        let mut modern_custom = GameState::new_two_player(11);
        modern_custom.format_config =
            FormatConfig::for_custom_rules(&test_rules_with_legacy(LegacyRuleSet::default()));
        assert!(!groups_across_controllers(&modern_custom));

        // And a built-in format carries no custom rules at all.
        assert!(!groups_across_controllers(&GameState::new_two_player(11)));
    }
}
