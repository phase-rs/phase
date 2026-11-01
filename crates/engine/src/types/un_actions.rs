//! Engine-side Un-card action state: the per-game opt-in, menu and Gotcha-offer
//! bookkeeping layered over the [`phase_ast::types::un_actions`] vocabulary.
//! Offers bind to [`ObjectIdentityBinding`], which lives in the engine.

pub use phase_ast::types::un_actions::*;

use serde::{Deserialize, Serialize};

use super::identifiers::{ObjectId, ObjectIdentityBinding};
use super::player::PlayerId;

#[inline]
fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// A live opportunity for `player` to call "Gotcha!" for the Gotcha ability on
/// `source`, returning `returns` to hand when the reflexive trigger resolves.
///
/// `source` and `returns` are each bound to the object's occurrence (object id +
/// incarnation) and the zone it was in when the window opened. CR 400.7: an object
/// that changes zones becomes a new object and bumps its incarnation, so a bare
/// ObjectId goes stale — the bindings let `offer_is_still_valid` reject a window
/// whose source or returned card has moved or been replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GotchaOffer {
    pub id: u64,
    pub player: PlayerId,
    /// The object carrying the Gotcha ability.
    pub source: ObjectIdentityBinding,
    /// The card the ability returns ("this card" → `source`; "that card" → the
    /// graveyard card whose name contained the spoken word).
    pub returns: ObjectIdentityBinding,
}

/// Per-game Un-card action state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnActionState {
    /// Players who opted in to Un-card actions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled_players: Vec<PlayerId>,
    /// Gesture kinds worth offering in the Un-actions menu, derived by
    /// `game::un_actions::refresh_menu` after every action.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub menu: Vec<UnTellKind>,
    /// Gotcha windows opened by the most recent [`UnTell`]. Cleared by the next
    /// ordinary action, so a window never outlives the moment it was offered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gotcha_offers: Vec<GotchaOffer>,
    /// Gotcha offers already matched by a gesture made while not at a priority
    /// boundary, waiting to be announced when priority is next reached. One per
    /// (player, source, returned card), so the list is bounded by the cards in play.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_offers: Vec<GotchaOffer>,
    /// Next unique identifier minted for a [`GotchaOffer`].
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub next_offer_id: u64,
}

impl UnActionState {
    pub fn mint_offer_id(&mut self) -> u64 {
        self.next_offer_id += 1;
        self.next_offer_id
    }

    pub fn is_enabled(&self, player: PlayerId) -> bool {
        self.enabled_players.contains(&player)
    }

    pub fn set_enabled(&mut self, player: PlayerId, enabled: bool) {
        self.enabled_players.retain(|p| *p != player);
        if enabled {
            self.enabled_players.push(player);
        }
    }

    pub fn is_default(&self) -> bool {
        self.enabled_players.is_empty()
            && self.gotcha_offers.is_empty()
            && self.menu.is_empty()
            && self.pending_offers.is_empty()
            && self.next_offer_id == 0
    }
}

/// The payload of `GameAction::UnAction`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum UnAction {
    /// Opt the acting player in or out of Un-card actions (a preference).
    SetEnabled { enabled: bool },
    /// Perform a gesture from the Un-actions menu.
    PerformTell { tell: UnTell },
    /// Call "Gotcha!" on an open offer.
    SayGotcha {
        source_id: ObjectId,
        returns: ObjectId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_set_toggles_idempotently() {
        let mut state = UnActionState::default();
        state.set_enabled(PlayerId(1), true);
        state.set_enabled(PlayerId(1), true);
        assert_eq!(state.enabled_players, vec![PlayerId(1)]);
        state.set_enabled(PlayerId(1), false);
        assert!(state.is_default());
    }
}
