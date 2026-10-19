//! Optional "Un-card" actions — the spoken and physical gestures that silver-border
//! cards (Unglued, Unhinged, Unstable, Unfinity, ...) ask players to perform.
//!
//! The Comprehensive Rules do not cover these cards, so nothing here is a CR
//! rule; the module models the *card text* instead. Two card families are
//! supported:
//!
//! * **Say-it costs** — "unless that player says \"Thank you, sir. May I have
//!   another?\"" (Rod of Spanking), "you may say \"INCARCERATE!\" ... If you do"
//!   (Impounding Lot-Bot), "you may say \"I'm here.\" If you do" (Infernius
//!   Spawnington III). These are [`AbilityCost::SayPhrase`](super::ability::AbilityCost)
//!   payments, routed through the ordinary unless-payment / optional-payment
//!   pipelines.
//! * **Gotcha** — "If an opponent touches their face, you may say \"Gotcha!\"
//!   When you do, return this card from your graveyard to your hand." A client
//!   cannot observe a player's face, so a player instead *performs* a gesture
//!   from the Un-actions menu ([`UnTell`]); the engine then offers every
//!   opponent holding a matching Gotcha card a short window to call "Gotcha!".
//!
//! Everything is opt-in per player ([`UnActionState::enabled_players`]): a
//! player who has not enabled Un-card actions is never prompted to say anything
//! (the "unless" branch simply happens) and never receives a Gotcha offer.

use serde::{Deserialize, Serialize};

use super::identifiers::ObjectId;
use super::player::PlayerId;

/// A physical gesture a player can perform from the Un-actions menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PhysicalTell {
    /// "touches their face" (Touch and Go).
    TouchFace,
    /// "touches the table with their hand" (Cardpecker).
    TouchTable,
    /// "laughs" (Laughing Hyena).
    Laugh,
    /// "audibly flicks the cards in their hand" (Stop That).
    FlickCards,
}

/// One gesture performed by a player: a physical gesture, or something said aloud.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum UnTell {
    Physical(PhysicalTell),
    /// A word, number, or phrase the player said aloud.
    Said {
        text: String,
    },
}

impl UnTell {
    /// Past-tense log phrase, subject omitted ("touches their face").
    pub fn log_phrase(&self) -> String {
        match self {
            Self::Physical(PhysicalTell::TouchFace) => "touches their face".to_string(),
            Self::Physical(PhysicalTell::TouchTable) => "touches the table".to_string(),
            Self::Physical(PhysicalTell::Laugh) => "laughs".to_string(),
            Self::Physical(PhysicalTell::FlickCards) => {
                "audibly flicks the cards in their hand".to_string()
            }
            Self::Said { text } => format!("says \"{text}\""),
        }
    }
}

/// The condition a Gotcha ability watches an opponent for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum GotchaCondition {
    /// "touches their face", "laughs", ...
    Physical(PhysicalTell),
    /// "says \"creature\" or \"guy\"" — any one of the listed (lowercase) words.
    SaysAnyOf { words: Vec<String> },
    /// "says a number" — a digit run or a spelled-out number word.
    SaysNumber,
    /// "says a word that's in the name of a card in your graveyard" (Name Dropping).
    SaysWordInGraveyardCardName,
}

/// One entry of the Un-actions menu: a kind of gesture, not a specific one. The
/// engine derives the menu from the Gotcha abilities in play so it never offers
/// a gesture no card in the game responds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum UnTellKind {
    Physical(PhysicalTell),
    /// Say a word (or several) aloud.
    SaysWord,
    /// Say a number aloud.
    SaysNumber,
}

#[inline]
fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// A live opportunity for `player` to call "Gotcha!" for the Gotcha ability on
/// `source_id`, returning `returns` to hand when the reflexive trigger resolves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GotchaOffer {
    pub id: u64,
    pub player: PlayerId,
    /// The object carrying the Gotcha ability.
    pub source_id: ObjectId,
    /// The card the ability returns ("this card" → `source_id`; "that card" → the
    /// graveyard card whose name contained the spoken word).
    pub returns: ObjectId,
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
    /// Spoken or physical gestures that occurred while not at a priority boundary,
    /// waiting to be processed when priority is next reached.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_tells: Vec<(PlayerId, UnTell)>,
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
            && self.pending_tells.is_empty()
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

/// Spelled-out number words recognised by [`GotchaCondition::SaysNumber`].
const NUMBER_WORDS: &[&str] = &[
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "thousand",
    "million",
    "billion",
];

/// Split spoken text into lowercase words. Hyphenated words are preserved as single words.
pub fn spoken_words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
        .map(|w| w.trim_matches(|c| c == '-' || c == '\''))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
}

/// Check if a spoken word matches a watched word: exact match or valid plural forms (+s, +es, y->ies).
pub fn word_matches(watched: &str, spoken: &str) -> bool {
    let w = watched.trim().to_lowercase();
    let s = spoken.trim().to_lowercase();
    if w.is_empty() || s.is_empty() {
        return false;
    }
    if w == s {
        return true;
    }
    // 1. y -> ies (e.g. "cherry" -> "cherries", "ability" -> "abilities"; but not vowel+y like "guy")
    if w.ends_with('y') && w.len() > 1 {
        let prev = w.as_bytes()[w.len() - 2] as char;
        let is_vowel = matches!(prev, 'a' | 'e' | 'i' | 'o' | 'u');
        if !is_vowel {
            let base = &w[..w.len() - 1];
            if s.len() == base.len() + 3 && s.starts_with(base) && s.ends_with("ies") {
                return true;
            }
        }
    }
    // 2. +es for words ending in s, x, z, ch, sh (e.g. "box" -> "boxes", "match" -> "matches")
    if w.ends_with('s')
        || w.ends_with('x')
        || w.ends_with('z')
        || w.ends_with("ch")
        || w.ends_with("sh")
    {
        if s.len() == w.len() + 2 && s.starts_with(&w) && s.ends_with("es") {
            return true;
        }
    }
    // 3. +s for regular words (e.g. "guy" -> "guys", "creature" -> "creatures")
    if !w.ends_with('s') && s.len() == w.len() + 1 && s.starts_with(&w) && s.ends_with('s') {
        return true;
    }
    false
}

fn is_number_word(word: &str) -> bool {
    word.chars().all(|c| c.is_ascii_digit()) || NUMBER_WORDS.contains(&word)
}

impl GotchaCondition {
    /// The menu entry that lets a player trigger this condition.
    pub fn tell_kind(&self) -> UnTellKind {
        match self {
            Self::Physical(tell) => UnTellKind::Physical(*tell),
            Self::SaysAnyOf { .. } | Self::SaysWordInGraveyardCardName => UnTellKind::SaysWord,
            Self::SaysNumber => UnTellKind::SaysNumber,
        }
    }

    /// Whether `tell` satisfies this condition. `SaysWordInGraveyardCardName` is
    /// resolved by the caller (it needs the graveyard); see
    /// [`GotchaCondition::matches_without_zone_context`] for the zone-free half.
    pub fn matches_without_zone_context(&self, tell: &UnTell) -> bool {
        match (self, tell) {
            (Self::Physical(expected), UnTell::Physical(actual)) => expected == actual,
            (Self::SaysAnyOf { words }, UnTell::Said { text }) => spoken_words(text)
                .any(|spoken| words.iter().any(|watched| word_matches(watched, &spoken))),
            (Self::SaysNumber, UnTell::Said { text }) => {
                spoken_words(text).any(|spoken| is_number_word(&spoken))
            }
            (Self::SaysWordInGraveyardCardName, _) => false,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(text: &str) -> UnTell {
        UnTell::Said {
            text: text.to_string(),
        }
    }

    #[test]
    fn says_any_of_matches_whole_words_case_insensitively() {
        let cond = GotchaCondition::SaysAnyOf {
            words: vec!["creature".into(), "guy".into()],
        };
        assert!(cond.matches_without_zone_context(&said("What a Guy!")));
        assert!(cond.matches_without_zone_context(&said("Those guys are cool")));
        assert!(!cond.matches_without_zone_context(&said("guyed")));
        assert!(!cond.matches_without_zone_context(&UnTell::Physical(PhysicalTell::Laugh)));
    }

    #[test]
    fn word_matches_accepts_plurals_and_rejects_arbitrary_extensions() {
        assert!(word_matches("guy", "guys"));
        assert!(word_matches("creature", "creatures"));
        assert!(word_matches("box", "boxes"));
        assert!(word_matches("cherry", "cherries"));
        assert!(!word_matches("guy", "guyed"));
        assert!(!word_matches("life", "lifeline"));
        assert!(!word_matches("stop", "stopping"));
    }

    #[test]
    fn spoken_words_preserves_hyphenated_words() {
        let words: Vec<String> = spoken_words("This is a touch-and-go scenario - right?").collect();
        assert!(words.contains(&"touch-and-go".to_string()));
        assert!(!words.contains(&"-".to_string()));
    }

    #[test]
    fn says_number_accepts_digits_and_number_words() {
        let cond = GotchaCondition::SaysNumber;
        assert!(cond.matches_without_zone_context(&said("42")));
        assert!(cond.matches_without_zone_context(&said("I have Seven cats")));
        assert!(!cond.matches_without_zone_context(&said("none of them")));
    }

    #[test]
    fn physical_tells_match_only_their_own_gesture() {
        let cond = GotchaCondition::Physical(PhysicalTell::TouchFace);
        assert!(cond.matches_without_zone_context(&UnTell::Physical(PhysicalTell::TouchFace)));
        assert!(!cond.matches_without_zone_context(&UnTell::Physical(PhysicalTell::TouchTable)));
    }

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
