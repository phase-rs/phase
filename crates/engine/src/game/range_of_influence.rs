//! CR 801: Limited range of influence.
//!
//! An optional multiplayer rule capping how far, measured in player seats, a
//! player can affect the game. It is opt-in per format through
//! [`FormatConfig::range_of_influence`]; `None` means unlimited, which is every
//! format shipped today, so games that do not use the option pay nothing beyond
//! one `Option` check.
//!
//! # Why the range is snapshotted rather than computed live
//!
//! CR 801.2c: "The particular players within each player's range of influence
//! are determined **as each turn begins**." The rule's own example makes the
//! consequence explicit — if a player leaves the game, the seats that were
//! separated by them come into range *at the start of the next turn*, not the
//! instant they left.
//!
//! Computing range live would therefore be wrong in a way that is invisible in
//! a two-player game and silently changes legality mid-turn in the multiplayer
//! games the option exists for: a spell that was legal when it was cast could
//! become illegal on resolution because an intervening player was eliminated in
//! between. So the snapshot is the authority, refreshed exactly once per turn
//! by [`refresh_for_turn`], and every CR 801 consumer reads it rather than
//! recomputing.
//!
//! # Scope of this module
//!
//! This is the CR 801.2 foundation plus the CR 801.4 targeting consumer. The
//! remaining consumers (CR 801.3 attacking, CR 801.5 choices, CR 801.6
//! activation, CR 801.7 triggering) and the CR 809 Emperor variant are
//! deliberately not implemented here — see the module's issue for the split. No
//! shipped format sets `range_of_influence`, so nothing changes for existing
//! games until a format opts in.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 801.2c: the players within each player's range of influence, fixed at the
/// start of the current turn.
///
/// Absent entirely when the game does not use the option, so `None` is the
/// "unlimited range" case rather than a degenerate map naming every player.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeOfInfluenceSnapshot {
    /// Keyed by the observing player; the value is every player inside that
    /// player's range, including themselves (CR 801.2b).
    within: BTreeMap<PlayerId, BTreeSet<PlayerId>>,
}

impl RangeOfInfluenceSnapshot {
    /// CR 801.2 + CR 801.2b: whether `other` is inside `observer`'s range.
    ///
    /// A player absent from the snapshot (joined after it was taken, or already
    /// eliminated when it was) is treated as out of range for everyone but
    /// themselves — CR 801.2b holds unconditionally.
    pub fn contains(&self, observer: PlayerId, other: PlayerId) -> bool {
        if observer == other {
            return true;
        }
        self.within
            .get(&observer)
            .is_some_and(|players| players.contains(&other))
    }
}

/// CR 801.2 + CR 801.2c: compute the range-of-influence snapshot for the CURRENT
/// seating, or `None` when the game does not use the option.
///
/// # Uniform range is the implemented subset
///
/// CR 801.2a notes "different players may have different ranges of influence."
/// This reads one match-wide range from `FormatConfig::range_of_influence`,
/// which covers CR 809 Emperor (all players share range 1) and every
/// configuration the format layer can express today. The stored snapshot is
/// deliberately *observer-keyed* rather than a single shared set, so
/// per-player ranges are a config-layer extension (a per-seat range map feeding
/// this loop) rather than a redesign of the snapshot or its consumers.
///
/// Distance is measured over the seats still in the game: CR 801.2c's example
/// has a departed player's neighbours become adjacent, so an eliminated seat
/// must not keep padding the distance between the players it used to separate.
/// Measuring over living seats is what makes that example come out right.
pub fn snapshot(state: &GameState) -> Option<RangeOfInfluenceSnapshot> {
    let range = state.format_config.range_of_influence?;

    // CR 801.2c: seats still in the game, in seat order. `seat_order` is the
    // authority on seating; `eliminated_players` removes the departed.
    let living: Vec<PlayerId> = state
        .seat_order
        .iter()
        .copied()
        .filter(|player| !state.eliminated_players.contains(player))
        .collect();

    let seats = living.len();
    let mut within: BTreeMap<PlayerId, BTreeSet<PlayerId>> = BTreeMap::new();
    for (index, &observer) in living.iter().enumerate() {
        let mut in_range = BTreeSet::new();
        for (other_index, &other) in living.iter().enumerate() {
            if seat_distance(index, other_index, seats) <= u32::from(range) {
                in_range.insert(other);
            }
        }
        // CR 801.2b: a player is always within their own range, even if the
        // configured range is 0.
        in_range.insert(observer);
        within.insert(observer, in_range);
    }

    Some(RangeOfInfluenceSnapshot { within })
}

/// CR 801.2: distance between two seats at a table, measured the short way
/// around. Players sit in a circle, so the two seats at the ends of the seat
/// order are adjacent.
fn seat_distance(a: usize, b: usize, seats: usize) -> u32 {
    if seats == 0 {
        return 0;
    }
    let forward = (a + seats - b) % seats;
    let backward = (b + seats - a) % seats;
    forward.min(backward) as u32
}

/// CR 801.2c: re-determine every player's range because a turn is beginning.
///
/// Called from the single turn-start authority so the snapshot advances in
/// lockstep with turns — the rule's example (a departure taking effect at the
/// next turn start) falls out of calling this here and nowhere else.
pub fn refresh_for_turn(state: &mut GameState) {
    state.range_of_influence = snapshot(state);
}

/// CR 801.2 + CR 801.2b: whether `other` is within `observer`'s range.
///
/// `true` for every pair when the game does not use the option, so callers can
/// gate unconditionally without first asking whether the option is on.
pub fn player_in_range(state: &GameState, observer: PlayerId, other: PlayerId) -> bool {
    match &state.range_of_influence {
        Some(snapshot) => snapshot.contains(observer, other),
        None => true,
    }
}

/// CR 801.2d: an object is within a player's range if the player whose range it
/// belongs to is.
///
/// On the battlefield and the stack an object has a controller, and range
/// follows the controller (CR 801.2d, CR 108.4). In every other zone a card has
/// no controller — only an owner (CR 108.3) — so range follows the owner there;
/// reading the residual `controller` field for a graveyard or hand card would
/// judge range by stale, non-authoritative state.
///
/// An object that no longer exists is reported out of range rather than in it,
/// so a stale id can never widen what a player may reach.
pub fn object_in_range(state: &GameState, observer: PlayerId, object: ObjectId) -> bool {
    if state.range_of_influence.is_none() {
        return true;
    }
    state.objects.get(&object).is_some_and(|object| {
        let seat = match object.zone {
            Zone::Battlefield | Zone::Stack => object.controller,
            _ => object.owner,
        };
        player_in_range(state, observer, seat)
    })
}
