use serde::{Deserialize, Serialize};

/// CR 400.1: The seven game zones — library, hand, battlefield, graveyard, stack, exile, and command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Zone {
    /// CR 401: The library — a player's draw pile, face-down, order matters.
    Library,
    Hand,
    /// CR 403: The battlefield — where permanents exist.
    Battlefield,
    Graveyard,
    Stack,
    /// CR 406: The exile zone — a holding area for removed objects.
    Exile,
    /// CR 408: The command zone — reserved for emblems, commanders, dungeons, and other specialized objects.
    Command,
}

impl Zone {
    /// CR 400.2: the battlefield, graveyard, stack, exile, and command zones are
    /// public; the library and hand are hidden even when their cards are
    /// momentarily revealed.
    ///
    /// The distinction is load-bearing for CR 608.2h: an effect that needs
    /// information from a specific object reads that object's CURRENT
    /// characteristics while it is in the public zone it was expected to be in,
    /// and falls back to last known information once it is not.
    pub fn is_public(self) -> bool {
        match self {
            Zone::Battlefield | Zone::Graveyard | Zone::Stack | Zone::Exile | Zone::Command => true,
            Zone::Library | Zone::Hand => false,
        }
    }

    /// Whether moving an object from this zone into a library is a *pure
    /// relocation* — a CR 400.7 zone change that creates a new object, and
    /// nothing more.
    ///
    /// CR 110.1: a permanent is a card or token *on the battlefield*, and it
    /// stops being a permanent as it moves to another zone — so a battlefield
    /// departure is a leaves-the-battlefield event (CR 603.6c), not a
    /// relocation: it fires LTB triggers, severs the attachment graph, and
    /// purges the trigger index.
    /// CR 112.1: a spell is a card *on the stack*, so removing a stack member
    /// would silently destroy a spell mid-resolution.
    /// CR 408 / CR 903.9b: a command-zone object (emblem, commander, dungeon)
    /// is governed by its own zone-change replacements, so it is deliberately
    /// excluded rather than merely unlisted.
    ///
    /// In hand, library, graveyard and exile the object is a card and the move
    /// is a plain relocation, which
    /// `zone_pipeline::move_objects_simultaneously_then` handles uniformly.
    ///
    /// Exhaustive rather than a `matches!` so that adding a `Zone` variant is a
    /// compile error forcing an explicit relocation/not-relocation decision —
    /// silently defaulting a new zone either way is a rules question, not a
    /// formatting one.
    pub fn is_library_relocation_origin(self) -> bool {
        match self {
            Zone::Hand | Zone::Library | Zone::Graveyard | Zone::Exile => true,
            Zone::Battlefield | Zone::Stack | Zone::Command => false,
        }
    }
}

/// CR 118.9a + CR 601.2b + CR 601.2h: Source zone for an `AbilityCost::Exile`
/// payment. Only `Hand` (pitch spells, CR 118.9a) and `Graveyard` (escape,
/// CR 702.138a) are valid; any other zone is rejected at cost-resolution time
/// and falls through to the non-interactive path. Making the invariant a type
/// removes the load-bearing `unreachable!` panics that previously guarded
/// downstream matches on the broader `Zone` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExileCostSourceZone {
    Hand,
    Graveyard,
}

impl ExileCostSourceZone {
    pub fn as_zone(self) -> Zone {
        match self {
            Self::Hand => Zone::Hand,
            Self::Graveyard => Zone::Graveyard,
        }
    }

    pub fn try_from_zone(zone: Zone) -> Option<Self> {
        match zone {
            Zone::Hand => Some(Self::Hand),
            Zone::Graveyard => Some(Self::Graveyard),
            _ => None,
        }
    }
}

/// CR 608.2c: whether a battlefield entry is the referent a following
/// demonstrative anaphor binds to ("manifest dread, then attach this Equipment
/// to **that creature**").
///
/// Carried on the entry request rather than derived at the delivery, because
/// the question is about the EFFECT that asked for the entry, not about the
/// permanent that arrives. Two effects can produce an identical face-down
/// permanent and only one of them be the producer the sentence refers back to,
/// so no property of the entrant — its zone, its face-down cause, its
/// characteristics — can answer it.
///
/// [`Silent`] is the default on purpose: an entry that has not been marked is
/// not a producer, so a delivery path added later cannot silently start
/// overwriting the game-lifetime `last_created_token_ids` ledger.
///
/// The engine's marked call sites and the parser's admission authority
/// (`oracle_effect::lower::publishes_chain_created_referent`) are the two halves
/// of one contract and have to be changed together.
///
/// [`Silent`]: ChainReferentIntent::Silent
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ChainReferentIntent {
    /// This entry names nothing. Every path that has not opted in.
    #[default]
    Silent,
    /// CR 608.2c: on successful delivery, this entrant becomes the chain's
    /// most-recent created referent.
    Publishes,
}

impl ChainReferentIntent {
    /// Whether a delivery carrying this intent publishes the referent.
    pub fn publishes(self) -> bool {
        matches!(self, Self::Publishes)
    }

    /// Whether the intent is the default, for wire-shape preservation.
    pub fn is_silent(&self) -> bool {
        matches!(self, Self::Silent)
    }
}

/// CR 614.1 / CR 110.5b: Whether an object enters the battlefield tapped.
///
/// Canonical type for all enter-tapped fields across ability AST, game-state
/// carriers, and the replacement pipeline. `Unspecified` means no replacement
/// or effect has set an explicit tap state yet; `Tapped` / `Untapped` are
/// authoritative once chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum EtbTapState {
    #[default]
    Unspecified,
    Tapped,
    Untapped,
}

impl EtbTapState {
    pub fn from_legacy_bool(tapped: bool) -> Self {
        if tapped {
            Self::Tapped
        } else {
            Self::Unspecified
        }
    }

    pub fn from_seeded_tapped(tapped: bool) -> Self {
        Self::from_legacy_bool(tapped)
    }

    pub fn is_unspecified(&self) -> bool {
        matches!(self, Self::Unspecified)
    }

    pub fn is_tapped(self) -> bool {
        matches!(self, Self::Tapped)
    }

    pub fn is_untapped(self) -> bool {
        matches!(self, Self::Untapped)
    }

    /// Resolve to a concrete tapped state. `fallback` is used only when no
    /// replacement has set an explicit tap-state (`Unspecified`). For
    /// `ZoneChange` events pass `false`; for `CreateToken` pass
    /// `spec.tapped` (the token spec's authored default).
    pub fn resolve(self, fallback: bool) -> bool {
        match self {
            Self::Unspecified => fallback,
            Self::Tapped => true,
            Self::Untapped => false,
        }
    }
}

/// CR 113.6 + CR 601.2f: Zones where a self-spell cost-reduction static must
/// function during cast-time cost determination (hand, library cast, commander,
/// graveyard cast, exile cast, and the stack step).
pub fn self_spell_cost_mod_active_zones() -> Vec<Zone> {
    vec![
        Zone::Hand,
        Zone::Stack,
        Zone::Command,
        Zone::Graveyard,
        Zone::Exile,
        Zone::Library,
    ]
}

/// Serde adapter for on-disk `enter_tapped: bool` fields in card-data.json.
pub mod etb_tap_bool_compat {
    use super::EtbTapState;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(state: &EtbTapState, serializer: S) -> Result<S::Ok, S::Error> {
        debug_assert!(
            !state.is_untapped(),
            "EtbTapState::Untapped is a runtime-only replacement override and must not be persisted through the legacy bool adapter"
        );
        serializer.serialize_bool(state.is_tapped())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<EtbTapState, D::Error> {
        let tapped = bool::deserialize(deserializer)?;
        Ok(EtbTapState::from_legacy_bool(tapped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_has_all_seven_mtg_zones() {
        let zones = [
            Zone::Library,
            Zone::Hand,
            Zone::Battlefield,
            Zone::Graveyard,
            Zone::Stack,
            Zone::Exile,
            Zone::Command,
        ];
        assert_eq!(zones.len(), 7);
    }

    #[test]
    fn zone_serializes_as_string() {
        let zone = Zone::Battlefield;
        let json = serde_json::to_value(zone).unwrap();
        assert_eq!(json, "Battlefield");
    }

    #[test]
    fn zone_roundtrips() {
        let zone = Zone::Graveyard;
        let serialized = serde_json::to_string(&zone).unwrap();
        let deserialized: Zone = serde_json::from_str(&serialized).unwrap();
        assert_eq!(zone, deserialized);
    }
}
