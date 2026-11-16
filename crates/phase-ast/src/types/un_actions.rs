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

use nom::character::complete::digit1;
use nom::combinator::all_consuming;
use nom::Parser;
use serde::{Deserialize, Serialize};

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

/// Magnitude words the shared number grammar (`parse_number`) does not cover.
const EXTRA_NUMBER_WORDS: &[&str] = &["zero", "hundred", "thousand", "million", "billion"];

/// Irregular plural forms the regular suffix rules don't produce, keyed by the
/// watched (singular) word. "life" -> "lives" (not "lifes"), "child" ->
/// "children", etc. Gotcha conditions watch single words, so the spoken form
/// must be the listed plural exactly.
const IRREGULAR_PLURALS: &[(&str, &str)] = &[
    ("life", "lives"),
    ("quiz", "quizzes"),
    ("child", "children"),
    ("person", "people"),
    ("mouse", "mice"),
    ("goose", "geese"),
    ("foot", "feet"),
    ("tooth", "teeth"),
    ("ox", "oxen"),
    ("man", "men"),
    ("woman", "women"),
    ("wolf", "wolves"),
    ("thief", "thieves"),
    ("leaf", "leaves"),
    ("elf", "elves"),
    ("knife", "knives"),
    ("half", "halves"),
    ("calf", "calves"),
    ("shelf", "shelves"),
    ("loaf", "loaves"),
    ("sheaf", "sheaves"),
    // /k/-sounding "ch" words take a plain "+s".
    ("monarch", "monarchs"),
    ("stomach", "stomachs"),
    ("patriarch", "patriarchs"),
    ("matriarch", "matriarchs"),
    ("epoch", "epochs"),
    ("eunuch", "eunuchs"),
    // Words ending in "o" with standard "-oes" plural.
    ("hero", "heroes"),
    ("echo", "echoes"),
    ("torpedo", "torpedoes"),
    ("veto", "vetoes"),
    ("embargo", "embargoes"),
    ("mosquito", "mosquitoes"),
    // Words with two accepted spellings: both "-oes" and plain "+s" are standard.
    ("volcano", "volcanoes"),
    ("volcano", "volcanos"),
    ("tornado", "tornadoes"),
    ("tornado", "tornados"),
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
    // Irregular plurals ("life" -> "lives", "child" -> "children", ...).
    // When the watched word is irregular, only its listed plural(s) match —
    // "lifes" is not the plural of "life" and must not fall through to the
    // regular "+s" rule. Some words have two accepted plurals (e.g. "volcano"
    // -> "volcanoes" and "volcanos"); all listed forms are accepted.
    if IRREGULAR_PLURALS
        .iter()
        .any(|(singular, plural)| *singular == w && *plural == s)
    {
        return true;
    }
    // If the watched word has irregular entries but the spoken form doesn't match
    // any of them, don't fall through to the regular rules — the irregular branch
    // alone decides for that word.
    if IRREGULAR_PLURALS.iter().any(|(singular, _)| *singular == w) {
        return false;
    }
    // Once a special-plural branch applies to the watched word, it alone decides:
    // "sanitys" is not the plural of "sanity" and "boxs" is not the plural of "box".
    // 1. consonant+y -> ies (e.g. "cherry" -> "cherries"; vowel+y like "guy" takes "+s")
    if let Some(base) = w.strip_suffix('y') {
        if !base.is_empty() && !base.ends_with(['a', 'e', 'i', 'o', 'u']) {
            return s.strip_prefix(base).is_some_and(|tail| tail == "ies");
        }
    }
    // 2. +es for words ending in s, x, z, ch, sh (e.g. "box" -> "boxes", "match" -> "matches").
    // /k/ "ch" words ("monarch", "stomach") are listed in IRREGULAR_PLURALS.
    if w.ends_with(['s', 'x', 'z']) || w.ends_with("ch") || w.ends_with("sh") {
        return s.strip_prefix(w.as_str()).is_some_and(|tail| tail == "es");
    }
    // 3. +s for regular words (e.g. "guy" -> "guys", "creature" -> "creatures")
    s.strip_prefix(w.as_str()).is_some_and(|tail| tail == "s")
}

/// Whether a single spoken word names a number. A digit run of any length counts
/// (a spoken number has no `u32` bound); otherwise delegates to the shared number
/// grammar ("seven", hyphenated "twenty-one"), rejecting the articles "a"/"an" it
/// also reads as one.
fn is_number_word(word: &str) -> bool {
    if EXTRA_NUMBER_WORDS.contains(&word) || is_digit_run(word) {
        return true;
    }
    if matches!(word, "a" | "an") {
        return false;
    }
    super::text_grammar::parse_number(word).is_ok_and(|(rest, _)| rest.is_empty())
}

/// Whether `word` is nothing but ASCII digits.
fn is_digit_run(word: &str) -> bool {
    all_consuming(digit1::<_, nom::error::Error<&str>>)
        .parse(word)
        .is_ok()
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
    fn word_matches_handles_irregular_plurals() {
        // Irregular plurals match; non-plural lookalikes do not.
        assert!(word_matches("life", "lives"));
        assert!(!word_matches("life", "lifes"));
        assert!(word_matches("child", "children"));
        assert!(!word_matches("child", "childs"));
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
        // A digit run wider than `u32` is still a number.
        assert!(cond.matches_without_zone_context(&said("4294967296")));
        assert!(cond.matches_without_zone_context(&said("99999999999999999999999")));
        assert!(!cond.matches_without_zone_context(&said("99999999999a")));
        assert!(cond.matches_without_zone_context(&said("I have Seven cats")));
        assert!(!cond.matches_without_zone_context(&said("none of them")));
        assert!(cond.matches_without_zone_context(&said("twenty-one")));
        assert!(cond.matches_without_zone_context(&said("Forty-Two!")));
        assert!(cond.matches_without_zone_context(&said("a thousand")));
        assert!(!cond.matches_without_zone_context(&said("a")));
        assert!(!cond.matches_without_zone_context(&said("an apple")));
        assert!(!cond.matches_without_zone_context(&said("twenty-apple")));
    }

    #[test]
    fn physical_tells_match_only_their_own_gesture() {
        let cond = GotchaCondition::Physical(PhysicalTell::TouchFace);
        assert!(cond.matches_without_zone_context(&UnTell::Physical(PhysicalTell::TouchFace)));
        assert!(!cond.matches_without_zone_context(&UnTell::Physical(PhysicalTell::TouchTable)));
    }
}
