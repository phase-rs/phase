//! Shared combo-corpus harness: the 53-row acceptance corpus + the bespoke driver
//! toolkit, parameterized on a `&CardDatabase` so BOTH the `#[cfg(test)]`
//! acceptance suite (`corpus_tests`) and the `combo-verify` CLI drive ONE shared
//! implementation. Gated `#[cfg(any(test, feature = "combo-verify"))]`, so it is
//! excluded from the shipped lib / WASM surface (no game behavior change).
//!
//! Zero game logic lives here: every loop is confirmed by the EXISTING detector
//! (`detect_loop` for offline combos, the per-beat `apply(PassPriority)` reducer +
//! `live_mandatory_loop_winner` §3 shortcut for the two live drain cascades). The
//! CLI is a thin formatter over [`drive_row`]; the engine owns all detection.
//!
//! The single test-only dependency the old `#[cfg(test)] mod corpus_tests` carried
//! (`card_db()` → `test_support::shared_card_db()`, a `#![cfg(test)]` module) is
//! removed here by threading `db: &CardDatabase` through every driver. The
//! `#[cfg(test)]` tests pass the committed fixture; the CLI passes the full export.

use crate::analysis::resource::ResourceAxis;
use crate::analysis::{detect_loop, LoopCertificate, LoopProbe, WinKind};
use crate::database::CardDatabase;
use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
use crate::types::ability::TargetRef;
use crate::types::actions::GameAction;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaType;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;

// ===========================================================================
// Data layer: the 53-row corpus.
// ===========================================================================

/// One row of the acceptance corpus: a combo, its documented unbounded resource
/// family, the expected [`WinKind`], and (for the 4 card-gated combos) the card
/// whose completion unblocks it.
///
/// Fields are `pub(crate)` so the `#[cfg(test)]` meta-tests can read them
/// directly; the external (CLI) consumer reads a derived [`RowReport`], never a
/// `ComboRow`.
pub struct ComboRow {
    /// Combo name (cards), for diagnostics.
    pub(crate) name: &'static str,
    /// The exact card names that make up this combo, as they appear in the
    /// card-data export. The corpus test loads each of these from the real export
    /// to confirm the combo is *available* (every card present + implemented).
    pub(crate) cards: &'static [&'static str],
    /// The unbounded-resource *family* the combo produces (the §12 "Category"
    /// column). The detector must name ≥1 axis of this family.
    pub(crate) family: ResourceFamily,
    /// The expected `WinKind` once the loop is driven.
    pub(crate) win_kind: WinKind,
    /// `Some(card)` if this row is gated on an unimplemented card — kept as a
    /// card-presence-only data row, not driven; `None` otherwise.
    pub(crate) gated_on: Option<&'static str>,
    /// `Some(bucket)` for a row that is neither driven nor gated: the measured
    /// structural class that keeps it off today's in-place loop model. Mutually
    /// exclusive with `gated_on` and the driven set (locked by the partition
    /// shape-test). `None` for a driven or gated row.
    pub(crate) deferral: Option<DeferralBucket>,
}

/// The §12 unbounded-resource families, mapped to the concrete [`ResourceAxis`]
/// the detector reports. Keeps the corpus table declarative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceFamily {
    Mana,
    Tokens,
    Damage,
    Drain,
    Mill,
    Death,
    Landfall,
    Draw,
    DrawDamage,
    Combat,
    Turns,
    Counters,
    Proliferate,
    Engine,
}

/// The measured structural reason a non-gated corpus row is not yet driven on
/// today's net-progress loop model. Declarative typed data on every deferred
/// [`ComboRow`] (NOT a per-index `match`), sourced from the "Remaining corpus
/// rows" analysis — see the named-bucket enumerations there. `Other` is the
/// honest catch-all for a deferred row not in one of the three explicitly
/// measured structural classes (no bespoke driver on the current in-place model).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferralBucket {
    /// Tokens / blink / persist / undying / recur engines: a permanent that
    /// dies/blinks/bounces and returns gets a FRESH `ObjectId` each cycle, so the
    /// id-keyed per-object loop equality sees a different board.
    ObjectReentry,
    /// Extra-turn / extra-combat re-entry: each cycle advances `turn_number` /
    /// combat count, so the loop point is a different turn/phase — not board-
    /// identical.
    ExtraTurnOrCombat,
    /// Color-converting net-progress the per-color rule rejects (a producer that
    /// gains one color while a different, unreplaceable color is consumed).
    ColorConverting,
    /// Deferred but not among the three explicitly measured structural classes;
    /// no bespoke driver on today's in-place loop model.
    Other,
}

/// The full 53-row acceptance corpus: 3 driving combos + the 50 card-disjoint
/// corpus combos. The 4 `gated_on`-nonempty rows correspond to the cards with
/// Unimplemented parts; the 37 `deferral`-nonempty rows are the non-driven,
/// non-gated combos with a measured structural deferral reason.
pub(crate) const CORPUS: &[ComboRow] = &[
    // ---- 3 driving combos ----
    ComboRow {
        name: "Heliod, Sun-Crowned + Walking Ballista",
        cards: &["Heliod, Sun-Crowned", "Walking Ballista"],
        family: ResourceFamily::Damage,
        win_kind: WinKind::LethalDamage,
        gated_on: None,
        deferral: None,
    },
    ComboRow {
        name: "Kilo, Apogee Mind + Freed from the Real + Relic of Legends",
        cards: &[
            "Kilo, Apogee Mind",
            "Freed from the Real",
            "Relic of Legends",
        ],
        family: ResourceFamily::Proliferate,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: None,
    },
    ComboRow {
        name: "Doc Aurlock, Grizzled Genius + Aang, Swift Savior + Appa, Steadfast Guardian",
        cards: &[
            "Doc Aurlock, Grizzled Genius",
            "Aang, Swift Savior",
            "Appa, Steadfast Guardian",
        ],
        family: ResourceFamily::Tokens,
        win_kind: WinKind::Advantage,
        gated_on: Some("Doc Aurlock, Grizzled Genius"),
        deferral: None,
    },
    // ---- 50 corpus combos (§12) ----
    ComboRow {
        name: "Basalt Monolith + Rings of Brighthearth",
        cards: &["Basalt Monolith", "Rings of Brighthearth"],
        family: ResourceFamily::Mana,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Grim Monolith + Power Artifact",
        cards: &["Grim Monolith", "Power Artifact"],
        family: ResourceFamily::Mana,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: None,
    },
    ComboRow {
        name: "Palinchron + Deadeye Navigator",
        cards: &["Palinchron", "Deadeye Navigator"],
        family: ResourceFamily::Mana,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ObjectReentry),
    },
    ComboRow {
        name: "Devoted Druid + Vizier of Remedies",
        cards: &["Devoted Druid", "Vizier of Remedies"],
        family: ResourceFamily::Mana,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: None,
    },
    ComboRow {
        name: "Dramatic Reversal + Isochron Scepter",
        cards: &["Dramatic Reversal", "Isochron Scepter"],
        family: ResourceFamily::Mana,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Pili-Pala + Grand Architect",
        cards: &["Pili-Pala", "Grand Architect"],
        family: ResourceFamily::Mana,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ColorConverting),
    },
    ComboRow {
        name: "Bloom Tender + Freed from the Real",
        cards: &["Bloom Tender", "Freed from the Real"],
        family: ResourceFamily::Mana,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: None,
    },
    ComboRow {
        name: "Priest of Titania + Umbral Mantle",
        cards: &["Priest of Titania", "Umbral Mantle"],
        family: ResourceFamily::Mana,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: None,
    },
    ComboRow {
        name: "Dockside Extortionist + Temur Sabertooth",
        cards: &["Dockside Extortionist", "Temur Sabertooth"],
        family: ResourceFamily::Mana,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ObjectReentry),
    },
    ComboRow {
        name: "Selvala, Heart of the Wilds + Staff of Domination",
        cards: &["Selvala, Heart of the Wilds", "Staff of Domination"],
        family: ResourceFamily::Mana,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: None,
    },
    ComboRow {
        name: "Faeburrow Elder + Pemmin's Aura",
        cards: &["Faeburrow Elder", "Pemmin's Aura"],
        family: ResourceFamily::Mana,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: None,
    },
    ComboRow {
        name: "Marwyn, the Nurturer + Sword of the Paruns",
        cards: &["Marwyn, the Nurturer", "Sword of the Paruns"],
        family: ResourceFamily::Mana,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: None,
    },
    ComboRow {
        name: "Heliod, Sun-Crowned + Walking Ballista [#13]",
        cards: &["Heliod, Sun-Crowned", "Walking Ballista"],
        family: ResourceFamily::Damage,
        win_kind: WinKind::LethalDamage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Mikaeus, the Unhallowed + Triskelion",
        cards: &["Mikaeus, the Unhallowed", "Triskelion"],
        family: ResourceFamily::Damage,
        win_kind: WinKind::LethalDamage,
        gated_on: None,
        deferral: Some(DeferralBucket::ObjectReentry),
    },
    ComboRow {
        name: "Sanguine Bond + Exquisite Blood",
        cards: &["Sanguine Bond", "Exquisite Blood"],
        family: ResourceFamily::Drain,
        win_kind: WinKind::LethalDamage,
        gated_on: None,
        deferral: None,
    },
    ComboRow {
        name: "Marauding Blight-Priest + Bloodthirsty Conqueror",
        cards: &["Marauding Blight-Priest", "Bloodthirsty Conqueror"],
        family: ResourceFamily::Drain,
        win_kind: WinKind::LethalDamage,
        gated_on: None,
        deferral: None,
    },
    ComboRow {
        name: "Niv-Mizzet, the Firemind + Curiosity",
        cards: &["Niv-Mizzet, the Firemind", "Curiosity"],
        family: ResourceFamily::DrawDamage,
        win_kind: WinKind::LethalDamage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Blasphemous Act + Repercussion",
        cards: &["Blasphemous Act", "Repercussion"],
        family: ResourceFamily::Damage,
        win_kind: WinKind::LethalDamage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Professor Onyx + Chain of Smog",
        cards: &["Professor Onyx", "Chain of Smog"],
        family: ResourceFamily::Drain,
        win_kind: WinKind::LethalDamage,
        gated_on: Some("Professor Onyx"),
        deferral: None,
    },
    ComboRow {
        name: "Kiki-Jiki, Mirror Breaker + Zealous Conscripts",
        cards: &["Kiki-Jiki, Mirror Breaker", "Zealous Conscripts"],
        family: ResourceFamily::Tokens,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ObjectReentry),
    },
    ComboRow {
        name: "Splinter Twin + Deceiver Exarch",
        cards: &["Splinter Twin", "Deceiver Exarch"],
        family: ResourceFamily::Tokens,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ObjectReentry),
    },
    ComboRow {
        name: "Midnight Guard + Presence of Gond",
        cards: &["Midnight Guard", "Presence of Gond"],
        family: ResourceFamily::Tokens,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ObjectReentry),
    },
    ComboRow {
        name: "Scurry Oak + Ivy Lane Denizen",
        cards: &["Scurry Oak", "Ivy Lane Denizen"],
        family: ResourceFamily::Tokens,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ObjectReentry),
    },
    ComboRow {
        name: "Dualcaster Mage + Twinflame",
        cards: &["Dualcaster Mage", "Twinflame"],
        family: ResourceFamily::Tokens,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Felidar Guardian + Saheeli Rai",
        cards: &["Felidar Guardian", "Saheeli Rai"],
        family: ResourceFamily::Tokens,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ObjectReentry),
    },
    ComboRow {
        name: "Basking Broodscale + Rosie Cotton of South Lane",
        cards: &["Basking Broodscale", "Rosie Cotton of South Lane"],
        family: ResourceFamily::Tokens,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Ratadrabik of Urborg + Boromir, Warden of the Tower",
        cards: &["Ratadrabik of Urborg", "Boromir, Warden of the Tower"],
        family: ResourceFamily::Tokens,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Niv-Mizzet, Parun + Ophidian Eye",
        cards: &["Niv-Mizzet, Parun", "Ophidian Eye"],
        family: ResourceFamily::Draw,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Narset's Reversal + Twinning Staff",
        cards: &["Narset's Reversal", "Twinning Staff"],
        family: ResourceFamily::Draw,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Aggravated Assault + Sword of Feast and Famine",
        cards: &["Aggravated Assault", "Sword of Feast and Famine"],
        family: ResourceFamily::Combat,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ExtraTurnOrCombat),
    },
    ComboRow {
        name: "Combat Celebrant + Helm of the Host",
        cards: &["Combat Celebrant", "Helm of the Host"],
        family: ResourceFamily::Combat,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ExtraTurnOrCombat),
    },
    ComboRow {
        name: "Time Sieve + Thopter Assembly",
        cards: &["Time Sieve", "Thopter Assembly"],
        family: ResourceFamily::Turns,
        win_kind: WinKind::ExtraTurns,
        gated_on: None,
        deferral: Some(DeferralBucket::ExtraTurnOrCombat),
    },
    ComboRow {
        name: "Lotus Cobra + Springheart Nantuko",
        cards: &["Lotus Cobra", "Springheart Nantuko"],
        family: ResourceFamily::Landfall,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Ashaya, Soul of the Wild + Quirion Ranger",
        cards: &["Ashaya, Soul of the Wild", "Quirion Ranger"],
        family: ResourceFamily::Landfall,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Scute Swarm + Retreat to Coralhelm",
        cards: &["Scute Swarm", "Retreat to Coralhelm"],
        family: ResourceFamily::Landfall,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Worldgorger Dragon + Animate Dead",
        cards: &["Worldgorger Dragon", "Animate Dead"],
        family: ResourceFamily::Engine,
        win_kind: WinKind::Advantage,
        gated_on: Some("Animate Dead"),
        deferral: None,
    },
    ComboRow {
        name: "Food Chain + Eternal Scourge",
        cards: &["Food Chain", "Eternal Scourge"],
        family: ResourceFamily::Engine,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ObjectReentry),
    },
    ComboRow {
        name: "Tidespout Tyrant + Sol Ring",
        cards: &["Tidespout Tyrant", "Sol Ring"],
        family: ResourceFamily::Engine,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Aetherflux Reservoir + Bolas's Citadel + Sensei's Divining Top",
        cards: &[
            "Aetherflux Reservoir",
            "Bolas's Citadel",
            "Sensei's Divining Top",
        ],
        family: ResourceFamily::Damage,
        win_kind: WinKind::LethalDamage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Abdel Adrian + Restoration Angel + Ephemerate",
        cards: &[
            "Abdel Adrian, Gorion's Ward",
            "Restoration Angel",
            "Ephemerate",
        ],
        family: ResourceFamily::Tokens,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Underworld Breach + Lion's Eye Diamond + Brain Freeze",
        cards: &["Underworld Breach", "Lion's Eye Diamond", "Brain Freeze"],
        family: ResourceFamily::Mill,
        win_kind: WinKind::Decking,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Gravecrawler + Phyrexian Altar + Blood Artist",
        cards: &["Gravecrawler", "Phyrexian Altar", "Blood Artist"],
        family: ResourceFamily::Death,
        win_kind: WinKind::LethalDamage,
        gated_on: None,
        deferral: Some(DeferralBucket::ObjectReentry),
    },
    ComboRow {
        name: "Karmic Guide + Reveillark + Viscera Seer",
        cards: &["Karmic Guide", "Reveillark", "Viscera Seer"],
        family: ResourceFamily::Death,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ObjectReentry),
    },
    ComboRow {
        name: "Chatterfang + Warren Soultrader + Academy Manufactor",
        cards: &[
            "Chatterfang, Squirrel General",
            "Warren Soultrader",
            "Academy Manufactor",
        ],
        family: ResourceFamily::Death,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Reassembling Skeleton + Ashnod's Altar + Nim Deathmantle",
        cards: &["Reassembling Skeleton", "Ashnod's Altar", "Nim Deathmantle"],
        family: ResourceFamily::Death,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ObjectReentry),
    },
    ComboRow {
        name: "Thopter Foundry + Sword of the Meek + Krark-Clan Ironworks",
        cards: &[
            "Thopter Foundry",
            "Sword of the Meek",
            "Krark-Clan Ironworks",
        ],
        family: ResourceFamily::Engine,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
    ComboRow {
        name: "Spike Feeder + Archangel of Thune",
        cards: &["Spike Feeder", "Archangel of Thune"],
        family: ResourceFamily::Counters,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: None,
    },
    ComboRow {
        name: "Earthcraft + Squirrel Nest",
        cards: &["Earthcraft", "Squirrel Nest"],
        family: ResourceFamily::Tokens,
        win_kind: WinKind::Advantage,
        gated_on: None,
        deferral: Some(DeferralBucket::ObjectReentry),
    },
    ComboRow {
        name: "Grindstone + Painter's Servant",
        cards: &["Grindstone", "Painter's Servant"],
        family: ResourceFamily::Mill,
        win_kind: WinKind::Decking,
        gated_on: Some("Grindstone"),
        deferral: None,
    },
    ComboRow {
        name: "Helm of Obedience + Rest in Peace",
        cards: &["Helm of Obedience", "Rest in Peace"],
        family: ResourceFamily::Mill,
        win_kind: WinKind::Decking,
        gated_on: None,
        deferral: Some(DeferralBucket::Other),
    },
];

#[cfg(test)]
impl ResourceFamily {
    /// The concrete [`ResourceAxis`] (against opponent `P1` where the family is
    /// directed at an opponent) this family is expected to name. The shape-lock
    /// meta-test asserts it is total over the enum; only the test build needs it.
    pub(crate) fn expected_axis(self) -> ResourceAxis {
        match self {
            ResourceFamily::Mana => ResourceAxis::Mana(ManaType::Colorless),
            ResourceFamily::Tokens => ResourceAxis::TokensCreated,
            ResourceFamily::Damage => ResourceAxis::DamageDealt(P1),
            ResourceFamily::Drain => ResourceAxis::Life(P1),
            ResourceFamily::Mill => ResourceAxis::LibraryDelta(P1),
            ResourceFamily::Death => ResourceAxis::DeathTriggers,
            ResourceFamily::Landfall => ResourceAxis::LandfallTriggers,
            ResourceFamily::Draw => ResourceAxis::CardsDrawn,
            ResourceFamily::DrawDamage => ResourceAxis::DamageDealt(P1),
            ResourceFamily::Combat => ResourceAxis::CombatPhases,
            ResourceFamily::Turns => ResourceAxis::ExtraTurns,
            ResourceFamily::Counters => ResourceAxis::Counter(
                crate::analysis::resource::CounterClass::Plus1Plus1,
                crate::analysis::resource::ObjectClass::Creature,
            ),
            ResourceFamily::Proliferate => {
                ResourceAxis::Trigger(crate::analysis::resource::TriggerKind::Proliferate)
            }
            ResourceFamily::Engine => ResourceAxis::Mana(ManaType::Colorless),
        }
    }
}

/// Whether `axis` belongs to `family` (color/opponent/counter-class agnostic).
/// Shared single authority: the CLI classifier ([`classify_status`]) and the
/// `#[cfg(test)]` `assert_combo` both use it to decide whether a driven
/// certificate names a documented-family axis.
pub(crate) fn family_matches_axis(family: ResourceFamily, axis: &ResourceAxis) -> bool {
    use ResourceFamily as F;
    match family {
        F::Mana => matches!(axis, ResourceAxis::Mana(_)),
        F::Tokens => matches!(axis, ResourceAxis::TokensCreated),
        F::Damage | F::DrawDamage => matches!(axis, ResourceAxis::DamageDealt(_)),
        F::Drain => matches!(axis, ResourceAxis::Life(_) | ResourceAxis::DamageDealt(_)),
        F::Mill => matches!(axis, ResourceAxis::LibraryDelta(_)),
        F::Death => matches!(
            axis,
            ResourceAxis::DeathTriggers
                | ResourceAxis::SacTriggers
                | ResourceAxis::LtbTriggers
                | ResourceAxis::TokensCreated
                | ResourceAxis::DamageDealt(_)
                | ResourceAxis::Life(_)
        ),
        F::Landfall => matches!(
            axis,
            ResourceAxis::LandfallTriggers | ResourceAxis::EtbTriggers | ResourceAxis::Mana(_)
        ),
        F::Draw => matches!(
            axis,
            ResourceAxis::CardsDrawn | ResourceAxis::DamageDealt(_)
        ),
        F::Combat => matches!(axis, ResourceAxis::CombatPhases),
        F::Turns => matches!(axis, ResourceAxis::ExtraTurns),
        F::Counters => matches!(axis, ResourceAxis::Counter(_, _) | ResourceAxis::Life(_)),
        F::Proliferate => matches!(axis, ResourceAxis::Trigger(_)),
        F::Engine => true, // engine combos pump heterogeneous axes (mana/ETB/tokens/…)
    }
}

// ===========================================================================
// CLI classification surface (tooling types — NOT engine enums).
// ===========================================================================

/// Per-row classification result of [`drive_row`].
#[derive(Debug)]
pub enum RowStatus {
    /// Driven through the existing detector; cert/family/win_kind match the row.
    Confirmed {
        unbounded: Vec<ResourceAxis>,
        win_kind: WinKind,
    },
    /// Driver ran but produced no/mismatched confirmation — a regression.
    Failed { detail: String },
    /// Card-gated on an unimplemented card (the 4 gated rows).
    Gated { card: &'static str },
    /// Testable but no driver yet (measured structural bucket).
    Deferred { bucket: DeferralBucket },
}

/// The derived, read-only report the CLI consumes for one corpus row.
#[derive(Debug)]
pub struct RowReport {
    pub name: &'static str,
    pub expected_family: ResourceFamily,
    pub expected_win_kind: WinKind,
    pub status: RowStatus,
}

/// Confirmation mechanism for a driven row.
///
/// `Offline` drives the combo's bespoke action cycle and classifies the offline
/// [`detect_loop`] certificate; `LiveDrain` drives the two drain cascades through
/// the per-beat `apply(PassPriority)` reducer and classifies the live `GameOver`.
pub(crate) enum ComboDriver {
    Offline(fn(&CardDatabase) -> Option<LoopCertificate>),
    LiveDrain,
}

/// Static map `idx -> driver` for the 12 confirmable rows. The single source of
/// truth for "which rows are driven" (the `#[cfg(test)]` meta/partition tests read
/// it, so adding a driver here is automatically reflected — no hand-listed index
/// array to drift).
pub(crate) const DRIVERS: &[(usize, ComboDriver)] = &[
    (0, ComboDriver::Offline(drive_offline_heliod_ballista)),
    (1, ComboDriver::Offline(drive_offline_kilo_freed_relic)),
    (4, ComboDriver::Offline(drive_offline_grim_power)),
    (6, ComboDriver::Offline(drive_offline_devoted_vizier)),
    (9, ComboDriver::Offline(drive_offline_bloom_freed)),
    (10, ComboDriver::Offline(drive_offline_priest_umbral)),
    (12, ComboDriver::Offline(drive_offline_selvala_staff)),
    (13, ComboDriver::Offline(drive_offline_faeburrow_pemmin)),
    (14, ComboDriver::Offline(drive_offline_marwyn_sword)),
    (17, ComboDriver::LiveDrain),
    (18, ComboDriver::LiveDrain),
    (49, ComboDriver::Offline(drive_offline_spike_archangel)),
];

/// Number of rows in the corpus.
pub fn corpus_len() -> usize {
    CORPUS.len()
}

/// The row at `idx` (panics if out of range — callers iterate `0..corpus_len()`).
pub fn row(idx: usize) -> &'static ComboRow {
    &CORPUS[idx]
}

/// Classify an OFFLINE driver outcome against the row's spec.
///
/// A certificate confirms the row ONLY when its `win_kind` matches AND it names ≥1
/// axis of the documented family. A `None` outcome (no certificate) or any spec
/// mismatch is a `Failed` — this is the comparison the revert-probe pins: dropping
/// the `win_kind == row.win_kind && family_matches_axis(..)` check (rubber-stamping
/// any `Some(cert)`), or treating `None` as confirmed, flips a Failed to Confirmed.
pub(crate) fn classify_status(row: &ComboRow, outcome: Option<LoopCertificate>) -> RowStatus {
    match outcome {
        Some(cert)
            if cert.win_kind == row.win_kind
                && cert
                    .unbounded
                    .iter()
                    .any(|a| family_matches_axis(row.family, a)) =>
        {
            RowStatus::Confirmed {
                unbounded: cert.unbounded,
                win_kind: cert.win_kind,
            }
        }
        Some(cert) => RowStatus::Failed {
            detail: format!(
                "cert {:?}/{:?} did not match expected {:?}/{:?}",
                cert.win_kind, cert.unbounded, row.win_kind, row.family
            ),
        },
        None => RowStatus::Failed {
            detail: "driver produced no certificate".to_string(),
        },
    }
}

/// Classify a LIVE drain outcome: the per-beat drive must reach `GameOver` with the
/// loop's controller (P0) as winner, and the row must be a `LethalDamage` drain.
/// A wrong winner or a `None` (no `GameOver`) is a `Failed` — the
/// `classify_live_compares_winner_not_rubber_stamp` revert-probe pins this so a
/// reverted `Some(_) => Confirmed` rubber-stamp fails a test.
pub(crate) fn classify_live(row: &ComboRow, outcome: Option<(usize, PlayerId)>) -> RowStatus {
    match outcome {
        // CR 704.5a: the live drain cascade eliminates the victim opponent; the
        // surviving controller P0 is the winner. The axis is the opponent's life.
        Some((_beat, winner)) if winner == P0 && row.win_kind == WinKind::LethalDamage => {
            RowStatus::Confirmed {
                unbounded: vec![ResourceAxis::Life(P1)],
                win_kind: WinKind::LethalDamage,
            }
        }
        Some((_beat, winner)) => RowStatus::Failed {
            detail: format!(
                "live drain winner {winner:?}/win_kind {:?} mismatched expected P0/{:?}",
                WinKind::LethalDamage,
                row.win_kind
            ),
        },
        None => RowStatus::Failed {
            detail: "live drain produced no GameOver within the driven window".to_string(),
        },
    }
}

/// THE shared entry point both the `#[cfg(test)]` tests and the CLI call. Pure
/// dispatch (no game logic): gated → `Gated`; a driven row → run its driver and
/// classify; otherwise → `Deferred` with the row's declared structural bucket.
pub fn drive_row(db: &CardDatabase, idx: usize) -> RowReport {
    let row = &CORPUS[idx];
    let status = if let Some(card) = row.gated_on {
        RowStatus::Gated { card }
    } else if let Some(driver) = DRIVERS.iter().find(|(i, _)| *i == idx).map(|(_, d)| d) {
        match driver {
            ComboDriver::Offline(f) => classify_status(row, f(db)),
            ComboDriver::LiveDrain => classify_live(row, drive_live_drain(db, idx)),
        }
    } else {
        match row.deferral {
            Some(bucket) => RowStatus::Deferred { bucket },
            // Unreachable while the partition shape-lock holds; defensive so a data
            // gap surfaces as a FAIL rather than a silent miss.
            None => RowStatus::Failed {
                detail: "row is neither driven, gated, nor deferred".to_string(),
            },
        }
    };
    RowReport {
        name: row.name,
        expected_family: row.family,
        expected_win_kind: row.win_kind,
        status,
    }
}

// ===========================================================================
// Real-card infrastructure: build combo boards from the actual parsed card-data,
// so a driven loop exercises the cards' real abilities. Every helper is
// parameterized on `db: &CardDatabase` (the test passes the committed fixture,
// the CLI the full export); a missing card returns `None` rather than panicking.
// ===========================================================================

/// Instantiate a real card by name directly onto `player`'s battlefield, with its
/// abilities/triggers/statics parsed from the export. Already-resolved (not
/// summoning-sick), so its activated abilities are usable the same turn. Returns
/// the new object's id, or `None` if the card is absent from the export.
pub(crate) fn install_on_battlefield(
    state: &mut GameState,
    db: &CardDatabase,
    name: &str,
    player: PlayerId,
) -> Option<ObjectId> {
    use crate::game::printed_cards::apply_card_face_to_object;
    use crate::types::identifiers::CardId;
    use crate::types::zones::Zone;

    let face = db.get_face_by_name(name)?;
    let card_id = CardId(state.next_object_id);
    let id = crate::game::zones::create_object(
        state,
        card_id,
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let ts = state.next_timestamp();
    {
        let obj = state.objects.get_mut(&id)?;
        apply_card_face_to_object(obj, face);
        // CR 302.6: a pre-existing battlefield permanent is not summoning-sick.
        obj.summoning_sick = false;
        obj.entered_battlefield_turn = Some(state.turn_number.saturating_sub(1));
        obj.timestamp = ts;
    }
    // CR 603.6: index the installed object's triggers so they fire during play.
    crate::game::trigger_index::reindex_object_triggers(state, id);
    Some(id)
}

/// Outcome of installing a combo: the runner plus the installed permanents in the
/// order their card names were given.
pub(crate) struct ComboBoard {
    pub(crate) runner: GameRunner,
    pub(crate) ids: Vec<ObjectId>,
}

/// Build a board with the named permanents installed on P0's battlefield, a large
/// finite mana pool floated (so mana-cost abilities can pay, while a mana-GAIN
/// axis is still measurable — not `debug_infinite_mana`), and layers settled.
/// `None` if any name is missing from `db`. Auras are installed but NOT
/// auto-attached (each driver attaches them to the correct host).
pub(crate) fn build_board(db: &CardDatabase, cards: &[&str]) -> Option<ComboBoard> {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 40);
    scenario.with_life(P1, 40);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
    }
    let mut ids = Vec::new();
    {
        let state = runner.state_mut();
        for &name in cards {
            ids.push(install_on_battlefield(state, db, name, P0)?);
        }
        float_mana(state, 500);
        settle_layers(state);
    }
    Some(ComboBoard { runner, ids })
}

/// Like [`build_board`], but floats GREEN-ONLY into P0's pool (no WUBRG+C pool).
/// For a green producer whose untap/activation costs are generic ({3} etc.), the
/// generic must be paid from the producer's own color so no per-color axis goes
/// net-negative: a floated colorless pool (CR 106.1/106.4) would be drained by the
/// generic costs and never replenished (these producers can't make colorless),
/// reading as a spurious per-color deficit the detector's `is_progress` rightly
/// rejects. Same trick `build_board_with_vanilla` uses for Selvala.
pub(crate) fn build_board_green(db: &CardDatabase, cards: &[&str]) -> Option<ComboBoard> {
    let mut board = build_board(db, cards)?;
    {
        let state = board.runner.state_mut();
        // CR 106.4: replace the WUBRG+C pool floated by `build_board` with a
        // green-only pool so the producer's generic costs draw from green.
        state.players[0].mana_pool.clear();
        float_single_color(state, ManaType::Green, 500);
        settle_layers(state);
    }
    Some(board)
}

/// Like [`build_board`], but first places a vanilla `power`/`toughness` creature
/// on P0's battlefield (so a power-scaling mana producer like Selvala reads a high
/// X). The vanilla creature is installed BEFORE the named combo cards, so the
/// combo-card ids are still `ids[0..]` in card order. Returns the board plus the
/// vanilla creature's id appended LAST in `ids`. `None` if any name is missing.
pub(crate) fn build_board_with_vanilla(
    db: &CardDatabase,
    cards: &[&str],
    power: i32,
    toughness: i32,
) -> Option<ComboBoard> {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 40);
    scenario.with_life(P1, 40);
    // CR 208.2: a high-power vanilla so a "greatest power among creatures you
    // control" producer reads a large X (the combo's documented prerequisite).
    let vanilla = scenario.add_vanilla(P0, power, toughness);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
    }
    let mut ids = Vec::new();
    {
        let state = runner.state_mut();
        for &name in cards {
            ids.push(install_on_battlefield(state, db, name, P0)?);
        }
        ids.push(vanilla);
        // Float GREEN only (not a full WUBRG+C pool): Selvala produces green and
        // its untap-chain costs are generic, so paying generic from green keeps
        // every per-color axis ≥ 0. A floated colorless pool would be consumed by
        // the generic costs and never replenished (Selvala can't make colorless),
        // a spurious per-color deficit that the detector rightly rejects.
        float_single_color(state, ManaType::Green, 500);
        settle_layers(state);
    }
    Some(ComboBoard { runner, ids })
}

/// Float `n` mana of a single `color` into P0's pool from a sentinel source.
/// Used by combos whose producer makes a specific color and whose costs are
/// generic, so paying generic from the same color avoids a spurious per-color
/// deficit (the detector's `is_progress` rejects any net-negative color).
fn float_single_color(state: &mut GameState, color: ManaType, n: usize) {
    for _ in 0..n {
        state.players[0]
            .mana_pool
            .add(crate::types::mana::ManaUnit::new(
                color,
                ObjectId(0),
                false,
                Vec::new(),
            ));
    }
}

/// Float `n` of each WUBRG+C mana into P0's pool from a sentinel source.
fn float_mana(state: &mut GameState, n: usize) {
    for color in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
        ManaType::Colorless,
    ] {
        for _ in 0..n {
            state.players[0]
                .mana_pool
                .add(crate::types::mana::ManaUnit::new(
                    color,
                    ObjectId(0),
                    false,
                    Vec::new(),
                ));
        }
    }
}

/// CR 613: mark layers dirty and recompute so granted keywords / aura effects /
/// counter-derived P/T apply before the loop is driven.
pub(crate) fn settle_layers(state: &mut GameState) {
    state.layers_dirty.mark_full();
    crate::game::layers::evaluate_layers(state);
}

/// Attach `aura` to `host` (CR 303.4): set both sides of the relationship and
/// re-settle layers so the aura's static/granted effects apply.
pub(crate) fn attach_aura(state: &mut GameState, aura: ObjectId, host: ObjectId) {
    if let Some(o) = state.objects.get_mut(&aura) {
        o.attached_to = Some(crate::game::game_object::AttachTarget::Object(host));
    }
    if let Some(h) = state.objects.get_mut(&host) {
        if !h.attachments.contains(&aura) {
            h.attachments.push(aura);
        }
    }
    settle_layers(state);
}

/// Index of `source`'s first ability whose effect matches `pred`. Reads the live
/// (post-layer) ability list so a granted ability is found too.
pub(crate) fn ability_index_where(
    state: &GameState,
    source: ObjectId,
    pred: impl Fn(&crate::types::ability::Effect) -> bool,
) -> Option<usize> {
    state
        .objects
        .get(&source)?
        .abilities
        .iter()
        .position(|a| pred(&a.effect))
}

/// Activate `source`'s ability `index`, then resolve the whole stack to a clean
/// priority window, answering any prompt by choosing `prefer_target` (or the
/// first legal target). Returns `false` if the activation is rejected.
pub(crate) fn activate_and_resolve(
    probe: &mut LoopProbe,
    source: ObjectId,
    index: usize,
    prefer_target: Option<TargetRef>,
) -> bool {
    if probe
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
        })
        .is_err()
    {
        return false;
    }
    resolve_to_priority(probe, prefer_target);
    true
}

/// Drive the stack to a clean priority window, auto-answering target / X prompts.
/// Prefers `prefer_target` when it is a currently-legal target; otherwise the
/// first legal target. Bounded so a stuck state can't hang the test.
pub(crate) fn resolve_to_priority(probe: &mut LoopProbe, prefer_target: Option<TargetRef>) {
    for _ in 0..32 {
        match &probe.runner().state().waiting_for {
            WaitingFor::Priority { .. } if probe.runner().state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                if probe.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::TargetSelection { selection, .. }
            | WaitingFor::TriggerTargetSelection { selection, .. } => {
                let legal = &selection.current_legal_targets;
                let pick = prefer_target
                    .clone()
                    .filter(|t| legal.contains(t))
                    .or_else(|| legal.first().cloned());
                let action = match pick {
                    Some(t) => GameAction::ChooseTarget { target: Some(t) },
                    None => GameAction::ChooseTarget { target: None },
                };
                if probe.act(action).is_err() {
                    break;
                }
            }
            WaitingFor::ChooseXValue { .. } => {
                if probe.act(GameAction::ChooseX { value: 1 }).is_err() {
                    break;
                }
            }
            WaitingFor::ChooseManaColor { choice, .. } => {
                use crate::types::game_state::{ManaChoice, ManaChoicePrompt};
                // Answer must MATCH the prompt shape, or the engine rejects it and
                // the state stays stuck (the bug a single-color answer hit for an
                // X-mana `AnyCombination` producer like Selvala). Pick Blue for
                // each unit (Blue pays {U} untap costs; with a large floated pool
                // the exact color is otherwise immaterial).
                let answer = match choice {
                    ManaChoicePrompt::SingleColor { .. } => GameAction::ChooseManaColor {
                        choice: ManaChoice::SingleColor(ManaType::Blue),
                        count: 1,
                    },
                    // An X-mana "any combination" producer wants one color per
                    // produced unit — answer with `count` Greens. Green is chosen
                    // (not Blue) so a green-cost producer like Selvala re-pays its
                    // own {G} from its production and the generic untap-chain costs
                    // draw from the same green surplus, keeping every per-color
                    // axis ≥ 0 (the detector rejects any color that goes
                    // net-negative).
                    ManaChoicePrompt::AnyCombination { count, .. } => GameAction::ChooseManaColor {
                        choice: ManaChoice::Combination(vec![ManaType::Green; *count]),
                        count: 1,
                    },
                    // Filter-land "pick one complete combination": take the first.
                    ManaChoicePrompt::Combination { options } => {
                        let combo = options.first().cloned().unwrap_or_default();
                        GameAction::ChooseManaColor {
                            choice: ManaChoice::Combination(combo),
                            count: 1,
                        }
                    }
                };
                if probe.act(answer).is_err() {
                    break;
                }
            }
            WaitingFor::PayCost { choices, count, .. } => {
                // Choose `count` objects to pay the cost (tap-creatures /
                // sacrifice / exile). Prefer `prefer_target`'s object if it is a
                // legal choice (so a "tap a creature" cost taps the intended one).
                let want = match &prefer_target {
                    Some(TargetRef::Object(o)) if choices.contains(o) => Some(*o),
                    _ => None,
                };
                let mut chosen: Vec<ObjectId> = Vec::new();
                if let Some(o) = want {
                    chosen.push(o);
                }
                for &c in choices {
                    if chosen.len() >= *count {
                        break;
                    }
                    if !chosen.contains(&c) {
                        chosen.push(c);
                    }
                }
                if probe
                    .act(GameAction::SelectCards { cards: chosen })
                    .is_err()
                {
                    break;
                }
            }
            // CR 608.2d: accept a beneficial resolution-time "may" choice (the
            // optional part of a "you may …" ability) so the loop's loop-closing
            // action proceeds — e.g. Sword of the Paruns' "{3}: You may tap or
            // untap equipped creature." Generalizes the optional-effect class.
            WaitingFor::OptionalEffectChoice { .. } => {
                if probe
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .is_err()
                {
                    break;
                }
            }
            // CR 608.2d: a resolution-time "choose one of A or B" (e.g. a "tap or
            // untap" ability, parsed to `Effect::ChooseOneOf`). Pick the branch
            // whose effect is `SetTapState { Untap }`; fall back to the last branch
            // if none match. Generalizes the modal-untap class, not one card.
            WaitingFor::ChooseOneOfBranch { branches, .. } => {
                use crate::types::ability::{Effect, TapStateChange};
                let index = branches
                    .iter()
                    .position(|b| {
                        matches!(
                            *b.effect,
                            Effect::SetTapState {
                                state: TapStateChange::Untap,
                                ..
                            }
                        )
                    })
                    .unwrap_or(branches.len().saturating_sub(1));
                if probe.act(GameAction::ChooseBranch { index }).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// Run a combo driver: `step` drives exactly one loop iteration's actions. The
/// harness warms up `WARMUP` cycles, then measures up to `STEADY` steady cycles,
/// returning the first confirmed certificate.
pub(crate) fn run_combo<S>(board: ComboBoard, mut step: S) -> Option<LoopCertificate>
where
    S: FnMut(&mut LoopProbe),
{
    const WARMUP: usize = 2;
    const STEADY: usize = 3;
    let mut runner = board.runner;
    let mut probe = LoopProbe::new(&mut runner);
    for _ in 0..WARMUP {
        step(&mut probe);
        let _ = probe.iteration_delta();
    }
    for _ in 0..STEADY {
        let start = probe.runner().state().clone();
        // CR 606.3 / CR 704.5a: the loop's controller scopes the consumed-axis and
        // win classification. Every combo scenario is built with the active player
        // (P0) controlling the engine.
        let controller = probe.runner().state().active_player;
        step(&mut probe);
        let delta = probe.iteration_delta();
        let end = probe.runner().state().clone();
        // Activated-ability loops are optional (CR 602.1), so `mandatory = false`.
        if let Some(cert) = detect_loop(&start, &end, &delta, controller, false) {
            return Some(cert);
        }
    }
    None
}

// Effect predicates shared by drivers.
pub(crate) fn is_mana_effect(e: &crate::types::ability::Effect) -> bool {
    matches!(e, crate::types::ability::Effect::Mana { .. })
}
pub(crate) fn is_untap_effect(e: &crate::types::ability::Effect) -> bool {
    use crate::types::ability::{Effect, TapStateChange};
    matches!(
        e,
        Effect::SetTapState {
            state: TapStateChange::Untap,
            ..
        }
    )
}
/// An untap effect that untaps the SOURCE itself (`SelfRef`) — e.g. Staff of
/// Domination's "{1}: Untap this artifact".
fn is_self_untap_effect(e: &crate::types::ability::Effect) -> bool {
    use crate::types::ability::{Effect, TapStateChange, TargetFilter};
    matches!(
        e,
        Effect::SetTapState {
            state: TapStateChange::Untap,
            target: TargetFilter::SelfRef,
            ..
        }
    )
}
/// An untap effect that untaps a *targeted* creature (a non-`SelfRef` filter) —
/// e.g. Staff of Domination's "{3}, {T}: Untap target creature".
fn is_target_creature_untap_effect(e: &crate::types::ability::Effect) -> bool {
    use crate::types::ability::{Effect, TapStateChange, TargetFilter};
    matches!(
        e,
        Effect::SetTapState {
            state: TapStateChange::Untap,
            target,
            ..
        } if !matches!(target, TargetFilter::SelfRef)
    )
}

/// Install `count` vanilla creatures of `subtype` on P0's battlefield (CR 205.3:
/// the subtype is what a subtype-counting filter matches). Seeded directly onto
/// the post-`build()` board (no enters trigger). Settles layers so the count is
/// live.
pub(crate) fn seed_subtype_creatures(state: &mut GameState, subtype: &str, count: usize) {
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::zones::Zone;
    for i in 0..count {
        let card_id = CardId(state.next_object_id);
        let id = crate::game::zones::create_object(
            state,
            card_id,
            P0,
            format!("{subtype} {i}"),
            Zone::Battlefield,
        );
        if let Some(o) = state.objects.get_mut(&id) {
            o.card_types.core_types.push(CoreType::Creature);
            o.card_types.subtypes.push(subtype.to_string());
            o.base_card_types = o.card_types.clone();
            o.power = Some(1);
            o.toughness = Some(1);
            o.base_power = Some(1);
            o.base_toughness = Some(1);
            // CR 302.6: a pre-existing battlefield creature is not summoning-sick.
            o.summoning_sick = false;
        }
    }
    settle_layers(state);
}

// ===========================================================================
// Offline drivers (real cards, real `apply()` pipeline). Each returns the
// `detect_loop` certificate (or `None` if a card/ability is absent), so the
// `#[cfg(test)]` per-combo tests assert against it and the CLI classifies it.
// ===========================================================================

/// HELIOD, SUN-CROWNED + WALKING BALLISTA — the canonical driving combo, driven
/// end-to-end through the real `apply()` pipeline with the cards' actual parsed
/// abilities. The repeating cycle: Ballista removes a +1/+1 counter to deal 1 to
/// the opponent → lifelink gains 1 life → Heliod returns the counter; board
/// identical, +1 damage and +1 life per cycle.
pub(crate) fn drive_offline_heliod_ballista(db: &CardDatabase) -> Option<LoopCertificate> {
    use crate::types::ability::Effect;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 40); // survive many pings within the window
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
    }

    let ballista = {
        let state = runner.state_mut();
        let _heliod = install_on_battlefield(state, db, "Heliod, Sun-Crowned", P0)?;
        let ballista = install_on_battlefield(state, db, "Walking Ballista", P0)?;
        // Pre-loop setup (one-time, not part of the repeating cycle): Ballista
        // carries +1/+1 counters to remove (the loop refills them), and is granted
        // lifelink (Heliod's {1}{W} ability does this once before the loop).
        {
            let obj = state.objects.get_mut(&ballista)?;
            // Start with 2 counters: removing one to ping leaves Ballista a live
            // 1/1 (not a 0/0 that dies to CR 704.5f before the Heliod trigger can
            // return the counter), and Heliod's trigger refills it to 2.
            obj.counters
                .insert(crate::types::counter::CounterType::Plus1Plus1, 2);
            // Grant lifelink on the BASE keywords so `evaluate_layers` (which
            // rebuilds `keywords` from `base_keywords` + layer effects) preserves
            // it — pushing only onto `keywords` would be wiped by the recompute.
            if !obj
                .base_keywords
                .contains(&crate::types::keywords::Keyword::Lifelink)
            {
                obj.base_keywords
                    .push(crate::types::keywords::Keyword::Lifelink);
                obj.keywords.push(crate::types::keywords::Keyword::Lifelink);
            }
        }
        // CR 613: recompute layers so the granted keyword / counters take effect.
        state.layers_dirty.mark_full();
        crate::game::layers::evaluate_layers(state);
        ballista
    };

    // Find Ballista's "Remove a +1/+1 counter: deal 1 damage to any target"
    // ability index by its deal-damage effect, so a card-data re-parse that
    // reorders abilities does not break the driver.
    let remove_counter_idx = {
        let obj = &runner.state().objects[&ballista];
        obj.abilities
            .iter()
            .position(|a| matches!(*a.effect, Effect::DealDamage { .. }))?
    };

    let mut probe = LoopProbe::new(&mut runner);

    // WARMUP one full cycle to saturate per-turn bookkeeping, then compare two
    // steady-state iterations.
    drive_ballista_ping(&mut probe, ballista, remove_counter_idx);
    let _ = probe.iteration_delta();

    let cycle_start = probe.runner().state().clone();
    drive_ballista_ping(&mut probe, ballista, remove_counter_idx);
    let delta = probe.iteration_delta();
    let cycle_end = probe.runner().state().clone();

    detect_loop(&cycle_start, &cycle_end, &delta, P0, false)
}

/// Drive one Walking Ballista "remove a +1/+1 counter: deal 1 to any target"
/// activation at the opponent, then resolve everything on the stack (the ping AND
/// the Heliod lifegain trigger that returns the counter).
fn drive_ballista_ping(probe: &mut LoopProbe, ballista: ObjectId, ability_index: usize) {
    let activated = probe
        .act(GameAction::ActivateAbility {
            source_id: ballista,
            ability_index,
        })
        .expect("activate Ballista remove-counter ability");
    // The ability targets "any target"; choose the opponent.
    if matches!(activated.waiting_for, WaitingFor::TargetSelection { .. }) {
        probe
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("target opponent with Ballista");
    }
    // CR 608: resolve the ping, then the Heliod lifegain trigger (which itself
    // targets a creature you control). Pass priority / select trigger targets
    // until the stack empties.
    for _ in 0..16 {
        if probe.runner().state().stack.is_empty()
            && matches!(
                probe.runner().state().waiting_for,
                WaitingFor::Priority { .. }
            )
        {
            break;
        }
        match &probe.runner().state().waiting_for {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                // Heliod's counter-return trigger: target Ballista (a creature you
                // control), so the board returns identical.
                if probe
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(ballista)],
                    })
                    .is_err()
                {
                    break;
                }
            }
            _ => {
                if probe.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

/// #4 DEVOTED DRUID + VIZIER OF REMEDIES — infinite green mana. Untap Druid (free
/// under Vizier's -1/-1 replacement), tap for {G}; board identical, +1 {G}/cycle.
pub(crate) fn drive_offline_devoted_vizier(db: &CardDatabase) -> Option<LoopCertificate> {
    let board = build_board(db, CORPUS[6].cards)?;
    let druid = board.ids[0];
    let untap_idx = ability_index_where(board.runner.state(), druid, is_untap_effect)?;
    run_combo(board, |probe| {
        activate_and_resolve(probe, druid, untap_idx, None);
        if let Some(tap_idx) = ability_index_where(probe.runner().state(), druid, is_mana_effect) {
            activate_and_resolve(probe, druid, tap_idx, None);
        }
    })
}

/// #2 GRIM MONOLITH + POWER ARTIFACT — infinite colorless mana. Power Artifact
/// reduces Grim's {4} untap to {2}; tap for {C}{C}{C} (+3), untap for {2} (−2) →
/// net +1/cycle, board identical.
pub(crate) fn drive_offline_grim_power(db: &CardDatabase) -> Option<LoopCertificate> {
    let mut board = build_board(db, CORPUS[4].cards)?;
    let grim = board.ids[0];
    let power_artifact = board.ids[1];
    attach_aura(board.runner.state_mut(), power_artifact, grim);
    let untap_idx = ability_index_where(board.runner.state(), grim, is_untap_effect)?;
    run_combo(board, |probe| {
        activate_and_resolve(probe, grim, untap_idx, None);
        if let Some(tap_idx) = ability_index_where(probe.runner().state(), grim, is_mana_effect) {
            activate_and_resolve(probe, grim, tap_idx, None);
        }
    })
}

/// #47 SPIKE FEEDER + ARCHANGEL OF THUNE — infinite +1/+1 counters + life. Spike
/// removes a counter to gain 2 life; Archangel returns a counter to each creature.
/// Board identical modulo counters; unbounded axes are counters + life.
pub(crate) fn drive_offline_spike_archangel(db: &CardDatabase) -> Option<LoopCertificate> {
    let mut board = build_board(db, CORPUS[49].cards)?;
    let spike = board.ids[0];
    {
        // CR 122: Spike Feeder "enters with two +1/+1 counters" — seed them (the
        // as-enters replacement does not run for a directly-installed permanent).
        let state = board.runner.state_mut();
        if let Some(o) = state.objects.get_mut(&spike) {
            o.counters
                .insert(crate::types::counter::CounterType::Plus1Plus1, 2);
        }
        settle_layers(state);
    }
    let gain_idx = ability_index_where(board.runner.state(), spike, |e| {
        matches!(e, crate::types::ability::Effect::GainLife { .. })
    })?;
    run_combo(board, |probe| {
        activate_and_resolve(probe, spike, gain_idx, None);
    })
}

/// #7 BLOOM TENDER + FREED FROM THE REAL — infinite mana. Freed grants "{U}: untap
/// enchanted creature"; Bloom Tender taps for green+blue (2). Tap (+2), untap for
/// {U} (−1) → net +1/cycle.
pub(crate) fn drive_offline_bloom_freed(db: &CardDatabase) -> Option<LoopCertificate> {
    let mut board = build_board(db, CORPUS[9].cards)?;
    let bloom = board.ids[0];
    let freed = board.ids[1];
    attach_aura(board.runner.state_mut(), freed, bloom);
    let untap_idx = ability_index_where(board.runner.state(), freed, is_untap_effect)?;
    run_combo(board, |probe| {
        if let Some(tap_idx) = ability_index_where(probe.runner().state(), bloom, is_mana_effect) {
            activate_and_resolve(probe, bloom, tap_idx, None);
        }
        activate_and_resolve(probe, freed, untap_idx, Some(TargetRef::Object(bloom)));
    })
}

/// #11 FAEBURROW ELDER + PEMMIN'S AURA — infinite mana. Pemmin's Aura grants
/// "{U}: untap enchanted creature"; Faeburrow taps for one mana of each color
/// among your permanents (≥ 2). Tap (+N), untap for {U} (−1) → net (N−1)/cycle.
pub(crate) fn drive_offline_faeburrow_pemmin(db: &CardDatabase) -> Option<LoopCertificate> {
    let mut board = build_board(db, CORPUS[13].cards)?;
    let faeburrow = board.ids[0];
    let pemmin = board.ids[1];
    attach_aura(board.runner.state_mut(), pemmin, faeburrow);
    let pemmin_untap = ability_index_where(board.runner.state(), pemmin, is_untap_effect)?;
    run_combo(board, |probe| {
        if let Some(tap_idx) =
            ability_index_where(probe.runner().state(), faeburrow, is_mana_effect)
        {
            activate_and_resolve(probe, faeburrow, tap_idx, None);
        }
        activate_and_resolve(
            probe,
            pemmin,
            pemmin_untap,
            Some(TargetRef::Object(faeburrow)),
        );
    })
}

/// #11 SELVALA, HEART OF THE WILDS + STAFF OF DOMINATION — infinite mana via a
/// multi-permanent untap chain. Staff's "{3}, {T}: untap target creature" untaps
/// Selvala and "{1}: untap this artifact" untaps the Staff; with a high-power
/// creature present, Selvala's X-mana tap is net mana-positive each cycle.
pub(crate) fn drive_offline_selvala_staff(db: &CardDatabase) -> Option<LoopCertificate> {
    // 7/7 vanilla ⇒ greatest power = 7 ⇒ Selvala adds 7 mana, net +2/cycle.
    let board = build_board_with_vanilla(db, CORPUS[12].cards, 7, 7)?;
    let selvala = board.ids[0];
    let staff = board.ids[1];
    let selvala_tap = ability_index_where(board.runner.state(), selvala, is_mana_effect)?;
    let staff_untap_creature =
        ability_index_where(board.runner.state(), staff, is_target_creature_untap_effect)?;
    let staff_untap_self = ability_index_where(board.runner.state(), staff, is_self_untap_effect)?;
    run_combo(board, |probe| {
        // Tap Selvala for X mana, untap her via Staff's targeted untap, then untap
        // the Staff itself so it is ready next cycle.
        activate_and_resolve(probe, selvala, selvala_tap, None);
        activate_and_resolve(
            probe,
            staff,
            staff_untap_creature,
            Some(TargetRef::Object(selvala)),
        );
        activate_and_resolve(probe, staff, staff_untap_self, None);
    })
}

/// D2 KILO, APOGEE MIND + FREED FROM THE REAL + RELIC OF LEGENDS — infinite
/// proliferate triggers (mana-NEUTRAL). Relic taps Kilo to add 1 mana; Kilo's
/// "becomes tapped → proliferate" fires; Freed untaps Kilo for {U}. Mana nets to
/// zero; +1 proliferate trigger/cycle, board identical.
pub(crate) fn drive_offline_kilo_freed_relic(db: &CardDatabase) -> Option<LoopCertificate> {
    let mut board = build_board(db, CORPUS[1].cards)?;
    let kilo = board.ids[0];
    let freed = board.ids[1];
    let relic = board.ids[2];
    attach_aura(board.runner.state_mut(), freed, kilo);
    // Relic's "tap a creature: add mana" ability (the one that taps Kilo) — found
    // by its `TapCreatures` cost (Relic has two mana abilities; the tap-self one
    // would not fire Kilo's trigger).
    let relic_tap_creature = board.runner.state().objects[&relic]
        .abilities
        .iter()
        .position(|a| {
            matches!(
                a.cost,
                Some(crate::types::ability::AbilityCost::TapCreatures { .. })
            )
        })?;
    let freed_untap = ability_index_where(board.runner.state(), freed, is_untap_effect)?;
    run_combo(board, |probe| {
        // Tap Kilo via Relic's tap-a-creature cost (fires Kilo's trigger), resolve,
        // then untap Kilo via Freed.
        activate_and_resolve(
            probe,
            relic,
            relic_tap_creature,
            Some(TargetRef::Object(kilo)),
        );
        activate_and_resolve(probe, freed, freed_untap, Some(TargetRef::Object(kilo)));
    })
}

/// #10 PRIEST OF TITANIA + UMBRAL MANTLE — infinite green mana. Priest taps for
/// {G} per Elf; Umbral Mantle's "{3}, {Q}" pump untaps Priest. With ≥4 Elves one
/// cycle is net mana-positive after the {3} untap cost (paid from green).
pub(crate) fn drive_offline_priest_umbral(db: &CardDatabase) -> Option<LoopCertificate> {
    use crate::types::ability::Effect;
    let mut board = build_board_green(db, CORPUS[10].cards)?;
    let priest = board.ids[0];
    let umbral = board.ids[1];
    // 4 seeded Elves + Priest (itself an Elf) ⇒ Priest taps for 5 green; net +2 a
    // cycle after the {3} untap cost.
    seed_subtype_creatures(board.runner.state_mut(), "Elf", 4);
    attach_aura(board.runner.state_mut(), umbral, priest);
    let untap_idx = ability_index_where(board.runner.state(), priest, |e| {
        matches!(e, Effect::Pump { .. })
    })?;
    run_combo(board, |probe| {
        if let Some(tap_idx) = ability_index_where(probe.runner().state(), priest, is_mana_effect) {
            activate_and_resolve(probe, priest, tap_idx, None);
        }
        // Umbral's {3},{Q} ability: the {Q} cost untaps Priest for the next tap.
        activate_and_resolve(probe, priest, untap_idx, None);
    })
}

/// #14 MARWYN, THE NURTURER + SWORD OF THE PARUNS — infinite green mana. Marwyn
/// taps for {G} equal to her power; Sword's "{3}: You may tap or untap equipped
/// creature" untaps her (auto-answered "may" + untap branch). With enough counters
/// that her power exceeds {3}, one cycle is net mana-positive.
pub(crate) fn drive_offline_marwyn_sword(db: &CardDatabase) -> Option<LoopCertificate> {
    use crate::types::ability::Effect;
    let mut board = build_board_green(db, CORPUS[14].cards)?;
    let marwyn = board.ids[0];
    let sword = board.ids[1];
    // CR 122: Marwyn's base power is 1; seed 6 +1/+1 counters (power 7) so a cycle
    // nets +4 green after the {3} untap cost. The "another Elf enters" trigger does
    // not fire for directly-installed permanents, so seed the counters directly.
    {
        let state = board.runner.state_mut();
        if let Some(o) = state.objects.get_mut(&marwyn) {
            o.counters
                .insert(crate::types::counter::CounterType::Plus1Plus1, 6);
        }
        settle_layers(state);
    }
    attach_aura(board.runner.state_mut(), sword, marwyn);
    let sword_modal = ability_index_where(board.runner.state(), sword, |e| {
        matches!(e, Effect::TargetOnly { .. })
    })?;
    run_combo(board, |probe| {
        if let Some(tap_idx) = ability_index_where(probe.runner().state(), marwyn, is_mana_effect) {
            activate_and_resolve(probe, marwyn, tap_idx, None);
        }
        // Sword's {3} modal: accept the "you may" + choose the untap branch
        // (auto-answered), targeting Marwyn (the equipped creature) so she readies.
        activate_and_resolve(probe, sword, sword_modal, Some(TargetRef::Object(marwyn)));
    })
}

// ===========================================================================
// Live drain-cascade drivers. These drive the REAL per-beat `apply(PassPriority)`
// reducer (not the offline `detect_loop` harness) so the persisted
// `loop_detect_ring` accumulation and the reconcile-seam win shortcut are
// exercised end-to-end.
// ===========================================================================

/// Build a 2-player board with the named permanents installed on P0, P1 set to a
/// high life total `victim_life` (so a natural CR 704.5a death cannot be the cause
/// of any early `GameOver`), and the active player P0 at a clean `PreCombatMain`
/// priority window. No mana is floated (the drain cascade is trigger-driven).
/// Returns `None` if any name is missing.
pub(crate) fn build_drain_board(
    db: &CardDatabase,
    cards: &[&str],
    victim_life: i32,
) -> Option<ComboBoard> {
    if cards.iter().any(|c| db.get_face_by_name(c).is_none()) {
        return None;
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 40);
    scenario.with_life(P1, victim_life);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
    }
    let mut ids = Vec::new();
    {
        let state = runner.state_mut();
        for &name in cards {
            ids.push(install_on_battlefield(state, db, name, P0)?);
        }
        settle_layers(state);
    }
    Some(ComboBoard { runner, ids })
}

/// Seed a drain cascade by gaining P0 1 life through the real life-gain pipeline
/// and placing the resulting "whenever you gain life" trigger on the stack via the
/// production trigger chokepoint (`process_triggers`). Leaves the board at a
/// priority window with exactly one trigger on the stack. Returns the stack length.
pub(crate) fn seed_lifegain_cascade(board: &mut ComboBoard) -> usize {
    let state = board.runner.state_mut();
    let mut events = Vec::new();
    // CR 119.3: gain P0 1 life — fires the "whenever you gain life" trigger.
    let _ = crate::game::effects::life::apply_life_gain(state, P0, 1, &mut events);
    // CR 603.3: put the triggered ability on the stack as a player would receive
    // priority — the production placement path.
    crate::game::triggers::process_triggers(state, &events);
    // Reset to a clean active-player priority window (the seed is pre-loop setup).
    state.priority_player = state.active_player;
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    state.stack.len()
}

/// One observation of the live per-beat drive. Fields beyond `beat`/`wf` are read
/// only by the `#[cfg(test)]` live-regression assertions; under the feature-only
/// build (the CLI) the driver reads just `beat`/`wf`, so silence the dead-field
/// lint there rather than dropping the diagnostics the tests rely on.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
pub(crate) struct BeatTrace {
    pub(crate) beat: usize,
    pub(crate) wf: WaitingFor,
    pub(crate) stack_len: usize,
    pub(crate) ring_len: usize,
    pub(crate) p0_life: i32,
    pub(crate) p1_life: i32,
}

/// Drive `runner.act(PassPriority)` up to `max_beats` times, recording one
/// [`BeatTrace`] per beat. Stops early on `GameOver`. Returns the trace.
pub(crate) fn drive_pass_priority(board: &mut ComboBoard, max_beats: usize) -> Vec<BeatTrace> {
    let mut trace = Vec::new();
    for beat in 1..=max_beats {
        if matches!(
            board.runner.state().waiting_for,
            WaitingFor::GameOver { .. }
        ) {
            break;
        }
        if board.runner.act(GameAction::PassPriority).is_err() {
            break;
        }
        let s = board.runner.state();
        trace.push(BeatTrace {
            beat,
            wf: s.waiting_for.clone(),
            stack_len: s.stack.len(),
            ring_len: s.loop_detect_ring.len(),
            p0_life: s.players[0].life,
            p1_life: s.players[1].life,
        });
        if matches!(
            board.runner.state().waiting_for,
            WaitingFor::GameOver { .. }
        ) {
            break;
        }
    }
    trace
}

/// Index of the first beat whose `waiting_for` is `GameOver { winner: Some(w) }`,
/// or `None` if no early win occurred within the driven window.
pub(crate) fn first_gameover_beat(trace: &[BeatTrace]) -> Option<(usize, PlayerId)> {
    trace.iter().find_map(|t| match t.wf {
        WaitingFor::GameOver {
            winner: Some(winner),
        } => Some((t.beat, winner)),
        _ => None,
    })
}

/// Drive one live drain cascade (idx 17 / idx 18) to its first `GameOver`. The two
/// rows differ only by `CORPUS[idx].cards` (targeted vs untargeted drain) and share
/// this body. P1 starts at 200 life so the win is the §3 ring shortcut, not the
/// ~400-beat natural CR 704.5a death.
fn drive_live_drain(db: &CardDatabase, idx: usize) -> Option<(usize, PlayerId)> {
    let mut board = build_drain_board(db, CORPUS[idx].cards, 200)?;
    seed_lifegain_cascade(&mut board);
    let trace = drive_pass_priority(&mut board, 40);
    first_gameover_beat(&trace)
}
