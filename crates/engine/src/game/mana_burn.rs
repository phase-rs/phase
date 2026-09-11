//! Mana burn — the pre-M10 rule that unspent mana costs a player life.
//!
//! The current Comprehensive Rules have no mana-burn rule; the glossary entry
//! "Mana Burn (Obsolete)" records what it was: "Older versions of the rules
//! stated that unspent mana caused a player to **lose life**." Life loss, not
//! damage — the two are behaviorally different here (damage can be prevented
//! or redirected and triggers "dealt damage" abilities; life loss does
//! neither).
//!
//! It differs from the modern rule on two axes, and both matter:
//!
//! 1. **When the pool empties.** CR 106.4 and CR 500.5 empty pools at the end
//!    of every step AND phase. The pre-M10 rule emptied at the end of each
//!    PHASE — CR 500.1's five — so mana survived the steps within one.
//! 2. **What emptying costs.** Modern emptying is free; here the emptied count
//!    is life lost.
//!
//! Only a custom format declaring `LegacyRuleSet.mana_burn` opts in, so every
//! built-in format is unaffected by construction rather than by a check.

use crate::types::custom_format::ManaBurnPolicy;
use crate::types::format::FormatConfig;
use crate::types::game_state::GameState;

/// The mana-burn policy a format declares.
///
/// A built-in format carries no `custom_rules` and resolves to the modern
/// default. Mirrors `game::ante::policy_of`, and for the same reason: the
/// policy lives on the resolved `FormatConfig`, so anything holding one can
/// ask without needing a running game.
pub(crate) fn policy_of(format_config: &FormatConfig) -> ManaBurnPolicy {
    format_config
        .custom_rules
        .as_deref()
        .map(|rules| rules.legality.legacy.mana_burn)
        .unwrap_or_default()
}

/// Whether this game empties mana pools the pre-M10 way.
pub(crate) fn applies(state: &GameState) -> bool {
    matches!(policy_of(&state.format_config), ManaBurnPolicy::Obsolete)
}
