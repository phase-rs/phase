use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::types::player::PlayerId;

/// CR 100.4 / CR 100.4a: Per-format sideboard rules.
///
/// - `Forbidden`: the format does not have a sideboard at all (Commander, Brawl,
///   Historic Brawl). Semantically distinct from `Limited(0)` — those formats
///   don't "have" a zero-size sideboard, they have no sideboard concept.
/// - `Limited(n)`: constructed formats cap the sideboard at `n` cards.
///   CR 100.4a sets this at 15 for standard constructed play.
/// - `Unlimited`: casual multiplayer variants (Free-for-All, Two-Headed Giant)
///   impose no size constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum SideboardPolicy {
    Forbidden,
    Limited(u32),
    Unlimited,
}

impl SideboardPolicy {
    /// CR 100.4 / CR 100.4a / CR 903.5e: whether `self` can never admit a
    /// larger sideboard than `ceiling` would. `Forbidden` (no sideboard at
    /// all) permits no more than anything; `Unlimited` permits no more than
    /// `Unlimited` only; `Limited(n)` permits no more than any equal-or-looser
    /// ceiling. A pure permissiveness comparison — never use it to decide
    /// "which format is bigger" in any other sense.
    ///
    /// The single authority `built_in_axes_no_looser_than_rules` uses to
    /// reject a built-in format's payload from declaring a sideboard
    /// allowance looser than `GameFormat::sideboard_policy()` actually
    /// permits. `Forbidden` is the bottom element, which is exactly
    /// `default_sideboard_policy_fallback()` — so a payload serialized
    /// before that field existed is admitted rather than hard-rejected.
    pub fn permits_no_more_than(self, ceiling: Self) -> bool {
        match (self, ceiling) {
            (SideboardPolicy::Forbidden, _) => true,
            (_, SideboardPolicy::Unlimited) => true,
            (SideboardPolicy::Unlimited, _) => false,
            (SideboardPolicy::Limited(_), SideboardPolicy::Forbidden) => false,
            (SideboardPolicy::Limited(n), SideboardPolicy::Limited(m)) => n <= m,
        }
    }
}

/// A deck-construction copy ceiling for one card name: either unbounded or
/// capped at `n`. Used at both levels of the rule — the format's default
/// (see [`GameFormat::default_deck_copy_limit`]) and a card's printed override.
///
/// CR 100.2a sets the default constructed limit to four of any card with a
/// particular English name (basic lands excepted). A handful of cards print an
/// explicit deck-construction override in their rules text:
///
/// - `Unlimited`: "A deck can have any number of cards named ~." (Relentless
///   Rats, Shadowborn Apostle, etc.) — no upper bound on copies.
/// - `UpTo(n)`: "A deck can have up to <n> cards named ~." (Seven Dwarves → 7,
///   Nazgûl → 9) and the Commander/companion singleton override "Your deck can
///   have only one copy of this card" (Vazal, the Compleat → `UpTo(1)`).
///
/// CR 903.5b's Commander singleton rule exempts basic lands; an `UpTo(n>1)`
/// override likewise raises the cap above the format default for that card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DeckCopyLimit {
    Unlimited,
    UpTo(u32),
}

impl DeckCopyLimit {
    /// CR 100.2a / CR 100.2b / CR 903.5b: whether `self` can never admit more
    /// copies of a card than `ceiling` would. `Unlimited` permits no more
    /// copies than `Unlimited` only; any `UpTo(n)` permits no more than any
    /// equal-or-looser ceiling. This is a pure permissiveness comparison —
    /// never use it to decide "which format is bigger" in any other sense.
    ///
    /// The single authority `FormatConfig`'s `Deserialize` impl uses to
    /// reject a built-in format's payload from declaring a copy ceiling
    /// looser than `GameFormat::default_deck_copy_limit()` actually allows.
    pub fn permits_no_more_than(self, ceiling: Self) -> bool {
        match (self, ceiling) {
            (DeckCopyLimit::Unlimited, DeckCopyLimit::Unlimited) => true,
            (DeckCopyLimit::UpTo(_), DeckCopyLimit::Unlimited) => true,
            (DeckCopyLimit::Unlimited, DeckCopyLimit::UpTo(_)) => false,
            (DeckCopyLimit::UpTo(n), DeckCopyLimit::UpTo(m)) => n <= m,
        }
    }
}

/// A format's deck-size legality rule: either a floor with no ceiling, or an
/// exact count that is simultaneously the minimum and the maximum.
///
/// - `Minimum(n)`: CR 100.5 — "If a deck must contain at least a certain number
///   of cards, that number is referred to as a minimum deck size. There is no
///   maximum deck size for non-Commander decks." Covers CR 100.2a's 60-card
///   constructed floor and CR 100.2b's 40-card limited floor.
/// - `Exactly(n)`: CR 903.5a — "the minimum deck size and the maximum deck size
///   are both 100" — and the Brawl/Tiny Leaders/Oathbreaker variants that
///   inherit an exact count.
///
/// CR 903.13f(1) is why this axis is typed rather than inferred: Commander
/// Draft is a command-zone format whose deck "must contain at least 60 cards.
/// There is no maximum deck size", so a format's command zone does not predict
/// its deck-size rule. Archenemy (command zone, minimum) and Momir (command
/// zone, exact) already disagreed under the old convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DeckSizeRule {
    Minimum(u16),
    Exactly(u16),
}

impl DeckSizeRule {
    /// CR 100.5 / CR 903.5a: whether `count` satisfies this rule. The single
    /// authority for deck-size legality — callers must not re-derive it with
    /// `<` or `!=` against `min_cards`.
    pub fn accepts(self, count: usize) -> bool {
        match self {
            DeckSizeRule::Minimum(min) => count >= usize::from(min),
            DeckSizeRule::Exactly(exact) => count == usize::from(exact),
        }
    }

    /// The smallest legal deck under this rule. Both variants carry a floor;
    /// `Exactly(n)`'s floor is `n`.
    pub fn min_cards(self) -> u16 {
        match self {
            DeckSizeRule::Minimum(min) => min,
            DeckSizeRule::Exactly(exact) => exact,
        }
    }

    /// Human-readable requirement fragment for validation messages, e.g.
    /// "at least 60" or "exactly 100". Keeps the message honest under
    /// `Minimum`, where the old hardcoded "exactly" would have been false.
    pub fn requirement_phrase(self) -> String {
        match self {
            DeckSizeRule::Minimum(min) => format!("at least {min}"),
            DeckSizeRule::Exactly(exact) => format!("exactly {exact}"),
        }
    }
}

/// Configuration for the limited range of influence option.
///
/// The engine does not implement limited-range rules yet. This type preserves
/// the full per-seat configuration shape so external boundaries can reject it
/// explicitly until the corresponding game-rule support exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeOfInfluenceConfig {
    /// The number of seats away that each player can influence by default.
    /// Zero is valid and means a player can influence only themself.
    pub default_range: u8,
    /// Per-seat exceptions to [`Self::default_range`].
    #[serde(default)]
    pub player_overrides: BTreeMap<PlayerId, u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deck_copy_limit_permits_no_more_than_is_a_sound_permissiveness_order() {
        use DeckCopyLimit::*;
        assert!(Unlimited.permits_no_more_than(Unlimited));
        assert!(!Unlimited.permits_no_more_than(UpTo(4)));
        assert!(UpTo(4).permits_no_more_than(Unlimited));
        assert!(UpTo(1).permits_no_more_than(UpTo(4)));
        assert!(UpTo(4).permits_no_more_than(UpTo(4)));
        assert!(!UpTo(5).permits_no_more_than(UpTo(4)));
    }

    #[test]
    fn sideboard_policy_permits_no_more_than_is_a_sound_permissiveness_order() {
        use SideboardPolicy::*;
        assert!(Forbidden.permits_no_more_than(Forbidden));
        assert!(Forbidden.permits_no_more_than(Limited(15)));
        assert!(Forbidden.permits_no_more_than(Unlimited));
        assert!(!Limited(15).permits_no_more_than(Forbidden));
        assert!(Limited(1).permits_no_more_than(Limited(15)));
        assert!(Limited(15).permits_no_more_than(Limited(15)));
        assert!(!Limited(16).permits_no_more_than(Limited(15)));
        assert!(Limited(15).permits_no_more_than(Unlimited));
        assert!(Unlimited.permits_no_more_than(Unlimited));
        assert!(!Unlimited.permits_no_more_than(Limited(15)));
        assert!(!Unlimited.permits_no_more_than(Forbidden));
    }

    #[test]
    fn sideboard_policy_serializes_as_tagged_union() {
        // Unit variants emit {"type": "..."} with no "data" field — the
        // frontend consumer must switch on `.type`, never destructure `.data`
        // unconditionally.
        let forbidden = serde_json::to_string(&SideboardPolicy::Forbidden).unwrap();
        assert_eq!(forbidden, r#"{"type":"Forbidden"}"#);

        let unlimited = serde_json::to_string(&SideboardPolicy::Unlimited).unwrap();
        assert_eq!(unlimited, r#"{"type":"Unlimited"}"#);

        // Tuple variant carries the cap in `data`.
        let limited = serde_json::to_string(&SideboardPolicy::Limited(15)).unwrap();
        assert_eq!(limited, r#"{"type":"Limited","data":15}"#);
    }

    #[test]
    fn deck_copy_limit_serializes_as_tagged_union() {
        // Unit variant emits {"type": "..."} with no "data" field; the frontend
        // must switch on `.type`, never destructure `.data` unconditionally.
        let unlimited = serde_json::to_string(&DeckCopyLimit::Unlimited).unwrap();
        assert_eq!(unlimited, r#"{"type":"Unlimited"}"#);

        // Tuple variant carries the cap in `data`.
        let up_to = serde_json::to_string(&DeckCopyLimit::UpTo(7)).unwrap();
        assert_eq!(up_to, r#"{"type":"UpTo","data":7}"#);

        // Round-trips both directions.
        let parsed: DeckCopyLimit = serde_json::from_str(r#"{"type":"Unlimited"}"#).unwrap();
        assert_eq!(parsed, DeckCopyLimit::Unlimited);
        let parsed: DeckCopyLimit = serde_json::from_str(r#"{"type":"UpTo","data":9}"#).unwrap();
        assert_eq!(parsed, DeckCopyLimit::UpTo(9));
    }

    #[test]
    fn deck_size_rule_serializes_as_tagged_union() {
        // Both variants carry their count in `data`; the frontend must switch
        // on `.type`, never assume a minimum. Mirrored by hand in
        // client/src/adapter/types.ts and by both adapter-contract fixtures.
        let minimum = serde_json::to_string(&DeckSizeRule::Minimum(60)).unwrap();
        assert_eq!(minimum, r#"{"type":"Minimum","data":60}"#);

        let exactly = serde_json::to_string(&DeckSizeRule::Exactly(100)).unwrap();
        assert_eq!(exactly, r#"{"type":"Exactly","data":100}"#);

        // Round-trips both directions.
        let parsed: DeckSizeRule = serde_json::from_str(r#"{"type":"Minimum","data":60}"#).unwrap();
        assert_eq!(parsed, DeckSizeRule::Minimum(60));
        let parsed: DeckSizeRule =
            serde_json::from_str(r#"{"type":"Exactly","data":100}"#).unwrap();
        assert_eq!(parsed, DeckSizeRule::Exactly(100));
    }

    #[test]
    fn range_of_influence_config_round_trips_with_player_overrides() {
        let config = RangeOfInfluenceConfig {
            default_range: 0,
            player_overrides: BTreeMap::from([(PlayerId(1), 1), (PlayerId(3), 2)]),
        };

        let json = serde_json::to_value(&config).expect("range config serializes");
        assert_eq!(json["default_range"], 0);
        assert_eq!(json["player_overrides"]["1"], 1);
        assert_eq!(json["player_overrides"]["3"], 2);
        assert_eq!(
            serde_json::from_value::<RangeOfInfluenceConfig>(json)
                .expect("range config deserializes"),
            config
        );
    }

    #[test]
    fn range_of_influence_config_defaults_missing_overrides_to_empty() {
        let config: RangeOfInfluenceConfig =
            serde_json::from_str(r#"{"default_range":0}"#).expect("range config deserializes");

        assert_eq!(config.default_range, 0);
        assert!(config.player_overrides.is_empty());
    }
}
