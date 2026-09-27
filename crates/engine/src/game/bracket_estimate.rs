//! Commander bracket estimator. Profiles a Commander deck along four
//! card-derived axes (Game Changers, Mass Land Denial, Extra Turns, Efficient
//! Tutors), folds in the deck owner's two-card-combo declaration, and returns a
//! `BracketEstimate` placing the deck in bracket B2–B4.
//!
//! Pure: no game state, no I/O, no randomness. Same `(deck, db)` →
//! identical `BracketEstimate`.
//!
//! Bracket policy is **not** part of the Comprehensive Rules — it is WotC's
//! Commander Format Panel guidance. No Comprehensive Rules annotations apply.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::database::CardDatabase;
use crate::game::deck_loading::PlayerDeckList;
use crate::types::ability::Comparator;

/// Commander bracket tier. The estimator never returns `Cedh` — that is a
/// meta self-declaration kept on the frontend's existing manual picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::EnumIter)]
#[serde(rename_all = "snake_case")]
pub enum CommanderBracketTier {
    Exhibition, // B1
    Core,       // B2
    Upgraded,   // B3
    Optimized,  // B4
    Cedh,       // B5 (manual-declaration only; estimator never returns this)
}

impl Default for CommanderBracketTier {
    /// Default to `Core` (B2) — the most common casual tier and the
    /// value used by all legacy construction sites that predate the
    /// `bracket_tier` field on `PlayerDeckPool`.
    fn default() -> Self {
        Self::Core
    }
}

impl std::fmt::Display for CommanderBracketTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Exhibition => "Exhibition",
            Self::Core => "Core",
            Self::Upgraded => "Upgraded",
            Self::Optimized => "Optimized",
            Self::Cedh => "Cedh",
        };
        write!(f, "{name}")
    }
}

/// Every label [`CommanderBracketTier::from_label`] accepts.
pub const ACCEPTED_BRACKET_LABELS: &[&str] =
    &["Exhibition", "Core", "Upgraded", "Optimized", "Cedh"];

impl CommanderBracketTier {
    /// Parses a case-insensitive, whitespace-trimmed bracket label.
    pub fn from_label(label: &str) -> Option<Self> {
        match label.trim().to_lowercase().as_str() {
            "exhibition" => Some(Self::Exhibition),
            "core" => Some(Self::Core),
            "upgraded" => Some(Self::Upgraded),
            "optimized" => Some(Self::Optimized),
            "cedh" => Some(Self::Cedh),
            _ => None,
        }
    }

    /// Numeric bracket level (B1..=B5 → 1..=5). Used for ordered
    /// comparisons when composing bracket floors.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Exhibition => 1,
            Self::Core => 2,
            Self::Upgraded => 3,
            Self::Optimized => 4,
            Self::Cedh => 5,
        }
    }
}

/// The tier the AI is allowed to act on.
///
/// Bracket policy is WotC Commander Format Panel guidance, **not** the Comprehensive
/// Rules — no `// CR` annotation applies (see the module header).
///
/// A declaration may raise this value and can never lower it below what the estimator
/// can prove from the deck's own contents. `Cedh` is declaration-only in both
/// directions: [`estimate_bracket`] never returns it, so a `Cedh` declaration always
/// survives and no estimate can manufacture one.
///
/// [`CommanderBracketTier::as_u8`] is the ordering authority. The enum deliberately
/// derives no `Ord`: the order is stated once rather than inherited from declaration
/// order.
pub fn effective_tier(
    declared: CommanderBracketTier,
    estimated: Option<CommanderBracketTier>,
) -> EffectiveBracketTier {
    let tier = match estimated {
        Some(floor) if floor.as_u8() > declared.as_u8() => floor,
        Some(_) | None => declared,
    };
    EffectiveBracketTier(tier)
}

/// A tier that has been through [`effective_tier`]. The inner field is private to this
/// module, so the only ways to obtain one are [`effective_tier`] and [`Default`] — a raw
/// declaration cannot reach AI deck features through the wrong parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectiveBracketTier(CommanderBracketTier);

impl EffectiveBracketTier {
    pub fn tier(self) -> CommanderBracketTier {
        self.0
    }
}

/// `Core`, matching [`CommanderBracketTier::default`] — the "no deck analysed" value
/// AI deck-feature defaults need. This is the one construction path that does not
/// reconcile, and it carries no deck to reconcile against.
impl Default for EffectiveBracketTier {
    fn default() -> Self {
        Self(CommanderBracketTier::default())
    }
}

/// The deck owner's answer to the two-card-infinite-combo barometer.
///
/// Brackets are a self-declaration system and this barometer is a statement about
/// deck-building INTENT ("no *intentional* two-card infinite combos" — Introducing
/// Commander Brackets Beta, Bracket 1 and Bracket 2 Deck Building; archived copy read
/// 2026-09-12), so no card list can answer it and only the deck's owner can.
///
/// Three states, not two: an UNANSWERED barometer is not a declared absence, and
/// collapsing them would let a deck nobody asked about read as a clean one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComboDeclaration {
    /// Nobody has answered. The estimate must say so rather than assume "none".
    Undeclared,
    /// The owner states the deck contains no intentional two-card infinite combo.
    NoneIntended,
    /// The owner states it does. `window` is the follow-up question — the published
    /// lines treat an early line differently from a late one — and stays `None` when
    /// the owner declared the combo but not when it assembles.
    Intended {
        #[serde(default)]
        window: Option<ComboWindow>,
    },
}

impl Default for ComboDeclaration {
    /// `Undeclared` — every payload that predates this field, and every deck nobody
    /// has been asked about, is unanswered, never "clean".
    fn default() -> Self {
        Self::Undeclared
    }
}

/// When a declared intentional two-card infinite combo can assemble.
///
/// The boundary is the published one and is deliberately NOT a turn number in the type:
/// Bracket 3's Experience paragraph words it as combos "that can happen cheaply and in
/// about the first six or so turns of the game" (Introducing Commander Brackets Beta),
/// and the 2025-10-21 update restates it as "you don't expect to win or lose before
/// turn six". Both are prose about expectation, not a threshold the engine measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComboWindow {
    EarlyGame,
    LateGame,
}

/// The bracket floor a combo declaration forces under the two published lines, or
/// `None` when nothing is claimed.
///
/// Bracket policy is not the Comprehensive Rules (see this module's header), so no
/// rules annotation applies. Each encoded line names the copy it was read from, which
/// is the discipline P3's `FloorRule` rows use:
///
/// * Brackets 1 and 2 Deck Building — "No intentional two-card infinite combos"
///   (Introducing Commander Brackets Beta, magic.wizards.com; archived copy read
///   2026-09-12). A declared intentional combo therefore cannot sit in B1 or B2 →
///   floor Upgraded.
/// * Bracket 3 Deck Building — "No intentional early-game two-card infinite combos"
///   (same source; the window is defined in that bracket's Experience paragraph and
///   restated in the 2025-10-21 update). An early line therefore cannot sit in B3 →
///   floor Optimized.
/// * Bracket 4 Deck Building — "There are no restrictions (other than the banned
///   list)", so there is no third row and this function never returns Cedh.
/// * The 2026-02-09 update made no bracket-level change; these lines are current.
pub fn combo_declaration_floor(declaration: ComboDeclaration) -> Option<CommanderBracketTier> {
    match declaration {
        ComboDeclaration::Undeclared | ComboDeclaration::NoneIntended => None,
        ComboDeclaration::Intended {
            window: None | Some(ComboWindow::LateGame),
        } => Some(CommanderBracketTier::Upgraded),
        ComboDeclaration::Intended {
            window: Some(ComboWindow::EarlyGame),
        } => Some(CommanderBracketTier::Optimized),
    }
}

/// A checkpoint the published Commander format page names. The live page names three
/// barometers — "two-card infinite combos, extra turns, mass land denial" — and
/// separately names the Game Changers list "that the brackets reference"; all four are
/// carried here because the panel must state the authority behind each of them.
///
/// Deliberately NOT the same type as `BracketAxis`, and not a variant added to it. An
/// axis is a curated-list COUNT the engine derives from the deck's cards; a barometer is
/// a question the format asks, which may be answered from an axis, from the deck owner,
/// or not at all. The two sets differ at both ends: `EfficientTutors` is still an axis
/// (evidence only) but stopped being a barometer when the 2025-10-21 update removed the
/// tutor restrictions, and `TwoCardCombos` is a barometer with no axis behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Barometer {
    GameChangers,
    ExtraTurns,
    MassLandDenial,
    TwoCardCombos,
}

/// Whose word a barometer's reading rests on. Brackets are a self-declaration system,
/// so "the deck's owner said so" is a first-class answer — and "nobody said" is a third
/// state that must never be rendered as a declared absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarometerAuthority {
    /// Read from the deck's cards against a dated, curated list.
    Engine,
    /// Stated by the deck's owner. The engine cannot verify it.
    DeckOwner,
    /// Nobody answered it.
    Unanswered,
}

/// The two-card-infinite-combo barometer's reading.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComboBarometer {
    pub declaration: ComboDeclaration,
    /// The floor `declaration` forces under the published lines, if any. The engine
    /// computes it (the frontend is a display layer and must not re-derive it) and
    /// `estimate_bracket` has ALREADY folded it into `BracketEstimate::tier`; it is
    /// carried separately so the panel can attribute that part of the tier to the deck
    /// owner rather than to the cards.
    pub floor: Option<CommanderBracketTier>,
}

/// One observable Commander Brackets policy axis.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, strum::EnumIter,
)]
#[serde(rename_all = "snake_case")]
pub enum BracketAxis {
    GameChangers,
    MassLandDenial,
    ExtraTurns,
    EfficientTutors,
}

/// One bracket axis's reading for a deck.
///
/// Bracket policy is WotC Commander Format Panel guidance, not the
/// Comprehensive Rules, so no rules annotation applies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxisReading {
    pub count: u8,
    /// Cards that counted toward this axis, in deck order (commander first).
    pub contributing: Vec<String>,
}

/// Where a floor rule's official line was read from. A `&'static str` struct,
/// not an enum: the engine never dispatches on the value, it only reports it.
/// The non-CR analogue of the descriptive half of a rules annotation.
#[derive(Debug, Clone, Copy)]
pub struct FloorRuleSource {
    /// WotC document title, as published.
    pub document: &'static str,
    /// Publication date, ISO-8601.
    pub published: &'static str,
    pub url: &'static str,
}

/// One official bracket line, encoded. `comparator`/`threshold` say when the
/// line's restriction is exceeded; `floor` is the lowest bracket that still
/// permits the observed count.
///
/// NOT a Comprehensive Rules artifact — Commander Brackets are WotC Commander
/// Format Panel format guidance. See the module header.
#[derive(Debug, Clone, Copy)]
pub struct FloorRule {
    pub axis: BracketAxis,
    pub comparator: Comparator,
    pub threshold: u8,
    pub floor: CommanderBracketTier,
    /// The official sentence this row encodes, quoted verbatim.
    pub official_line: &'static str,
    pub source: FloorRuleSource,
}

const SRC_FORMAT_PAGE: FloorRuleSource = FloorRuleSource {
    document: "MTG Commander Format — Game Changers",
    published: "2026-02-09",
    url: "https://magic.wizards.com/en/formats/commander",
};
const SRC_INTRO: FloorRuleSource = FloorRuleSource {
    document: "Introducing Commander Brackets Beta",
    published: "2025-02-11",
    url: "https://magic.wizards.com/en/news/announcements/introducing-commander-brackets-beta",
};

const FLOOR_RULES: &[FloorRule] = &[
    FloorRule {
        axis: BracketAxis::GameChangers,
        comparator: Comparator::GE,
        threshold: 1,
        floor: CommanderBracketTier::Upgraded,
        official_line: "Bracket 1 and 2 decks exclude Game Changers. \
                        Bracket 3 allows for up to three Game Changers.",
        source: SRC_FORMAT_PAGE,
    },
    FloorRule {
        axis: BracketAxis::GameChangers,
        comparator: Comparator::GE,
        threshold: 4,
        floor: CommanderBracketTier::Optimized,
        official_line: "Bracket 3 allows for up to three Game Changers. \
                        Brackets 4 and 5 allow for unlimited Game Changers.",
        source: SRC_FORMAT_PAGE,
    },
    FloorRule {
        axis: BracketAxis::MassLandDenial,
        comparator: Comparator::GE,
        threshold: 1,
        floor: CommanderBracketTier::Optimized,
        official_line: "you should not expect to see these cards anywhere in Brackets 1-3",
        source: SRC_INTRO,
    },
    // Bracket 1 alone excludes extra-turn cards outright. Brackets 2 and 3 both
    // permit them "in low quantities"; WotC has published no integer for "low
    // quantities" or for "chained in succession", so there is NO second
    // extra-turns row. The count ships as uncalibrated evidence instead of a
    // fabricated threshold. (Intro article, Bracket 2 and Bracket 3 Deck
    // Building lines.)
    FloorRule {
        axis: BracketAxis::ExtraTurns,
        comparator: Comparator::GE,
        threshold: 1,
        floor: CommanderBracketTier::Core,
        official_line: "No intentional two-card infinite combos, mass land denial, \
                        or extra-turn cards.",
        source: SRC_INTRO,
    },
    // RETIRED 2025-10-21: EfficientTutors. "the avenue we'd like to take is to
    // remove the tutor restrictions from Commander Brackets entirely and rely on
    // Game Changers to catch the most efficient tutors."
    // — Commander Brackets Beta Update, 2025-10-21.
    // The axis survives as evidence only (no row here): 6 of the 13 curated
    // tutor names are themselves Game Changers, so a tutor floor double-counted
    // cards the Game Changer rows already price.
];

/// Where the estimator starts before any rule fires.
///
/// NOT Exhibition. Bracket 1 is defined by theme, not by power: "A deck is not
/// Bracket 1 based on power level alone. It's really about the gameplay
/// experience and trying to engender highly thematic gameplay."
/// — Commander Brackets Beta Update, 2025-10-21.
/// An estimator that reads card names cannot observe theme, so returning
/// Exhibition is a claim it has no evidence for. Core is the lowest tier this
/// estimator can assert. A player whose deck IS thematic declares Bracket 1
/// themselves; that declaration governs.
const BASE_FLOOR: CommanderBracketTier = CommanderBracketTier::Core;

/// What a rule did when it was evaluated. Every rule in `FLOOR_RULES` produces
/// one of these, fired or not, so the reader can see an axis was looked at and
/// found clean, and how far it is from the next step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BracketCheckOutcome {
    /// The comparator is not satisfied. `cards_until_fired` is the smallest
    /// number of additional matching cards that would satisfy it, or `None`
    /// when no number of additions can.
    Clear { cards_until_fired: Option<u8> },
    /// The comparator is satisfied; this rule contributed `floor`.
    Fired,
}

/// One evaluated `FloorRule`. Wire-visible, so every provenance field is owned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketCheck {
    pub axis: BracketAxis,
    pub comparator: Comparator,
    pub threshold: u8,
    /// The tier this rule forces when it fires.
    pub floor: CommanderBracketTier,
    pub observed: u8,
    pub outcome: BracketCheckOutcome,
    /// The official sentence this rule encodes, quoted verbatim.
    pub official_line: String,
    pub source_document: String,
    /// ISO-8601.
    pub source_published: String,
    pub source_url: String,
    /// Card names on this axis, in deck order. The evidence for `observed`.
    pub evidence: Vec<String>,
}

/// How much of the deck the estimator could actually read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EstimateConfidence {
    /// Every counted deck name resolved to a printed card face.
    Complete,
    /// At least one counted name did not resolve. Counts are lower bounds.
    Partial,
}

/// What the estimator read, and what it could not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketCoverage {
    /// Names in the counted sections, including duplicates.
    pub counted: u16,
    /// How many of those resolved through `CardDatabase::get_face_by_name`.
    pub resolved: u16,
    /// Distinct names that did not resolve, sorted.
    pub unresolved: Vec<String>,
    pub confidence: EstimateConfidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketEstimate {
    pub tier: CommanderBracketTier,
    /// Every `BracketAxis` is present, including zero-count axes.
    pub axes: BTreeMap<BracketAxis, AxisReading>,
    /// One row per `FLOOR_RULES` entry, fired or not, in table order.
    pub checks: Vec<BracketCheck>,
    pub coverage: BracketCoverage,
    /// `BracketLists.version`, passed through from the export pipeline.
    pub data_version: String,
    /// `None` when the request carried no declaration. Nested `Option` is
    /// avoided by keeping presence-of-a-declaration in the `Option` and the
    /// verdict in the enum: `Undeclared` is the absence of an INPUT, not a
    /// verdict on one, and putting it inside `DeclarationVerdict` would mix
    /// two abstraction layers in one enum (CLAUDE.md, "Separate abstraction
    /// layers in enum design").
    #[serde(default)]
    pub declaration: Option<DeclarationVerdict>,
    #[serde(default)]
    pub combo_barometer: ComboBarometer,
    /// What this estimate could answer, and on whose authority, for every checkpoint the
    /// published format page names. Rendered verbatim by the panel so a barometer nobody
    /// answered can never read as a clean one. A `BTreeMap` for the same reason
    /// `violations` is one: ordered, keyed, and the invariant "at most one entry per
    /// barometer" is expressed in the type.
    #[serde(default)]
    pub barometers: BTreeMap<Barometer, BarometerAuthority>,
}

/// How a player's declared bracket relates to the floor the deck's contents
/// establish.
///
/// Bracket policy is **not** part of the Comprehensive Rules — it is WotC's
/// Commander Format Panel guidance, so no rules annotation applies (see the
/// module header). WotC state the constraint three times: the brackets are
/// "a tool to guide pregame conversations—not an ultimate arbiter of who can
/// play against whom" (Commander Brackets Beta Update, 2026-02-09), they are
/// "an entirely optional way to help matchmake your Commander games" (live
/// format page), and "Rule Zero still exists" (Introducing Commander Brackets
/// Beta, 2025-02-11).
///
/// The engine therefore makes exactly one claim: whether a declaration sits
/// strictly BELOW the floor the deck's contents establish. It never claims a
/// declaration above the floor is wrong — `estimate_bracket` returns a floor,
/// and everything above it is the player's to declare. In particular a `Cedh`
/// declaration over an `Optimized` floor is `AtOrAboveFloor`: `decide_tier`
/// never returns `Cedh`, pinned by `estimator_never_returns_cedh`, so B5 is
/// unreachable by design and comparing for equality warns every correctly
/// declared cEDH deck.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeclarationVerdict {
    /// The declaration is at or above the derived floor. Nothing to report.
    AtOrAboveFloor,
    /// The declaration is strictly below the floor the deck's contents
    /// establish. `raised_by` names the axes that established that floor, in
    /// `BracketAxis` declaration order; the caller reads their card evidence
    /// out of the estimate's own per-axis reading rather than duplicating it
    /// here, so there is exactly one name alphabet on the wire. `raised_by`
    /// names card-derived axes only and is empty when the floor rests on the
    /// owner's own combo declaration (the panel reads `combo_barometer.floor`
    /// for that case).
    BelowFloor {
        floor: CommanderBracketTier,
        raised_by: Vec<BracketAxis>,
    },
}

/// One bracket-estimate request: a decklist plus, optionally, the bracket its
/// owner declared.
#[derive(Debug, Clone, Deserialize)]
pub struct BracketEstimateRequest {
    pub deck: PlayerDeckList,
    /// `None` means UNDECLARED — not `Core`.
    ///
    /// Deliberately NOT read from `PlayerDeckList::bracket_tier`
    /// (`game/deck_loading.rs`): that field is `#[serde(default)]` over a
    /// `CommanderBracketTier` whose `Default` is `Core`, so a request that
    /// simply omits a declaration would arrive as an explicit `Core`
    /// declaration and every undeclared deck with a floor above Core would
    /// report `BelowFloor`.
    #[serde(default)]
    pub declared_tier: Option<CommanderBracketTier>,
}

/// Reconciles a declared bracket against a computed estimate.
///
/// Ordering goes through [`CommanderBracketTier::as_u8`] — the enum derives
/// `PartialEq, Eq, Hash` but deliberately **not** `Ord`, so a `<` on the
/// variants would not compile and must not be added just to make this read
/// nicer.
pub fn reconcile(declared: CommanderBracketTier, estimate: &BracketEstimate) -> DeclarationVerdict {
    if declared.as_u8() >= estimate.tier.as_u8() {
        return DeclarationVerdict::AtOrAboveFloor;
    }
    DeclarationVerdict::BelowFloor {
        floor: estimate.tier,
        raised_by: floor_raising_axes(estimate),
    }
}

fn floor_raising_axes(estimate: &BracketEstimate) -> Vec<BracketAxis> {
    estimate
        .checks
        .iter()
        .filter(|check| {
            check.outcome == BracketCheckOutcome::Fired
                && check.floor.as_u8() == estimate.tier.as_u8()
        })
        .map(|check| check.axis)
        .collect()
}

/// Profiles the deck's cards and folds in the owner's combo declaration.
/// Returns `None` when the deck has no commander.
pub fn estimate_bracket(deck: &PlayerDeckList, db: &CardDatabase) -> Option<BracketEstimate> {
    use strum::IntoEnumIterator;

    if deck.commander.is_empty() {
        return None;
    }

    // Which deck sections carry cards that can be in a Commander-family game.
    // Enforced by an exhaustive destructure: adding a field to `PlayerDeckList`
    // breaks this binding and forces the next author to classify it.
    //
    // This is WotC Commander Format Panel guidance, not the Comprehensive Rules —
    // no Comprehensive Rules annotation applies (see the module header).
    let PlayerDeckList {
        commander,       // counted — the commander is a card in the game
        main_deck,       // counted — the 99
        companion,       // counted — starts outside the game but can be brought in
        signature_spell, // counted — Oathbreaker RC second command-zone card
        sideboard,       // NOT counted — Commander has no sideboard; the deck
        //   builder parses one, and the request still carries it
        attraction_deck,   // NOT counted — Unfinity variant, outside the 100
        planar_deck,       // NOT counted — Planechase variant deck
        scheme_deck,       // NOT counted — Archenemy variant deck
        contraption_deck,  // NOT counted — Unstable variant deck
        sticker_sheets,    // NOT counted — Unfinity stickers, not cards in the deck
        bracket_tier: _,   // the player's declaration, not a card list
        combo_declaration, // the owner's barometer answer, not a card list
    } = deck;
    let _ = (
        sideboard,
        attraction_deck,
        planar_deck,
        scheme_deck,
        contraption_deck,
        sticker_sheets,
    );

    let mut axes: BTreeMap<BracketAxis, AxisReading> = BracketAxis::iter()
        .map(|axis| (axis, AxisReading::default()))
        .collect();

    let counted_names = commander
        .iter()
        .chain(main_deck.iter())
        .chain(companion.iter())
        .chain(signature_spell.iter());
    let mut counted = 0_u16;
    let mut resolved = 0_u16;
    let mut unresolved = BTreeSet::new();
    for name in counted_names {
        counted = counted.saturating_add(1);
        if db.get_face_by_name(name).is_some() {
            resolved = resolved.saturating_add(1);
        } else {
            unresolved.insert(name.clone());
        }
        for axis in db.bracket_signals_for(name).axes() {
            let reading = axes.entry(axis).or_default();
            reading.count = reading.count.saturating_add(1);
            reading.contributing.push(name.clone());
        }
    }

    let unresolved: Vec<String> = unresolved.into_iter().collect();
    let confidence = if unresolved.is_empty() {
        EstimateConfidence::Complete
    } else {
        EstimateConfidence::Partial
    };
    let (tier, checks) = decide_tier(&axes);
    let combo_floor = combo_declaration_floor(*combo_declaration);
    // The published combo lines are floors like any other; the resolved tier is the
    // highest floor across every checkpoint. Ordering goes through `as_u8()` —
    // `CommanderBracketTier` derives no `Ord` and this unit does not add one.
    let tier = match combo_floor {
        Some(floor) if floor.as_u8() > tier.as_u8() => floor,
        _ => tier,
    };
    let barometers = BTreeMap::from([
        (Barometer::GameChangers, BarometerAuthority::Engine),
        (Barometer::ExtraTurns, BarometerAuthority::Engine),
        (Barometer::MassLandDenial, BarometerAuthority::Engine),
        (
            Barometer::TwoCardCombos,
            match combo_declaration {
                ComboDeclaration::Undeclared => BarometerAuthority::Unanswered,
                ComboDeclaration::NoneIntended | ComboDeclaration::Intended { .. } => {
                    BarometerAuthority::DeckOwner
                }
            },
        ),
    ]);

    Some(BracketEstimate {
        tier,
        axes,
        checks,
        coverage: BracketCoverage {
            counted,
            resolved,
            unresolved,
            confidence,
        },
        data_version: db.bracket_lists.version.clone(),
        declaration: None,
        combo_barometer: ComboBarometer {
            declaration: *combo_declaration,
            floor: combo_floor,
        },
        barometers,
    })
}

/// Estimate a deck's bracket and, when the request carries a declaration,
/// reconcile it. Pure: no game state, no I/O, no randomness, no floats.
pub fn estimate_bracket_for_request(
    request: &BracketEstimateRequest,
    db: &CardDatabase,
) -> Option<BracketEstimate> {
    let mut estimate = estimate_bracket(&request.deck, db)?;
    estimate.declaration = request
        .declared_tier
        .map(|declared| reconcile(declared, &estimate));
    Some(estimate)
}

/// Smallest `d >= 0` that makes the rule fire, or `None` if additions cannot.
fn cards_until_fired(rule: &FloorRule, observed: u8) -> Option<u8> {
    (0..=(u8::MAX - observed)).find(|d| {
        rule.comparator
            .evaluate(i32::from(observed + d), i32::from(rule.threshold))
    })
}

/// Evaluates every cited floor rule and composes the maximum fired floor.
fn decide_tier(
    readings: &BTreeMap<BracketAxis, AxisReading>,
) -> (CommanderBracketTier, Vec<BracketCheck>) {
    let mut floor = BASE_FLOOR;
    let mut checks = Vec::with_capacity(FLOOR_RULES.len());
    for rule in FLOOR_RULES {
        let reading = readings.get(&rule.axis);
        let observed = reading.map_or(0, |value| value.count);
        let fired = rule
            .comparator
            .evaluate(i32::from(observed), i32::from(rule.threshold));
        if fired && rule.floor.as_u8() > floor.as_u8() {
            floor = rule.floor;
        }
        checks.push(BracketCheck {
            axis: rule.axis,
            comparator: rule.comparator,
            threshold: rule.threshold,
            floor: rule.floor,
            observed,
            outcome: if fired {
                BracketCheckOutcome::Fired
            } else {
                BracketCheckOutcome::Clear {
                    cards_until_fired: cards_until_fired(rule, observed),
                }
            },
            official_line: rule.official_line.to_string(),
            source_document: rule.source.document.to_string(),
            source_published: rule.source.published.to_string(),
            source_url: rule.source.url.to_string(),
            evidence: reading.map_or_else(Vec::new, |value| value.contributing.clone()),
        });
    }
    (floor, checks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::bracket_lists::{BracketCardClass, BracketLists};
    use crate::database::{BracketSignals, CardDatabase};
    use crate::game::deck_loading::PlayerDeckList;
    use strum::IntoEnumIterator;

    #[test]
    fn effective_tier_truth_table() {
        let estimated = std::iter::once(None)
            .chain(CommanderBracketTier::iter().map(Some))
            .collect::<Vec<_>>();

        let mut rows = 0;
        for declared in CommanderBracketTier::iter() {
            for estimate in &estimated {
                let expected = match estimate {
                    Some(floor) if floor.as_u8() > declared.as_u8() => *floor,
                    Some(_) | None => declared,
                };
                assert_eq!(effective_tier(declared, *estimate).tier(), expected);
                rows += 1;
            }
        }

        assert_eq!(rows, 30);
    }

    fn db_with_signals(entries: &[(&str, BracketSignals)]) -> CardDatabase {
        let mass_land_denial: Vec<&str> = entries
            .iter()
            .filter(|(_, signals)| signals.mass_land_denial)
            .map(|(name, _)| *name)
            .collect();
        let extra_turns: Vec<&str> = entries
            .iter()
            .filter(|(_, signals)| signals.extra_turn)
            .map(|(name, _)| *name)
            .collect();
        let efficient_tutors: Vec<&str> = entries
            .iter()
            .filter(|(_, signals)| signals.efficient_tutor)
            .map(|(name, _)| *name)
            .collect();
        let mut db = CardDatabase::default().with_bracket_lists(BracketLists::from_pairs(
            "test-1",
            &[
                (
                    BracketCardClass::MassLandSweepers,
                    mass_land_denial.as_slice(),
                ),
                (BracketCardClass::ExtraTurns, extra_turns.as_slice()),
                (
                    BracketCardClass::EfficientTutors,
                    efficient_tutors.as_slice(),
                ),
            ],
        ));
        db.bracket_signals_by_name = entries
            .iter()
            .map(|(name, signals)| (name.to_lowercase(), *signals))
            .collect();
        db
    }

    fn deck(commander: Vec<&str>, main: Vec<&str>) -> PlayerDeckList {
        PlayerDeckList {
            commander: commander.into_iter().map(String::from).collect(),
            main_deck: main.into_iter().map(String::from).collect(),
            sideboard: Vec::new(),
            ..Default::default()
        }
    }

    const CARD_DATA_WITH_KNOWN_FACES: &str = r#"{
        "known commander": {
            "name": "Known Commander", "mana_cost": { "type": "NoCost" },
            "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
            "power": null, "toughness": null, "loyalty": null, "defense": null,
            "oracle_text": null, "abilities": [], "triggers": [], "static_abilities": [],
            "replacements": [], "keywords": []
        },
        "forest": {
            "name": "Forest", "mana_cost": { "type": "NoCost" },
            "card_type": { "supertypes": ["Basic"], "core_types": ["Land"], "subtypes": ["Forest"] },
            "power": null, "toughness": null, "loyalty": null, "defense": null,
            "oracle_text": null, "abilities": [], "triggers": [], "static_abilities": [],
            "replacements": [], "keywords": []
        },
        "smothering tithe": {
            "name": "Smothering Tithe", "mana_cost": { "type": "NoCost" },
            "card_type": { "supertypes": [], "core_types": ["Enchantment"], "subtypes": [] },
            "power": null, "toughness": null, "loyalty": null, "defense": null,
            "oracle_text": null, "abilities": [], "triggers": [], "static_abilities": [],
            "replacements": [], "keywords": [],
            "bracket_signals": { "game_changer": true }
        }
    }"#;

    fn db_with_known_faces() -> CardDatabase {
        CardDatabase::from_json_str(CARD_DATA_WITH_KNOWN_FACES)
            .unwrap()
            .with_bracket_lists(BracketLists::from_pairs("faces-1", &[]))
    }

    #[test]
    fn combo_floor_is_none_when_unanswered_or_declared_absent() {
        assert_eq!(combo_declaration_floor(ComboDeclaration::Undeclared), None);
        assert_eq!(
            combo_declaration_floor(ComboDeclaration::NoneIntended),
            None
        );
    }

    #[test]
    fn combo_floor_is_b3_for_a_declared_combo_without_an_early_window() {
        assert_eq!(
            combo_declaration_floor(ComboDeclaration::Intended { window: None }),
            Some(CommanderBracketTier::Upgraded)
        );
        assert_eq!(
            combo_declaration_floor(ComboDeclaration::Intended {
                window: Some(ComboWindow::LateGame),
            }),
            Some(CommanderBracketTier::Upgraded)
        );
    }

    #[test]
    fn combo_floor_is_b4_for_a_declared_early_combo() {
        assert_eq!(
            combo_declaration_floor(ComboDeclaration::Intended {
                window: Some(ComboWindow::EarlyGame),
            }),
            Some(CommanderBracketTier::Optimized)
        );
    }

    #[test]
    fn declared_early_combo_raises_tier_above_the_card_derived_floor() {
        let d = PlayerDeckList {
            combo_declaration: ComboDeclaration::Intended {
                window: Some(ComboWindow::EarlyGame),
            },
            ..deck(vec!["Cmdr"], vec!["Forest"])
        };
        let estimate = estimate_bracket(&d, &db_with_signals(&[])).unwrap();

        assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
        assert_eq!(
            estimate.combo_barometer.floor,
            Some(CommanderBracketTier::Optimized)
        );
    }

    #[test]
    fn declared_combo_never_lowers_a_higher_card_derived_floor() {
        let db = db_with_signals(&[(
            "Armageddon",
            BracketSignals {
                mass_land_denial: true,
                ..Default::default()
            },
        )]);
        let d = PlayerDeckList {
            combo_declaration: ComboDeclaration::Intended { window: None },
            ..deck(vec!["Cmdr"], vec!["Armageddon"])
        };
        let estimate = estimate_bracket(&d, &db).unwrap();

        assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
        assert_eq!(
            estimate.combo_barometer.floor,
            Some(CommanderBracketTier::Upgraded)
        );
    }

    #[test]
    fn barometer_authority_distinguishes_unanswered_from_declared_absence() {
        let cases = [
            (ComboDeclaration::Undeclared, BarometerAuthority::Unanswered),
            (
                ComboDeclaration::NoneIntended,
                BarometerAuthority::DeckOwner,
            ),
            (
                ComboDeclaration::Intended { window: None },
                BarometerAuthority::DeckOwner,
            ),
        ];

        for (declaration, expected_combo_authority) in cases {
            let d = PlayerDeckList {
                combo_declaration: declaration,
                ..deck(vec!["Cmdr"], vec!["Forest"])
            };
            let estimate = estimate_bracket(&d, &db_with_signals(&[])).unwrap();

            assert_eq!(estimate.barometers.len(), 4);
            assert_eq!(
                estimate.barometers[&Barometer::TwoCardCombos],
                expected_combo_authority
            );
            for barometer in [
                Barometer::GameChangers,
                Barometer::ExtraTurns,
                Barometer::MassLandDenial,
            ] {
                assert_eq!(estimate.barometers[&barometer], BarometerAuthority::Engine);
            }

            if declaration == ComboDeclaration::Undeclared {
                let value = serde_json::to_value(&estimate).unwrap();
                assert_eq!(
                    value["combo_barometer"],
                    serde_json::json!({
                        "declaration": { "kind": "undeclared" },
                        "floor": null,
                    })
                );
                assert_eq!(
                    value["barometers"],
                    serde_json::json!({
                        "extra_turns": "engine",
                        "game_changers": "engine",
                        "mass_land_denial": "engine",
                        "two_card_combos": "unanswered",
                    })
                );
            }
        }
    }

    #[test]
    fn combo_declaration_round_trips_through_json() {
        let declarations = [
            ComboDeclaration::Undeclared,
            ComboDeclaration::NoneIntended,
            ComboDeclaration::Intended { window: None },
            ComboDeclaration::Intended {
                window: Some(ComboWindow::EarlyGame),
            },
        ];

        for declaration in declarations {
            let json = serde_json::to_string(&declaration).unwrap();
            assert_eq!(
                serde_json::from_str::<ComboDeclaration>(&json).unwrap(),
                declaration
            );
        }
        assert_eq!(
            serde_json::from_str::<ComboDeclaration>(r#"{"kind":"intended"}"#).unwrap(),
            ComboDeclaration::Intended { window: None }
        );
        assert_eq!(
            serde_json::to_string(&ComboDeclaration::Undeclared).unwrap(),
            r#"{"kind":"undeclared"}"#
        );
        assert_eq!(
            serde_json::to_string(&ComboDeclaration::Intended {
                window: Some(ComboWindow::EarlyGame),
            })
            .unwrap(),
            r#"{"kind":"intended","window":"early_game"}"#
        );
    }

    #[test]
    fn empty_deck_returns_none() {
        let db = CardDatabase::default();
        let d = deck(vec![], vec![]);
        assert!(estimate_bracket(&d, &db).is_none());
    }

    #[test]
    fn no_commander_returns_none() {
        let db = CardDatabase::default();
        let d = deck(vec![], vec!["Forest", "Island"]);
        assert!(estimate_bracket(&d, &db).is_none());
    }

    #[test]
    fn clean_deck_is_base_floor_core() {
        let db = db_with_signals(&[]);
        let d = deck(vec!["Atraxa, Praetors' Voice"], vec!["Forest", "Island"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Core);
        assert!(e
            .axes
            .values()
            .all(|reading| reading.count == 0 && reading.contributing.is_empty()));
        assert!(e
            .checks
            .iter()
            .all(|check| matches!(check.outcome, BracketCheckOutcome::Clear { .. })));
    }

    #[test]
    fn one_game_changer_forces_b3() {
        let db = db_with_signals(&[(
            "Smothering Tithe",
            BracketSignals {
                game_changer: true,
                ..Default::default()
            },
        )]);
        let d = deck(vec!["Atraxa, Praetors' Voice"], vec!["Smothering Tithe"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Upgraded);
        assert_eq!(e.axes[&BracketAxis::GameChangers].count, 1);
        assert!(e
            .checks
            .iter()
            .any(|check| check.axis == BracketAxis::GameChangers
                && check.threshold == 1
                && check.outcome == BracketCheckOutcome::Fired));
    }

    #[test]
    fn companion_card_is_counted() {
        let db = db_with_signals(&[(
            "Lutri, the Spellchaser",
            BracketSignals {
                game_changer: true,
                ..Default::default()
            },
        )]);
        let d = PlayerDeckList {
            companion: vec!["Lutri, the Spellchaser".to_string()],
            ..deck(vec!["Cmdr"], vec!["Forest"])
        };
        let e = estimate_bracket(&d, &db).unwrap();

        assert_eq!(e.tier, CommanderBracketTier::Upgraded);
        assert_eq!(
            e.axes[&BracketAxis::GameChangers].contributing,
            vec!["Lutri, the Spellchaser"]
        );
    }

    #[test]
    fn signature_spell_card_is_counted() {
        let db = db_with_signals(&[(
            "Smothering Tithe",
            BracketSignals {
                game_changer: true,
                ..Default::default()
            },
        )]);
        let d = PlayerDeckList {
            signature_spell: vec!["Smothering Tithe".to_string()],
            ..deck(vec!["Cmdr"], vec!["Forest"])
        };
        let e = estimate_bracket(&d, &db).unwrap();

        assert_eq!(e.tier, CommanderBracketTier::Upgraded);
        assert_eq!(
            e.axes[&BracketAxis::GameChangers].contributing,
            vec!["Smothering Tithe"]
        );
    }

    #[test]
    fn sideboard_card_is_not_counted() {
        let db = db_with_signals(&[(
            "Smothering Tithe",
            BracketSignals {
                game_changer: true,
                ..Default::default()
            },
        )]);
        let d = PlayerDeckList {
            sideboard: vec!["Smothering Tithe".to_string()],
            ..deck(vec!["Cmdr"], vec!["Forest"])
        };
        let e = estimate_bracket(&d, &db).unwrap();

        assert_eq!(e.tier, BASE_FLOOR);
        assert!(e.axes[&BracketAxis::GameChangers].contributing.is_empty());
    }

    #[test]
    fn variant_format_decks_are_not_counted() {
        let db = db_with_signals(&[(
            "Smothering Tithe",
            BracketSignals {
                game_changer: true,
                ..Default::default()
            },
        )]);
        let excluded_card = vec!["Smothering Tithe".to_string()];
        let d = PlayerDeckList {
            attraction_deck: excluded_card.clone(),
            planar_deck: excluded_card.clone(),
            scheme_deck: excluded_card.clone(),
            contraption_deck: excluded_card.clone(),
            sticker_sheets: excluded_card,
            ..deck(vec!["Cmdr"], vec!["Forest"])
        };
        let e = estimate_bracket(&d, &db).unwrap();

        assert_eq!(e.tier, BASE_FLOOR);
        assert!(e.axes[&BracketAxis::GameChangers].contributing.is_empty());
    }

    #[test]
    fn four_game_changers_forces_b4() {
        let sig = BracketSignals {
            game_changer: true,
            ..Default::default()
        };
        let db = db_with_signals(&[("A", sig), ("B", sig), ("C", sig), ("D", sig)]);
        let d = deck(vec!["Cmdr"], vec!["A", "B", "C", "D"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Optimized);
        assert_eq!(e.axes[&BracketAxis::GameChangers].count, 4);
    }

    #[test]
    fn any_mass_land_denial_forces_b4() {
        let db = db_with_signals(&[(
            "Armageddon",
            BracketSignals {
                mass_land_denial: true,
                ..Default::default()
            },
        )]);
        let d = deck(vec!["Cmdr"], vec!["Armageddon"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Optimized);
    }

    #[test]
    fn one_extra_turn_forces_core_not_upgraded() {
        let db = db_with_signals(&[(
            "Time Warp",
            BracketSignals {
                extra_turn: true,
                ..Default::default()
            },
        )]);
        let d = deck(vec!["Cmdr"], vec!["Time Warp"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Core);
    }

    #[test]
    fn overlapping_lists_register_on_every_matching_axis() {
        let db = db_with_signals(&[(
            "Demonic Tutor",
            BracketSignals {
                game_changer: true,
                efficient_tutor: true,
                ..Default::default()
            },
        )]);
        let d = deck(vec!["Cmdr"], vec!["Demonic Tutor"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.axes[&BracketAxis::GameChangers].count, 1);
        assert_eq!(e.axes[&BracketAxis::EfficientTutors].count, 1);
        assert_eq!(e.tier, CommanderBracketTier::Upgraded);
        assert_eq!(
            e.axes[&BracketAxis::GameChangers].contributing,
            vec!["Demonic Tutor"]
        );
        assert_eq!(
            e.axes[&BracketAxis::EfficientTutors].contributing,
            vec!["Demonic Tutor"]
        );
    }

    #[test]
    fn estimator_never_returns_cedh() {
        let sig = BracketSignals {
            game_changer: true,
            mass_land_denial: true,
            extra_turn: true,
            efficient_tutor: true,
        };
        let entries: Vec<(String, BracketSignals)> =
            (0..40).map(|i| (format!("Card{i}"), sig)).collect();
        let entry_refs: Vec<(&str, BracketSignals)> = entries
            .iter()
            .map(|(name, signals)| (name.as_str(), *signals))
            .collect();
        let db = db_with_signals(&entry_refs);
        let main: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        let d = deck(vec!["Cmdr"], main);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(
            e.tier,
            CommanderBracketTier::Optimized,
            "estimator caps at B4"
        );
    }

    #[test]
    fn cedh_declaration_over_optimized_floor_is_at_or_above() {
        let signals = BracketSignals {
            game_changer: true,
            mass_land_denial: true,
            extra_turn: true,
            efficient_tutor: true,
        };
        let entries: Vec<(String, BracketSignals)> = (0..40)
            .map(|index| (format!("Card{index}"), signals))
            .collect();
        let entry_refs: Vec<(&str, BracketSignals)> = entries
            .iter()
            .map(|(name, signals)| (name.as_str(), *signals))
            .collect();
        let db = db_with_signals(&entry_refs);
        let main = entries.iter().map(|(name, _)| name.as_str()).collect();
        let estimate = estimate_bracket(&deck(vec!["Cmdr"], main), &db).unwrap();

        assert_eq!(
            reconcile(CommanderBracketTier::Cedh, &estimate),
            DeclarationVerdict::AtOrAboveFloor
        );
    }

    #[test]
    fn declaration_equal_to_floor_is_at_or_above() {
        let db = db_with_signals(&[(
            "Smothering Tithe",
            BracketSignals {
                game_changer: true,
                ..Default::default()
            },
        )]);
        let estimate =
            estimate_bracket(&deck(vec!["Cmdr"], vec!["Smothering Tithe"]), &db).unwrap();

        assert_eq!(estimate.tier, CommanderBracketTier::Upgraded);
        assert_eq!(
            reconcile(CommanderBracketTier::Upgraded, &estimate),
            DeclarationVerdict::AtOrAboveFloor
        );
    }

    #[test]
    fn declaration_below_floor_reports_floor_and_axes() {
        let db = db_with_signals(&[(
            "Armageddon",
            BracketSignals {
                mass_land_denial: true,
                ..Default::default()
            },
        )]);
        let estimate = estimate_bracket(&deck(vec!["Cmdr"], vec!["Armageddon"]), &db).unwrap();

        assert_eq!(
            reconcile(CommanderBracketTier::Core, &estimate),
            DeclarationVerdict::BelowFloor {
                floor: CommanderBracketTier::Optimized,
                raised_by: vec![BracketAxis::MassLandDenial],
            }
        );
    }

    #[test]
    fn declaration_above_floor_is_at_or_above() {
        let estimate =
            estimate_bracket(&deck(vec!["Cmdr"], vec!["Forest"]), &db_with_signals(&[])).unwrap();

        assert_eq!(estimate.tier, CommanderBracketTier::Core);
        assert_eq!(
            reconcile(CommanderBracketTier::Optimized, &estimate),
            DeclarationVerdict::AtOrAboveFloor
        );
    }

    #[test]
    fn undeclared_request_yields_no_verdict() {
        let request = BracketEstimateRequest {
            deck: deck(vec!["Cmdr"], vec!["Forest"]),
            declared_tier: None,
        };
        let estimate = estimate_bracket_for_request(&request, &db_with_signals(&[])).unwrap();

        assert_eq!(estimate.declaration, None);
    }

    #[test]
    fn request_missing_declared_tier_deserializes_to_none_not_core() {
        let request: BracketEstimateRequest =
            serde_json::from_str(r#"{"deck":{"main_deck":[],"commander":["Cmdr"]}}"#).unwrap();
        let bare_deck: PlayerDeckList =
            serde_json::from_str(r#"{"main_deck":[],"commander":["Cmdr"]}"#).unwrap();

        assert_eq!(request.declared_tier, None);
        assert_eq!(bare_deck.bracket_tier, CommanderBracketTier::Core);
    }

    #[test]
    fn verdict_wire_shape_is_internally_tagged() {
        assert_eq!(
            serde_json::to_value(DeclarationVerdict::AtOrAboveFloor).unwrap(),
            serde_json::json!({ "kind": "at_or_above_floor" })
        );
        assert_eq!(
            serde_json::to_value(DeclarationVerdict::BelowFloor {
                floor: CommanderBracketTier::Optimized,
                raised_by: vec![BracketAxis::MassLandDenial],
            })
            .unwrap(),
            serde_json::json!({
                "kind": "below_floor",
                "floor": "optimized",
                "raised_by": ["mass_land_denial"],
            })
        );
    }

    #[test]
    fn contributing_cards_listed_per_axis() {
        let db = db_with_signals(&[
            (
                "Smothering Tithe",
                BracketSignals {
                    game_changer: true,
                    ..Default::default()
                },
            ),
            (
                "Cyclonic Rift",
                BracketSignals {
                    game_changer: true,
                    ..Default::default()
                },
            ),
            (
                "Demonic Tutor",
                BracketSignals {
                    efficient_tutor: true,
                    ..Default::default()
                },
            ),
        ]);
        let d = deck(
            vec!["Cmdr"],
            vec!["Smothering Tithe", "Cyclonic Rift", "Demonic Tutor"],
        );
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(
            e.axes[&BracketAxis::GameChangers].contributing,
            vec!["Smothering Tithe", "Cyclonic Rift"]
        );
        assert_eq!(
            e.axes[&BracketAxis::EfficientTutors].contributing,
            vec!["Demonic Tutor"]
        );
    }

    #[test]
    fn determinism_same_inputs_same_estimate() {
        let db = db_with_signals(&[(
            "Demonic Tutor",
            BracketSignals {
                efficient_tutor: true,
                ..Default::default()
            },
        )]);
        let d = deck(vec!["Cmdr"], vec!["Demonic Tutor"]);
        let a = estimate_bracket(&d, &db).unwrap();
        let b = estimate_bracket(&d, &db).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn data_version_is_passed_through() {
        let db = db_with_signals(&[]);
        let d = deck(vec!["Cmdr"], vec!["Forest"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.data_version, "test-1");
    }

    #[test]
    fn signal_on_commander_card_is_counted() {
        let db = db_with_signals(&[(
            "Sol Ring",
            BracketSignals {
                game_changer: true,
                ..Default::default()
            },
        )]);
        let d = deck(vec!["Sol Ring"], vec!["Forest", "Forest"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.axes[&BracketAxis::GameChangers].count, 1);
        assert_eq!(e.tier, CommanderBracketTier::Upgraded);
    }

    #[test]
    fn highest_axis_wins_and_checks_recorded_per_rule() {
        let db = db_with_signals(&[
            (
                "Smothering Tithe",
                BracketSignals {
                    game_changer: true,
                    ..Default::default()
                },
            ),
            (
                "Armageddon",
                BracketSignals {
                    mass_land_denial: true,
                    ..Default::default()
                },
            ),
        ]);
        let d = deck(vec!["Cmdr"], vec!["Smothering Tithe", "Armageddon"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Optimized, "MLD pushes to B4");
        assert!(e
            .checks
            .iter()
            .any(|check| check.axis == BracketAxis::MassLandDenial
                && check.floor == CommanderBracketTier::Optimized
                && check.outcome == BracketCheckOutcome::Fired));
        assert!(e
            .checks
            .iter()
            .any(|check| check.axis == BracketAxis::GameChangers
                && check.floor == CommanderBracketTier::Upgraded
                && check.outcome == BracketCheckOutcome::Fired));
    }

    #[test]
    fn every_axis_is_present_even_at_zero() {
        use strum::IntoEnumIterator;

        let estimate =
            estimate_bracket(&deck(vec!["Cmdr"], vec!["Forest"]), &db_with_signals(&[])).unwrap();
        assert_eq!(estimate.axes.len(), BracketAxis::iter().count());
        for axis in BracketAxis::iter() {
            let reading = &estimate.axes[&axis];
            assert_eq!(reading.count, 0);
            assert!(reading.contributing.is_empty());
        }
    }

    #[test]
    fn tutors_are_counted_but_force_no_floor() {
        let tutor = BracketSignals {
            efficient_tutor: true,
            ..Default::default()
        };
        let names = [
            "Tutor 1", "Tutor 2", "Tutor 3", "Tutor 4", "Tutor 5", "Tutor 6", "Tutor 7", "Tutor 8",
            "Tutor 9", "Tutor 10",
        ];
        let entries: Vec<(&str, BracketSignals)> =
            names.iter().map(|name| (*name, tutor)).collect();
        let estimate = estimate_bracket(
            &deck(vec!["Cmdr"], names.to_vec()),
            &db_with_signals(&entries),
        )
        .unwrap();

        assert_eq!(estimate.tier, BASE_FLOOR);
        assert_eq!(estimate.axes[&BracketAxis::EfficientTutors].count, 10);
        assert!(estimate
            .checks
            .iter()
            .all(|check| check.axis != BracketAxis::EfficientTutors));
    }

    #[test]
    fn every_rule_emits_a_check_row() {
        let estimate =
            estimate_bracket(&deck(vec!["Cmdr"], vec!["Forest"]), &db_with_signals(&[])).unwrap();
        assert_eq!(estimate.checks.len(), FLOOR_RULES.len());
        assert!(estimate
            .checks
            .iter()
            .all(|check| matches!(check.outcome, BracketCheckOutcome::Clear { .. })));
    }

    #[test]
    fn clear_row_reports_cards_until_fired() {
        let signal = BracketSignals {
            game_changer: true,
            ..Default::default()
        };
        let db = db_with_signals(&[("A", signal), ("B", signal), ("C", signal)]);
        let estimate = estimate_bracket(&deck(vec!["Cmdr"], vec!["A", "B", "C"]), &db).unwrap();
        let row = estimate
            .checks
            .iter()
            .find(|check| check.axis == BracketAxis::GameChangers && check.threshold == 4)
            .unwrap();
        assert_eq!(
            row.outcome,
            BracketCheckOutcome::Clear {
                cards_until_fired: Some(1)
            }
        );
    }

    #[test]
    fn two_game_changer_rules_both_present() {
        let estimate =
            estimate_bracket(&deck(vec!["Cmdr"], vec![]), &db_with_signals(&[])).unwrap();
        let thresholds: Vec<u8> = estimate
            .checks
            .iter()
            .filter(|check| check.axis == BracketAxis::GameChangers)
            .map(|check| check.threshold)
            .collect();
        assert_eq!(thresholds, [1, 4]);
    }

    #[test]
    fn every_floor_rule_cites_a_source() {
        for rule in FLOOR_RULES {
            let published = rule.source.published.as_bytes();
            assert!(!rule.official_line.is_empty());
            assert!(!rule.source.document.is_empty());
            assert!(!rule.source.url.is_empty());
            assert_eq!(published.len(), 10);
            assert!(published
                .iter()
                .enumerate()
                .all(|(index, byte)| matches!(index, 4 | 7) && *byte == b'-'
                    || !matches!(index, 4 | 7) && byte.is_ascii_digit()));
        }
    }

    #[test]
    fn floor_is_max_over_fired_rules() {
        let db = db_with_signals(&[
            (
                "Smothering Tithe",
                BracketSignals {
                    game_changer: true,
                    ..Default::default()
                },
            ),
            (
                "Armageddon",
                BracketSignals {
                    mass_land_denial: true,
                    ..Default::default()
                },
            ),
        ]);
        let estimate = estimate_bracket(
            &deck(vec!["Cmdr"], vec!["Smothering Tithe", "Armageddon"]),
            &db,
        )
        .unwrap();
        assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
        assert!(estimate.checks.iter().any(|check| {
            check.axis == BracketAxis::GameChangers
                && check.threshold == 1
                && check.outcome == BracketCheckOutcome::Fired
        }));
        assert!(estimate.checks.iter().any(|check| {
            check.axis == BracketAxis::MassLandDenial && check.outcome == BracketCheckOutcome::Fired
        }));
    }

    #[test]
    fn cards_until_fired_is_none_when_unreachable() {
        let rule = FloorRule {
            axis: BracketAxis::GameChangers,
            comparator: Comparator::LE,
            threshold: 0,
            floor: CommanderBracketTier::Core,
            official_line: "test",
            source: SRC_INTRO,
        };
        assert_eq!(cards_until_fired(&rule, 3), None);
    }

    #[test]
    fn unresolved_names_are_reported() {
        let estimate = estimate_bracket(
            &deck(
                vec!["Known Commander"],
                vec!["Zulu Missing", "Alpha Missing"],
            ),
            &db_with_known_faces(),
        )
        .unwrap();
        assert_eq!(
            estimate.coverage.unresolved,
            ["Alpha Missing", "Zulu Missing"]
        );
        assert_eq!(estimate.coverage.confidence, EstimateConfidence::Partial);
    }

    #[test]
    fn empty_database_is_partial_not_clean() {
        let estimate = estimate_bracket(
            &deck(vec!["Cmdr"], vec!["Forest", "Island"]),
            &CardDatabase::default(),
        )
        .unwrap();
        assert_eq!(estimate.coverage.counted, 3);
        assert_eq!(estimate.coverage.resolved, 0);
        assert_eq!(estimate.coverage.confidence, EstimateConfidence::Partial);
    }

    #[test]
    fn fully_resolved_deck_is_complete() {
        let estimate = estimate_bracket(
            &deck(vec!["Known Commander"], vec!["Forest"]),
            &db_with_known_faces(),
        )
        .unwrap();
        assert_eq!(estimate.coverage.counted, 2);
        assert_eq!(estimate.coverage.resolved, 2);
        assert!(estimate.coverage.unresolved.is_empty());
        assert_eq!(estimate.coverage.confidence, EstimateConfidence::Complete);
    }

    #[test]
    fn unresolved_names_are_deduplicated_and_sorted() {
        let estimate = estimate_bracket(
            &deck(
                vec!["Missing B"],
                vec!["Missing A", "Missing B", "Missing B", "Missing B"],
            ),
            &CardDatabase::default(),
        )
        .unwrap();
        assert_eq!(estimate.coverage.unresolved, ["Missing A", "Missing B"]);
        assert_eq!(estimate.coverage.counted, 5);
    }

    #[test]
    fn signals_are_read_for_unresolved_names() {
        let db = db_with_signals(&[(
            "Armageddon",
            BracketSignals {
                mass_land_denial: true,
                ..Default::default()
            },
        )]);
        let estimate = estimate_bracket(&deck(vec!["Cmdr"], vec!["Armageddon"]), &db).unwrap();
        assert_eq!(estimate.axes[&BracketAxis::MassLandDenial].count, 1);
        assert!(estimate
            .coverage
            .unresolved
            .contains(&"Armageddon".to_string()));
        assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    }

    #[test]
    fn coverage_does_not_change_the_tier() {
        let signal = BracketSignals {
            game_changer: true,
            ..Default::default()
        };
        let unresolved_db = db_with_signals(&[("Smothering Tithe", signal)]);
        let mut resolved_db = db_with_known_faces();
        resolved_db
            .bracket_signals_by_name
            .insert("smothering tithe".to_string(), signal);
        let input = deck(vec!["Known Commander"], vec!["Smothering Tithe"]);
        let unresolved = estimate_bracket(&input, &unresolved_db).unwrap();
        let resolved = estimate_bracket(&input, &resolved_db).unwrap();
        assert_eq!(unresolved.tier, resolved.tier);
        assert_eq!(unresolved.axes, resolved.axes);
        assert_eq!(unresolved.checks, resolved.checks);
        assert_eq!(unresolved.coverage.confidence, EstimateConfidence::Partial);
        assert_eq!(resolved.coverage.confidence, EstimateConfidence::Complete);
    }

    #[test]
    fn estimate_serializes_as_an_axis_keyed_reading_map() {
        let db = db_with_signals(&[(
            "Smothering Tithe",
            BracketSignals {
                game_changer: true,
                ..Default::default()
            },
        )]);
        let estimate =
            estimate_bracket(&deck(vec!["Cmdr"], vec!["Smothering Tithe"]), &db).unwrap();
        let value = serde_json::to_value(estimate).unwrap();

        assert_eq!(value["axes"]["game_changers"]["count"], 1);
        assert_eq!(
            value["axes"]["game_changers"]["contributing"][0],
            "Smothering Tithe"
        );
        assert_eq!(value["checks"][0]["comparator"], "GE");
        assert_eq!(
            value["checks"][0]["outcome"],
            serde_json::json!({ "kind": "fired" })
        );
        assert_eq!(
            value["checks"][1]["outcome"],
            serde_json::json!({ "kind": "clear", "cards_until_fired": 3 })
        );
        assert_eq!(value["coverage"]["confidence"], "partial");
        assert_eq!(value["declaration"], serde_json::Value::Null);
        let complete = estimate_bracket(
            &deck(vec!["Known Commander"], vec!["Forest"]),
            &db_with_known_faces(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(complete).unwrap()["coverage"]["confidence"],
            "complete"
        );
    }
}
