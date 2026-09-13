//! Pre-M10 Wish reach — the legacy rule under which a Wish could retrieve a
//! card its owner had removed from the game.
//!
//! Magic 2010 (July 2009) rewrote the zone model. Today CR 400.11 says
//! "outside the game is not a zone" and CR 400.11a says only a player's
//! sideboard is outside it, so CR 701.23j's "search outside the game for a
//! card" reaches the sideboard and nothing else. Exile is an ordinary in-game
//! zone and a Wish cannot see it.
//!
//! Before M10 there was no exile zone — cards were **removed from the game**,
//! which counted as *outside* it. A Wish could therefore retrieve an owned
//! card that had been removed, and pre-M10 formats are played that way.
//!
//! This module does not implement current CR; it deliberately re-enables
//! behavior the M10 update removed, and only for a custom format that declares
//! [`WishOutsideGameScope::PreM10ReachesExile`]. Every built-in format resolves
//! to the modern default by construction rather than by a check.
//!
//! **The pool it widens to is the one the engine already has.** A pre-M10
//! removed-from-the-game card a Wish could name was, in practice, exactly a
//! card its namer OWNS and can SEE: you cannot choose a card you do not own,
//! and a face-down removed card was never an eligible Wish target. That is
//! precisely what [`OutsideGameSourcePool::SideboardAndFaceUpExile`] already
//! means for the Karn/Coax class — so the legacy rule is expressed by widening
//! a Wish-class search's *effective* pool to it, not by a second collector.
//!
//! **Which searches are Wish-class is carried, never inferred from the pool.**
//! Learn (CR 701.48a) also says "from outside the game" and also declares
//! `Sideboard`, but it was templated in 2021 and means the sideboard in every
//! format. [`OutsideGameReach`] is what separates them, and it is set by
//! whichever builder constructs the effect.

use crate::types::ability::{OutsideGameReach, OutsideGameSourcePool};
use crate::types::custom_format::WishOutsideGameScope;
use crate::types::format::FormatConfig;
use crate::types::game_state::GameState;

/// The Wish scope a format declares.
///
/// A built-in format carries no `custom_rules` and resolves to the modern
/// default. Mirrors `game::ante::policy_of` and `game::mana_burn::policy_of`,
/// and for the same reason: the policy lives on the resolved `FormatConfig`,
/// so anything holding one can ask without needing a running game.
pub(crate) fn policy_of(format_config: &FormatConfig) -> WishOutsideGameScope {
    format_config
        .custom_rules
        .as_deref()
        .map(|rules| rules.legality.legacy.wish_scope)
        .unwrap_or_default()
}

/// The pool a search actually reaches in this game, given the pool its effect
/// declares and the era it was templated against.
///
/// Returns the widened pool rather than a bare "does it reach exile?" so a
/// caller that needs the pool itself — to describe the search, or to route a
/// later stage of it — gets a truthful answer instead of having to re-derive
/// one. The declared pool is never narrowed: a card that already reaches
/// face-up exile under the modern rules (Karn Liberated, Coax from the Blind
/// Eternities) keeps doing so in every format.
///
/// CR 400.11 + CR 400.11a + CR 701.23j, relaxed: under
/// [`WishOutsideGameScope::PreM10ReachesExile`] a plain
/// [`OutsideGameSourcePool::Sideboard`] search reads as
/// [`OutsideGameSourcePool::SideboardAndFaceUpExile`] — the pre-M10 reach —
/// **but only for [`OutsideGameReach::WishCycle`]**. `reach` is the whole
/// reason this takes three arguments: Learn declares the same `Sideboard` pool
/// and was templated in 2021, so widening by pool alone would hand a modern
/// mechanic a zone the pre-M10 rule never gave it.
pub(crate) fn effective_pool(
    state: &GameState,
    declared: OutsideGameSourcePool,
    reach: OutsideGameReach,
) -> OutsideGameSourcePool {
    match (policy_of(&state.format_config), reach) {
        // Modern format, or an effect written against the modern boundary:
        // the declared pool stands.
        (WishOutsideGameScope::PostM10SideboardOnly, _) | (_, OutsideGameReach::CurrentRules) => {
            declared
        }
        (WishOutsideGameScope::PreM10ReachesExile, OutsideGameReach::WishCycle) => match declared {
            OutsideGameSourcePool::Sideboard => OutsideGameSourcePool::SideboardAndFaceUpExile,
            already_wide @ OutsideGameSourcePool::SideboardAndFaceUpExile => already_wide,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::custom_format::LegacyRuleSet;

    fn game_with_scope(scope: WishOutsideGameScope) -> GameState {
        let mut state = GameState::new_two_player(7);
        state.format_config = FormatConfig::for_custom_rules(
            &crate::types::custom_format::test_rules_with_legacy(LegacyRuleSet {
                wish_scope: scope,
                ..LegacyRuleSet::default()
            }),
        );
        state
    }

    /// The whole point of the axis: the modern Wish cycle reaches exile only
    /// under the legacy scope.
    #[test]
    fn only_the_legacy_scope_widens_a_sideboard_search() {
        let modern = game_with_scope(WishOutsideGameScope::PostM10SideboardOnly);
        assert_eq!(
            effective_pool(
                &modern,
                OutsideGameSourcePool::Sideboard,
                OutsideGameReach::WishCycle
            ),
            OutsideGameSourcePool::Sideboard
        );

        let legacy = game_with_scope(WishOutsideGameScope::PreM10ReachesExile);
        assert_eq!(
            effective_pool(
                &legacy,
                OutsideGameSourcePool::Sideboard,
                OutsideGameReach::WishCycle
            ),
            OutsideGameSourcePool::SideboardAndFaceUpExile
        );
    }

    /// A card that already reaches exile is never narrowed by the axis, in
    /// either direction. Karn Liberated does not stop working because a format
    /// declined a legacy rule.
    #[test]
    fn an_already_wide_pool_is_untouched_by_either_scope() {
        for scope in [
            WishOutsideGameScope::PostM10SideboardOnly,
            WishOutsideGameScope::PreM10ReachesExile,
        ] {
            assert_eq!(
                effective_pool(
                    &game_with_scope(scope),
                    OutsideGameSourcePool::SideboardAndFaceUpExile,
                    OutsideGameReach::WishCycle
                ),
                OutsideGameSourcePool::SideboardAndFaceUpExile,
                "{scope:?} must not narrow a pool the card itself declares"
            );
        }
    }

    /// The Learn case, and the reason `reach` exists: an effect templated
    /// against the CURRENT boundary is never widened, even by a format that
    /// reverts the boundary for the Wish cycle. Without this parameter the
    /// pool alone would be indistinguishable and Learn would reach exile.
    #[test]
    fn a_current_rules_search_is_never_widened() {
        let legacy = game_with_scope(WishOutsideGameScope::PreM10ReachesExile);
        assert_eq!(
            effective_pool(
                &legacy,
                OutsideGameSourcePool::Sideboard,
                OutsideGameReach::CurrentRules
            ),
            OutsideGameSourcePool::Sideboard,
            "Learn declares the same pool as a Wish; only the era separates them"
        );
        // Paired control on the same format: the Wish cycle IS widened, so the
        // assertion above is about `reach` and not about the scope being off.
        assert_eq!(
            effective_pool(
                &legacy,
                OutsideGameSourcePool::Sideboard,
                OutsideGameReach::WishCycle
            ),
            OutsideGameSourcePool::SideboardAndFaceUpExile
        );
    }

    /// A built-in format carries no custom rules at all, so it resolves to the
    /// modern scope without any format needing to say so.
    #[test]
    fn a_built_in_format_resolves_to_the_modern_scope() {
        let state = GameState::new_two_player(7);
        assert_eq!(
            policy_of(&state.format_config),
            WishOutsideGameScope::PostM10SideboardOnly
        );
        assert_eq!(
            effective_pool(
                &state,
                OutsideGameSourcePool::Sideboard,
                OutsideGameReach::WishCycle
            ),
            OutsideGameSourcePool::Sideboard
        );
    }
}
