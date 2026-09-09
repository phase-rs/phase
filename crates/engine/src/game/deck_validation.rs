use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::database::legality::{LegalityFormat, LegalityStatus};
use crate::database::CardDatabase;
use crate::game::ante::face_uses_ante;
use crate::game::companion::{companion_starting_deck, is_eligible_companion};
use crate::game::deck_loading::{deserialize_draft_set_codes, DeckEntry};
use crate::parser::oracle::{compute_deck_copy_limit_from_text, oracle_text_allows_commander};
use crate::types::card::{CardFace, CardRules, PrintedCardRef};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::custom_format::{
    passes_legacy_axis_gate, AntePolicy, CommandZoneMode, LegalityRules, SetCode,
};
use crate::types::format::{
    DeckCopyLimit, FormatConfig, GameFormat, SelectedFormat, SideboardPolicy,
};
use crate::types::keywords::{Keyword, PartnerType};
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::match_config::MatchType;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeckCompatibilityRequest {
    #[serde(default)]
    pub main_deck: Vec<String>,
    #[serde(default)]
    pub sideboard: Vec<String>,
    #[serde(default)]
    pub commander: Vec<String>,
    /// Commander-family companion outside the 100-card deck.
    #[serde(default)]
    pub companion: Vec<String>,
    #[serde(default)]
    pub planar_deck: Vec<String>,
    #[serde(default)]
    pub scheme_deck: Vec<String>,
    /// Oathbreaker RC: the signature spell card name. Empty for all non-Oathbreaker
    /// formats. Included in `all_deck_cards` so copy-count and identity checks are
    /// accurate regardless of which validation path is active.
    #[serde(default)]
    pub signature_spell: Vec<String>,
    #[serde(default)]
    pub selected_format: Option<SelectedFormat>,
    #[serde(default)]
    pub selected_match_type: Option<MatchType>,
    #[serde(default = "default_player_count")]
    pub player_count: usize,
    #[serde(default)]
    pub summary_only: bool,
    /// CR 903.13e / CR 903.13f(3): every set whose draft boosters this deck's
    /// draft CONTAINED. EMPTY for constructed play, which is why a constructed
    /// Commander deck is unaffected by the Commander Draft concessions.
    /// Latched by the draft session; never derived from a card's printing.
    ///
    /// Plural because both rules condition on containment, so a mixed-set draft
    /// carries every set it contained and `commander_draft_partner_grant` takes
    /// the union. Accepts the legacy single `draft_set_code` string.
    #[serde(
        default,
        alias = "draft_set_code",
        deserialize_with = "deserialize_draft_set_codes"
    )]
    pub draft_set_codes: Vec<String>,
}

/// Engine-authored deck-builder state for selecting an Oathbreaker's signature
/// spell. The frontend renders this policy but never derives the candidate list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "data")]
pub enum SignatureSpellSelectionPolicy {
    None,
    Required { candidates: Vec<String> },
}

/// Returns the legal main-deck cards that may be moved into Oathbreaker's
/// signature-spell slot. Selection itself remains validated by
/// `evaluate_oathbreaker`; this is the engine-owned presentation policy.
pub fn signature_spell_selection_policy(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> SignatureSpellSelectionPolicy {
    if request.selected_format.as_ref().map(SelectedFormat::tag) != Some(GameFormat::Oathbreaker) {
        return SignatureSpellSelectionPolicy::None;
    }

    let commander_identity = request.commander.first().and_then(|name| {
        db.get_face_by_name(name)
            .filter(|face| face.is_oathbreaker)
            .map(card_color_identity)
    });
    let candidates = commander_identity.map_or_else(Vec::new, |identity| {
        request
            .main_deck
            .iter()
            .filter_map(|name| {
                let face = db.get_face_by_name(name)?;
                (is_instant_or_sorcery(face)
                    && card_color_identity(face)
                        .iter()
                        .all(|color| identity.contains(color)))
                .then(|| face.name.clone())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    });

    SignatureSpellSelectionPolicy::Required { candidates }
}

/// Returns eligible Commander-family companion cards that can be moved from
/// the main deck into the dedicated companion slot. Candidate evaluation uses
/// the same typed predicate as pregame reveal validation.
pub fn companion_candidates(db: &CardDatabase, request: &DeckCompatibilityRequest) -> Vec<String> {
    // `SelectedFormat::rules()` returns `Err` for `Tag(Custom(_))` (a bare
    // GameFormat cannot resolve it — see types::format). `selected_format`
    // arrives from an untrusted request (this function is exposed directly
    // via engine-wasm's `companion_candidates_js`), so it can only ever be a
    // `Tag` (see the Wire-Inertness Invariant on `SelectedFormat`) — but no
    // companion-candidate resolution exists for Custom formats yet anyway,
    // so treating an `Err`/absent format the same as any other non-commander
    // format (empty result) is the honest answer.
    let uses_commander = request
        .selected_format
        .as_ref()
        .and_then(|selected| selected.rules().ok())
        .is_some_and(|rules| rules.uses_commander);
    if !uses_commander {
        return Vec::new();
    }

    request
        .main_deck
        .iter()
        .enumerate()
        .filter_map(|(index, name)| {
            let face = db.get_face_by_name(name)?;
            let mut remaining_main = request.main_deck.clone();
            remaining_main.remove(index);
            let companion = DeckEntry::from_resolved_face(db, face, 1);
            let main = deck_entries_for_names(db, &remaining_main);
            let commanders = deck_entries_for_names(db, &request.commander);
            let starting = companion_starting_deck(&main, &commanders, uses_commander);
            is_eligible_companion(&companion, &starting, &commanders, uses_commander)
                .then(|| face.name.clone())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn default_player_count() -> usize {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityCheck {
    pub compatible: bool,
    #[serde(default)]
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeckColorDistributionEntry {
    pub color: ManaColor,
    pub count: usize,
    pub percentage: f64,
    pub display_percentage: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckCompatibilityResult {
    pub standard: CompatibilityCheck,
    pub commander: CompatibilityCheck,
    pub bo3_ready: bool,
    #[serde(default)]
    pub unknown_cards: Vec<String>,
    #[serde(default)]
    pub selected_format_compatible: Option<bool>,
    #[serde(default)]
    pub selected_format_reasons: Vec<String>,
    /// Combined color identity of all cards in the deck, in WUBRG order.
    /// Each entry is a single-letter color code: "W", "U", "B", "R", or "G".
    #[serde(default)]
    pub color_identity: Vec<String>,
    /// Per-color distribution of known main-deck cards, in WUBRG order.
    #[serde(default)]
    pub color_distribution: Vec<DeckColorDistributionEntry>,
    /// Engine coverage summary for the deck's unique cards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<DeckCoverage>,
    /// Per-format legality: maps format key (e.g. "standard", "modern") to the
    /// deck's aggregate status ("legal", "not_legal", or "banned").
    /// A deck is "legal" only if every card is legal in that format.
    #[serde(default)]
    pub format_legality: BTreeMap<String, String>,
}

/// Per-card engine coverage gap info with detailed parse breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsupportedCard {
    pub name: String,
    pub gaps: Vec<String>,
    /// Number of copies of this card in the deck (main + sideboard + commander).
    #[serde(default = "default_one")]
    pub copies: usize,
    /// Original Oracle text for the card face.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oracle_text: Option<String>,
    /// Hierarchical parse tree — same structure used by the coverage dashboard.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub parse_details: Vec<crate::game::coverage::ParsedItem>,
}

fn default_one() -> usize {
    1
}

/// Engine coverage summary for a deck: how many unique cards are fully supported.
///
/// The three fields satisfy `supported_unique + unsupported_cards.len() ==
/// total_unique` by construction: `total_unique` counts only cards that
/// resolved to a database face, since a card with no face lands in neither
/// coverage bucket. Names that do not resolve at all are reported separately as
/// unknown cards, not folded into this ratio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckCoverage {
    pub total_unique: usize,
    pub supported_unique: usize,
    pub unsupported_cards: Vec<UnsupportedCard>,
}

pub fn evaluate_deck_compatibility(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> DeckCompatibilityResult {
    if request.summary_only && request.selected_format.is_some() {
        return evaluate_deck_compatibility_summary(db, request);
    }

    let unknown_cards = collect_unknown_cards(db, request);
    let standard = evaluate_standard(db, request, &unknown_cards);
    let commander = evaluate_commander(db, request, &unknown_cards);
    // CR 100.4a / CR 903.5e: A "BO3-ready" deck is one with a real sideboard
    // the format actually uses. Decks that declare a commander are
    // Commander-style (CR 903) — their submitted sideboard slot is Phase's
    // builder-only Maybeboard staging area and the engine drops it at load
    // time, so they are never BO3-ready regardless of slot occupancy.
    let bo3_ready = !request.sideboard.is_empty() && request.commander.is_empty();
    let color_identity = collect_color_identity(db, request);
    let color_distribution = collect_main_deck_color_distribution(db, request);

    let (mut selected_format_compatible, mut selected_format_reasons) =
        evaluate_selected_format(db, request, &unknown_cards, bo3_ready);

    // UI-HINT ONLY. This function feeds the lobby's live deck-legality chip
    // (`classifyCompatResult` reads `None` as "idle"/no opinion). Phase 1d
    // wired a real evaluator (`evaluate_custom_format`), but the Wire-Inertness
    // Invariant on `SelectedFormat` means this WASM-facing function can only
    // ever receive a bare `Tag(Custom(_))` — never the `Resolved` variant the
    // evaluator needs — so `selected.rules()` is unconditionally `Err` here and
    // the engine genuinely has no verdict to report. A hard "illegal" badge
    // would assert a rules verdict nothing computed. "No opinion" is the
    // honest hint.
    //
    // Scoped three ways so this can never widen into a real permission:
    //   1. It lives HERE, not in `evaluate_selected_format`, which stays
    //      fail-closed because it also backstops the authoritative gate.
    //   2. The authoritative gate (`validate_deck_for_format`) has its own
    //      independent unresolvable-format guard above and never reaches this
    //      code.
    //   3. Only the exact `CUSTOM_FORMAT_UNRESOLVED` sentinel is downgraded —
    //      a genuinely different rejection (a real card-pool failure from
    //      `evaluate_custom_format`, one of the other three definite-verdict
    //      Custom sentinels, "BO3 requires a sideboard", ...) still surfaces
    //      normally. In production this function never sees anything else for
    //      Custom, precisely because of the Wire-Inertness Invariant — but the
    //      check stays keyed on the sentinel, not on "is Custom", so a future
    //      caller that DOES hand this function a `Resolved` custom config gets
    //      its real verdict instead of a silently swallowed one.
    // The P2P host's per-guest deck-kick gate must NOT use this function; it
    // has its own always-strict `evaluate_deck_format_gate`.
    if matches!(
        request.selected_format.as_ref().map(SelectedFormat::tag),
        Some(GameFormat::Custom(_))
    ) && selected_format_reasons == [CUSTOM_FORMAT_UNRESOLVED.to_string()]
    {
        selected_format_compatible = None;
        selected_format_reasons = Vec::new();
    }

    let coverage = evaluate_deck_coverage(db, request);
    let format_legality = evaluate_format_legality(db, request);

    DeckCompatibilityResult {
        standard,
        commander,
        bo3_ready,
        unknown_cards: unknown_cards.into_iter().collect(),
        selected_format_compatible,
        selected_format_reasons,
        color_identity,
        color_distribution,
        coverage: Some(coverage),
        format_legality,
    }
}

fn evaluate_deck_compatibility_summary(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> DeckCompatibilityResult {
    // CR 100.4a / CR 903.5e: A "BO3-ready" deck is one with a real sideboard
    // the format actually uses. Decks that declare a commander are
    // Commander-style (CR 903) — their submitted sideboard slot is Phase's
    // builder-only Maybeboard staging area and the engine drops it at load
    // time, so they are never BO3-ready regardless of slot occupancy.
    let bo3_ready = !request.sideboard.is_empty() && request.commander.is_empty();
    let (mut selected_format_compatible, mut selected_format_reasons, unknown_cards) =
        evaluate_selected_format_summary(db, request);

    if matches!(request.selected_match_type, Some(MatchType::Bo3)) && !bo3_ready {
        selected_format_compatible = Some(false);
        selected_format_reasons.push("BO3 requires a sideboard".to_string());
    }

    DeckCompatibilityResult {
        standard: CompatibilityCheck {
            compatible: matches!(
                (
                    request.selected_format.as_ref().map(SelectedFormat::tag),
                    selected_format_compatible
                ),
                (Some(GameFormat::Standard), Some(true))
            ),
            reasons: Vec::new(),
        },
        commander: CompatibilityCheck {
            compatible: matches!(
                (
                    request.selected_format.as_ref().map(SelectedFormat::tag),
                    selected_format_compatible
                ),
                (Some(GameFormat::Commander), Some(true))
            ),
            reasons: Vec::new(),
        },
        bo3_ready,
        unknown_cards: unknown_cards.into_iter().collect(),
        selected_format_compatible,
        selected_format_reasons,
        color_identity: collect_color_identity(db, request),
        color_distribution: collect_main_deck_color_distribution(db, request),
        coverage: None,
        format_legality: BTreeMap::new(),
    }
}

/// Validate a deck against its selected format, returning `Ok(())` if legal or
/// `Err` with human-readable reasons if not. Delegates to the same validation
/// chain used by `evaluate_deck_compatibility`.
///
/// Returns `Ok(())` when no format is selected, or for formats without card-pool
/// restrictions (FreeForAll, TwoHeadedGiant).
pub fn validate_deck_for_format(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> Result<(), Vec<String>> {
    if request.selected_format.is_none() {
        return Ok(());
    }
    // The AUTHORITATIVE game-creation gate: this function is what
    // `validate_name_deck_for_format_full` runs, and that is called at the real
    // `CreateGameWithSettings` / `initialize_game` boundaries
    // (`phase-server/src/main.rs`, `engine-wasm/src/lib.rs`). It therefore fails
    // closed *on its own* whenever the selected format cannot resolve real
    // rules — `SelectedFormat::rules()` is `Err` only for a bare
    // `Tag(Custom(_))` (see types::format) — independently of whatever
    // `evaluate_selected_format` below decides — a future change to that shared
    // function's unresolvable-format handling (it also feeds the
    // non-authoritative UI-hint path in `evaluate_deck_compatibility`, which
    // deliberately downgrades this exact sentinel to "no opinion") can never
    // reopen this gate by accident. Once `rules()` succeeds — as it always
    // does for a trusted `Resolved` Custom config, e.g. one built by
    // `validate_name_deck_for_format_full` itself — this guard steps aside and
    // `evaluate_selected_format`'s own `evaluate_custom_format` arm decides on
    // the real, resolved rules. Same shared wording as every other
    // unresolvable-format rejection, so the two paths agree on one sentence —
    // see `validate_name_deck_for_format_with_sig`, which still answers a bare
    // `GameFormat` (never a `SelectedFormat`) and so keeps its own equivalent
    // "is Custom" test.
    if request
        .selected_format
        .as_ref()
        .is_some_and(|selected| selected.rules().is_err())
    {
        return Err(vec![CUSTOM_FORMAT_UNRESOLVED.to_string()]);
    }
    let unknown_cards = collect_unknown_cards(db, request);
    // CR 100.4a / CR 903.5e: A "BO3-ready" deck is one with a real sideboard
    // the format actually uses. Decks that declare a commander are
    // Commander-style (CR 903) — their submitted sideboard slot is Phase's
    // builder-only Maybeboard staging area and the engine drops it at load
    // time, so they are never BO3-ready regardless of slot occupancy.
    let bo3_ready = !request.sideboard.is_empty() && request.commander.is_empty();
    let (compatible, reasons) = evaluate_selected_format(db, request, &unknown_cards, bo3_ready);
    match compatible {
        Some(false) => Err(reasons),
        _ => Ok(()),
    }
}

/// The verdict of [`evaluate_deck_format_gate`]: always definite, never
/// "no opinion".
///
/// Deliberately NOT [`DeckCompatibilityResult`]. That type carries the
/// UI-hint surface — a tri-state `Option<bool>`, a `summary_only` dispatch,
/// coverage, per-format legality — and `evaluate_deck_compatibility` downgrades
/// its Custom verdict to `None` so the lobby shows "unknown" instead of a rules
/// claim nothing computed. A security gate must never be able to inherit that
/// downgrade, so it does not share the type that carries it: `compatible` here
/// is a bare `bool` with no representable third state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckFormatGateResult {
    pub compatible: bool,
    #[serde(default)]
    pub reasons: Vec<String>,
}

/// Always-fail-closed deck/format gate for callers that ENFORCE rather than
/// hint — currently exactly one: the P2P host's per-guest deck check
/// (`validateGuestDeck` in `client/src/adapter/p2p-adapter.ts`, via
/// `evaluateDeckFormatGate`), which kicks a joining guest whose deck is illegal
/// for the room's format.
///
/// A thin wrapper over [`validate_deck_for_format`] — the same authoritative
/// function the real game-creation boundary runs — so the host's admission
/// decision and the engine's own game-init decision can never disagree. In
/// particular, a Custom format is rejected here unconditionally, because
/// [`validate_deck_for_format`]'s own independent Custom guard rejects it.
///
/// Every UI-HINT caller must keep using [`evaluate_deck_compatibility`]
/// instead: it deliberately answers "no opinion" for Custom, which is the right
/// answer for a legality chip and the wrong answer for a kick.
pub fn evaluate_deck_format_gate(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> DeckFormatGateResult {
    match validate_deck_for_format(db, request) {
        Ok(()) => DeckFormatGateResult {
            compatible: true,
            reasons: Vec::new(),
        },
        Err(reasons) => DeckFormatGateResult {
            compatible: false,
            reasons,
        },
    }
}

pub fn validate_name_deck_for_format(
    db: &CardDatabase,
    main_deck: &[String],
    sideboard: &[String],
    commander: &[String],
    selected_format: GameFormat,
    selected_match_type: Option<MatchType>,
) -> Result<(), Vec<String>> {
    validate_name_deck_for_format_with_sig(
        db,
        main_deck,
        sideboard,
        commander,
        &[],
        selected_format,
        selected_match_type,
    )
}

/// Extended variant of `validate_name_deck_for_format` that accepts a
/// signature spell slot for Oathbreaker validation. All other callers
/// continue to use `validate_name_deck_for_format` with an implicit empty slice.
pub fn validate_name_deck_for_format_with_sig(
    db: &CardDatabase,
    main_deck: &[String],
    sideboard: &[String],
    commander: &[String],
    signature_spell: &[String],
    selected_format: GameFormat,
    selected_match_type: Option<MatchType>,
) -> Result<(), Vec<String>> {
    // A bare `GameFormat` holder can legitimately pass Custom here (e.g. from
    // untrusted input). Unlike `validate_deck_for_format`, this function has no
    // `SelectedFormat` to call `.rules()` on — only the bare tag — so it keeps
    // its own direct `GameFormat::Custom` test rather than the `rules().is_err()`
    // check used elsewhere. Route it through the same shared rejection text
    // `validate_deck_for_format` uses for an unresolvable format
    // (CUSTOM_FORMAT_UNRESOLVED) instead of `FormatConfig::for_format`'s
    // differently-worded "no default exists" error, so both unresolvable-format
    // rejection paths agree on one wording — never two drifting ones.
    if matches!(selected_format, GameFormat::Custom(_)) {
        return Err(vec![CUSTOM_FORMAT_UNRESOLVED.to_string()]);
    }
    let format_config = FormatConfig::for_format(selected_format)
        .expect("format is guaranteed non-Custom by the preceding check");
    validate_name_deck_for_format_full(
        db,
        main_deck,
        sideboard,
        commander,
        &[],
        &[],
        &[],
        signature_spell,
        // CR 903.13e/f: no draft behind this deck — every caller of this
        // variant is constructed play, which concedes nothing.
        &[],
        &format_config,
        selected_match_type,
        default_player_count(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn validate_name_deck_for_format_full(
    db: &CardDatabase,
    main_deck: &[String],
    sideboard: &[String],
    commander: &[String],
    companion: &[String],
    planar_deck: &[String],
    scheme_deck: &[String],
    signature_spell: &[String],
    draft_set_codes: &[String],
    format_config: &FormatConfig,
    selected_match_type: Option<MatchType>,
    player_count: usize,
) -> Result<(), Vec<String>> {
    let request = DeckCompatibilityRequest {
        main_deck: main_deck.to_vec(),
        sideboard: sideboard.to_vec(),
        commander: commander.to_vec(),
        companion: companion.to_vec(),
        planar_deck: planar_deck.to_vec(),
        scheme_deck: scheme_deck.to_vec(),
        signature_spell: signature_spell.to_vec(),
        // The sole production construction site of `SelectedFormat::Resolved`
        // (Wire-Inertness Invariant clause (2) on `SelectedFormat`) — trusted
        // Rust handing off the `&FormatConfig` it already holds.
        selected_format: Some(SelectedFormat::Resolved(Box::new(format_config.clone()))),
        selected_match_type,
        player_count,
        summary_only: false,
        draft_set_codes: draft_set_codes.to_vec(),
    };
    validate_deck_for_format(db, &request)
}

/// Format-INDEPENDENT reference column — `evaluate_deck_compatibility:222` is
/// its sole caller (a fix round removed `validate_deck_for_format`'s own use
/// of this reference column; it now calls `evaluate_constructed` fresh via
/// `evaluate_selected_format` instead), and runs this for EVERY request
/// regardless of selection. `result.standard` renders as the STD badge
/// (`client/src/components/menu/MyDecks.tsx:411`,
/// `client/src/pages/GameSetupPage.tsx:438`). It must therefore read the
/// REGISTRY config for its own format, never `request`'s resolved rules —
/// doing the latter would report a different selected format's resolved
/// ceiling in the Standard column.
fn evaluate_standard(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
) -> CompatibilityCheck {
    evaluate_constructed(
        db,
        request,
        unknown_cards,
        &FormatConfig::standard(),
        CardPoolAuthority::LegalityTable(LegalityFormat::Standard),
        "Standard",
    )
}

/// Where a constructed-shaped evaluation's per-card legality verdict comes
/// from: the built-in `LegalityFormat` table for a sanctioned format, or a
/// custom format's own declared card pool (`DeclaredPool`) for
/// `GameFormat::Custom`. Both resolve to the same four-variant
/// [`LegalityStatus`] — this type only decides WHICH source answers; it never
/// introduces a fifth verdict.
#[derive(Debug, Clone, Copy)]
enum CardPoolAuthority<'a> {
    LegalityTable(LegalityFormat),
    Declared(&'a DeclaredPool),
}

impl CardPoolAuthority<'_> {
    fn status(self, db: &CardDatabase, name: &str) -> Option<LegalityStatus> {
        match self {
            CardPoolAuthority::LegalityTable(format) => db.legality_status(name, format),
            CardPoolAuthority::Declared(pool) => pool.status(db, name),
        }
    }
}

/// A custom format's resolved card pool: which cards are IN the pool (by
/// printing — `legal_sets: None` means unrestricted), overlaid with its
/// banned/restricted lists. Built once per evaluation by [`Self::resolve`]
/// from a [`LegalityRules`] value, never assembled piecemeal.
///
/// `Debug` is required because [`CardPoolAuthority`] borrows this type and
/// derives `Debug` itself.
#[derive(Debug)]
struct DeclaredPool {
    legal_sets: Option<Vec<SetCode>>,
    /// CR 201.3b: canonical (`canonical_deck_count_key`) names, so a banned
    /// entry naming a split/DFC's whole-card identity ("Fire // Ice") matches
    /// a decklist naming just one face ("Fire"). A banned/restricted entry
    /// that does not itself resolve to a real card silently matches nothing —
    /// that is a preset-authoring integrity concern, deferred to the
    /// `custom_format_registry()` landing phase, not an evaluator concern.
    banned: HashSet<String>,
    restricted: HashSet<String>,
}

impl DeclaredPool {
    fn resolve(db: &CardDatabase, rules: &LegalityRules) -> Self {
        Self {
            legal_sets: rules.legal_sets.clone(),
            banned: rules
                .banned
                .iter()
                .map(|name| canonical_deck_count_key(db, name))
                .collect(),
            restricted: rules
                .restricted
                .iter()
                .map(|name| canonical_deck_count_key(db, name))
                .collect(),
        }
    }

    /// Order: pool membership → banned → restricted → legal. Returns `None`
    /// (never `Some(LegalityStatus::NotLegal)`) for a card outside a declared
    /// `legal_sets` restriction, matching `LegalityTable`'s own "no data for
    /// this format" `None` — both feed the same "(not legal in
    /// {format_label})" message in the shared evaluator, so the two card-pool
    /// authorities must report absence identically.
    fn status(&self, db: &CardDatabase, name: &str) -> Option<LegalityStatus> {
        if let Some(sets) = &self.legal_sets {
            if !printed_in_any_set(db, name, sets) {
                return None;
            }
        }
        // CR 407.3 is deliberately NOT checked here. It is not a property of
        // this format's declared card pool — it holds for every format that is
        // not played for ante — and enforcing it in this authority would reach
        // only constructed-shaped formats, leaving the commander validator and
        // the FreeForAll / TwoHeadedGiant / Limited routes to disagree with it.
        // `ante_deck_violations` applies it once, for all of them.
        let canonical = canonical_deck_count_key(db, name);
        if self.banned.contains(&canonical) {
            return Some(LegalityStatus::Banned);
        }
        if self.restricted.contains(&canonical) {
            return Some(LegalityStatus::Restricted);
        }
        Some(LegalityStatus::Legal)
    }
}

/// ANY-quantified membership (CR 100.6's tournament-rules card-pool concept —
/// no numbered CR governs custom-format legal-sets restrictions directly):
/// whether `name` has at least one printing whose set code appears in `sets`.
/// Case-insensitive (`eq_ignore_ascii_case`), matching this module's existing
/// set-code convention (`draft_set_concessions`) — never uppercase-normalize.
///
/// Fails CLOSED when `db.printings_for` returns `None`: "no evidence of a
/// legal printing" is not "a legal printing." Two production paths hit this
/// today: `phase-server` under `PHASE_DEV_FIXTURE=1` (`main.rs:2055-2062` →
/// `from_mtgjson` → `oracle_loader.rs:147`'s empty printings index) and
/// `CardDatabase::default()` (`server-core/src/draft_session.rs:1369`) — in
/// both, every card in the deck is rejected as "(not legal in
/// {format_label})" rather than silently passing a `legal_sets`-restricted
/// custom format.
fn printed_in_any_set(db: &CardDatabase, name: &str, sets: &[SetCode]) -> bool {
    let Some(printings) = db.printings_for(name) else {
        return false;
    };
    printings.iter().any(|printed| {
        sets.iter()
            .any(|allowed| allowed.0.eq_ignore_ascii_case(printed))
    })
}

/// Shared validation for constructed-shaped formats (Standard, Pioneer,
/// Pauper, etc., and — since Phase 1d — a non-command-zone custom format):
/// checks unknown cards, no commander slot, the format's own deck-size rule,
/// sideboard size, combined main+sideboard copy-limit ceiling, and legality
/// against the given [`CardPoolAuthority`].
///
/// CR 100.2a + CR 100.4a: The 4-card-per-name limit applies to the combined
/// deck and sideboard, with basic lands and "A deck can have any number"
/// cards exempt.
fn evaluate_constructed(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    format_rules: &FormatConfig,
    pool: CardPoolAuthority<'_>,
    format_label: &str,
) -> CompatibilityCheck {
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    if !request.commander.is_empty() {
        reasons.push(format!("{format_label} decks do not use a commander slot"));
    }

    // CR 100.5 / CR 903.5a: the format's own deck-size rule is authoritative —
    // never re-derive a hardcoded 60 by hand.
    if !format_rules.deck_size.accepts(request.main_deck.len()) {
        reasons.push(format!(
            "{format_label} deck must have {} cards (found {})",
            format_rules.deck_size.requirement_phrase(),
            request.main_deck.len()
        ));
    }

    // CR 100.4: Sideboard availability and its size are format rules.
    match format_rules.sideboard_policy {
        SideboardPolicy::Forbidden if !request.sideboard.is_empty() => {
            reasons.push(format!("{format_label} does not allow a sideboard"));
        }
        SideboardPolicy::Limited(max) if request.sideboard.len() as u32 > max => {
            reasons.push(format!(
                "Sideboard has {} cards (maximum {})",
                request.sideboard.len(),
                max
            ));
        }
        SideboardPolicy::Forbidden | SideboardPolicy::Limited(_) | SideboardPolicy::Unlimited => {}
    }

    // CR 100.2a + CR 100.4a: The copy limit applies to main + sideboard
    // combined, at the ceiling the resolved format config carries.
    let limit = format_rules.default_deck_copy_limit;
    let counts = combined_copy_counts(db, request, CommandZoneNetting::CountVerbatim);
    let over_limit = copy_limit_violations(db, &counts, limit);
    if !over_limit.is_empty() {
        reasons.push(summarize_cards(&copy_limit_label(limit), &over_limit, 6));
    }

    let mut illegal_cards = BTreeSet::new();
    let mut restricted_canonical: HashSet<String> = HashSet::new();
    for name in construction_deck_cards(request) {
        if unknown_cards.contains(name) {
            continue;
        }
        match pool.status(db, name) {
            Some(LegalityStatus::Legal) => {}
            // CR 100.6: Tournament rules may limit a card's use. Phase's
            // `Restricted` status represents the established one-copy
            // format-policy, enforced below; the card itself is not
            // "illegal" — that was the bug that flagged Power 9 as banned
            // in Vintage.
            Some(LegalityStatus::Restricted) => {
                restricted_canonical.insert(canonical_deck_count_key(db, name));
            }
            Some(status) => {
                illegal_cards.insert(format!(
                    "{} ({})",
                    display_name(db, name),
                    status_label(status)
                ));
            }
            None => {
                illegal_cards.insert(format!(
                    "{} (not legal in {format_label})",
                    display_name(db, name)
                ));
            }
        }
    }

    if !illegal_cards.is_empty() {
        reasons.push(summarize_cards(
            &format!("Not {format_label} legal"),
            &illegal_cards,
            6,
        ));
    }

    // CR 100.6: Tournament rules may limit a card's use; Phase's `Restricted`
    // status uses the established one-copy format-policy.
    let restricted_violations = restricted_copy_violations(db, &counts, &restricted_canonical);
    if !restricted_violations.is_empty() {
        reasons.push(summarize_cards(
            "More than 1 copy of a restricted card",
            &restricted_violations,
            6,
        ));
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

/// The gates every custom-format evaluation path (`evaluate_custom_format`,
/// `quick_custom_format_check`) must pass before its declared card pool can be
/// trusted, plus the pool itself on success. `Err(reason)` names exactly one
/// of the three non-`UNRESOLVED` sentinels documented on
/// [`CUSTOM_FORMAT_UNRESOLVED`] — see that doc comment for which of the three
/// is genuinely production-reachable versus defense-in-depth.
fn custom_format_pool(
    db: &CardDatabase,
    format_rules: &FormatConfig,
) -> Result<DeclaredPool, String> {
    let Some(rules) = format_rules.custom_rules.as_deref() else {
        return Err(CUSTOM_FORMAT_MISSING_RULES.to_string());
    };
    // Equivalent to `format_rules.command_zone_holds_decklist_commander()`
    // (by that method's own doc comment, every `Enabled` command zone
    // designates a decklist commander and `Disabled` never does) — matched
    // directly on `rules.structural.command_zone_mode` here since this
    // function already holds `rules` and the indirection would add nothing.
    if matches!(
        rules.structural.command_zone_mode,
        CommandZoneMode::Enabled { .. }
    ) {
        return Err(CUSTOM_FORMAT_COMMAND_ZONE_UNSUPPORTED.to_string());
    }
    if !passes_legacy_axis_gate(&rules.legality.legacy) {
        return Err(CUSTOM_FORMAT_UNIMPLEMENTED_LEGACY_AXIS.to_string());
    }
    Ok(DeclaredPool::resolve(db, &rules.legality))
}

/// Phase 1d: the real custom-format evaluator, reached only for a `Resolved`
/// Custom `FormatConfig` (an unresolvable bare `Tag(Custom(_))` is answered
/// earlier by the `rules().is_err()` guards in `evaluate_selected_format` /
/// `validate_deck_for_format`). Scoped to non-command-zone (constructed-shaped)
/// custom formats only — see `custom_format_pool`'s gates — and otherwise a
/// thin wrapper: it builds the declared card pool and delegates the entire
/// deck-shape check to the same [`evaluate_constructed`] every built-in
/// constructed format uses, just pointed at [`CardPoolAuthority::Declared`]
/// instead of a [`LegalityFormat`] table.
fn evaluate_custom_format(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    format_rules: &FormatConfig,
) -> CompatibilityCheck {
    let pool = match custom_format_pool(db, format_rules) {
        Ok(pool) => pool,
        Err(reason) => {
            return CompatibilityCheck {
                compatible: false,
                reasons: vec![reason],
            }
        }
    };
    let format_label = format_rules.format.label();
    evaluate_constructed(
        db,
        request,
        unknown_cards,
        format_rules,
        CardPoolAuthority::Declared(&pool),
        &format_label,
    )
}

/// Summary-path twin of [`evaluate_custom_format`] — see its doc comment.
///
/// The frontend cannot reach this today: `DeckCompatibilityRequest.selected_format`
/// is wire-inert (Wire-Inertness Invariant, `types::format::SelectedFormat`) —
/// its JSON form is always the bare `GameFormat` tag, never `Resolved` — so a
/// WASM-boundary request always fails the `rules().is_err()` guard above this
/// dispatch arm before reaching here. The path forward is NOT a `Resolved`
/// variant on the wire (that would violate the invariant), but a separate
/// gated parameter alongside the request: deserialize a full `FormatConfig`
/// through its own gated `Deserialize` and pass it independently, exactly as
/// `maxDeckCopies(name, format_config)` (`engine-wasm/src/lib.rs`'s
/// `max_deck_copies_for_format`) already threads a resolved `FormatConfig`
/// across the boundary rather than smuggling one through `DeckCompatibilityRequest`.
fn quick_custom_format_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    format_rules: &FormatConfig,
) -> QuickCheckResult {
    let pool = match custom_format_pool(db, format_rules) {
        Ok(pool) => pool,
        Err(reason) => return QuickCheckResult::incompatible(reason),
    };
    let format_label = format_rules.format.label();
    quick_constructed_check(
        db,
        request,
        format_rules,
        CardPoolAuthority::Declared(&pool),
        &format_label,
    )
}

// Called from BOTH `evaluate_selected_format`'s Planechase arm (the full
// path) and `quick_planechase_check` (the summary path's Planechase arm) —
// both already hold `format_rules` and pass it straight through.
fn evaluate_planechase(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    format_rules: &FormatConfig,
) -> CompatibilityCheck {
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }
    if !(2..=4).contains(&request.player_count) {
        reasons.push(format!(
            "Planechase requires 2 to 4 players (found {})",
            request.player_count
        ));
    }
    if !request.commander.is_empty() {
        reasons.push("Planechase decks do not use a commander slot".to_string());
    }
    // CR 100.5: `DeckSizeRule::accepts` is the sole authority — Planechase's
    // registry value is `Minimum(60)`, so this is behavior-neutral versus the
    // hardcoded `< 60` it replaces.
    if !format_rules.deck_size.accepts(request.main_deck.len()) {
        let format_label = format_rules.format.label();
        reasons.push(format!(
            "{format_label} deck must have {} cards (found {})",
            format_rules.deck_size.requirement_phrase(),
            request.main_deck.len()
        ));
    }

    let limit = format_rules.default_deck_copy_limit;
    let counts = combined_copy_counts(db, request, CommandZoneNetting::CountVerbatim);
    let over_limit = copy_limit_violations(db, &counts, limit);
    if !over_limit.is_empty() {
        reasons.push(summarize_cards(&copy_limit_label(limit), &over_limit, 6));
    }

    if request.planar_deck.is_empty() {
        return CompatibilityCheck {
            compatible: reasons.is_empty(),
            reasons,
        };
    }

    // CR 901.15a: a shared planar deck must contain at least 40 cards, or at
    // least ten cards per player if there are fewer than four players.
    let min_planar_cards = 40usize.min(request.player_count.saturating_mul(10));
    if request.planar_deck.len() < min_planar_cards {
        reasons.push(format!(
            "Planar deck has {} cards (minimum {min_planar_cards})",
            request.planar_deck.len()
        ));
    }

    let mut seen_names = HashSet::new();
    let mut duplicates = BTreeSet::new();
    let mut non_planar = BTreeSet::new();
    let mut planes = 0usize;
    let mut phenomena = 0usize;
    for name in &request.planar_deck {
        let Some(face) = db.get_face_by_name(name) else {
            continue;
        };
        let canonical = face.name.to_lowercase();
        if !seen_names.insert(canonical) {
            duplicates.insert(face.name.clone());
        }
        let is_plane = face.card_type.core_types.contains(&CoreType::Plane);
        let is_phenomenon = face.card_type.core_types.contains(&CoreType::Phenomenon);
        if is_plane {
            planes += 1;
        }
        if is_phenomenon {
            phenomena += 1;
        }
        if !is_plane && !is_phenomenon {
            non_planar.insert(face.name.clone());
        }
    }
    if !duplicates.is_empty() {
        reasons.push(summarize_cards(
            "Planar deck singleton violations",
            &duplicates,
            6,
        ));
    }
    if !non_planar.is_empty() {
        reasons.push(summarize_cards(
            "Planar deck cards must be Plane or Phenomenon",
            &non_planar,
            6,
        ));
    }
    if planes == 0 {
        reasons.push("Planar deck must contain at least one Plane".to_string());
    }
    let max_phenomena = request.player_count.saturating_mul(2);
    if phenomena > max_phenomena {
        reasons.push(format!(
            "Planar deck has {phenomena} phenomena (maximum {max_phenomena})"
        ));
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

// Called from BOTH `evaluate_selected_format`'s Archenemy arm (the full
// path) and `quick_archenemy_check` (the summary path's Archenemy arm) —
// both already hold `format_rules` and pass it straight through.
fn evaluate_archenemy(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    format_rules: &FormatConfig,
) -> CompatibilityCheck {
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }
    if !(2..=6).contains(&request.player_count) {
        reasons.push(format!(
            "Archenemy requires 2 to 6 players (found {})",
            request.player_count
        ));
    }
    if !request.commander.is_empty() {
        reasons.push("Archenemy decks do not use a commander slot".to_string());
    }
    // CR 100.5: `DeckSizeRule::accepts` is the sole authority — Archenemy's
    // registry value is `Minimum(60)`, so this is behavior-neutral versus the
    // hardcoded `< 60` it replaces.
    if !format_rules.deck_size.accepts(request.main_deck.len()) {
        let format_label = format_rules.format.label();
        reasons.push(format!(
            "{format_label} deck must have {} cards (found {})",
            format_rules.deck_size.requirement_phrase(),
            request.main_deck.len()
        ));
    }

    let limit = format_rules.default_deck_copy_limit;
    let counts = combined_copy_counts(db, request, CommandZoneNetting::CountVerbatim);
    let over_limit = copy_limit_violations(db, &counts, limit);
    if !over_limit.is_empty() {
        reasons.push(summarize_cards(&copy_limit_label(limit), &over_limit, 6));
    }

    if !request.scheme_deck.is_empty() {
        validate_scheme_deck(db, &request.scheme_deck, &mut reasons);
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

fn validate_scheme_deck(db: &CardDatabase, scheme_deck: &[String], reasons: &mut Vec<String>) {
    // CR 904.3: A scheme deck must contain at least twenty scheme cards and
    // can't contain more than two copies of any card by English name.
    if scheme_deck.len() < 20 {
        reasons.push(format!(
            "Scheme deck has {} cards (minimum 20)",
            scheme_deck.len()
        ));
    }

    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut non_scheme = BTreeSet::new();
    let mut unsupported = BTreeSet::new();
    for name in scheme_deck {
        let Some(face) = db.get_face_by_name(name) else {
            continue;
        };
        *counts.entry(face.name.to_lowercase()).or_insert(0) += 1;
        if !face.card_type.core_types.contains(&CoreType::Scheme) {
            non_scheme.insert(face.name.clone());
        }
        if !crate::game::coverage::card_face_gaps(face).is_empty() {
            unsupported.insert(face.name.clone());
        }
    }

    let over_limit: BTreeSet<String> = counts
        .into_iter()
        .filter(|(_, count)| *count > 2)
        .filter_map(|(name, count)| {
            db.get_face_by_name(&name)
                .map(|face| format!("{} ({count} copies)", face.name))
        })
        .collect();
    if !over_limit.is_empty() {
        reasons.push(summarize_cards(
            "Scheme deck copy-limit violations",
            &over_limit,
            6,
        ));
    }
    if !non_scheme.is_empty() {
        reasons.push(summarize_cards(
            "Scheme deck cards must be Scheme cards",
            &non_scheme,
            6,
        ));
    }
    if !unsupported.is_empty() {
        reasons.push(summarize_cards("Unsupported scheme cards", &unsupported, 6));
    }
}

/// Format-INDEPENDENT reference column — see the doc comment on
/// `evaluate_standard`, which states the same invariant for its own format:
/// `evaluate_deck_compatibility:223` is this function's sole caller. This one
/// feeds `result.commander` / the CMD badge and must never read `request`'s
/// resolved rules.
fn evaluate_commander(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
) -> CompatibilityCheck {
    evaluate_commander_with_format(
        db,
        request,
        unknown_cards,
        CommanderVariantRules::commander(),
        &FormatConfig::commander(),
    )
}

struct CommanderVariantRules {
    eligible: fn(&CardFace) -> bool,
    eligibility_error: &'static str,
    skip_commander_legality: bool,
    /// CR 903.13f(3): the deckbuilding partner grant in force, or `None` for
    /// every variant the rule does not reach — which is all of them except
    /// Commander Draft from a granting set.
    partner_grant: Option<PartnerGrant>,
}

impl CommanderVariantRules {
    fn commander() -> Self {
        Self {
            eligible: is_commander_eligible,
            eligibility_error:
                "Commander cards must be legendary creatures or explicitly allow being a commander",
            skip_commander_legality: false,
            partner_grant: None,
        }
    }

    fn duel_commander() -> Self {
        Self {
            eligible: is_commander_eligible,
            eligibility_error:
                "Duel Commander cards must be legendary creatures or explicitly allow being a commander",
            skip_commander_legality: false,
            partner_grant: None,
        }
    }

    fn pauper_commander() -> Self {
        Self {
            eligible: is_pauper_commander_eligible,
            eligibility_error:
                "Pauper Commander commander must be an uncommon creature, Vehicle, or Spacecraft",
            skip_commander_legality: true,
            partner_grant: None,
        }
    }

    /// CR 903.13f: "Commander Draft deck construction follows the same rules as
    /// Commander deck construction (see rule 903.5) with three exceptions."
    ///
    /// Two of the three exceptions are format axes already answered by
    /// `GameFormat` and read through it by the shared validators —
    /// CR 903.13f(1) deck size via `FormatConfig::for_format(..).deck_size`
    /// (`Minimum(60)`), and CR 903.13f(2) the copy limit via
    /// `default_deck_copy_limit()` (`Unlimited`). The third, CR 903.13f(3), is
    /// a per-SESSION property and arrives here as `partner_grant`.
    ///
    /// `eligible` is CR 903.3, identical to Commander: CR 903.13f names no
    /// exception to it, and the grant affects pairing rather than eligibility.
    /// `skip_commander_legality` is `true` because CR 903.13e makes the drafted
    /// cards the pool — there is no constructed legality table to consult, and
    /// `GameFormat::CommanderDraft.legality_format()` is correspondingly `None`.
    ///
    /// # CR 702.124h is NOT enforced on this path, and that is not an oversight
    ///
    /// CR 702.124h: "You may designate two legendary CARDS as your commander
    /// rather than one if each of them has partner." Two designations need two
    /// cards. This validator cannot decide that, because it has no pool and
    /// every production producer of a Commander Draft request is
    /// commanders-OUTSIDE (`removeCommandersFromMain` on import,
    /// `handleSetCommander`'s `main.filter` in the builder). On that shape
    /// `{ main_deck: <no X>, commander: [X, X] }` is BYTE-IDENTICAL to the
    /// request of a LEGAL deck holding two copies of X — legal because
    /// CR 903.13f(2) sets no copy limit and CR 903.13f(1) sets no maximum deck
    /// size. Any guard written here would therefore reject legal decks,
    /// including the ordinary one-commander case.
    ///
    /// The rule is decidable only against a complete, authoritative list PLUS a
    /// pool, which is `validate_limited_deck`'s step 5 in draft-core — and that
    /// path IS guarded.
    ///
    /// Nothing is deferred here. On the request shape this validator receives the
    /// rule is undecidable by the argument above, and its decidable form — the
    /// CR 702.124h + CR 903.3 multiset comparison — is landed in draft-core's
    /// `validate_limited_deck`, which owns the pool this one lacks.
    ///
    /// This block lives on the single constructor both dispatch arms route
    /// through rather than being copied into each of them, so the two can never
    /// drift apart.
    fn commander_draft(partner_grant: Option<PartnerGrant>) -> Self {
        Self {
            eligible: is_commander_eligible,
            eligibility_error:
                "Commander Draft cards must be legendary creatures or explicitly allow being a commander",
            skip_commander_legality: true,
            partner_grant,
        }
    }
}

/// Strip the sideboard slot from a request before running the validators
/// that share `all_deck_cards`/`combined_copy_counts`. Commander, Brawl, and
/// their variants drop the sideboard at game load (CR 903.5e), so its
/// contents must not contribute to singleton, color-identity, or legality
/// rules. The original request is preserved for callers that still need it
/// (e.g. unknown-card collection, which is computed once upstream).
fn request_without_sideboard(request: &DeckCompatibilityRequest) -> DeckCompatibilityRequest {
    DeckCompatibilityRequest {
        sideboard: Vec::new(),
        ..request.clone()
    }
}

fn deck_entries_for_names(db: &CardDatabase, names: &[String]) -> Vec<DeckEntry> {
    let mut entries: Vec<DeckEntry> = Vec::new();
    for name in names {
        let Some(face) = db.get_face_by_name(name) else {
            continue;
        };
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.card.name.eq_ignore_ascii_case(&face.name))
        {
            entry.count += 1;
        } else {
            entries.push(DeckEntry::from_resolved_face(db, face, 1));
        }
    }
    entries
}

/// CR 702.139a/b + CR 903.11a: a Commander-family companion is one external
/// card, has Companion, satisfies the starting-deck condition (including
/// commanders), and is legal to bring in under Commander color/name limits.
fn validate_commander_companion(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    format_rules: &FormatConfig,
    reasons: &mut Vec<String>,
) {
    if request.companion.is_empty() {
        return;
    }
    if request.companion.len() != 1 {
        reasons.push(format!(
            "{} decks may register exactly one companion (found {})",
            format_rules.format.label(),
            request.companion.len()
        ));
        return;
    }

    let companion_name = &request.companion[0];
    let Some(face) = db.get_face_by_name(companion_name) else {
        return;
    };
    let companion = DeckEntry::from_resolved_face(db, face, 1);
    let main = deck_entries_for_names(db, &request.main_deck);
    let commanders = deck_entries_for_names(db, &request.commander);
    // Reads the STORED `uses_commander` field rather than the bare
    // `GameFormat::uses_commander()` method: `format_rules` may be a
    // `Resolved` custom config for which the bare method would return `Err`
    // (see `SelectedFormat`) — the stored field is always available.
    let uses_commander = format_rules.uses_commander;
    let starting = companion_starting_deck(&main, &commanders, uses_commander);
    if !is_eligible_companion(&companion, &starting, &commanders, uses_commander) {
        reasons.push(format!(
            "{companion_name}: not a legal companion for this starting deck"
        ));
    }
}

/// Shared commander-variant validator. Commander, Duel Commander, and Pauper
/// Commander all use 100-card-singleton deck shape with a command zone; only
/// the legality table, commander eligibility, and display label differ.
/// DuelCommander's 30-life / 1v1-only rules are expressed in `FormatConfig`,
/// not deck validation.
///
/// A fact one twin DERIVES from `format_rules` and the other HARD-CODES is a
/// divergence: the full and summary paths would then state different verdicts
/// for the same request. Derive on both sides, or pass on both sides — the
/// invariant is that the two agree, not that any particular fact is derived.
///
/// `legality_format()` is an `Option` because CR 903.13e leaves Commander
/// Draft with no constructed legality table at all — `None` skips the legality
/// loop, which is the only shape that can express "the drafted cards ARE the
/// pool".
fn evaluate_commander_with_format(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    rules: CommanderVariantRules,
    format_rules: &FormatConfig,
) -> CompatibilityCheck {
    let legality_format = format_rules.format.legality_format();
    let format_label = format_rules.format.label();
    // CR 903.5a / CR 903.13f(1): the format's `DeckSizeRule` is the single
    // authority for min-vs-exact. Compared only through `accepts`, never by
    // hand — CR 903.13f(1) sets a minimum with NO maximum, which a literal
    // equality cannot express.
    let deck_size = format_rules.deck_size;
    // CR 903.5e: the sideboard is dropped at game load for Commander-style
    // formats. Re-scope the request so singleton, color-identity, and legality
    // checks operate on the actual game deck.
    let stripped = request_without_sideboard(request);
    let request = &stripped;
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    if request.commander.is_empty() || request.commander.len() > 2 {
        reasons.push(format!(
            "{format_label} decks require 1 or 2 commanders (found {})",
            request.commander.len()
        ));
    }

    if !request.commander.is_empty() && request.commander.len() <= 2 {
        let mut ineligible_commanders = BTreeSet::new();

        for name in &request.commander {
            let Some(face) = db.get_face_by_name(name) else {
                continue;
            };

            if !(rules.eligible)(face) {
                ineligible_commanders.insert(name.clone());
            }
        }

        if !ineligible_commanders.is_empty() {
            reasons.push(summarize_cards(
                rules.eligibility_error,
                &ineligible_commanders,
                6,
            ));
        }

        // CR 702.124: Validate partner pairing for two-commander setups
        if request.commander.len() == 2 {
            let face_a = db.get_face_by_name(&request.commander[0]);
            let face_b = db.get_face_by_name(&request.commander[1]);
            if let (Some(a), Some(b)) = (face_a, face_b) {
                // CR 903.13f(3): the grant is a per-variant axis, so it comes
                // from the variant rules rather than from the cards.
                if !are_valid_partners(a, b, rules.partner_grant) {
                    reasons.push(format!(
                        "Invalid partner pairing: {} and {} do not have compatible partner keywords",
                        request.commander[0], request.commander[1]
                    ));
                }
            }
        }
    }

    // CR 903.5e (+ variant rules): Commander-style formats do not start the
    // game with a sideboard. We accept extra entries in the submitted list
    // (Phase's deck builder uses that slot as a builder-only "Maybeboard"
    // staging area) and enforce CR 903.5e at game load by dropping them —
    // see `load_deck_into_state` in `deck_loading.rs`.

    let represented_in_main = commanders_represented_in_main(db, request);
    let total_cards = request.main_deck.len() + (request.commander.len() - represented_in_main);
    if !deck_size.accepts(total_cards) {
        reasons.push(format!(
            "{format_label} deck must have {} cards (found {total_cards})",
            deck_size.requirement_phrase()
        ));
    }

    // CR 903.5b: Other than basic lands, each card in a Commander deck must have
    // a different English name. Canonicalization to one bucket per PHYSICAL
    // card (CR 709.2 / CR 712.1 for multi-face cards) is handled inside the
    // shared helper.
    //
    // CR 903.13f(2) displaces that rule for Commander Draft — "A player's deck
    // may include any number of cards from that player's card pool with the
    // same name" — which is exactly what `default_deck_copy_limit()` already
    // reports as `Unlimited`, so the format answers this rather than the
    // caller.
    let counts = combined_copy_counts(db, request, CommandZoneNetting::NetAgainstMainDeck);
    let singleton_violations =
        copy_limit_violations(db, &counts, format_rules.default_deck_copy_limit);
    if !singleton_violations.is_empty() {
        reasons.push(summarize_cards(
            "Singleton violations",
            &singleton_violations,
            6,
        ));
    }

    let mut illegal_cards = BTreeSet::new();
    // CR 903.13e: a format with no constructed legality table has nothing to
    // check here — the drafted cards ARE the pool.
    if let Some(legality_format) = legality_format {
        for name in all_deck_cards(request) {
            if unknown_cards.contains(name) {
                continue;
            }
            if rules.skip_commander_legality && is_commander_entry(db, request, name) {
                continue;
            }
            match db.legality_status(name, legality_format) {
                Some(status) if status.is_legal() => {}
                Some(status) => {
                    illegal_cards.insert(format!(
                        "{} ({})",
                        display_name(db, name),
                        status_label(status)
                    ));
                }
                None => {
                    illegal_cards.insert(format!(
                        "{} (not legal in {format_label})",
                        display_name(db, name)
                    ));
                }
            }
        }
    }
    if !illegal_cards.is_empty() {
        reasons.push(summarize_cards(
            &format!("Not {format_label} legal"),
            &illegal_cards,
            6,
        ));
    }

    // CR 903.4: Each non-commander card's color identity must be a subset of
    // the commander(s)' combined color identity.
    let mut commander_identity = HashSet::new();
    for name in &request.commander {
        if let Some(face) = db.get_face_by_name(name) {
            commander_identity.extend(card_color_identity(face));
        }
    }
    let identity_violations = color_identity_violations(
        db,
        &request.main_deck,
        &commander_identity,
        unknown_cards,
        // `same_card` compares resolved keys, not raw spellings: a DFC
        // commander listed in the command zone by its composite name
        // ("Tovolar, Dire Overlord // Tovolar, the Midnight Scourge") and in
        // the 99 by its front name is the same card. Without this the
        // commander escapes the command-zone skip and is reported as a
        // CR 903.5c violation against its own color identity.
        |name| is_commander_entry(db, request, name),
    );
    if !identity_violations.is_empty() {
        reasons.push(summarize_cards(
            "Cards outside commander's color identity",
            &identity_violations,
            6,
        ));
    }

    validate_commander_companion(db, request, format_rules, &mut reasons);

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

/// Brawl variant of CR 903.3: a legendary planeswalker is also eligible as a Brawl commander.
/// Uses the pre-computed `brawl_commander` field (union of MTGJSON leadershipSkills
/// and type-line analysis). Falls back to type-line check for cards loaded from
/// test fixtures that may not have the field set.
pub fn is_brawl_commander_eligible(face: &CardFace) -> bool {
    if face.brawl_commander {
        return true;
    }
    // Fallback: type-line check for cards without pre-computed field (e.g. test DB)
    let is_legendary = face.card_type.supertypes.contains(&Supertype::Legendary);
    let is_creature = face.card_type.core_types.contains(&CoreType::Creature);
    let is_planeswalker = face.card_type.core_types.contains(&CoreType::Planeswalker);
    let explicitly_allowed = face
        .oracle_text
        .as_ref()
        .is_some_and(|text| oracle_text_allows_commander(text, &face.name));

    (is_legendary && (is_creature || is_planeswalker)) || explicitly_allowed
}

/// Shared validation for Brawl and Historic Brawl: singleton with a commander
/// (deck size from the format's `FormatConfig` — 60 for Standard Brawl, 100 for
/// Historic Brawl), legendary creature or planeswalker as commander, no
/// partner, no sideboard.
fn evaluate_brawl(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    legality_format: LegalityFormat,
    format_label: &str,
    format_rules: &FormatConfig,
) -> CompatibilityCheck {
    // CR 903.5e (Brawl variant): drop the sideboard slot before shape /
    // singleton / identity checks — it is not part of the loaded deck.
    let stripped = request_without_sideboard(request);
    let request = &stripped;
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    // Brawl requires exactly 1 commander (no partner)
    if request.commander.len() != 1 {
        reasons.push(format!(
            "{format_label} decks require exactly 1 commander (found {})",
            request.commander.len()
        ));
    }

    // Validate commander eligibility: legendary creature OR legendary planeswalker
    if request.commander.len() == 1 {
        let name = &request.commander[0];
        if let Some(face) = db.get_face_by_name(name) {
            if !is_brawl_commander_eligible(face) {
                reasons.push(format!(
                    "{format_label} commander must be a legendary creature or legendary planeswalker: {name}"
                ));
            }
        }
    }

    validate_commander_companion(db, request, format_rules, &mut reasons);

    // CR 903.5e (via Brawl variant): Brawl formats do not start the game with
    // a sideboard. Extra entries in the submitted list are silently ignored at
    // load time — see `load_deck_into_state` in `deck_loading.rs`.

    // CR 903.5a (via Brawl variant): the format's rule is authoritative for the
    // total card count (main + commander, accounting for commander listed in
    // main) — never re-derive min-vs-exact here.
    let deck_size = format_rules.deck_size;
    let represented_in_main = commanders_represented_in_main(db, request);
    let total_cards = request.main_deck.len() + (request.commander.len() - represented_in_main);
    if !deck_size.accepts(total_cards) {
        reasons.push(format!(
            "{format_label} deck must have {} cards (found {total_cards})",
            deck_size.requirement_phrase()
        ));
    }

    // CR 903.5b (Brawl variant): singleton rule, basic lands exempt,
    // canonicalized to one bucket per physical card (CR 709.2 / CR 712.1) in
    // the shared helper.
    let counts = combined_copy_counts(db, request, CommandZoneNetting::NetAgainstMainDeck);
    let singleton_violations =
        copy_limit_violations(db, &counts, format_rules.default_deck_copy_limit);
    if !singleton_violations.is_empty() {
        reasons.push(summarize_cards(
            "Singleton violations",
            &singleton_violations,
            6,
        ));
    }

    // Legality check
    let mut illegal_cards = BTreeSet::new();
    for name in all_deck_cards(request) {
        if unknown_cards.contains(name) {
            continue;
        }
        match db.legality_status(name, legality_format) {
            Some(status) if status.is_legal() => {}
            Some(status) => {
                illegal_cards.insert(format!(
                    "{} ({})",
                    display_name(db, name),
                    status_label(status)
                ));
            }
            None => {
                illegal_cards.insert(format!(
                    "{} (not legal in {format_label})",
                    display_name(db, name)
                ));
            }
        }
    }
    if !illegal_cards.is_empty() {
        reasons.push(summarize_cards(
            &format!("Not {format_label} legal"),
            &illegal_cards,
            6,
        ));
    }

    // CR 903.4: Each non-commander card's color identity must be a subset of
    // the commander's color identity.
    if request.commander.len() == 1 {
        let cmd_name = &request.commander[0];
        if let Some(face) = db.get_face_by_name(cmd_name) {
            let commander_identity = card_color_identity(face);
            // Same CR 903.5c subset check as the other command-zone formats —
            // share the one authority instead of re-deriving it, so the
            // command-zone skip and the violation keys stay resolved on both
            // sides here too.
            let identity_violations = color_identity_violations(
                db,
                &request.main_deck,
                &commander_identity,
                unknown_cards,
                |name| same_card(db, name, cmd_name),
            );
            if !identity_violations.is_empty() {
                reasons.push(summarize_cards(
                    &format!("Cards outside {format_label} commander's color identity"),
                    &identity_violations,
                    6,
                ));
            }
        }
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

/// Official Tiny Leaders: Reborn banlist snapshot.
/// Source: https://official-tlr.com/banlist/ — latest update 2026-04-29.
const TINY_LEADERS_DECK_BANNED: &[&str] = &[
    "Ancestral Recall",
    "Black Lotus",
    "Balance",
    "Channel",
    "Chaos Orb",
    "Chrome Mox",
    "Counterbalance",
    "Court of Cunning",
    "Deflecting Swat",
    "Demonic Tutor",
    "Earthcraft",
    "Falling Star",
    "Fastbond",
    "Fierce Guardianship",
    "Forth Eorlingas!",
    "Gaea's Cradle",
    "Grindstone",
    "Hermit Druid",
    "High Tide",
    "Imperial Seal",
    "Jeweled Lotus",
    "Karakas",
    "Library of Alexandria",
    "Lion's Eye Diamond",
    "Maddening Hex",
    "Mana Crypt",
    "Mana Vault",
    "Mind Twist",
    "Mishra's Workshop",
    "Mox Amber",
    "Mox Diamond",
    "Mox Emerald",
    "Mox Jet",
    "Mox Opal",
    "Mox Pearl",
    "Mox Ruby",
    "Mox Sapphire",
    "Mystical Tutor",
    "Necropotence",
    "Oko, Thief of Crowns",
    "Price of Progress",
    "Shahrazad",
    "Skullclamp",
    "Sol Ring",
    "Strip Mine",
    "Survival of the Fittest",
    "Tasha's Hideous Laughter",
    "Teferi, Time Raveler",
    "Thassa's Oracle",
    "The Tabernacle at Pendrell Vale",
    "Time Vault",
    "Time Walk",
    "Timetwister",
    "Tolarian Academy",
    "True-Name Nemesis",
    "Umezawa's Jitte",
    "Vampiric Tutor",
    "Wheel of Fortune",
    "White Plume Adventurer",
    "Yawgmoth's Will",
];

const TINY_LEADERS_COMMANDER_BANNED: &[&str] = &[
    "Ajani, Nacatl Pariah",
    "Ashiok, Dream Render",
    "Derevi, Imperial Tactician",
    "Erayo, Soratami Ascendant",
    "Jeska, Thrice Reborn",
    "Ketramose, the New Dawn",
    "Nadu, Winged Wisdom",
    "Rofellos, Llanowar Emissary",
    "Uro, Titan of Nature's Wrath",
    "Wrenn and Six",
];

const TINY_LEADERS_COMPANION_BANNED: &[&str] = &["Lutri, the Spellchaser"];

pub(crate) fn tiny_leaders_companion_banned(name: &str) -> bool {
    name_in_list(name, TINY_LEADERS_COMPANION_BANNED)
}

// Called from BOTH `evaluate_selected_format`'s TinyLeaders arm (the full
// path) and `quick_tiny_leaders_check` (the summary path's TinyLeaders arm)
// — both already hold `format_rules` and pass it straight through.
fn evaluate_tiny_leaders(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    format_rules: &FormatConfig,
) -> CompatibilityCheck {
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    if request.commander.is_empty() || request.commander.len() > 2 {
        reasons.push(format!(
            "Tiny Leaders: Reborn decks require 1 or 2 commanders (found {})",
            request.commander.len()
        ));
    }

    if request.commander.len() <= 2 {
        let mut ineligible_commanders = BTreeSet::new();
        let mut commander_bans = BTreeSet::new();
        for name in &request.commander {
            let Some(face) = db.get_face_by_name(name) else {
                continue;
            };
            if !is_tiny_leader_eligible(face) {
                ineligible_commanders.insert(name.clone());
            }
            if name_in_list(&face.name, TINY_LEADERS_COMMANDER_BANNED) {
                commander_bans.insert(face.name.clone());
            }
        }
        if !ineligible_commanders.is_empty() {
            reasons.push(summarize_cards(
                "Tiny Leader must be a legendary creature, Vehicle, Spacecraft, planeswalker, or explicitly allow being a commander",
                &ineligible_commanders,
                6,
            ));
        }
        if !commander_bans.is_empty() {
            reasons.push(summarize_cards("Banned as Tiny Leader", &commander_bans, 6));
        }

        if request.commander.len() == 2 {
            let face_a = db.get_face_by_name(&request.commander[0]);
            let face_b = db.get_face_by_name(&request.commander[1]);
            if let (Some(a), Some(b)) = (face_a, face_b) {
                // CR 903.13f(3) is scoped to Commander Draft. Tiny Leaders has
                // no `CommanderVariantRules` and no draft set code, so it
                // passes `None` UNCONDITIONALLY — a grant reaching this arm
                // would silently change a second format's legality. Do not
                // "clean this up" into a variable.
                if !are_valid_partners(a, b, None) {
                    reasons.push(format!(
                        "Invalid partner pairing: {} and {} do not have compatible partner keywords",
                        request.commander[0], request.commander[1]
                    ));
                }
            }
        }
    }

    if request.sideboard.len() > 10 {
        reasons.push(format!(
            "Sideboard has {} cards (maximum 10)",
            request.sideboard.len()
        ));
    }

    let represented_in_main = commanders_represented_in_main(db, request);
    let total_cards = request.main_deck.len() + (request.commander.len() - represented_in_main);
    if total_cards != 50 {
        reasons.push(format!(
            "Tiny Leaders: Reborn deck must have exactly 50 main+commander cards (found {total_cards})"
        ));
    }

    let limit = format_rules.default_deck_copy_limit;
    let counts = combined_copy_counts(db, request, CommandZoneNetting::NetAgainstMainDeck);
    let singleton_violations = copy_limit_violations(db, &counts, limit);
    if !singleton_violations.is_empty() {
        reasons.push(summarize_cards(
            "Singleton violations",
            &singleton_violations,
            6,
        ));
    }

    let mut commander_identity = HashSet::new();
    for name in &request.commander {
        if let Some(face) = db.get_face_by_name(name) {
            commander_identity.extend(card_color_identity(face));
        }
    }

    let mut identity_violations = BTreeSet::new();
    let mut basic_land_type_violations = BTreeSet::new();
    let mut tiny_identity_violations = BTreeSet::new();
    let mut deck_bans = BTreeSet::new();
    let mut category_bans = BTreeSet::new();

    for name in all_deck_cards(request) {
        if unknown_cards.contains(name) {
            continue;
        }
        let Some(face) = db.get_face_by_name(name) else {
            continue;
        };

        if name_in_list(&face.name, TINY_LEADERS_DECK_BANNED) {
            deck_bans.insert(face.name.clone());
        }
        if tiny_leaders_category_banned(face) {
            category_bans.insert(face.name.clone());
        }

        if !is_commander_entry(db, request, name) {
            for color in card_color_identity(face) {
                if !commander_identity.contains(&color) {
                    // Resolved face name, not the caller's raw spelling — the
                    // same convention `color_identity_violations` documents and
                    // the sibling `basic_land_type_violations` /
                    // `tiny_identity_violations` inserts below already follow,
                    // so the `BTreeSet` dedups a card listed under two spellings.
                    identity_violations.insert(face.name.clone());
                    break;
                }
            }
        }

        for color in basic_land_type_colors(face) {
            if !commander_identity.contains(&color) {
                basic_land_type_violations.insert(face.name.clone());
                break;
            }
        }

        if !tiny_leaders_cost_identity_ok(db, name) {
            tiny_identity_violations.insert(face.name.clone());
        }
    }

    if !deck_bans.is_empty() {
        reasons.push(summarize_cards(
            "Banned in Tiny Leaders: Reborn deck construction",
            &deck_bans,
            6,
        ));
    }
    if !category_bans.is_empty() {
        reasons.push(summarize_cards(
            "Categorically excluded in Tiny Leaders: Reborn",
            &category_bans,
            6,
        ));
    }
    if !identity_violations.is_empty() {
        reasons.push(summarize_cards(
            "Cards outside Tiny Leader color identity",
            &identity_violations,
            6,
        ));
    }
    if !basic_land_type_violations.is_empty() {
        reasons.push(summarize_cards(
            "Cards with off-identity basic land types",
            &basic_land_type_violations,
            6,
        ));
    }
    if !tiny_identity_violations.is_empty() {
        reasons.push(summarize_cards(
            "Cards outside Tiny cost identity",
            &tiny_identity_violations,
            6,
        ));
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

fn quick_tiny_leaders_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    format_rules: &FormatConfig,
) -> QuickCheckResult {
    let unknown_cards = collect_unknown_cards(db, request);
    let check = evaluate_tiny_leaders(db, request, &unknown_cards, format_rules);
    QuickCheckResult {
        reason: check.reasons.into_iter().next(),
        unknown_cards,
    }
}

pub fn is_tiny_leader_eligible(face: &CardFace) -> bool {
    let is_legendary = face.card_type.supertypes.contains(&Supertype::Legendary);
    let subtypes = &face.card_type.subtypes;
    let is_creature = face.card_type.core_types.contains(&CoreType::Creature);
    let is_planeswalker = face.card_type.core_types.contains(&CoreType::Planeswalker);
    let is_vehicle = subtypes.iter().any(|s| s.eq_ignore_ascii_case("Vehicle"));
    let is_spacecraft_with_pt = subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Spacecraft"))
        && face.power.is_some()
        && face.toughness.is_some();
    let explicitly_allowed = face
        .oracle_text
        .as_ref()
        .is_some_and(|text| oracle_text_allows_commander(text, &face.name));

    explicitly_allowed
        || (is_legendary && (is_creature || is_vehicle || is_spacecraft_with_pt || is_planeswalker))
}

fn basic_land_type_colors(face: &CardFace) -> Vec<ManaColor> {
    let mut colors = Vec::new();
    for subtype in &face.card_type.subtypes {
        let color = match subtype.as_str() {
            "Plains" => ManaColor::White,
            "Island" => ManaColor::Blue,
            "Swamp" => ManaColor::Black,
            "Mountain" => ManaColor::Red,
            "Forest" => ManaColor::Green,
            _ => continue,
        };
        if !colors.contains(&color) {
            colors.push(color);
        }
    }
    colors
}

fn tiny_leaders_cost_identity_ok(db: &CardDatabase, name: &str) -> bool {
    tiny_leaders_cost_faces(db, name)
        .into_iter()
        .all(|face| tiny_leaders_face_cost_identity_ok(db, face))
}

fn tiny_leaders_face_cost_identity_ok(db: &CardDatabase, face: &CardFace) -> bool {
    // CR 202.3d + CR 709.4b: off the stack a split card's mana value is the COMBINED
    // value of both halves, so the Tiny Leaders MV <= 3 cap must use the combined
    // value (a Fire // Ice-style card is MV 4, not 2). `off_stack_mana_value_for_face`
    // combines for split cards and is the face's own value for every other layout,
    // preserving the per-face check for DFC/MDFC/Adventure cards.
    db.off_stack_mana_value_for_face(face) <= 3
        && face.keywords.iter().all(|keyword| match keyword {
            Keyword::Prototype { cost, .. } => cost.mana_value() <= 3,
            _ => true,
        })
}

fn tiny_leaders_cost_faces<'a>(db: &'a CardDatabase, name: &str) -> Vec<&'a CardFace> {
    if let Some(rules) = db.get_by_name(name) {
        return card_rules_faces(rules);
    }

    let Some(face) = db.get_face_by_name(name) else {
        return Vec::new();
    };
    let mut faces = vec![face];
    if let Some(oracle_id) = &face.scryfall_oracle_id {
        let printed_ref = PrintedCardRef {
            oracle_id: oracle_id.clone(),
            face_name: face.name.clone(),
        };
        if let Some(other) = db.get_other_face_by_printed_ref(&printed_ref) {
            faces.push(other);
        }
    }
    faces
}

fn card_rules_faces(rules: &CardRules) -> Vec<&CardFace> {
    crate::database::synthesis::layout_faces(&rules.layout)
}

fn tiny_leaders_category_banned(face: &CardFace) -> bool {
    let text = face
        .oracle_text
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    face.card_type.subtypes.iter().any(|subtype| {
        subtype.eq_ignore_ascii_case("Conspiracy") || subtype.eq_ignore_ascii_case("Attraction")
    }) || face_uses_ante(face)
        || text.contains("sticker")
        || text.contains("attraction")
}

/// CR 407.3: "When not playing for ante, players can't include these cards in
/// their decks or sideboards." Returns the offending cards, by display name.
///
/// Applied once per evaluation, beside the other cross-format rules at the
/// dispatch seam, rather than inside a card-pool authority. Three routes decide
/// per-card legality — `CardPoolAuthority::LegalityTable` for built-in
/// constructed formats, `CardPoolAuthority::Declared` for custom ones, and the
/// commander validator — while FreeForAll, TwoHeadedGiant and Limited answer
/// `true` with no per-card check at all. CR 407.3 binds all four: it is not a
/// card-pool restriction a permissive format may waive, but a rule about
/// whether this game is played for ante. Enforcing it in any one authority
/// would let the others diverge — and the permissive route, which has no
/// authority to enforce it in, is exactly where an ante card would slip
/// through.
///
/// Scans `construction_deck_cards`, so the deck and the sideboard are covered
/// by the same pass; CR 407.3 names both. Unknown names are skipped — they are
/// reported separately, by name, and an unresolvable spelling is not evidence
/// of an ante card.
fn ante_deck_violations(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    format_rules: &FormatConfig,
) -> BTreeSet<String> {
    // CR 407.2: a format actually played for ante admits the class.
    if crate::game::ante::policy_of(format_rules) == AntePolicy::Enabled {
        return BTreeSet::new();
    }
    construction_deck_cards(request)
        .filter(|name| !unknown_cards.contains(*name))
        .filter(|name| deck_entry_uses_ante(db, name))
        .map(|name| display_name(db, name))
        .collect()
}

/// Whether a decklist entry resolves to a card of the CR 407.3 ante class.
/// Reads the entry's resolved face, the same way `tiny_leaders_category_banned`'s
/// call site does; an unknown name is not an ante card (unknown entries are
/// reported separately, by name, before any legality verdict is formed).
fn deck_entry_uses_ante(db: &CardDatabase, name: &str) -> bool {
    db.get_face_by_name(name).is_some_and(face_uses_ante)
}

fn name_in_list(name: &str, list: &[&str]) -> bool {
    list.iter().any(|banned| names_match(name, banned))
}

fn names_match(a: &str, b: &str) -> bool {
    fn normalize(raw: &str) -> String {
        raw.chars()
            .map(|c| match c {
                '\u{2019}' => '\'',
                _ => c,
            })
            .flat_map(|c| c.to_lowercase())
            .collect()
    }
    normalize(a) == normalize(b)
}

/// Oathbreaker RC: returns `true` if `face` is an instant or sorcery.
fn is_instant_or_sorcery(face: &CardFace) -> bool {
    face.card_type.core_types.contains(&CoreType::Instant)
        || face.card_type.core_types.contains(&CoreType::Sorcery)
}

/// Oathbreaker RC: full deck compatibility check.
// Called from BOTH `evaluate_selected_format`'s Oathbreaker arm (the full
// path) and `quick_oathbreaker_check` (the summary path's Oathbreaker arm)
// — both already hold `format_rules` and pass it straight through.
fn evaluate_oathbreaker(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    format_rules: &FormatConfig,
) -> CompatibilityCheck {
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    // Oathbreaker RC: exactly one Oathbreaker (legendary Planeswalker).
    if request.commander.len() != 1 {
        reasons.push(format!(
            "Oathbreaker decks require exactly 1 Oathbreaker (found {})",
            request.commander.len()
        ));
    } else {
        let name = &request.commander[0];
        if let Some(face) = db.get_face_by_name(name) {
            if !face.is_oathbreaker {
                reasons.push(format!(
                    "{name}: Oathbreaker must be a legendary Planeswalker"
                ));
            }
        }
    }

    let oathbreaker_identity = request.commander.first().and_then(|ob_name| {
        db.get_face_by_name(ob_name)
            .filter(|face| face.is_oathbreaker)
            .map(|face| {
                card_color_identity(face)
                    .into_iter()
                    .collect::<HashSet<_>>()
            })
    });

    // Oathbreaker RC: exactly one signature spell (instant or sorcery within color identity).
    if request.signature_spell.len() != 1 {
        reasons.push(format!(
            "Oathbreaker decks require exactly 1 signature spell (found {})",
            request.signature_spell.len()
        ));
    } else {
        let sig_name = &request.signature_spell[0];
        if let Some(face) = db.get_face_by_name(sig_name) {
            if !is_instant_or_sorcery(face) {
                reasons.push(format!(
                    "{sig_name}: signature spell must be an instant or sorcery"
                ));
            }
            // Signature spell must be within the Oathbreaker's color identity.
            if let Some(identity) = &oathbreaker_identity {
                for color in card_color_identity(face) {
                    if !identity.contains(&color) {
                        reasons.push(format!(
                            "{sig_name}: signature spell is outside the Oathbreaker's color identity"
                        ));
                        break;
                    }
                }
            }
        }
    }

    // Oathbreaker RC: exactly 60 cards total (main + commander + signature spell,
    // de-duplicating any that appear in both main and a command-zone slot).
    // CR 709.2 / CR 712.1: both slots compare resolved card identities, not raw
    // spellings, because a split or double-faced card is one physical card,
    // so a composite-named ("Front // Back") Oathbreaker or signature spell
    // listed in the main deck by its front face is recognized as the same
    // physical card instead of being counted twice.
    let commander_represented = commanders_represented_in_main(db, request);
    let sig_represented = request
        .signature_spell
        .iter()
        .filter(|n| request.main_deck.iter().any(|c| same_card(db, c, n)))
        .count();
    let total_cards = request.main_deck.len()
        + (request
            .commander
            .len()
            .saturating_sub(commander_represented))
        + (request
            .signature_spell
            .len()
            .saturating_sub(sig_represented));
    if total_cards != 60 {
        reasons.push(format!(
            "Oathbreaker deck must have exactly 60 cards (found {total_cards})"
        ));
    }

    // Oathbreaker RC: singleton (basic lands exempt, consistent with other
    // singleton command-zone formats). `construction_deck_cards` includes
    // `signature_spell`, so a genuine second copy of a card in both the main
    // deck and the signature-spell slot is caught here; a single physical card
    // named in both slots under different spellings is netted out first by
    // `CommandZoneNetting::NetAgainstMainDeck`, which resolves both command-zone
    // slots by card identity (CR 709.2 / CR 712.1: one physical card) rather
    // than by raw spelling.
    let limit = format_rules.default_deck_copy_limit;
    let counts = combined_copy_counts(db, request, CommandZoneNetting::NetAgainstMainDeck);
    let singleton_violations = copy_limit_violations(db, &counts, limit);
    if !singleton_violations.is_empty() {
        reasons.push(summarize_cards(
            "Singleton violations",
            &singleton_violations,
            6,
        ));
    }

    // Oathbreaker RC: every main-deck card must be within the Oathbreaker's
    // color identity. CR 903.5c (color identity) is shared with the other
    // command-zone formats via `color_identity_violations`; CR 903.5d (off-
    // identity basic land types) is reported in its own bucket alongside it.
    let mut identity_violations = BTreeSet::new();
    let mut basic_type_violations = BTreeSet::new();
    if let Some(identity) = &oathbreaker_identity {
        identity_violations =
            color_identity_violations(db, &request.main_deck, identity, unknown_cards, |_| false);
        for name in request.main_deck.iter().map(String::as_str) {
            if unknown_cards.contains(name) {
                continue;
            }
            let Some(face) = db.get_face_by_name(name) else {
                continue;
            };
            for color in basic_land_type_colors(face) {
                if !identity.contains(&color) {
                    basic_type_violations.insert(face.name.clone());
                    break;
                }
            }
        }
    }
    if !identity_violations.is_empty() {
        reasons.push(summarize_cards(
            "Cards outside Oathbreaker color identity",
            &identity_violations,
            6,
        ));
    }
    if !basic_type_violations.is_empty() {
        reasons.push(summarize_cards(
            "Cards with off-identity basic land types",
            &basic_type_violations,
            6,
        ));
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

/// CR 305.6: the five basic land types (Plains/Island/Swamp/Mountain/Forest).
/// "Wastes" is the basic colorless land but is NOT a basic land type, so
/// Snow-Covered Wastes is naturally excluded from the Momir's Madness deck by
/// requiring a subtype in this set.
const BASIC_LAND_TYPES: [&str; 5] = ["Plains", "Island", "Swamp", "Mountain", "Forest"];

/// Momir's Madness format deck rule: the deck is fixed at exactly 12 copies of
/// each of the five snow basic lands (Snow-Covered Plains/Island/Swamp/Mountain/
/// Forest), totaling 60, with nothing else. Players cannot adjust this ratio.
///
/// A "snow basic land" is identified by typed checks (CR 205.4a Snow + Basic
/// supertypes, CR 305 Land core type, and a CR 305.6 basic land type subtype) —
/// never by matching printed card names. Snow-Covered Wastes is excluded because
/// its subtype is "Wastes", which is not a basic land type (CR 305.6). This is a
/// format-construction rule, not a Comprehensive Rule; CR 100.2a's basic-land
/// copy exception is what makes the 12-of-each copies legal despite the
/// four-copy default.
fn evaluate_momir(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
) -> CompatibilityCheck {
    const EXPECTED_PER_TYPE: usize = 12;

    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    if request.main_deck.len() != 60 {
        reasons.push(format!(
            "Momir's Madness decks must have exactly 60 cards (found {})",
            request.main_deck.len()
        ));
    }

    // Tally snow-basic copies per basic land type; collect anything that is not a
    // snow basic land of a CR 305.6 basic land type.
    let mut per_type: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut non_snow_basic = BTreeSet::new();
    for name in request.main_deck.iter().map(String::as_str) {
        if unknown_cards.contains(name) {
            continue;
        }
        let Some(face) = db.get_face_by_name(name) else {
            continue;
        };
        // CR 205.4a + CR 305: Snow + Basic supertypes on a Land.
        let is_snow_basic_land = face.card_type.supertypes.contains(&Supertype::Snow)
            && face.card_type.supertypes.contains(&Supertype::Basic)
            && face.card_type.core_types.contains(&CoreType::Land);
        // CR 305.6: must carry one of the five basic land type subtypes
        // (excludes Snow-Covered Wastes, whose subtype is "Wastes").
        let basic_type = is_snow_basic_land
            .then(|| {
                BASIC_LAND_TYPES.into_iter().find(|bt| {
                    face.card_type
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(bt))
                })
            })
            .flatten();
        match basic_type {
            Some(bt) => *per_type.entry(bt).or_insert(0) += 1,
            None => {
                non_snow_basic.insert(face.name.clone());
            }
        }
    }
    if !non_snow_basic.is_empty() {
        reasons.push(summarize_cards(
            "Momir's Madness decks may only contain the five snow basic lands \
             (Snow-Covered Plains/Island/Swamp/Mountain/Forest)",
            &non_snow_basic,
            6,
        ));
    }

    // The ratio is fixed: exactly 12 of each of the five snow basic types.
    for bt in BASIC_LAND_TYPES {
        let count = per_type.get(bt).copied().unwrap_or(0);
        if count != EXPECTED_PER_TYPE {
            reasons.push(format!(
                "Momir's Madness decks must contain exactly {EXPECTED_PER_TYPE} \
                 copies of Snow-Covered {bt} (found {count})"
            ));
        }
    }

    if !request.sideboard.is_empty() {
        reasons.push("Momir's Madness does not use a sideboard".to_string());
    }
    if !request.commander.is_empty() || !request.signature_spell.is_empty() {
        reasons.push("Momir's Madness does not use command-zone cards".to_string());
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

fn quick_momir_check(db: &CardDatabase, request: &DeckCompatibilityRequest) -> QuickCheckResult {
    let unknown_cards = collect_unknown_cards(db, request);
    let check = evaluate_momir(db, request, &unknown_cards);
    QuickCheckResult {
        reason: check.reasons.into_iter().next(),
        unknown_cards,
    }
}

fn quick_oathbreaker_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    format_rules: &FormatConfig,
) -> QuickCheckResult {
    let unknown_cards = collect_unknown_cards(db, request);
    let check = evaluate_oathbreaker(db, request, &unknown_cards, format_rules);
    QuickCheckResult {
        reason: check.reasons.into_iter().next(),
        unknown_cards,
    }
}

fn quick_planechase_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    format_rules: &FormatConfig,
) -> QuickCheckResult {
    let unknown_cards = collect_unknown_cards(db, request);
    let check = evaluate_planechase(db, request, &unknown_cards, format_rules);
    QuickCheckResult {
        reason: check.reasons.into_iter().next(),
        unknown_cards,
    }
}

fn quick_archenemy_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    format_rules: &FormatConfig,
) -> QuickCheckResult {
    let unknown_cards = collect_unknown_cards(db, request);
    let check = evaluate_archenemy(db, request, &unknown_cards, format_rules);
    QuickCheckResult {
        reason: check.reasons.into_iter().next(),
        unknown_cards,
    }
}

/// The Custom-format rejection sentinels shared by the summary and full
/// deck-compatibility paths (and the authoritative `validate_deck_for_format`
/// gate) so no two call sites can drift apart on wording. Four consts, not
/// one, because only ONE of them names "nothing was computed" — the other
/// three are definite verdicts about a format whose rules the engine DOES
/// hold, and a UI-hint caller must see them as real `Some(false)`
/// rejections, never silently downgraded to "no opinion" the way
/// `evaluate_deck_compatibility` downgrades the first.
///
/// - [`CUSTOM_FORMAT_UNRESOLVED`]: `SelectedFormat::rules()` returned `Err` —
///   a bare, untrusted `Tag(Custom(_))` with no `FormatConfig` behind it at
///   all (see the Wire-Inertness Invariant on `SelectedFormat`,
///   `types::format`). The ONLY sentinel `evaluate_deck_compatibility`'s "no
///   opinion" downgrade matches: there is genuinely no rules verdict to
///   report.
/// - [`CUSTOM_FORMAT_MISSING_RULES`]: `rules()` succeeded but
///   `FormatConfig.custom_rules` is `None`. Defense-in-depth only —
///   `validate_custom_rules_consistency` already forbids this shape at
///   `FormatConfig::deserialize`.
/// - [`CUSTOM_FORMAT_UNIMPLEMENTED_LEGACY_AXIS`]: the resolved rules declare
///   a `LegacyRuleSet` axis outside `IMPLEMENTED_LEGACY_AXES`.
///   Defense-in-depth only — `FormatConfig::deserialize` already rejects an
///   undeclared axis via `passes_legacy_axis_gate`, and
///   `FormatConfig::for_custom_rules` (the only resolver) is total and
///   applies no gate of its own.
/// - [`CUSTOM_FORMAT_COMMAND_ZONE_UNSUPPORTED`]: the resolved rules declare
///   `CommandZoneMode::Enabled`. The one genuinely PRODUCTION-REACHABLE gate
///   of these last three: `CustomFormatDef::from_lobby_config` produces
///   `Enabled` whenever a host saves a Commander/Brawl/Tiny-Leaders/
///   Oathbreaker-shaped lobby as a custom format, and no commander-style
///   evaluator is wired for custom formats yet — this phase only widens the
///   constructed-shaped (no command zone) evaluation path.
const CUSTOM_FORMAT_UNRESOLVED: &str = "This format's rules could not be resolved.";
const CUSTOM_FORMAT_MISSING_RULES: &str = "Custom format is missing its declared rules.";
const CUSTOM_FORMAT_UNIMPLEMENTED_LEGACY_AXIS: &str =
    "Custom format declares a legacy rules axis the engine does not enforce yet.";
const CUSTOM_FORMAT_COMMAND_ZONE_UNSUPPORTED: &str =
    "Custom format deck-compatibility checks are not yet supported for command-zone formats.";

fn evaluate_selected_format_summary(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> (Option<bool>, Vec<String>, BTreeSet<String>) {
    let Some(selected) = request.selected_format.as_ref() else {
        return (None, Vec::new(), BTreeSet::new());
    };
    let format = selected.tag();

    // `SelectedFormat::rules()` returns `Err` only for a bare `Tag(Custom(_))`
    // (see types::format's Wire-Inertness Invariant), and the companion /
    // signature-spell pre-guards below need the resolved rules. `selected_format`
    // arrives from an untrusted request, so an unresolvable format must be
    // answered before those guards run.
    //
    // The answer is "no opinion" (`None`), matching the idle downgrade
    // `evaluate_deck_compatibility` applies to the full path: this function is
    // reachable ONLY from `evaluate_deck_compatibility_summary`, itself
    // reachable ONLY from `evaluate_deck_compatibility`'s own `summary_only`
    // branch. Unlike `evaluate_selected_format`, it has no path into
    // `validate_deck_for_format`/the authoritative game-creation gate at all,
    // so answering honestly here grants nothing. Verified by tracing every
    // caller; keep it that way — a new caller outside that chain must use
    // `evaluate_deck_format_gate` instead.
    let Ok(format_rules) = selected.rules() else {
        return (None, Vec::new(), BTreeSet::new());
    };
    let uses_commander = format_rules.uses_commander;

    if !uses_commander && !request.companion.is_empty() {
        return (
            Some(false),
            vec![format!(
                "{} does not use a dedicated companion slot",
                format.label()
            )],
            BTreeSet::new(),
        );
    }
    if format != GameFormat::Oathbreaker && !request.signature_spell.is_empty() {
        return (
            Some(false),
            vec![format!(
                "{} does not use a signature spell slot",
                format.label()
            )],
            BTreeSet::new(),
        );
    }

    let result = match format {
        GameFormat::Standard => quick_constructed_check(
            db,
            request,
            &format_rules,
            CardPoolAuthority::LegalityTable(LegalityFormat::Standard),
            "Standard",
        ),
        GameFormat::Pioneer
        | GameFormat::Modern
        | GameFormat::Premodern
        | GameFormat::Legacy
        | GameFormat::Vintage
        | GameFormat::Historic
        | GameFormat::Timeless
        | GameFormat::Pauper => quick_constructed_check(
            db,
            request,
            &format_rules,
            CardPoolAuthority::LegalityTable(format.legality_format().unwrap()),
            &format.label(),
        ),
        GameFormat::Commander => quick_commander_check(
            db,
            request,
            CommanderVariantRules::commander(),
            &format_rules,
        ),
        GameFormat::PauperCommander | GameFormat::DuelCommander => quick_commander_check(
            db,
            request,
            match format {
                GameFormat::PauperCommander => CommanderVariantRules::pauper_commander(),
                GameFormat::DuelCommander => CommanderVariantRules::duel_commander(),
                _ => unreachable!("commander variant branch only handles PDH and Duel"),
            },
            &format_rules,
        ),
        // CR 903.13f: Commander Draft deck construction follows CR 903.5 with
        // three exceptions, so it routes through the SHARED commander
        // validator rather than a second one. The exceptions arrive as format
        // axes (CR 903.13f(1) `DeckSizeRule::Minimum(60)`, CR 903.13f(2)
        // `DeckCopyLimit::Unlimited`, no legality table) plus the CR 903.13f(3)
        // grant below.
        GameFormat::CommanderDraft => quick_commander_check(
            db,
            request,
            CommanderVariantRules::commander_draft(commander_draft_partner_grant(request)),
            &format_rules,
        ),
        GameFormat::TinyLeaders => quick_tiny_leaders_check(db, request, &format_rules),
        GameFormat::Oathbreaker => quick_oathbreaker_check(db, request, &format_rules),
        GameFormat::Momir => quick_momir_check(db, request),
        GameFormat::Planechase => quick_planechase_check(db, request, &format_rules),
        GameFormat::Archenemy => quick_archenemy_check(db, request, &format_rules),
        GameFormat::Brawl | GameFormat::HistoricBrawl => {
            quick_brawl_check(db, request, &format.label(), &format_rules)
        }
        GameFormat::FreeForAll | GameFormat::TwoHeadedGiant | GameFormat::Limited => {
            QuickCheckResult::compatible()
        }
        // Phase 1d: reachable for a `Resolved` Custom config (the early guard
        // above only answers "no opinion" for an unresolvable bare
        // `Tag(Custom(_))`), and delegates to the real evaluator.
        GameFormat::Custom(_) => quick_custom_format_check(db, request, &format_rules),
    };

    // CR 407.3, same cross-format rule the authoritative path applies — so the
    // UI hint and the game-creation gate cannot disagree about an ante card.
    let mut reasons: Vec<String> = result.reason.into_iter().collect();
    let ante_cards = ante_deck_violations(db, request, &result.unknown_cards, &format_rules);
    if !ante_cards.is_empty() {
        reasons.push(summarize_cards(
            "Can't be in a deck or sideboard unless the game is played for ante",
            &ante_cards,
            6,
        ));
    }

    (Some(reasons.is_empty()), reasons, result.unknown_cards)
}

struct QuickCheckResult {
    reason: Option<String>,
    unknown_cards: BTreeSet<String>,
}

impl QuickCheckResult {
    fn compatible() -> Self {
        Self {
            reason: None,
            unknown_cards: BTreeSet::new(),
        }
    }

    fn incompatible(reason: String) -> Self {
        Self {
            reason: Some(reason),
            unknown_cards: BTreeSet::new(),
        }
    }

    fn unknown(name: &str) -> Self {
        Self {
            reason: Some(format!("Unknown cards: {name}")),
            unknown_cards: BTreeSet::from([name.to_string()]),
        }
    }
}

fn quick_constructed_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    format_rules: &FormatConfig,
    pool: CardPoolAuthority<'_>,
    format_label: &str,
) -> QuickCheckResult {
    if !request.commander.is_empty() {
        return QuickCheckResult::incompatible(format!(
            "{format_label} decks do not use a commander slot"
        ));
    }
    // CR 100.5 / CR 903.5a: the format's own deck-size rule is authoritative —
    // never re-derive a hardcoded 60 by hand.
    if !format_rules.deck_size.accepts(request.main_deck.len()) {
        return QuickCheckResult::incompatible(format!(
            "{format_label} deck must have {} cards (found {})",
            format_rules.deck_size.requirement_phrase(),
            request.main_deck.len()
        ));
    }
    match format_rules.sideboard_policy {
        SideboardPolicy::Forbidden if !request.sideboard.is_empty() => {
            return QuickCheckResult::incompatible(format!(
                "{format_label} does not allow a sideboard"
            ));
        }
        SideboardPolicy::Limited(max) if request.sideboard.len() as u32 > max => {
            return QuickCheckResult::incompatible(format!(
                "Sideboard has {} cards (maximum {})",
                request.sideboard.len(),
                max
            ));
        }
        SideboardPolicy::Forbidden | SideboardPolicy::Limited(_) | SideboardPolicy::Unlimited => {}
    }

    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut restricted = HashSet::new();
    for name in construction_deck_cards(request) {
        let resolved = db.lookup_key(name);
        if db.get_face_by_name(&resolved).is_none() {
            return QuickCheckResult::unknown(name);
        }
        let canonical = canonical_deck_count_key(db, name);
        *counts.entry(canonical.clone()).or_insert(0) += 1;
        match pool.status(db, &resolved) {
            Some(LegalityStatus::Legal) => {}
            Some(LegalityStatus::Restricted) => {
                restricted.insert(canonical);
            }
            Some(status) => {
                return QuickCheckResult::incompatible(format!(
                    "Not {format_label} legal: {name} ({})",
                    status_label(status)
                ));
            }
            None => {
                return QuickCheckResult::incompatible(format!(
                    "Not {format_label} legal: {name} (not legal in {format_label})"
                ));
            }
        }
    }

    let limit = format_rules.default_deck_copy_limit;
    if let Some(reason) = copy_limit_violations(db, &counts, limit).into_iter().next() {
        return QuickCheckResult::incompatible(format!("{}: {reason}", copy_limit_label(limit)));
    }
    if let Some(reason) = restricted_copy_violations(db, &counts, &restricted)
        .into_iter()
        .next()
    {
        return QuickCheckResult::incompatible(format!(
            "More than 1 copy of a restricted card: {reason}"
        ));
    }

    QuickCheckResult::compatible()
}

/// Summary-path twin of [`evaluate_commander_with_format`], and bound by the
/// same invariant: a fact one twin derives from `format_rules` and the other
/// hard-codes is a divergence between the full and summary verdicts for the
/// same deck. The two must state the same answer for the same request.
fn quick_commander_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    rules: CommanderVariantRules,
    format_rules: &FormatConfig,
) -> QuickCheckResult {
    let legality_format = format_rules.format.legality_format();
    let format_label = format_rules.format.label();
    let expected = format_rules.deck_size;
    // CR 702.124g: at most two commanders. Pre-existing and unchanged by this
    // phase; named here so a later reader does not mistake draft-core's two
    // authorities for the complete set.
    if request.commander.is_empty() || request.commander.len() > 2 {
        return QuickCheckResult::incompatible(format!(
            "{format_label} decks require 1 or 2 commanders (found {})",
            request.commander.len()
        ));
    }
    // CR 903.5e: Commander-style formats do not start with a sideboard. Extra
    // entries in the submitted list are silently ignored at game load (see
    // `load_deck_into_state` in `deck_loading.rs`) — strip them here so the
    // shape, singleton, and color-identity checks below operate on the actual
    // loaded deck.
    let stripped = request_without_sideboard(request);
    let request = &stripped;

    if let Some(unknown_companion) = request
        .companion
        .iter()
        .find(|name| db.get_face_by_name(name).is_none())
    {
        return QuickCheckResult::unknown(unknown_companion);
    }

    let mut companion_reasons = Vec::new();
    validate_commander_companion(db, request, format_rules, &mut companion_reasons);
    if let Some(reason) = companion_reasons.into_iter().next() {
        return QuickCheckResult::incompatible(reason);
    }

    let represented_in_main = commanders_represented_in_main(db, request);
    let total_cards = request.main_deck.len() + (request.commander.len() - represented_in_main);
    // CR 903.5a: the format's `DeckSizeRule` is the single authority for
    // min-vs-exact; this seam must not re-derive it with `!=`.
    if !expected.accepts(total_cards) {
        return QuickCheckResult::incompatible(format!(
            "{format_label} deck must have {} cards (found {total_cards})",
            expected.requirement_phrase()
        ));
    }

    let mut commander_identity = HashSet::new();
    for name in &request.commander {
        let Some(face) = db.get_face_by_name(name) else {
            return QuickCheckResult::unknown(name);
        };
        if !(rules.eligible)(face) {
            return QuickCheckResult::incompatible(format!("{}: {name}", rules.eligibility_error));
        }
        commander_identity.extend(card_color_identity(face));
    }
    if request.commander.len() == 2 {
        let face_a = db.get_face_by_name(&request.commander[0]);
        let face_b = db.get_face_by_name(&request.commander[1]);
        if let (Some(a), Some(b)) = (face_a, face_b) {
            // CR 903.13f(3): the summary-path twin of the full validator's
            // pairing check; same per-variant axis, same source.
            if !are_valid_partners(a, b, rules.partner_grant) {
                return QuickCheckResult::incompatible(format!(
                    "Invalid partner pairing: {} and {} do not have compatible partner keywords",
                    request.commander[0], request.commander[1]
                ));
            }
        }
    }

    // CR 903.5b copy counting is the full validator's `combined_copy_counts`,
    // not a second inline tally: that helper keys every listing by RESOLVED
    // card identity (CR 709.2 / CR 712.1 — one physical card per multi-face
    // card), so re-deriving counts here would make the summary path reach a
    // different singleton verdict than the full path for the same decklist.
    let counts = combined_copy_counts(db, request, CommandZoneNetting::NetAgainstMainDeck);
    for name in construction_deck_cards(request) {
        let resolved = db.lookup_key(name);
        let Some(face) = db.get_face_by_name(&resolved) else {
            return QuickCheckResult::unknown(name);
        };
        // CR 903.13e: `None` means the format has no constructed legality
        // table, so there is nothing to check.
        if let Some(legality_format) = legality_format {
            if !rules.skip_commander_legality || !is_commander_entry(db, request, name) {
                match db.legality_status(&resolved, legality_format) {
                    Some(status) if status.is_legal() => {}
                    Some(status) => {
                        return QuickCheckResult::incompatible(format!(
                            "Not {format_label} legal: {name} ({})",
                            status_label(status)
                        ));
                    }
                    None => {
                        return QuickCheckResult::incompatible(format!(
                            "Not {format_label} legal: {name} (not legal in {format_label})"
                        ));
                    }
                }
            }
        }
        if is_commander_entry(db, request, name) {
            continue;
        }
        for color in card_color_identity(face) {
            if !commander_identity.contains(&color) {
                return QuickCheckResult::incompatible(format!(
                    "Cards outside commander's color identity: {name}"
                ));
            }
        }
    }

    // CR 702.139b: A companion is outside the Commander starting deck. Check
    // its format legality separately without contributing to the starting
    // deck's size or singleton count.
    for name in &request.companion {
        // CR 903.13e: as above — no legality table, nothing to check.
        let Some(legality_format) = legality_format else {
            break;
        };
        match db.legality_status(name, legality_format) {
            Some(status) if status.is_legal() => {}
            Some(status) => {
                return QuickCheckResult::incompatible(format!(
                    "Not {format_label} legal: {name} ({})",
                    status_label(status)
                ));
            }
            None => {
                return QuickCheckResult::incompatible(format!(
                    "Not {format_label} legal: {name} (not legal in {format_label})"
                ));
            }
        }
    }

    // CR 903.5b: other than basic lands, each card in a Commander deck must
    // have a different English name — but CR 903.13f(2) disapplies that for
    // Commander Draft. The limit is a format axis, so this asks `format_rules`
    // the same question the full validator asks and the two cannot disagree.
    if let Some(reason) = copy_limit_violations(db, &counts, format_rules.default_deck_copy_limit)
        .into_iter()
        .next()
    {
        return QuickCheckResult::incompatible(format!("Singleton violations: {reason}"));
    }

    QuickCheckResult::compatible()
}

/// `legality_format` is no longer a parameter: `quick_commander_check` derives
/// it from `format_rules`, and nothing else in this function consulted it.
/// That also removes an `unwrap()` on `legality_format()` at the call site.
fn quick_brawl_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    format_label: &str,
    format_rules: &FormatConfig,
) -> QuickCheckResult {
    if request.commander.len() != 1 {
        return QuickCheckResult::incompatible(format!(
            "{format_label} decks require exactly 1 commander (found {})",
            request.commander.len()
        ));
    }
    let name = &request.commander[0];
    let Some(face) = db.get_face_by_name(name) else {
        return QuickCheckResult::unknown(name);
    };
    if !is_brawl_commander_eligible(face) {
        return QuickCheckResult::incompatible(format!(
            "{format_label} commander must be a legendary creature or legendary planeswalker: {name}"
        ));
    }

    quick_commander_check(
        db,
        request,
        CommanderVariantRules {
            eligible: is_brawl_commander_eligible,
            eligibility_error:
                "Brawl commander must be a legendary creature or legendary planeswalker",
            skip_commander_legality: false,
            // CR 903.13f(3) is scoped to Commander Draft; Brawl never grants.
            partner_grant: None,
        },
        format_rules,
    )
}

fn evaluate_selected_format(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    bo3_ready: bool,
) -> (Option<bool>, Vec<String>) {
    let Some(selected) = request.selected_format.as_ref() else {
        return (None, Vec::new());
    };
    let format = selected.tag();

    // See `evaluate_selected_format_summary`: an unresolvable format must be
    // answered before the `uses_commander`-reading pre-guards.
    let Ok(format_rules) = selected.rules() else {
        return (Some(false), vec![CUSTOM_FORMAT_UNRESOLVED.to_string()]);
    };
    let uses_commander = format_rules.uses_commander;

    if !uses_commander && !request.companion.is_empty() {
        return (
            Some(false),
            vec![format!(
                "{} does not use a dedicated companion slot",
                format.label()
            )],
        );
    }
    if format != GameFormat::Oathbreaker && !request.signature_spell.is_empty() {
        return (
            Some(false),
            vec![format!(
                "{} does not use a signature spell slot",
                format.label()
            )],
        );
    }

    let mut reasons = Vec::new();
    let mut compatible = match format {
        GameFormat::Standard => {
            let check = evaluate_constructed(
                db,
                request,
                unknown_cards,
                &format_rules,
                CardPoolAuthority::LegalityTable(LegalityFormat::Standard),
                "Standard",
            );
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::Commander => {
            let check = evaluate_commander_with_format(
                db,
                request,
                unknown_cards,
                CommanderVariantRules::commander(),
                &format_rules,
            );
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::Pioneer
        | GameFormat::Modern
        | GameFormat::Premodern
        | GameFormat::Legacy
        | GameFormat::Vintage
        | GameFormat::Historic
        | GameFormat::Timeless
        | GameFormat::Pauper => {
            let check = evaluate_constructed(
                db,
                request,
                unknown_cards,
                &format_rules,
                CardPoolAuthority::LegalityTable(format.legality_format().unwrap()),
                &format.label(),
            );
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::PauperCommander | GameFormat::DuelCommander => {
            // Both variants share Commander's structural rules (100-card
            // singleton, command zone). We route them through the existing
            // Commander check against the format's own legality table — the
            // card pool differs from Commander but the deck shape is identical.
            let check = evaluate_commander_with_format(
                db,
                request,
                unknown_cards,
                match format {
                    GameFormat::PauperCommander => CommanderVariantRules::pauper_commander(),
                    GameFormat::DuelCommander => CommanderVariantRules::duel_commander(),
                    _ => unreachable!("commander variant branch only handles PDH and Duel"),
                },
                &format_rules,
            );
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::Brawl | GameFormat::HistoricBrawl => {
            let check = evaluate_brawl(
                db,
                request,
                unknown_cards,
                format.legality_format().unwrap(),
                &format.label(),
                &format_rules,
            );
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::TinyLeaders => {
            let check = evaluate_tiny_leaders(db, request, unknown_cards, &format_rules);
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::Oathbreaker => {
            let check = evaluate_oathbreaker(db, request, unknown_cards, &format_rules);
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::Momir => {
            let check = evaluate_momir(db, request, unknown_cards);
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::Planechase => {
            let check = evaluate_planechase(db, request, unknown_cards, &format_rules);
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::Archenemy => {
            let check = evaluate_archenemy(db, request, unknown_cards, &format_rules);
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        // CR 903.13f: routes through the shared commander validator — see the
        // matching arm in the quick-check dispatch.
        GameFormat::CommanderDraft => {
            let check = evaluate_commander_with_format(
                db,
                request,
                unknown_cards,
                CommanderVariantRules::commander_draft(commander_draft_partner_grant(request)),
                &format_rules,
            );
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::FreeForAll | GameFormat::TwoHeadedGiant | GameFormat::Limited => true,
        // Phase 1d: reachable for a `Resolved` Custom config (the early guard
        // above only fails closed for an unresolvable bare `Tag(Custom(_))`),
        // and delegates to the real evaluator.
        GameFormat::Custom(_) => {
            let check = evaluate_custom_format(db, request, unknown_cards, &format_rules);
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
    };

    // CR 407.3: ante cards are barred from decks and sideboards regardless of
    // format — see `ante_deck_violations` for why this cannot live inside any
    // one of the per-format arms above.
    let ante_cards = ante_deck_violations(db, request, unknown_cards, &format_rules);
    if !ante_cards.is_empty() {
        compatible = false;
        reasons.push(summarize_cards(
            "Can't be in a deck or sideboard unless the game is played for ante",
            &ante_cards,
            6,
        ));
    }

    // CR 100.4 × MatchType::Bo3: BO3 requires a sideboard regardless of format.
    // `SideboardPolicy::Unlimited` formats (FreeForAll, TwoHeadedGiant) impose
    // no size cap, so the only cross-cutting requirement is non-empty. The
    // constructed-policy branches above enforce the 15-card upper bound.
    if matches!(request.selected_match_type, Some(MatchType::Bo3)) && !bo3_ready {
        compatible = false;
        reasons.push("BO3 requires a sideboard".to_string());
    }

    (Some(compatible), reasons)
}

fn evaluate_deck_coverage(db: &CardDatabase, request: &DeckCompatibilityRequest) -> DeckCoverage {
    // One canonical key drives all three outputs — the copy counts, the unique
    // set, and `total_unique`. `canonical_deck_count_key` is the single
    // copy-count authority in this module (shared with `combined_copy_counts`,
    // and therefore with the CR 100.2a copy-limit verdict), so coverage and
    // copy limits can never bucket the same card differently. Keying by
    // `lookup_key` here instead would diverge for the ~30 cards `oracle-gen`
    // stores under a hidden `[oracle-id]` key (minted by
    // `oracle_gen::insert_hidden_multiface`, preserved by
    // `CardDatabase::export_subset_json`),
    // whose storage key is not `name.to_lowercase()`.
    //
    // Each key keeps one raw spelling for display/lookup, so a decklist mixing
    // a composite name ("Fire // Ice"), a glued one ("Fire//Ice"), a front-face
    // name ("Fire"), and an unaccented alias ("Nazgul") resolves to a single
    // entry counted once — not N entries each claiming the full copy count.
    let mut copy_counts: HashMap<String, usize> = HashMap::new();
    let mut unique_names: HashMap<String, &str> = HashMap::new();
    for name in all_deck_cards(request) {
        let canonical = canonical_deck_count_key(db, name);
        *copy_counts.entry(canonical.clone()).or_insert(0) += 1;
        unique_names.entry(canonical).or_insert(name);
    }

    let mut unsupported_cards = Vec::new();
    let mut supported_count = 0usize;
    // An unresolvable name (typo, un-exported card) belongs to NEITHER coverage
    // bucket — `card_face_gaps` needs a face to inspect. Counting it in
    // `total_unique` anyway would silently break the deck builder's
    // supported/total ratio and the
    // `supported_unique + unsupported_cards.len() == total_unique` invariant, so
    // it is excluded from the total instead. Unknown cards are surfaced to the
    // user separately, through `collect_unknown_cards`.
    let mut resolved_unique = 0usize;

    for (canonical, name) in &unique_names {
        if let Some(face) = db.get_face_by_name(name) {
            resolved_unique += 1;
            let gaps = crate::game::coverage::card_face_gaps(face);
            if gaps.is_empty() {
                supported_count += 1;
            } else {
                let copies = copy_counts.get(canonical).copied().unwrap_or(1);
                let parse_details = crate::game::coverage::build_parse_details_for_face(face);
                unsupported_cards.push(UnsupportedCard {
                    name: face.name.clone(),
                    gaps,
                    oracle_text: face.oracle_text.clone(),
                    parse_details,
                    copies,
                });
            }
        }
        // Unknown cards are already tracked separately; skip them here.
    }

    unsupported_cards.sort_by(|a, b| a.name.cmp(&b.name));

    DeckCoverage {
        total_unique: resolved_unique,
        supported_unique: supported_count,
        unsupported_cards,
    }
}

/// Check deck legality across all known formats. A deck is "legal" in a format
/// only if every card is legal there. If any card is banned, the deck is "banned".
/// Otherwise if any card is not legal, the deck is "not_legal".
fn evaluate_format_legality(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> BTreeMap<String, String> {
    let unique_names: HashSet<&str> = all_deck_cards(request).collect();
    let mut result = BTreeMap::new();

    for format in LegalityFormat::ALL {
        let mut worst = LegalityStatus::Legal;
        for name in &unique_names {
            let status = db
                .legality_status(name, format)
                .unwrap_or(LegalityStatus::NotLegal);
            match status {
                LegalityStatus::Banned => {
                    worst = LegalityStatus::Banned;
                    break; // Can't get worse
                }
                LegalityStatus::NotLegal => {
                    worst = LegalityStatus::NotLegal;
                    break; // Deck is already illegal — no need to scan further
                }
                LegalityStatus::Restricted | LegalityStatus::Legal => {}
            }
        }
        result.insert(
            format.as_key().to_string(),
            worst.as_export_str().to_string(),
        );
    }

    result
}

fn collect_unknown_cards(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> BTreeSet<String> {
    let mut unknown = BTreeSet::new();
    for name in all_deck_cards(request) {
        if !card_is_known(db, name) {
            unknown.insert(name.to_string());
        }
    }
    for name in &request.planar_deck {
        if !card_is_known(db, name) {
            unknown.insert(name.to_string());
        }
    }
    for name in &request.scheme_deck {
        if !card_is_known(db, name) {
            unknown.insert(name.to_string());
        }
    }
    unknown
}

/// CR 903.5c: collect every main-deck card whose color identity is not a
/// subset of `identity`. Shared by the command-zone formats so the
/// color-identity-subset loop lives in one place instead of being copied per
/// format. `is_command_zone_card` skips cards that occupy the command zone
/// (e.g. a commander also listed in the main deck); unknown cards are skipped
/// so they are reported only once under "Unknown cards".
fn color_identity_violations(
    db: &CardDatabase,
    main_deck: &[String],
    identity: &HashSet<ManaColor>,
    unknown_cards: &BTreeSet<String>,
    is_command_zone_card: impl Fn(&str) -> bool,
) -> BTreeSet<String> {
    let mut violations = BTreeSet::new();
    for name in main_deck {
        if is_command_zone_card(name.as_str()) || unknown_cards.contains(name.as_str()) {
            continue;
        }
        if let Some(face) = db.get_face_by_name(name) {
            if card_color_identity(face)
                .iter()
                .any(|color| !identity.contains(color))
            {
                // Insert the resolved face name, not the caller's raw spelling,
                // so the `BTreeSet` actually dedups a card listed under two
                // spellings (composite + front face, or accented + unaccented).
                // Matches the sibling `basic_type_violations` loop in
                // `evaluate_oathbreaker`, which already keys by `face.name`.
                violations.insert(face.name.clone());
            }
        }
    }
    violations
}

/// CR 903.4: Compute color identity of a single card from mana cost + color indicator.
///
/// Public because the Commander Draft bot deck-builder (`draft-wasm`) must reach
/// the same CR 903.5c verdict this module's `color_identity_violations` reaches
/// for the human deck-builder. Reading `CardFace::color_identity` directly is
/// not equivalent: this function also falls back to the mana cost's shards and
/// `color_override` when the field is empty.
pub fn card_color_identity(face: &CardFace) -> HashSet<ManaColor> {
    if !face.color_identity.is_empty() {
        return face.color_identity.iter().copied().collect();
    }

    let mut colors = HashSet::new();
    if let ManaCost::Cost { shards, .. } = &face.mana_cost {
        for shard in shards {
            for color in ManaColor::ALL {
                if shard.contributes_to(color) {
                    colors.insert(color);
                }
            }
        }
    }
    if let Some(overrides) = &face.color_override {
        for color in overrides {
            colors.insert(*color);
        }
    }
    colors
}

/// Collects the combined color identity of all cards in the deck from their mana costs
/// and color overrides, returned as single-letter codes in WUBRG order.
fn collect_color_identity(db: &CardDatabase, request: &DeckCompatibilityRequest) -> Vec<String> {
    let mut colors = HashSet::new();

    // Deduplicate card names — we only need each unique card once
    let unique_names: HashSet<&str> = all_deck_cards(request).collect();

    for name in unique_names {
        if let Some(face) = db.get_face_by_name(name) {
            colors.extend(card_color_identity(face));
        }
    }

    // Return in canonical WUBRG order
    ManaColor::ALL
        .iter()
        .filter(|c| colors.contains(c))
        .map(mana_color_letter)
        .collect()
}

/// Returns the engine-authored WUBRG color distribution for known main-deck cards.
fn collect_main_deck_color_distribution(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> Vec<DeckColorDistributionEntry> {
    let mut counts = HashMap::new();

    for name in &request.main_deck {
        let Some(face) = db.get_face_by_name(name) else {
            continue;
        };
        for color in card_color_identity(face) {
            *counts.entry(color).or_insert(0usize) += 1;
        }
    }

    let total: usize = counts.values().sum();
    if total == 0 {
        return Vec::new();
    }

    ManaColor::ALL
        .iter()
        .filter_map(|color| {
            let count = *counts.get(color)?;
            let percentage = count as f64 / total as f64 * 100.0;
            Some(DeckColorDistributionEntry {
                color: *color,
                count,
                percentage,
                display_percentage: percentage.round() as u8,
            })
        })
        .collect()
}

fn mana_color_letter(color: &ManaColor) -> String {
    match color {
        ManaColor::White => "W",
        ManaColor::Blue => "U",
        ManaColor::Black => "B",
        ManaColor::Red => "R",
        ManaColor::Green => "G",
    }
    .to_string()
}

/// Returns true if the card is in the database. Composite multi-face names
/// ("Front // Back", glued or spaced) and unaccented aliases are resolved by
/// `CardDatabase::lookup_key` inside the accessor — this function must not
/// pre-split the name itself (see `lookup_key`'s ordering contract).
fn card_is_known(db: &CardDatabase, name: &str) -> bool {
    db.get_face_by_name(name).is_some()
}

/// Single authority for "are these two decklist entries the same card?".
///
/// Two spellings denote the same card when they resolve to the same database
/// key, which is what `CardDatabase::lookup_key` decides — composite multi-face
/// names (`"Front // Back"`, spaced or glued) collapse to their front face, and
/// unaccented aliases fold to the indexed name.
///
/// CR 709.2 (although split cards have two castable halves, each split card is
/// only one card) and CR 712.1 + CR 712.8a (a double-faced card is one card
/// with two faces; outside the battlefield and stack — which includes every
/// deck-construction zone — it has only its front face's characteristics) are
/// what authorize collapsing a composite spelling and a front-face spelling to
/// one identity. This is deliberately NOT CR 201.3: interchangeable names are
/// two separately printed cards pinned together by a printed indicator
/// (CR 201.3c), a different mechanic that this module does not implement. A
/// split card and a DFC each have two *distinct* names (CR 709.4a, CR 201.4d),
/// so they are not interchangeable-name cards — they collapse here because they
/// are one physical card, not because their names are equated.
///
/// Raw
/// `eq_ignore_ascii_case` is NOT equivalent and must not be used for
/// name-identity decisions in this module: a commander listed in the command
/// zone by its composite name and in the 99 by its front name is one card, and
/// the raw comparison reports it as two.
///
/// Every CR 903.5a deck-size, CR 903.5b singleton, CR 903.5c color-identity,
/// and commander-legality-skip decision routes through here so the module can
/// never resolve a name two different ways.
fn same_card(db: &CardDatabase, left: &str, right: &str) -> bool {
    db.lookup_key(left) == db.lookup_key(right)
}

/// CR 903.5a: a commander is part of the 100, so a decklist that names it in
/// both the command zone and the main deck describes ONE physical card. Counts
/// how many command-zone entries are also present in the main deck, comparing
/// resolved keys via `same_card` (CR 709.2 / CR 712.1 one-physical-card
/// identity) so a composite/front-face spelling split is not mistaken for two
/// distinct cards.
fn commanders_represented_in_main(db: &CardDatabase, request: &DeckCompatibilityRequest) -> usize {
    request
        .commander
        .iter()
        .filter(|name| {
            request
                .main_deck
                .iter()
                .any(|card| same_card(db, card, name))
        })
        .count()
}

/// True when `name` denotes one of the deck's commanders under any spelling.
/// Shared by the CR 903.5b singleton exemption, the CR 903.5c color-identity
/// skip, and the commander legality skip.
fn is_commander_entry(db: &CardDatabase, request: &DeckCompatibilityRequest, name: &str) -> bool {
    request
        .commander
        .iter()
        .any(|commander| same_card(db, commander, name))
}

/// Combined copy counts across main deck + sideboard + commander, keyed by the
/// canonical (DFC-resolved, lowercased) card name so `"Plains"`/`"plains"` and
/// `"Delver of Secrets // Insectile Aberration"`/`"Delver of Secrets"` are
/// counted as the same card.
///
/// CR 100.2a (no more than four of any card with a particular English name) and
/// CR 903.5b (each card in a Commander deck must have a different English name)
/// both count PHYSICAL cards, so the bucket must be one per physical card. A
/// multi-face card is one card — CR 709.2 for split cards, CR 712.1 + CR 712.8a
/// for double-faced cards, which have only their front face's characteristics
/// in every deck-construction zone — so both of its spellings key to one
/// bucket. That collapse comes from the one-card rule, NOT from CR 201.3:
/// a split card has two names (CR 709.4a) and a DFC's back face is a separately
/// choosable name (CR 201.4d), so these faces are not interchangeable names in
/// the CR 201.3 sense (which requires a printed indicator per CR 201.3c).
///
/// Uses the indexed face name when the card resolves so alias spellings
/// ("Nazgul" vs "Nazgûl") merge into one bucket for copy-limit checks.
fn canonical_deck_count_key(db: &CardDatabase, name: &str) -> String {
    let resolved = db.lookup_key(name);
    db.get_face_by_name(&resolved)
        .map(|face| face.name.to_lowercase())
        .unwrap_or(resolved)
}

/// Whether the evaluating format has a command zone whose entries are part of
/// the deck proper.
///
/// CR 903.5a makes the commander one of the 100, so a decklist naming a
/// command-zone card in BOTH its zone slot and the main deck still describes a
/// single physical card, and the duplicate must be netted out before the copy
/// limit is applied. Formats with no command zone (constructed, Planechase,
/// Archenemy) have no such identity: they must count `construction_deck_cards`
/// verbatim, or netting would silently hide one copy of any card that also
/// appears in a (rejected, but still populated) commander slot and turn a real
/// CR 100.2a violation into a pass, from a rule that has no commander concept.
///
/// The set of formats whose `evaluate_*`/`quick_*` function passes
/// `NetAgainstMainDeck` is exactly the set for which
/// `GameFormat::command_zone_holds_decklist_commander()` returns `true`, and
/// `game::deck_loading` reads that predicate to decide both what it nets out of
/// the library and what it places in the command zone. The two must agree: a
/// format netted here but not placed there starts the game a card short, and
/// one placed but not netted starts it a card long.
/// `types::format`'s
/// `command_zone_holds_decklist_commander_matches_the_validator_netting_set`
/// locks that agreement — update it when adding a format to either side.
///
/// Typed rather than a `bool` so each call site states which rule it is under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandZoneNetting {
    /// CR 903.5a: net a command-zone card that is also listed in the main deck
    /// down to the one physical card it is.
    NetAgainstMainDeck,
    /// No command zone: count every listed slot verbatim.
    CountVerbatim,
}

fn combined_copy_counts(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    netting: CommandZoneNetting,
) -> HashMap<String, u32> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for name in construction_deck_cards(request) {
        let canonical = canonical_deck_count_key(db, name);
        *counts.entry(canonical).or_insert(0) += 1;
    }
    if netting == CommandZoneNetting::CountVerbatim {
        return counts;
    }
    // CR 903.5a: the commander is one of the 100, so a decklist naming it in
    // both the command zone and the main deck still describes a single physical
    // card. `construction_deck_cards` chains both slots, so that card was just
    // counted twice — decrement the duplicate before CR 903.5b sees it, or the
    // commander is reported as a singleton violation against itself. Comparing
    // resolved keys (CR 709.2 / CR 712.1: a split or double-faced card is one
    // physical card) is what makes this fire for a composite-named
    // commander listed in the 99 by its front name; the same double-listing
    // is already netted out of the CR 903.5a total by
    // `commanders_represented_in_main`.
    //
    // The Oathbreaker signature spell (Oathbreaker RC) is a second command-zone
    // slot with the same "one physical card" identity, and
    // `construction_deck_cards` chains it too — so it is netted on the same
    // axis rather than as a special case. Without it, a compositely-named
    // signature spell listed by its front face in the 58 is reported as a
    // CR 903.5b singleton violation against itself.
    //
    // The decrement is bounded by OCCURRENCE, not merely gated on presence:
    // subtract `min(command_zone_entries, main_deck_occurrences)` per canonical
    // key. A per-entry `saturating_sub(1)` gated only on "the main deck
    // contains this card at all" over-credits whenever two command-zone slots
    // resolve to the same card — partner slots spelled composite and front-face
    // ("Fire // Ice" + "Fire"), or an Oathbreaker whose commander and signature
    // spell name one card — netting two copies away against a single main-deck
    // listing and hiding a genuine CR 903.5b violation. Netting must be the
    // exact CR 903.5a double-listing correction, never a blanket amnesty.
    let mut main_deck_occurrences: HashMap<String, u32> = HashMap::new();
    for name in &request.main_deck {
        *main_deck_occurrences
            .entry(canonical_deck_count_key(db, name))
            .or_insert(0) += 1;
    }
    let mut command_zone_entries: HashMap<String, u32> = HashMap::new();
    for entry in request
        .commander
        .iter()
        .chain(request.signature_spell.iter())
    {
        *command_zone_entries
            .entry(canonical_deck_count_key(db, entry))
            .or_insert(0) += 1;
    }
    for (canonical, command_copies) in command_zone_entries {
        let netted = command_copies.min(
            main_deck_occurrences
                .get(&canonical)
                .copied()
                .unwrap_or_default(),
        );
        if let Some(count) = counts.get_mut(&canonical) {
            *count = count.saturating_sub(netted);
        }
    }
    counts
}

/// CR 100.2a: Flag card names whose combined count exceeds `max_copies`,
/// excluding basic lands and cards whose Oracle text grants a per-card deck-limit
/// override (e.g. Relentless Rats — "any number"; Seven Dwarves → 7, Nazgûl → 9
/// via "up to N"; Vazal singleton → 1). The typed override is resolved from
/// `face.deck_copy_limit`, falling back to a live Oracle-text parse for faces
/// loaded without synthesis (test fixtures, `from_json_str`).
///
/// Input counts must be keyed by canonical (DFC-resolved, lowercased) names —
/// use `combined_copy_counts`.
/// `format_default` is a `DeckCopyLimit` rather than a `u32` because a `u32`
/// cannot express `Unlimited`, which is precisely CR 903.13f(2)'s value. The
/// function already wrapped its argument in `DeckCopyLimit::UpTo(..)`
/// internally, so this REMOVES a conversion rather than adding one.
///
/// Note the default is a default: `effective_copy_limit` ends
/// `deck_copy_limit_for(..).unwrap_or(format_default)`, so a card's PRINTED
/// deck-construction limit still binds under `Unlimited`. That is
/// rules-correct — CR 903.13f's exception is to CR 903.5, not to a card's own
/// deck-construction ability — so do not skip the ladder for Commander Draft.
fn copy_limit_violations(
    db: &CardDatabase,
    counts: &HashMap<String, u32>,
    format_default: DeckCopyLimit,
) -> BTreeSet<String> {
    let mut violations = BTreeSet::new();
    for (canonical_name, count) in counts {
        match effective_copy_limit(db, canonical_name, format_default) {
            DeckCopyLimit::Unlimited => continue,
            DeckCopyLimit::UpTo(n) if *count <= n => continue,
            DeckCopyLimit::UpTo(_) => {} // cap exceeded — flag
        }
        // Prefer the database's canonical display casing for error messages;
        // fall back to the lowercased key if the face is missing (e.g. for
        // tests with unresolved names).
        let display = db
            .get_face_by_name(canonical_name)
            .map(|f| f.name.clone())
            .unwrap_or_else(|| canonical_name.clone());
        violations.insert(format!("{display} ({count} copies)"));
    }
    violations
}

/// CR 100.6: Tournament rules may limit a card's use. Flag any card the
/// active format marks as `Restricted` whose combined main+sideboard count
/// exceeds Phase's established one-copy format-policy. Vintage is the
/// canonical consumer, but the policy applies to any format whose legality
/// table uses `Restricted`.
/// `restricted_canonical` is the set of canonical (DFC-resolved, lowercased)
/// names that the legality table marks as `Restricted` for the active format;
/// `counts` is the combined main+sideboard map produced by `combined_copy_counts`.
///
/// Note: this hardcodes the `<= 1` Restricted ceiling and does NOT consult any
/// per-card `DeckCopyLimit` override — no override card is currently
/// Vintage-Restricted, so the interaction is out of scope.
fn restricted_copy_violations(
    db: &CardDatabase,
    counts: &HashMap<String, u32>,
    restricted_canonical: &HashSet<String>,
) -> BTreeSet<String> {
    let mut violations = BTreeSet::new();
    for canonical in restricted_canonical {
        let Some(count) = counts.get(canonical) else {
            continue;
        };
        if *count <= 1 {
            continue;
        }
        let display = db
            .get_face_by_name(canonical)
            .map(|f| f.name.clone())
            .unwrap_or_else(|| canonical.clone());
        violations.insert(format!("{display} ({count} copies)"));
    }
    violations
}

/// CR 100.2a / CR 100.4a: the violation-message prefix for an overrun of
/// `limit`, derived from the limit actually enforced rather than a hardcoded
/// "More than 4" — the resolved ceiling can legitimately be stricter than a
/// format's bare default, and a message naming the wrong number is a lie to
/// the deck builder. Single authority for this label so the four
/// combined-main+sideboard consumers can never disagree.
fn copy_limit_label(limit: DeckCopyLimit) -> String {
    match limit {
        DeckCopyLimit::UpTo(n) => format!("More than {n} copies (main + sideboard combined)"),
        // `copy_limit_violations` never populates violations under
        // `DeckCopyLimit::Unlimited` (an unlimited ceiling has no overrun to
        // report), so callers only reach this with a non-empty violation set
        // under `UpTo(..)`. Kept total rather than `unreachable!` so a future
        // caller that labels a limit without a violation set cannot panic.
        DeckCopyLimit::Unlimited => {
            "More than the allowed copies (main + sideboard combined)".to_string()
        }
    }
}

/// CR 100.2a / CR 205.4c / CR 903.5b: The copy ceiling that actually applies to
/// one canonical card name, layering the three rules in precedence order:
///
/// 1. Basic lands are exempt from every copy limit. "Basic" is a supertype
///    (covering Plains/Island/Swamp/Mountain/Forest, Snow-Covered variants,
///    Wastes, and any future basic), not a fixed name allowlist — trust the
///    MTGJSON-populated supertype field. Checked FIRST so basics never cap.
/// 2. A card's printed override (Relentless Rats, Seven Dwarves, Nazgûl,
///    Vazal) replaces the format default in either direction.
/// 3. Otherwise the format default applies.
///
/// The single place this rule is expressed — both `copy_limit_violations`
/// (validation) and [`max_deck_copies`] (deck-builder query) route through it.
fn effective_copy_limit(
    db: &CardDatabase,
    canonical_name: &str,
    format_default: DeckCopyLimit,
) -> DeckCopyLimit {
    if db
        .get_face_by_name(canonical_name)
        .is_some_and(|face| face.card_type.supertypes.contains(&Supertype::Basic))
    {
        return DeckCopyLimit::Unlimited;
    }
    deck_copy_limit_for(db, canonical_name).unwrap_or(format_default)
}

/// CR 100.2a / CR 100.2b / CR 903.5b: How many copies of `name` a deck built
/// under `format_config` may legally contain, counting main deck, sideboard,
/// and command zone together (CR 100.4a: "The four-card limit applies to the
/// combined deck and sideboard"). `Unlimited` means no ceiling.
///
/// The format half of the rule comes from `format_config`'s **resolved**
/// `default_deck_copy_limit` field, never from a bare
/// `GameFormat::default_deck_copy_limit()` call: for a built-in format the two
/// always agree, but for `GameFormat::Custom` only the stored field can carry
/// the format's real declared limit.
///
/// This is the query-shaped counterpart to `copy_limit_violations` and the
/// single authority consumers outside the engine must use — the deck builder
/// disables its increment control from this, and must never re-derive the
/// limit from Oracle text, card type, or a hardcoded four.
///
/// Being query-shaped is why the restricted list is applied here rather than in
/// [`effective_copy_limit`]: validation reports a restricted overrun as its own
/// distinct violation (`restricted_copy_violations`), so the shared helper must
/// keep the two failures separable. A caller asking "how many may I have?"
/// wants the one ceiling that actually binds.
///
/// Admission (every `evaluate_*`/`quick_*` dispatch function in this module)
/// now reads the identical field off the identical type — `format_rules.
/// default_deck_copy_limit`, threaded in via `SelectedFormat::rules()` — so
/// the two halves of the copy rule can never diverge.
///
/// Dispatches on the format DISCRIMINANT (`format_config.format`), never on
/// whether `custom_rules` happens to be populated: matching on
/// `custom_rules.is_some()` instead would put two representable-but-invalid
/// states in the wrong arm — `(format: Vintage, custom_rules: Some(..))`
/// would silently drop Vintage's restricted list, and `(Custom(_),
/// custom_rules: None)` would apply no cap at all (fail-OPEN, against this
/// function's own fail-closed `UpTo(1)` convention below).
/// `validate_custom_rules_consistency` enforces the `format ==
/// Custom(id) ⟺ custom_rules == Some(rules with that id)` biconditional only
/// inside `FormatConfig::deserialize` (an ingress property), never as a type
/// invariant `FormatConfig` itself enforces — so a caller holding a bare,
/// non-deserialized `FormatConfig` value can still violate it, and this
/// function is `pub`.
///
/// Pre-existing, but newly load-bearing: `max_deck_copies_for_format`
/// (`engine-wasm/src/lib.rs:999-1000`) returns `DeckCopyLimit::Unlimited`
/// whenever the `FormatConfig` JSON it receives fails to deserialize. A stale
/// localStorage custom-format save — exactly what the deserialize equality
/// gate rejects after a host edits, say, seat count without re-deriving the
/// rest of the config — therefore leaves the deck builder's `+` control
/// unbounded instead of enforcing this function's declared limit.
pub fn max_deck_copies(
    db: &CardDatabase,
    name: &str,
    format_config: &FormatConfig,
) -> DeckCopyLimit {
    let canonical = canonical_deck_count_key(db, name);
    // CR 100.6: a card a format's tournament rules mark Restricted is legal at
    // no more than one copy, whichever way the format default or the card's
    // printed override would otherwise point. Vintage is the canonical
    // built-in user, but the rule is format-general.
    let restricted =
        match format_config.format {
            GameFormat::Custom(_) => {
                // A custom format's restricted list lives on `custom_rules`, never
                // on the built-in `LegalityFormat` table (`legality_format()` is
                // always `None` for `Custom` — see `types::format.rs`).
                // `custom_rules: None` here means this `FormatConfig` never passed
                // `validate_custom_rules_consistency` — should be unreachable, but
                // if it happens, fail CLOSED: treat it as restricted (`UpTo(1)`),
                // matching this function's own fail-closed convention, rather than
                // silently applying no cap.
                let Some(rules) = format_config.custom_rules.as_deref() else {
                    return DeckCopyLimit::UpTo(1);
                };
                rules.legality.restricted.iter().any(|restricted_name| {
                    canonical_deck_count_key(db, restricted_name) == canonical
                })
            }
            _ => format_config
                .format
                .legality_format()
                .and_then(|legality_format| db.legality_status(name, legality_format))
                .is_some_and(|status| status == LegalityStatus::Restricted),
        };
    if restricted {
        return DeckCopyLimit::UpTo(1);
    }
    effective_copy_limit(db, &canonical, format_config.default_deck_copy_limit)
}

/// CR 100.2a / CR 903.5b: Resolve a card's deck-construction copy-limit override.
/// Reads the precomputed `face.deck_copy_limit` field; falls back to a live
/// Oracle-text parse for faces loaded without synthesis (test fixtures and
/// `CardDatabase::from_json_str` / `from_export_entries`, which skip synthesis).
/// Mirrors `is_commander_eligible`'s synthesized-field-with-live-fallback shape.
pub fn deck_copy_limit_for(db: &CardDatabase, canonical_name: &str) -> Option<DeckCopyLimit> {
    let face = db.get_face_by_name(canonical_name)?;
    if let Some(limit) = face.deck_copy_limit {
        return Some(limit);
    }
    compute_deck_copy_limit_from_text(face.oracle_text.as_deref()?)
}

/// CR 903.13e: a commander filler card that a Commander Draft's booster set
/// lets a player ADD to their card pool, and CR 903.13e's cap on how many.
///
/// The cap applies to the ADDED copies only. The filler cards are themselves
/// printed in the granting sets' own draft boosters, so a player can draft one
/// like any other card; a drafted copy is an ordinary pool card and is neither
/// capped nor required to be a commander.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantableCommanderFiller {
    pub card_name: String,
    /// CR 903.13e: "each player may add up to two cards named ...".
    pub max_copies: u32,
}

/// CR 903.13f(3): a deckbuilding-only extension of the partner ability
/// (CR 702.124h, and per CR 702.124n *only* the partner family -- never
/// "choose a Background" and never "Doctor's companion") to every card that
/// can be a commander by itself and whose colour identity is within
/// `max_colors`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartnerGrant {
    /// CR 903.13f(3): "whose color identity includes one or fewer colors".
    pub max_colors: u8,
}

/// CR 903.13e + CR 903.13f(3): the deck-construction concessions a Commander
/// Draft's booster sets make, as a UNION.
///
/// Every grant in CR 903.13 is conditioned on what the draft CONTAINED ("If the
/// draft contained draft boosters from ..."), and each of the three conditions
/// is stated independently. A draft whose boosters came from several named sets
/// therefore satisfies several conditions at once and carries every grant they
/// make — which is why `fillers` is a collection and not a single card. A draft
/// containing both Commander Masters and Battle for Baldur's Gate boosters
/// concedes The Prismatic Piper AND Faceless One.
///
/// The empty value (`Default`) is "no concession", the rules-correct answer for
/// a deck with no Commander Draft behind it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftSetConcessions {
    /// CR 903.13e: every filler card the draft's sets concede, deduplicated by
    /// card name. Order follows [`DRAFT_SET_CONCESSIONS`] — see
    /// [`draft_set_concessions_for`] — so the value is a function of WHICH sets
    /// the draft contained and not of the order the host named them.
    pub fillers: Vec<GrantableCommanderFiller>,
    pub partner_grant: Option<PartnerGrant>,
}

impl DraftSetConcessions {
    /// CR 903.13e + CR 903.13f(3): merge the concessions of two of a draft's
    /// sets. Each rule's condition is satisfied independently, so the merge is
    /// a union and never an override.
    ///
    /// Neither axis STACKS. CR 903.13e reads "each player may add up to two
    /// cards named The Prismatic Piper" — one allowance for that name, however
    /// many of the sets naming it the draft contained (Commander Legends and
    /// Commander Masters both do). So a filler already present keeps the
    /// larger cap rather than gaining a second one. CR 903.13f(3)'s colour
    /// bound merges the same way: the widest bound any contained set grants.
    fn union(mut self, other: Self) -> Self {
        for filler in other.fillers {
            match self
                .fillers
                .iter_mut()
                .find(|held| held.card_name == filler.card_name)
            {
                Some(held) => held.max_copies = held.max_copies.max(filler.max_copies),
                None => self.fillers.push(filler),
            }
        }
        self.partner_grant = match (self.partner_grant, other.partner_grant) {
            (Some(held), Some(added)) => Some(PartnerGrant {
                max_colors: held.max_colors.max(added.max_colors),
            }),
            (held, added) => held.or(added),
        };
        self
    }
}

/// CR 903.13e + CR 903.13f(3): the complete set of booster sets the
/// Comprehensive Rules name, and what each concedes at deck construction.
///
/// Keyed by SET CODE because both rules condition the concession on what the
/// DRAFT CONTAINED ("If the draft contained draft boosters from ..."), never on
/// a card's own printing. Deriving either concession from a card's printings
/// would grant partner to any Commander-Masters-printed mono-colour legend in
/// constructed Commander, a format CR 903.13f(3) says nothing about -- so
/// `CardDatabase::printings_for` is deliberately NOT consulted here.
///
/// A future CR-named set is one row here and one line in the table test.
///
/// Columns: set code, granted filler card name, CR 903.13f(3) colour bound.
const DRAFT_SET_CONCESSIONS: &[(&str, &str, Option<u8>)] = &[
    // CR 903.13e: "draft boosters from Commander Legends or Commander Masters
    // ... up to two cards named The Prismatic Piper".
    ("CMR", "The Prismatic Piper", None),
    // CR 903.13f(3) names Commander Masters and nothing else, so only this row
    // carries a partner grant.
    ("CMM", "The Prismatic Piper", Some(1)),
    // CR 903.13e: "draft boosters from Commander Legends: Battle for Baldur's
    // Gate ... up to two cards named Faceless One".
    ("CLB", "Faceless One", None),
];

/// CR 903.13e: "each player may add up to two ...".
const GRANTABLE_FILLER_MAX_COPIES: u32 = 2;

/// CR 903.13f(3): the partner grant in force for a Commander Draft request.
///
/// The grant is a property of WHAT THE DRAFT CONTAINED, so it is read from the
/// request's latched set codes and never from a card's printing. An EMPTY list
/// — which is constructed play and the server-hosted path — yields
/// `DraftSetConcessions::default()`, i.e. no grant, which is the rules-correct
/// answer for a deck with no draft behind it, and the same value the
/// `DraftSource::Cube` arm of draft-core's latch produces.
///
/// A mixed-set draft names every set it contained, and the union is taken over
/// all of them: a CMM+CLB draft grants partner because it contained Commander
/// Masters boosters, regardless of which pack CMM filled.
fn commander_draft_partner_grant(request: &DeckCompatibilityRequest) -> Option<PartnerGrant> {
    draft_set_concessions_for(request.draft_set_codes.iter().map(String::as_str)).partner_grant
}

/// CR 903.13e / CR 903.13f(3): what a draft from `set_code` concedes at deck
/// construction. Returns the empty concession for every set the CR does not
/// name, which is the rules-correct answer for a draft the rules say nothing
/// about.
///
/// Matched case-insensitively: `DraftConfig.set_code` is caller-supplied while
/// MTGJSON set codes are uppercase, the same convention
/// `set_gating::parse_gated_sets` already uses.
pub fn draft_set_concessions(set_code: &str) -> DraftSetConcessions {
    DRAFT_SET_CONCESSIONS
        .iter()
        .find(|(code, _, _)| code.eq_ignore_ascii_case(set_code))
        .map_or_else(DraftSetConcessions::default, row_concessions)
}

/// One [`DRAFT_SET_CONCESSIONS`] row as the concession it names. The single
/// place a row becomes a value, so the one-set and union lookups below can
/// never disagree about what a row means.
fn row_concessions((_, card_name, max_colors): &(&str, &str, Option<u8>)) -> DraftSetConcessions {
    DraftSetConcessions {
        fillers: vec![GrantableCommanderFiller {
            card_name: (*card_name).to_string(),
            max_copies: GRANTABLE_FILLER_MAX_COPIES,
        }],
        partner_grant: max_colors.map(|max_colors| PartnerGrant { max_colors }),
    }
}

/// CR 903.13e / CR 903.13f(3): what a draft that CONTAINED boosters from every
/// one of `set_codes` concedes at deck construction.
///
/// The single authority for a draft's concessions, and the one every caller
/// outside this module's own table test should use — `draft_set_concessions`
/// above answers for ONE set, which is only ever a row of this answer.
///
/// Both rules condition their grant on set CONTAINMENT, not on a pack index or
/// a majority, so this is order-insensitive and repetition-insensitive: naming
/// CMM three times, or once, concedes the same thing, and so does CMM+CLB in
/// either order. Sets the rules do not name contribute nothing rather than
/// suppressing what their neighbours concede, which is what makes an
/// ISD+CMM chaos draft still grant Commander Masters' partner ability.
///
/// An empty iterator yields `DraftSetConcessions::default()` — no draft, no
/// concession.
pub fn draft_set_concessions_for<'a>(
    set_codes: impl IntoIterator<Item = &'a str>,
) -> DraftSetConcessions {
    let contained: Vec<&str> = set_codes.into_iter().collect();
    // Driven by the TABLE, not by the caller's sequence, so the result is a
    // function of WHICH named sets the draft contained and nothing else: a
    // repeated code contributes once, and two codes concede the same whichever
    // order the host named them in.
    DRAFT_SET_CONCESSIONS
        .iter()
        .filter(|(code, _, _)| {
            contained
                .iter()
                .any(|named| named.eq_ignore_ascii_case(code))
        })
        .map(row_concessions)
        .fold(DraftSetConcessions::default(), DraftSetConcessions::union)
}

/// Every card reference, including dedicated companion. Use this for card-data
/// lookup, coverage, and legality reporting; use `construction_deck_cards`
/// for ordinary deck-size and copy calculations.
fn all_deck_cards(request: &DeckCompatibilityRequest) -> impl Iterator<Item = &str> {
    request
        .main_deck
        .iter()
        .chain(request.sideboard.iter())
        .chain(request.commander.iter())
        .chain(request.signature_spell.iter())
        .chain(request.companion.iter())
        .map(String::as_str)
}

fn construction_deck_cards(request: &DeckCompatibilityRequest) -> impl Iterator<Item = &str> {
    request
        .main_deck
        .iter()
        .chain(request.sideboard.iter())
        .chain(request.commander.iter())
        .chain(request.signature_spell.iter())
        .map(String::as_str)
}

/// The name to SHOW the user for a decklist spelling — the resolved face name,
/// falling back to the raw spelling when the card is unknown. Presentation and
/// dedup only: this enforces no game rule and intentionally carries no CR
/// annotation. The identity it displays is decided upstream by `same_card` /
/// `canonical_deck_count_key`, which carry the rule citations.
///
/// Every `illegal_cards`-style `BTreeSet` must key on this rather than on the
/// caller's raw spelling, or one physical card listed under several spellings
/// ("Fire // Ice", "Fire//Ice", "Fire") is reported as several separate illegal
/// cards, inflating the reason string and burning the `summarize_cards` name
/// cap on duplicates of one card. Matches the dedup `color_identity_violations`
/// performs by keying on `face.name`.
fn display_name(db: &CardDatabase, name: &str) -> String {
    db.get_face_by_name(name)
        .map(|face| face.name.clone())
        .unwrap_or_else(|| name.to_string())
}

fn status_label(status: LegalityStatus) -> &'static str {
    match status {
        LegalityStatus::Legal => "legal",
        LegalityStatus::NotLegal => "not legal",
        LegalityStatus::Banned => "banned",
        LegalityStatus::Restricted => "restricted",
    }
}

fn summarize_cards(prefix: &str, cards: &BTreeSet<String>, max_names: usize) -> String {
    let mut listed = cards.iter().take(max_names).cloned().collect::<Vec<_>>();
    if cards.len() > max_names {
        listed.push(format!("+{} more", cards.len() - max_names));
    }
    format!("{prefix}: {}", listed.join(", "))
}

/// CR 903.3: A card is eligible to be a commander if it is a legendary creature
/// (903.3a), a legendary Vehicle (903.3b), a legendary Spacecraft with one or more
/// power/toughness boxes (903.3c), a legendary Background enchantment (CR 702.124),
/// or has "can be your commander" in its rules text (903.3a override).
///
/// Reads the pre-computed `face.is_commander` field (union of MTGJSON
/// `leadershipSkills.commander` and our own type-line analysis, synthesized at
/// card-data build time). Falls back to a live type-line check for cards loaded
/// from test fixtures that may not have the field set — mirrors
/// `is_brawl_commander_eligible`.
pub fn is_commander_eligible(face: &CardFace) -> bool {
    if face.is_commander {
        return true;
    }
    crate::database::synthesis::type_line_commander_eligible(face)
}

fn is_pauper_commander_eligible(face: &CardFace) -> bool {
    use crate::types::card::Rarity;

    let is_creature_or_vehicle = face.card_type.core_types.contains(&CoreType::Creature)
        || face.card_type.subtypes.iter().any(|subtype| {
            subtype.eq_ignore_ascii_case("Vehicle") || subtype.eq_ignore_ascii_case("Spacecraft")
        });
    let has_uncommon_printing = face.rarities.contains(&Rarity::Uncommon);
    is_creature_or_vehicle && has_uncommon_printing
}

/// CR 702.124: Public entry point — can these two named cards form a legal
/// co-commander pair? Resolves both faces in the database and applies the full
/// partner-family rules. Returns false if either name is unknown.
///
/// This is the single authority for partner-pairing legality. Deck-builder UIs
/// consume it through the WASM bridge rather than re-implementing the rules, so
/// the engine and frontend can never disagree about a pairing.
/// `grant` is the CR 903.13f(3) deckbuilding partner grant this deck is being
/// built under, or `None` for constructed play. It is passed EXPLICITLY rather
/// than derived from either card, because CR 903.13f(3) conditions the grant on
/// what the DRAFT contained — a property of the session, not of a card.
pub fn can_pair_commanders(
    db: &CardDatabase,
    name_a: &str,
    name_b: &str,
    grant: Option<PartnerGrant>,
) -> bool {
    match (db.get_face_by_name(name_a), db.get_face_by_name(name_b)) {
        (Some(a), Some(b)) => are_valid_partners(a, b, grant),
        _ => false,
    }
}

/// CR 903.13f(3) + CR 702.124h + CR 702.124n: the partner abilities a card has
/// for the purposes of deckbuilding, which under a granting draft is its
/// printed set plus a synthesised generic Partner.
///
/// CR 903.13f(3): "any card which can be a player's commander BY ITSELF and
/// whose color identity includes ONE OR FEWER COLORS is considered to have the
/// partner ability for the purposes of deckbuilding." CR 702.124n bounds an
/// unqualified "the partner ability" to the partner family — partner,
/// partner—[text], and partner with [name] — so the grant is never "choose a
/// Background" and never "Doctor's companion", which CR 702.124f says could not
/// combine with it anyway. Choosing `PartnerType::Generic` from within that
/// family is this engine's decision rather than something CR 702.124n compels:
/// the other two members are parameterised by a [text] or a [name] that
/// CR 903.13f(3) never supplies.
///
/// Expressing the grant as a SYNTHESISED KEYWORD rather than as a special case
/// inside the pairing check is what makes a granted mono-colour legend pair
/// correctly with a card carrying PRINTED generic Partner: a naive
/// `granted(a) && granted(b)` conjunction gets that case wrong.
///
/// Deckbuilding only — nothing is written to the `CardFace`, so no in-game
/// keyword, trigger, or static ability is affected.
fn partner_types_for(face: &CardFace, grant: Option<PartnerGrant>) -> Vec<PartnerType> {
    let mut types: Vec<PartnerType> = face
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            Keyword::Partner(pt) => Some(pt.clone()),
            _ => None,
        })
        .collect();

    if let Some(grant) = grant {
        // CR 903.3 + CR 702.124k: CR 903.3 admits a legendary creature,
        // Vehicle, or Spacecraft card as a commander; CR 702.124k adds that a
        // legendary Background enchantment "can't be your commander unless you
        // have also designated a commander with 'choose a Background'". A
        // Background therefore fails CR 903.13f(3)'s "by itself" condition.
        let by_itself = matches!(
            crate::database::synthesis::commander_qualification(face),
            crate::database::synthesis::CommanderQualification::ByItself
        );
        // CR 903.4 + CR 903.13f(3): CR 903.4 defines the colour identity that
        // CR 903.13f(3)'s "whose color identity includes one or fewer colors"
        // bound is measured against.
        let within_color_bound = card_color_identity(face).len() <= usize::from(grant.max_colors);
        if by_itself && within_color_bound && !types.contains(&PartnerType::Generic) {
            types.push(PartnerType::Generic);
        }
    }

    types
}

/// CR 702.124: Check if two cards form a valid partner pair for co-commanders.
/// Handles the full partner family: Generic Partner, Partner with [Name],
/// Friends Forever, Character Select, Doctor's Companion, and Choose a Background.
///
/// `grant` carries CR 903.13f(3); see [`partner_types_for`].
fn are_valid_partners(face_a: &CardFace, face_b: &CardFace, grant: Option<PartnerGrant>) -> bool {
    let owned_a = partner_types_for(face_a, grant);
    let owned_b = partner_types_for(face_b, grant);
    let partners_a: Vec<&PartnerType> = owned_a.iter().collect();
    let partners_b: Vec<&PartnerType> = owned_b.iter().collect();

    // Any compatible combination across both cards' partner keywords is valid
    partners_a
        .iter()
        .any(|a| partners_b.iter().any(|b| partner_types_compatible(a, b, face_a, face_b)))
        // Also check asymmetric cases: one card has ChooseABackground/DoctorsCompanion
        // and the other has the matching subtype but no partner keyword
        || partners_a
            .iter()
            .any(|a| subtype_partner_match(a, face_b))
        || partners_b
            .iter()
            .any(|b| subtype_partner_match(b, face_a))
}

/// CR 702.124: Check if two partner types are compatible with each other.
fn partner_types_compatible(
    a: &crate::types::keywords::PartnerType,
    b: &crate::types::keywords::PartnerType,
    face_a: &CardFace,
    face_b: &CardFace,
) -> bool {
    use crate::types::keywords::PartnerType;

    match (a, b) {
        (PartnerType::Generic, PartnerType::Generic) => true,
        (PartnerType::With(x), PartnerType::With(y)) => {
            x.eq_ignore_ascii_case(&face_b.name) && y.eq_ignore_ascii_case(&face_a.name)
        }
        (PartnerType::FriendsForever, PartnerType::FriendsForever) => true,
        (PartnerType::CharacterSelect, PartnerType::CharacterSelect) => true,
        _ => false,
    }
}

/// CR 702.124m: Doctor's companion pairs with a legendary Time Lord Doctor
/// creature card that has no other creature types.
fn is_time_lord_doctor_commander(face: &CardFace) -> bool {
    if !face.card_type.supertypes.contains(&Supertype::Legendary)
        || !face.card_type.core_types.contains(&CoreType::Creature)
    {
        return false;
    }
    if !is_commander_eligible(face) {
        return false;
    }
    let subtypes = &face.card_type.subtypes;
    // MTGJSON may emit the single two-word subtype or the split pair.
    if subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Time Lord Doctor"))
    {
        return subtypes.len() == 1;
    }
    subtypes.len() == 2
        && subtypes.iter().any(|s| s.eq_ignore_ascii_case("Doctor"))
        && subtypes.iter().any(|s| s.eq_ignore_ascii_case("Time Lord"))
        && subtypes
            .iter()
            .all(|s| s.eq_ignore_ascii_case("Doctor") || s.eq_ignore_ascii_case("Time Lord"))
}

/// CR 702.124k + CR 702.124m: Check if a partner type matches the other face by subtype.
/// Doctor's Companion pairs with a Time Lord Doctor commander; Choose a Background
/// pairs with any Background.
fn subtype_partner_match(
    partner_type: &crate::types::keywords::PartnerType,
    other_face: &CardFace,
) -> bool {
    use crate::types::keywords::PartnerType;

    match partner_type {
        PartnerType::DoctorsCompanion => is_time_lord_doctor_commander(other_face),
        PartnerType::ChooseABackground => other_face
            .card_type
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Background")),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::types::custom_format::{
        CombatDamageTiming, CommanderEligibilityRule, CustomFormatDef, CustomFormatId,
        CustomFormatRules, LegacyRuleSet, LegendRuleScope, ManaBurnPolicy, PrintingFidelity,
        ReprintPolicy, StructuralRules, WishOutsideGameScope,
    };
    use crate::types::format::DeckSizeRule;
    use crate::types::keywords::PartnerType;
    use serde_json::{Map, Value};

    fn test_db_json() -> String {
        serde_json::json!({
            "legal standard": {
                "name": "Legal Standard",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal",
                    "premodern": "legal",
                    "pioneer": "legal",
                    "modern": "legal",
                    "pauper": "legal",
                    "standardbrawl": "legal",
                    "brawl": "legal"
                }
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"],
                    "core_types": ["Land"],
                    "subtypes": ["Plains"]
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal",
                    "premodern": "legal",
                    "pioneer": "legal",
                    "modern": "legal",
                    "pauper": "legal",
                    "standardbrawl": "legal",
                    "brawl": "legal"
                }
            },
            "not standard": {
                "name": "Not Standard",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "not_legal",
                    "commander": "legal",
                    "premodern": "not_legal",
                    "pioneer": "legal",
                    "pauper": "not_legal",
                    "standardbrawl": "not_legal",
                    "brawl": "legal"
                }
            },
            "pioneer only": {
                "name": "Pioneer Only",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "not_legal",
                    "commander": "legal",
                    "pioneer": "legal",
                    "pauper": "not_legal"
                }
            },
            "premodern banned": {
                "name": "Premodern Banned",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "premodern": "banned"
                }
            },
            "commander banned": {
                "name": "Commander Banned",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "banned"
                }
            },
            "legal commander": {
                "name": "Legal Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Legendary"],
                    "core_types": ["Creature"],
                    "subtypes": []
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal",
                    "standardbrawl": "legal",
                    "brawl": "legal"
                }
            },
            "legendary planeswalker": {
                "name": "Legendary Planeswalker",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Legendary"],
                    "core_types": ["Planeswalker"],
                    "subtypes": []
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal",
                    "standardbrawl": "legal",
                    "brawl": "legal"
                }
            },
            "partner commander": {
                "name": "Partner Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Legendary"],
                    "core_types": ["Creature"],
                    "subtypes": []
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": "Partner",
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [{ "Partner": { "type": "Generic" } }],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal"
                }
            },
            "grub commander": {
                "name": "Grub Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Legendary"],
                    "core_types": ["Creature"],
                    "subtypes": []
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "color_identity": ["Black", "Red"],
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal"
                }
            },
            "red card": {
                "name": "Red Card",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "color_identity": ["Red"],
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal"
                }
            },
            "mountain": {
                "name": "Mountain",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic"], "core_types": ["Land"], "subtypes": ["Mountain"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "color_identity": ["Red"], "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "relentless rats": {
                "name": "Relentless Rats",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": ["Rat"] },
                "power": "2", "toughness": "2", "loyalty": null, "defense": null,
                "oracle_text": "This creature gets +1/+1 for each other creature on the battlefield named Relentless Rats.\nA deck can have any number of cards named Relentless Rats.",
                "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "color_identity": ["Black"], "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "seven dwarves": {
                "name": "Seven Dwarves",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": ["Dwarf"] },
                "power": "3", "toughness": "3", "loyalty": null, "defense": null,
                "oracle_text": "This creature gets +1/+1 for each other creature named Seven Dwarves you control.\nA deck can have up to seven cards named Seven Dwarves.",
                "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "color_identity": ["Red"], "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "nazgûl": {
                "name": "Nazgûl",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": ["Wraith"] },
                "power": "3", "toughness": "3", "loyalty": null, "defense": null,
                "oracle_text": "Deathtouch\nA deck can have up to nine cards named Nazgûl.",
                "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "color_identity": ["Black"], "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "snow-covered plains": {
                "name": "Snow-Covered Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic", "Snow"], "core_types": ["Land"], "subtypes": ["Plains"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "snow-covered island": {
                "name": "Snow-Covered Island",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic", "Snow"], "core_types": ["Land"], "subtypes": ["Island"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "snow-covered swamp": {
                "name": "Snow-Covered Swamp",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic", "Snow"], "core_types": ["Land"], "subtypes": ["Swamp"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "snow-covered mountain": {
                "name": "Snow-Covered Mountain",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic", "Snow"], "core_types": ["Land"], "subtypes": ["Mountain"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "snow-covered forest": {
                "name": "Snow-Covered Forest",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic", "Snow"], "core_types": ["Land"], "subtypes": ["Forest"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "snow-covered wastes": {
                "name": "Snow-Covered Wastes",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic", "Snow"], "core_types": ["Land"], "subtypes": ["Wastes"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            }
        })
        .to_string()
    }

    fn expand(name: &str, count: usize) -> Vec<String> {
        (0..count).map(|_| name.to_string()).collect()
    }

    /// Build a 60-card main deck with 4x `name` plus 56x Plains, respecting the
    /// 4-per-name rule (CR 100.2a) while keeping the target card in the deck.
    fn legal_60_main(name: &str) -> Vec<String> {
        let mut deck = expand(name, 4);
        deck.extend(expand("Plains", 56));
        deck
    }

    fn planechase_card_json(name: &str, supertypes: &[&str], core_types: &[&str]) -> Value {
        serde_json::json!({
            "name": name,
            "mana_cost": { "type": "NoCost" },
            "card_type": {
                "supertypes": supertypes,
                "core_types": core_types,
                "subtypes": []
            },
            "power": null,
            "toughness": null,
            "loyalty": null,
            "defense": null,
            "oracle_text": null,
            "non_ability_text": null,
            "flavor_name": null,
            "keywords": [],
            "abilities": [],
            "triggers": [],
            "static_abilities": [],
            "replacements": [],
            "color_override": null,
            "scryfall_oracle_id": null,
            "legalities": {
                "standard": "legal",
                "commander": "legal"
            }
        })
    }

    fn insert_planechase_card(
        cards: &mut Map<String, Value>,
        name: &str,
        supertypes: &[&str],
        core_types: &[&str],
    ) {
        cards.insert(
            name.to_lowercase(),
            planechase_card_json(name, supertypes, core_types),
        );
    }

    fn planechase_test_db() -> CardDatabase {
        let mut cards = Map::new();
        insert_planechase_card(&mut cards, "Legal Standard", &[], &[]);
        insert_planechase_card(&mut cards, "Plains", &["Basic"], &["Land"]);
        for index in 1..=40 {
            insert_planechase_card(&mut cards, &format!("Plane {index}"), &[], &["Plane"]);
        }
        for index in 1..=5 {
            insert_planechase_card(
                &mut cards,
                &format!("Phenomenon {index}"),
                &[],
                &["Phenomenon"],
            );
        }
        CardDatabase::from_json_str(&Value::Object(cards).to_string()).unwrap()
    }

    fn plane_names(count: usize) -> Vec<String> {
        (1..=count).map(|index| format!("Plane {index}")).collect()
    }

    fn phenomenon_names(count: usize) -> Vec<String> {
        (1..=count)
            .map(|index| format!("Phenomenon {index}"))
            .collect()
    }

    fn planechase_request(
        player_count: usize,
        planar_deck: Vec<String>,
    ) -> DeckCompatibilityRequest {
        DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck,
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Planechase)),
            selected_match_type: None,
            player_count,
            summary_only: false,
            draft_set_codes: Vec::new(),
        }
    }

    fn insert_scheme_card(cards: &mut Map<String, Value>, name: &str) {
        cards.insert(
            name.to_lowercase(),
            planechase_card_json(name, &[], &["Scheme"]),
        );
    }

    fn insert_unsupported_scheme_card(cards: &mut Map<String, Value>, name: &str) {
        let mut card = planechase_card_json(name, &[], &["Scheme"]);
        card["abilities"] = serde_json::to_value(vec![crate::types::AbilityDefinition::new(
            crate::types::AbilityKind::Spell,
            crate::types::Effect::unimplemented("scheme_test", "unsupported scheme test"),
        )])
        .unwrap();
        cards.insert(name.to_lowercase(), card);
    }

    fn archenemy_test_db() -> CardDatabase {
        let mut cards = Map::new();
        insert_planechase_card(&mut cards, "Legal Standard", &[], &[]);
        insert_planechase_card(&mut cards, "Plains", &["Basic"], &["Land"]);
        for index in 1..=20 {
            insert_scheme_card(&mut cards, &format!("Scheme {index}"));
        }
        insert_unsupported_scheme_card(&mut cards, "Unsupported Scheme");
        CardDatabase::from_json_str(&Value::Object(cards).to_string()).unwrap()
    }

    fn scheme_names(count: usize) -> Vec<String> {
        (1..=count).map(|index| format!("Scheme {index}")).collect()
    }

    fn archenemy_request(scheme_deck: Vec<String>) -> DeckCompatibilityRequest {
        DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck,
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Archenemy)),
            selected_match_type: None,
            player_count: 4,
            summary_only: false,
            draft_set_codes: Vec::new(),
        }
    }

    #[test]
    fn archenemy_accepts_valid_twenty_card_scheme_deck() {
        let db = archenemy_test_db();
        let request = archenemy_request(scheme_names(20));
        let check = evaluate_archenemy(&db, &request, &BTreeSet::new(), &FormatConfig::archenemy());

        assert!(check.compatible, "reasons: {:?}", check.reasons);
    }

    #[test]
    fn archenemy_rejects_short_scheme_deck() {
        let db = archenemy_test_db();
        let request = archenemy_request(scheme_names(19));
        let check = evaluate_archenemy(&db, &request, &BTreeSet::new(), &FormatConfig::archenemy());

        assert!(!check.compatible);
        assert!(
            check
                .reasons
                .iter()
                .any(|reason| reason.contains("minimum 20")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn archenemy_rejects_third_scheme_copy() {
        let db = archenemy_test_db();
        let mut scheme_deck = scheme_names(18);
        scheme_deck.extend(["Scheme 1".to_string(), "Scheme 1".to_string()]);
        let request = archenemy_request(scheme_deck);
        let check = evaluate_archenemy(&db, &request, &BTreeSet::new(), &FormatConfig::archenemy());

        assert!(!check.compatible);
        assert!(
            check
                .reasons
                .iter()
                .any(|reason| reason.contains("copy-limit")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn archenemy_rejects_non_scheme_in_scheme_deck() {
        let db = archenemy_test_db();
        let mut scheme_deck = scheme_names(19);
        scheme_deck.push("Legal Standard".to_string());
        let request = archenemy_request(scheme_deck);
        let check = evaluate_archenemy(&db, &request, &BTreeSet::new(), &FormatConfig::archenemy());

        assert!(!check.compatible);
        assert!(
            check
                .reasons
                .iter()
                .any(|reason| reason.contains("must be Scheme")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn archenemy_rejects_unsupported_scheme() {
        let db = archenemy_test_db();
        let mut scheme_deck = scheme_names(19);
        scheme_deck.push("Unsupported Scheme".to_string());
        let request = archenemy_request(scheme_deck);
        let check = evaluate_archenemy(&db, &request, &BTreeSet::new(), &FormatConfig::archenemy());

        assert!(!check.compatible);
        assert!(
            check
                .reasons
                .iter()
                .any(|reason| reason.contains("Unsupported scheme cards")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn planechase_planar_minimum_scales_with_actual_player_count() {
        let db = planechase_test_db();

        for (player_count, minimum) in [(2, 20), (3, 30), (4, 40)] {
            let short = planechase_request(player_count, plane_names(minimum - 1));
            let check =
                evaluate_planechase(&db, &short, &BTreeSet::new(), &FormatConfig::planechase());
            assert!(
                !check.compatible,
                "{player_count}-player Planechase must reject a {minimum_minus_one}-card planar deck",
                minimum_minus_one = minimum - 1
            );
            assert!(
                check
                    .reasons
                    .iter()
                    .any(|reason| reason.contains(&format!("minimum {minimum}"))),
                "reasons: {:?}",
                check.reasons
            );

            let exact = planechase_request(player_count, plane_names(minimum));
            let check =
                evaluate_planechase(&db, &exact, &BTreeSet::new(), &FormatConfig::planechase());
            assert!(
                check.compatible,
                "{player_count}-player Planechase must accept exactly {minimum} planar cards, reasons: {:?}",
                check.reasons
            );
        }
    }

    #[test]
    fn planechase_phenomenon_cap_is_player_count_scaled() {
        let db = planechase_test_db();
        let mut planar_deck = plane_names(15);
        planar_deck.extend(phenomenon_names(5));

        let check = evaluate_planechase(
            &db,
            &planechase_request(2, planar_deck),
            &BTreeSet::new(),
            &FormatConfig::planechase(),
        );

        assert!(!check.compatible, "five phenomena exceeds the 2-player cap");
        assert!(
            check
                .reasons
                .iter()
                .any(|reason| reason.contains("5 phenomena (maximum 4)")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn planechase_planar_deck_is_singleton_by_english_name() {
        let db = planechase_test_db();
        let mut planar_deck = plane_names(19);
        planar_deck.push("Plane 1".to_string());

        let check = evaluate_planechase(
            &db,
            &planechase_request(2, planar_deck),
            &BTreeSet::new(),
            &FormatConfig::planechase(),
        );

        assert!(
            !check.compatible,
            "duplicate English names must be rejected"
        );
        assert!(
            check.reasons.iter().any(|reason| {
                reason.contains("Planar deck singleton violations") && reason.contains("Plane 1")
            }),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn planechase_planar_deck_rejects_non_planar_cards() {
        let db = planechase_test_db();
        let mut planar_deck = plane_names(19);
        planar_deck.push("Legal Standard".to_string());

        let check = evaluate_planechase(
            &db,
            &planechase_request(2, planar_deck),
            &BTreeSet::new(),
            &FormatConfig::planechase(),
        );

        assert!(
            !check.compatible,
            "main-deck cards cannot appear in the planar deck"
        );
        assert!(
            check.reasons.iter().any(|reason| {
                reason.contains("Planar deck cards must be Plane or Phenomenon")
                    && reason.contains("Legal Standard")
            }),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn planechase_empty_custom_planar_deck_is_allowed() {
        let db = planechase_test_db();
        let check = evaluate_planechase(
            &db,
            &planechase_request(2, Vec::new()),
            &BTreeSet::new(),
            &FormatConfig::planechase(),
        );

        assert!(
            check.compatible,
            "empty custom planar deck should pass validation so loading can use the default deck, reasons: {:?}",
            check.reasons
        );
    }

    fn tiny_leaders_test_db_json() -> String {
        serde_json::json!({
            "white tiny leader": {
                "name": "White Tiny Leader",
                "mana_cost": { "type": "Cost", "shards": [], "generic": 2 },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "color_identity": ["White"],
                "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "ajani, nacatl pariah": {
                "name": "Ajani, Nacatl Pariah",
                "mana_cost": { "type": "Cost", "shards": [], "generic": 2 },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Planeswalker"], "subtypes": ["Ajani"] },
                "power": null,
                "toughness": null,
                "loyalty": "3",
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "color_identity": ["White"],
                "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic"], "core_types": ["Land"], "subtypes": ["Plains"] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "big spell": {
                "name": "Big Spell",
                "mana_cost": { "type": "Cost", "shards": [], "generic": 4 },
                "card_type": { "supertypes": [], "core_types": ["Sorcery"], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "sol ring": {
                "name": "Sol Ring",
                "mana_cost": { "type": "Cost", "shards": [], "generic": 1 },
                "card_type": { "supertypes": [], "core_types": ["Artifact"], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "small spell": {
                "name": "Small Spell",
                "mana_cost": { "type": "Cost", "shards": [], "generic": 1 },
                "card_type": { "supertypes": [], "core_types": ["Sorcery"], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            }
        })
        .to_string()
    }

    #[test]
    fn standard_legal_deck_passes() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.standard.compatible,
            "expected legal deck to pass, reasons: {:?}",
            result.standard.reasons
        );
    }

    #[test]
    fn standard_illegal_deck_reports_reasons() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut deck = expand("Legal Standard", 59);
        deck.push("Not Standard".to_string());
        let request = DeckCompatibilityRequest {
            main_deck: deck,
            sideboard: Vec::new(),
            commander: vec!["Legal Standard".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(!result.standard.compatible);
        assert!(result
            .standard
            .reasons
            .iter()
            .any(|r| r.contains("Standard decks do not use a commander slot")));
        assert!(result
            .standard
            .reasons
            .iter()
            .any(|r| r.contains("Not Standard")));
    }

    // CR 100.2a / CR 903.5b: per-card copy-limit overrides drive
    // `copy_limit_violations`. Faces loaded via `from_json_str` skip synthesis,
    // so the limit is resolved through the live Oracle-text fallback in
    // `deck_copy_limit_for`. Helpers below build the canonical count map the way
    // the production callers do.
    fn counts_of(pairs: &[(&str, u32)]) -> HashMap<String, u32> {
        pairs
            .iter()
            .map(|(name, n)| (name.to_ascii_lowercase(), *n))
            .collect()
    }

    #[test]
    fn copy_limit_respects_typed_overrides_constructed() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();

        // Seven Dwarves: UpTo(7) — 7 legal, 8 illegal.
        assert!(copy_limit_violations(
            &db,
            &counts_of(&[("Seven Dwarves", 7)]),
            DeckCopyLimit::UpTo(4)
        )
        .is_empty());
        assert!(!copy_limit_violations(
            &db,
            &counts_of(&[("Seven Dwarves", 8)]),
            DeckCopyLimit::UpTo(4)
        )
        .is_empty());

        // Nazgûl: UpTo(9) — 8 legal, 10 illegal.
        assert!(
            copy_limit_violations(&db, &counts_of(&[("Nazgûl", 8)]), DeckCopyLimit::UpTo(4))
                .is_empty()
        );
        assert!(
            !copy_limit_violations(&db, &counts_of(&[("Nazgûl", 10)]), DeckCopyLimit::UpTo(4))
                .is_empty()
        );

        // Relentless Rats: Unlimited — 5 legal.
        assert!(copy_limit_violations(
            &db,
            &counts_of(&[("Relentless Rats", 5)]),
            DeckCopyLimit::UpTo(4)
        )
        .is_empty());

        // Mountain: basic-land exemption — 30 legal.
        assert!(copy_limit_violations(
            &db,
            &counts_of(&[("Mountain", 30)]),
            DeckCopyLimit::UpTo(4)
        )
        .is_empty());

        // A normal card with no override is still flagged at 5.
        let violations =
            copy_limit_violations(&db, &counts_of(&[("Red Card", 5)]), DeckCopyLimit::UpTo(4));
        assert!(violations.iter().any(|v| v.contains("Red Card")));
    }

    #[test]
    fn copy_limit_override_fires_before_commander_singleton() {
        // CR 903.5b: in a singleton (Commander) context max_copies = 1, but
        // Nazgûl's UpTo(9) override must raise the cap so 9 copies are legal.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        assert!(
            copy_limit_violations(&db, &counts_of(&[("Nazgûl", 9)]), DeckCopyLimit::UpTo(1))
                .is_empty()
        );
        // A normal card is still singleton-restricted to 1.
        assert!(!copy_limit_violations(
            &db,
            &counts_of(&[("Red Card", 2)]),
            DeckCopyLimit::UpTo(1)
        )
        .is_empty());
    }

    /// CR 100.2a / CR 100.2b / CR 903.5b: `max_deck_copies` is the query-shaped
    /// authority the deck builder gates its increment control on. It must layer
    /// the basic-land exemption, the printed override, and the format default
    /// in that precedence order — same rule `copy_limit_violations` enforces.
    #[test]
    fn max_deck_copies_resolves_format_default_and_overrides() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();

        // Format default: CR 100.2a four-of in constructed, CR 903.5b singleton
        // in the Commander family.
        assert_eq!(
            max_deck_copies(&db, "Red Card", &FormatConfig::modern()),
            DeckCopyLimit::UpTo(4)
        );
        assert_eq!(
            max_deck_copies(&db, "Red Card", &FormatConfig::commander()),
            DeckCopyLimit::UpTo(1)
        );
        // CR 100.2b: limited decks may contain as many duplicates as the
        // product provides, so no ceiling applies.
        assert_eq!(
            max_deck_copies(&db, "Red Card", &FormatConfig::limited()),
            DeckCopyLimit::Unlimited
        );

        // Printed overrides replace the format default in both directions.
        assert_eq!(
            max_deck_copies(&db, "Seven Dwarves", &FormatConfig::modern()),
            DeckCopyLimit::UpTo(7)
        );
        assert_eq!(
            max_deck_copies(&db, "Nazgûl", &FormatConfig::commander()),
            DeckCopyLimit::UpTo(9)
        );
        assert_eq!(
            max_deck_copies(&db, "Relentless Rats", &FormatConfig::modern()),
            DeckCopyLimit::Unlimited
        );

        // CR 205.4c: basic lands are exempt regardless of format.
        assert_eq!(
            max_deck_copies(&db, "Mountain", &FormatConfig::commander()),
            DeckCopyLimit::Unlimited
        );

        // Names are canonicalized before lookup, so spelling variants that
        // resolve to the same card share one ceiling.
        assert_eq!(
            max_deck_copies(&db, "Nazgul", &FormatConfig::modern()),
            DeckCopyLimit::UpTo(9)
        );
    }

    /// CR 100.6: Tournament rules may limit a card's use; Phase's restricted
    /// list uses its established one-copy format-policy, so the query the deck
    /// builder gates its increment control on has to honour it — otherwise the
    /// `+` stays live through four Black Lotuses and the deck only fails later,
    /// at validation. The restriction is format-scoped: the same card is a
    /// plain four-of wherever its legality table doesn't restrict it.
    #[test]
    fn max_deck_copies_honours_the_format_restricted_list() {
        let db = CardDatabase::from_json_str(&vintage_test_db()).unwrap();

        // Reach guard: the ceiling below is only meaningful if the fixture
        // really does mark this card Restricted in Vintage.
        assert_eq!(
            db.legality_status(
                "Black Lotus",
                GameFormat::Vintage.legality_format().unwrap()
            ),
            Some(LegalityStatus::Restricted),
            "fixture must present Black Lotus as Vintage-restricted"
        );

        assert_eq!(
            max_deck_copies(&db, "Black Lotus", &FormatConfig::vintage()),
            DeckCopyLimit::UpTo(1),
            "a Vintage-restricted card is capped at one copy, not the four-of default"
        );

        // Format-scoped: Legacy's legality table says nothing about this card,
        // so it falls through to the CR 100.2a default rather than inheriting
        // Vintage's restriction.
        assert_eq!(
            max_deck_copies(&db, "Black Lotus", &FormatConfig::legacy()),
            DeckCopyLimit::UpTo(4)
        );

        // CR 205.4c still wins for basics — a restricted lookup must not
        // shadow the exemption for cards the list doesn't name.
        assert_eq!(
            max_deck_copies(&db, "Island", &FormatConfig::vintage()),
            DeckCopyLimit::Unlimited
        );
    }

    #[test]
    fn combined_copy_counts_merge_nazgul_spelling_variants() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Nazgul", 5);
        main.extend(expand("Nazgûl", 5));
        main.extend(expand("Plains", 80));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let counts = combined_copy_counts(&db, &request, CommandZoneNetting::NetAgainstMainDeck);
        assert_eq!(counts.get("nazgûl"), Some(&10));
        assert!(!copy_limit_violations(&db, &counts, DeckCopyLimit::UpTo(1)).is_empty());
    }

    #[test]
    fn commander_accepts_nine_nazgul_copies() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Nazgul", 9);
        main.extend(expand("Mountain", 90));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Grub Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "expected compatible commander deck, got: {:?}",
            result.commander.reasons
        );
    }

    #[test]
    fn summary_commander_accepts_nine_nazgul_copies() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Nazgul", 9);
        main.extend(expand("Mountain", 90));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Grub Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: true,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn color_distribution_is_main_deck_only_and_matches_summary() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: vec![
                "Grub Commander".to_string(),
                "Grub Commander".to_string(),
                "Red Card".to_string(),
                "Legal Standard".to_string(),
                "Unknown Card".to_string(),
            ],
            sideboard: vec!["Grub Commander".to_string()],
            commander: vec!["Grub Commander".to_string()],
            companion: vec!["Grub Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: vec!["Grub Commander".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Standard)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let full = evaluate_deck_compatibility(&db, &request);
        assert_eq!(
            full.color_distribution,
            vec![
                DeckColorDistributionEntry {
                    color: ManaColor::Black,
                    count: 2,
                    percentage: 40.0,
                    display_percentage: 40,
                },
                DeckColorDistributionEntry {
                    color: ManaColor::Red,
                    count: 3,
                    percentage: 60.0,
                    display_percentage: 60,
                },
            ]
        );
        assert_eq!(full.unknown_cards, vec!["Unknown Card"]);

        let summary = evaluate_deck_compatibility(
            &db,
            &DeckCompatibilityRequest {
                summary_only: true,
                ..request
            },
        );
        assert_eq!(summary.color_distribution, full.color_distribution);

        let mut old_payload = serde_json::to_value(full).unwrap();
        old_payload
            .as_object_mut()
            .unwrap()
            .remove("color_distribution");
        let decoded: DeckCompatibilityResult = serde_json::from_value(old_payload).unwrap();
        assert!(decoded.color_distribution.is_empty());
    }

    #[test]
    fn dedicated_companion_rejection_matches_full_and_summary_validation() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 99),
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: vec!["Legal Standard".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let full = evaluate_deck_compatibility(&db, &request);
        assert_eq!(full.selected_format_compatible, Some(false));
        assert!(full
            .selected_format_reasons
            .iter()
            .any(|reason| reason.contains("not a legal companion")));

        let summary = evaluate_deck_compatibility(
            &db,
            &DeckCompatibilityRequest {
                summary_only: true,
                ..request
            },
        );
        assert_eq!(summary.selected_format_compatible, Some(false));
        assert!(summary
            .selected_format_reasons
            .iter()
            .any(|reason| reason.contains("not a legal companion")));
    }

    #[test]
    fn banned_dedicated_companion_matches_full_and_summary_validation() {
        let mut db_json: Value = serde_json::from_str(&test_db_json()).unwrap();
        db_json["commander banned"]["keywords"] = serde_json::json!([
            { "Companion": { "type": "Singleton" } }
        ]);
        let db = CardDatabase::from_json_str(&db_json.to_string()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 99),
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: vec!["Commander Banned".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let full = evaluate_deck_compatibility(&db, &request);
        assert_eq!(full.selected_format_compatible, Some(false));
        assert!(full
            .selected_format_reasons
            .iter()
            .any(|reason| reason.contains("Commander Banned (banned)")));

        let summary = evaluate_deck_compatibility(
            &db,
            &DeckCompatibilityRequest {
                summary_only: true,
                ..request
            },
        );
        assert_eq!(summary.selected_format_compatible, Some(false));
        assert!(summary
            .selected_format_reasons
            .iter()
            .any(|reason| reason.contains("Commander Banned (banned)")));
    }

    #[test]
    fn commander_rules_detect_size_singleton_and_legality_failures() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Legal Standard", 97);
        main.push("Commander Banned".to_string());
        main.push("Commander Banned".to_string());
        let request = DeckCompatibilityRequest {
            main_deck: main,
            // CR 903.5e: a Commander deck's sideboard slot is Phase's
            // builder-only Maybeboard — extra entries are accepted by the
            // validator and dropped at game load. They must not contribute
            // to the singleton count below.
            sideboard: vec!["Legal Standard".to_string()],
            commander: vec!["Legal Standard".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(!result.commander.compatible);
        assert!(result
            .commander
            .reasons
            .iter()
            .any(|r| r.contains("Singleton violations")));
        assert!(result
            .commander
            .reasons
            .iter()
            .any(|r| r.contains("Commander Banned")));
    }

    #[test]
    fn bo3_ready_depends_on_sideboard() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let no_sideboard = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 60),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Standard)),
            selected_match_type: Some(MatchType::Bo3),
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let with_sideboard = DeckCompatibilityRequest {
            sideboard: vec!["Legal Standard".to_string()],
            ..no_sideboard.clone()
        };

        let no_sb_result = evaluate_deck_compatibility(&db, &no_sideboard);
        assert!(!no_sb_result.bo3_ready);
        assert_eq!(no_sb_result.selected_format_compatible, Some(false));
        assert!(no_sb_result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("BO3 requires a sideboard")));

        let with_sb_result = evaluate_deck_compatibility(&db, &with_sideboard);
        assert!(with_sb_result.bo3_ready);
    }

    #[test]
    fn unknown_cards_are_reported() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: vec!["Mystery Card".to_string()],
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.unknown_cards, vec!["Mystery Card".to_string()]);
        assert!(!result.standard.compatible);
        assert!(!result.commander.compatible);
        assert!(result
            .standard
            .reasons
            .iter()
            .any(|reason| reason.contains("Unknown cards")));
    }

    #[test]
    fn commander_requires_eligible_commander_cards() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 99),
            sideboard: Vec::new(),
            commander: vec!["Legal Standard".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(!result.commander.compatible);
        assert!(result
            .commander
            .reasons
            .iter()
            .any(|reason| reason.contains("must be legendary creatures")));
    }

    #[test]
    fn pauper_commander_allows_nonlegendary_creature_commander_slot() {
        let db_json = serde_json::json!({
            "pdh commander": {
                "name": "PDH Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "rarities": ["uncommon"],
                "legalities": {}
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"],
                    "core_types": ["Land"],
                    "subtypes": ["Plains"]
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": { "paupercommander": "legal" }
            }
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 99),
            sideboard: Vec::new(),
            commander: vec!["PDH Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::PauperCommander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(true));
        assert!(
            result.selected_format_reasons.is_empty(),
            "{:?}",
            result.selected_format_reasons
        );
    }

    #[test]
    fn pauper_commander_rejects_rare_only_creature() {
        let db_json = serde_json::json!({
            "rare creature": {
                "name": "Rare Creature",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "rarities": ["rare"],
                "legalities": {}
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"],
                    "core_types": ["Land"],
                    "subtypes": ["Plains"]
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": { "paupercommander": "legal" }
            }
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 99),
            sideboard: Vec::new(),
            commander: vec!["Rare Creature".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::PauperCommander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("uncommon creature")));
    }

    #[test]
    fn pauper_commander_rejects_uncommon_noncreature() {
        let db_json = serde_json::json!({
            "uncommon sorcery": {
                "name": "Uncommon Sorcery",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Sorcery"], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "rarities": ["uncommon"],
                "legalities": { "paupercommander": "legal" }
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"],
                    "core_types": ["Land"],
                    "subtypes": ["Plains"]
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": { "paupercommander": "legal" }
            }
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 99),
            sideboard: Vec::new(),
            commander: vec!["Uncommon Sorcery".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::PauperCommander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("uncommon creature")));
    }

    #[test]
    fn commander_partners_require_partner_keyword() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 98),
            sideboard: Vec::new(),
            commander: vec![
                "Partner Commander".to_string(),
                "Legal Commander".to_string(),
            ],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(!result.commander.compatible);
        assert!(result
            .commander
            .reasons
            .iter()
            .any(|reason| reason.contains("Invalid partner pairing")));
    }

    #[test]
    fn selected_format_defaults_to_true_for_ffa_and_two_headed_giant() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: Vec::new(),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeForAll)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let thg_request = DeckCompatibilityRequest {
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::TwoHeadedGiant)),
            ..request.clone()
        };

        assert_eq!(
            evaluate_deck_compatibility(&db, &request).selected_format_compatible,
            Some(true)
        );
        assert_eq!(
            evaluate_deck_compatibility(&db, &thg_request).selected_format_compatible,
            Some(true)
        );
    }

    #[test]
    fn selected_standard_and_commander_use_corresponding_checks() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let standard_request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Standard)),
            selected_match_type: Some(MatchType::Bo1),
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let commander_request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 99),
            sideboard: Vec::new(),
            commander: vec!["Legal Standard".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: Some(MatchType::Bo1),
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let standard_result = evaluate_deck_compatibility(&db, &standard_request);
        let commander_result = evaluate_deck_compatibility(&db, &commander_request);

        assert!(standard_result.standard.compatible);
        assert_eq!(standard_result.selected_format_compatible, Some(true));
        assert_eq!(
            commander_result.selected_format_compatible,
            Some(commander_result.commander.compatible)
        );
    }

    #[test]
    fn summarize_cards_limits_output() {
        let cards = (0..10)
            .map(|i| format!("Card {i}"))
            .collect::<BTreeSet<String>>();
        let text = summarize_cards("Example", &cards, 3);
        assert!(text.contains("+7 more"));
    }

    #[test]
    fn pioneer_selected_format_validates_legality() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // Legal deck: all cards are pioneer-legal
        let legal_request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Pioneer Only"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Pioneer)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &legal_request);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn premodern_selected_format_validates_legality() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Premodern)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn premodern_selected_format_rejects_banned_cards() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Premodern Banned"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Premodern)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Not Premodern legal") && r.contains("banned")));
    }

    #[test]
    fn premodern_selected_format_rejects_missing_legality() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Pioneer Only"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Premodern)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Pioneer Only") && r.contains("not legal in Premodern")));
    }

    #[test]
    fn premodern_selected_format_enforces_constructed_deck_shape() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();

        let commander_request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: vec!["Legal Standard".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Premodern)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &commander_request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Premodern decks do not use a commander slot")));

        let oversize_sideboard = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: expand("Plains", 16),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Premodern)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &oversize_sideboard);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Sideboard has 16") && r.contains("maximum 15")));

        let mut main = expand("Legal Standard", 5);
        main.extend(expand("Plains", 55));
        let copy_limit = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Premodern)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &copy_limit);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("More than 4 copies")));
    }

    #[test]
    fn validate_name_deck_for_format_rejects_non_premodern_cards() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let main_deck = legal_60_main("Pioneer Only");
        let result =
            validate_name_deck_for_format(&db, &main_deck, &[], &[], GameFormat::Premodern, None);

        let reasons = result.expect_err("Premodern validation must reject missing legality");
        assert!(reasons.iter().any(|r| r.contains("Not Premodern legal")));
    }

    #[test]
    fn validate_name_deck_for_format_full_reads_resolved_format_config() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();

        let standard_deck = legal_60_main("Legal Standard");
        assert!(validate_name_deck_for_format_full(
            &db,
            &standard_deck,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::standard(),
            None,
            default_player_count(),
        )
        .is_ok());

        let non_premodern_deck = legal_60_main("Pioneer Only");
        let reasons = validate_name_deck_for_format_full(
            &db,
            &non_premodern_deck,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::premodern(),
            None,
            default_player_count(),
        )
        .expect_err("Premodern validation must reject missing legality");
        assert!(reasons.iter().any(|r| r.contains("Not Premodern legal")));
    }

    #[test]
    fn validate_name_deck_for_format_rejects_custom_with_the_shared_message() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let result = validate_name_deck_for_format(
            &db,
            &[],
            &[],
            &[],
            GameFormat::Custom(CustomFormatId(1)),
            None,
        );
        assert_eq!(result, Err(vec![CUSTOM_FORMAT_UNRESOLVED.to_string()]));
    }

    /// A deck that is genuinely, fully legal in Standard — 4 copies of a
    /// Standard-legal card plus 56 Plains. Used by the gate tests below so the
    /// Custom rejection is demonstrably about the FORMAT and not about the deck
    /// being empty or otherwise degenerate: the very same deck passes as
    /// Standard and fails as Custom.
    fn fully_legal_standard_request(format: GameFormat) -> DeckCompatibilityRequest {
        DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(format)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        }
    }

    /// The AUTHORITATIVE game-creation gate keeps rejecting an unresolvable
    /// bare Custom tag on its own, independently of `evaluate_selected_format`'s
    /// answer. This is the guard that makes `evaluate_deck_compatibility`'s
    /// "no opinion" UI downgrade safe: revert the `rules().is_err()` guard at
    /// the top of `validate_deck_for_format` and this assertion flips to
    /// `Ok(())`, because the downgrade path and this path would then share one
    /// verdict.
    #[test]
    fn validate_deck_for_format_rejects_custom_independently_of_the_ui_hint() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = fully_legal_standard_request(GameFormat::Custom(CustomFormatId(1)));
        assert_eq!(
            validate_deck_for_format(&db, &request),
            Err(vec![CUSTOM_FORMAT_UNRESOLVED.to_string()]),
            "the authoritative gate must fail closed on an unresolvable Custom tag even for a \
             deck that is fully legal in a real format"
        );
    }

    #[test]
    fn evaluate_deck_format_gate_passes_a_legal_built_in_format_deck() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let result =
            evaluate_deck_format_gate(&db, &fully_legal_standard_request(GameFormat::Standard));
        assert!(
            result.compatible,
            "expected a Standard-legal deck to pass the gate, reasons: {:?}",
            result.reasons
        );
        assert!(result.reasons.is_empty());
    }

    /// The regression the dedicated gate exists for: the P2P host's guest-kick
    /// check must keep getting a definite `false` for Custom, even though the
    /// shared UI-hint function now answers "no opinion" for the same request.
    /// Asserted against the SAME fully-legal deck the test above passes, so a
    /// pass here could only come from the format being ignored.
    #[test]
    fn evaluate_deck_format_gate_rejects_custom_even_for_an_otherwise_legal_deck() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = fully_legal_standard_request(GameFormat::Custom(CustomFormatId(1)));

        let gate = evaluate_deck_format_gate(&db, &request);
        assert!(
            !gate.compatible,
            "the guest-kick gate must never silently pass a Custom-format deck"
        );
        assert_eq!(gate.reasons, vec![CUSTOM_FORMAT_UNRESOLVED.to_string()]);

        // The whole point of the split: the UI-hint function, on this exact
        // request, deliberately answers "no opinion" instead. If these two ever
        // agree again, one of them is wrong.
        let hint = evaluate_deck_compatibility(&db, &request);
        assert_eq!(hint.selected_format_compatible, None);
        assert!(hint.selected_format_reasons.is_empty());
    }

    #[test]
    fn pauper_selected_format_rejects_illegal_cards() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // Pioneer Only card is not pauper-legal
        let illegal_request = DeckCompatibilityRequest {
            main_deck: expand("Pioneer Only", 60),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Pauper)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &illegal_request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Not Pauper legal")));
    }

    #[test]
    fn evaluate_constructed_checks_deck_size_and_commander_slot() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let unknown_cards = BTreeSet::new();
        // Too few cards + has commander slot
        let request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 30),
            sideboard: Vec::new(),
            commander: vec!["Legal Standard".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let check = evaluate_constructed(
            &db,
            &request,
            &unknown_cards,
            &FormatConfig::pioneer(),
            CardPoolAuthority::LegalityTable(LegalityFormat::Pioneer),
            "Pioneer",
        );
        assert!(!check.compatible);
        assert!(check.reasons.iter().any(|r| r.contains("at least 60")));
        assert!(check
            .reasons
            .iter()
            .any(|r| r.contains("Pioneer decks do not use a commander slot")));
    }

    #[test]
    fn brawl_valid_deck_passes() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 59),
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Brawl)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn brawl_planeswalker_commander_is_valid() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 59),
            sideboard: Vec::new(),
            commander: vec!["Legendary Planeswalker".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Brawl)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn brawl_rejects_non_legendary_commander() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 59),
            sideboard: Vec::new(),
            commander: vec!["Legal Standard".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Brawl)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("legendary creature or legendary planeswalker")));
    }

    #[test]
    fn brawl_rejects_partner() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 58),
            sideboard: Vec::new(),
            commander: vec![
                "Legal Commander".to_string(),
                "Partner Commander".to_string(),
            ],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Brawl)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("exactly 1 commander")));
    }

    #[test]
    fn brawl_rejects_wrong_deck_size() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 99),
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Brawl)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("exactly 60 cards")));
    }

    #[test]
    fn historic_brawl_rejects_sixty_card_deck() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 59),
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::HistoricBrawl)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("exactly 100 cards")));
    }

    #[test]
    fn tiny_leaders_valid_deck_passes() {
        let db = CardDatabase::from_json_str(&tiny_leaders_test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 49),
            sideboard: expand("Plains", 10),
            commander: vec!["White Tiny Leader".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::TinyLeaders)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "{:?}",
            result.selected_format_reasons
        );
    }

    #[test]
    fn tiny_leaders_allows_legendary_planeswalker_leaders() {
        let db = CardDatabase::from_json_str(&tiny_leaders_test_db_json()).unwrap();
        let face = db
            .get_face_by_name("Ajani, Nacatl Pariah")
            .expect("fixture planeswalker exists");

        assert!(is_tiny_leader_eligible(face));
    }

    #[test]
    fn tiny_leaders_rejects_cost_identity_and_deck_ban() {
        let db = CardDatabase::from_json_str(&tiny_leaders_test_db_json()).unwrap();
        let mut main = expand("Plains", 47);
        main.push("Big Spell".to_string());
        main.push("Sol Ring".to_string());
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["White Tiny Leader".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::TinyLeaders)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Tiny cost identity") && r.contains("Big Spell")));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Banned in Tiny Leaders") && r.contains("Sol Ring")));
    }

    #[test]
    fn tiny_leaders_rejects_commander_only_ban() {
        let db = CardDatabase::from_json_str(&tiny_leaders_test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 49),
            sideboard: Vec::new(),
            commander: vec!["Ajani, Nacatl Pariah".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::TinyLeaders)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Banned as Tiny Leader") && r.contains("Ajani")));
    }

    #[test]
    fn historic_brawl_uses_brawl_legality() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // "Not Standard" has brawl: legal but standardbrawl: not_legal
        // Use basic lands to avoid singleton violations, plus one non-basic to
        // test legality. Historic Brawl is a 100-card format (99 + commander).
        let mut main = expand("Plains", 98);
        main.push("Not Standard".to_string());
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::HistoricBrawl)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(true));

        // Same deck should fail Standard Brawl
        let brawl_request = DeckCompatibilityRequest {
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Brawl)),
            ..request
        };
        let brawl_result = evaluate_deck_compatibility(&db, &brawl_request);
        assert_eq!(brawl_result.selected_format_compatible, Some(false));
        assert!(brawl_result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Not Brawl legal")));
    }

    // --- Partner family validation tests ---

    /// Build a minimal CardFace with specific partner keywords for unit testing.
    fn partner_face(name: &str, keywords: Vec<Keyword>, subtypes: Vec<&str>) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: crate::types::card_type::CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: subtypes.into_iter().map(String::from).collect(),
            },
            keywords,
            ..CardFace::default()
        }
    }

    #[test]
    fn partner_generic_pair_is_valid() {
        use crate::types::keywords::PartnerType;
        let a = partner_face("A", vec![Keyword::Partner(PartnerType::Generic)], vec![]);
        let b = partner_face("B", vec![Keyword::Partner(PartnerType::Generic)], vec![]);
        assert!(are_valid_partners(&a, &b, None));
    }

    #[test]
    fn partner_with_matched_pair_is_valid() {
        use crate::types::keywords::PartnerType;
        let a = partner_face(
            "Brallin, Skyshark Rider",
            vec![Keyword::Partner(PartnerType::With(
                "Shabraz, the Skyshark".to_string(),
            ))],
            vec![],
        );
        let b = partner_face(
            "Shabraz, the Skyshark",
            vec![Keyword::Partner(PartnerType::With(
                "Brallin, Skyshark Rider".to_string(),
            ))],
            vec![],
        );
        assert!(are_valid_partners(&a, &b, None));
    }

    #[test]
    fn partner_with_mismatched_names_rejected() {
        use crate::types::keywords::PartnerType;
        let a = partner_face(
            "A",
            vec![Keyword::Partner(PartnerType::With("C".to_string()))],
            vec![],
        );
        let b = partner_face(
            "B",
            vec![Keyword::Partner(PartnerType::With("D".to_string()))],
            vec![],
        );
        assert!(!are_valid_partners(&a, &b, None));
    }

    #[test]
    fn friends_forever_pair_is_valid() {
        use crate::types::keywords::PartnerType;
        let a = partner_face(
            "A",
            vec![Keyword::Partner(PartnerType::FriendsForever)],
            vec![],
        );
        let b = partner_face(
            "B",
            vec![Keyword::Partner(PartnerType::FriendsForever)],
            vec![],
        );
        assert!(are_valid_partners(&a, &b, None));
    }

    #[test]
    fn character_select_pair_is_valid() {
        use crate::types::keywords::PartnerType;
        let a = partner_face(
            "A",
            vec![Keyword::Partner(PartnerType::CharacterSelect)],
            vec![],
        );
        let b = partner_face(
            "B",
            vec![Keyword::Partner(PartnerType::CharacterSelect)],
            vec![],
        );
        assert!(are_valid_partners(&a, &b, None));
    }

    #[test]
    fn doctors_companion_pairs_with_doctor_subtype() {
        use crate::types::keywords::PartnerType;
        let companion = partner_face(
            "Amy Pond",
            vec![Keyword::Partner(PartnerType::DoctorsCompanion)],
            vec![],
        );
        let doctor = partner_face("The Thirteenth Doctor", vec![], vec!["Doctor", "Time Lord"]);
        assert!(are_valid_partners(&companion, &doctor, None));
        // Reversed order also works
        assert!(are_valid_partners(&doctor, &companion, None));
    }

    #[test]
    fn choose_a_background_pairs_with_background_subtype() {
        use crate::types::keywords::PartnerType;
        let commander = partner_face(
            "Wilson, Refined Grizzly",
            vec![Keyword::Partner(PartnerType::ChooseABackground)],
            vec![],
        );
        // Background enchantment (not a creature)
        let mut bg = CardFace {
            name: "Criminal Past".to_string(),
            card_type: crate::types::card_type::CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Enchantment],
                subtypes: vec!["Background".to_string()],
            },
            ..CardFace::default()
        };
        assert!(are_valid_partners(&commander, &bg, None));
        // Background enchantment is commander-eligible
        assert!(is_commander_eligible(&bg));

        // Non-Background enchantment is not a valid partner
        bg.card_type.subtypes = vec!["Aura".to_string()];
        assert!(!are_valid_partners(&commander, &bg, None));
    }

    #[test]
    fn commander_eligibility_uses_parsed_permission_text() {
        let mut face = CardFace {
            name: "Teferi, Temporal Archmage".to_string(),
            oracle_text: Some("Teferi, Temporal Archmage can be your commander.".to_string()),
            ..CardFace::default()
        };
        face.card_type.supertypes.push(Supertype::Legendary);
        face.card_type.core_types.push(CoreType::Planeswalker);

        assert!(is_commander_eligible(&face));

        face.oracle_text = Some("Teferi, Temporal Archmage can't be your commander.".to_string());
        assert!(!is_commander_eligible(&face));
    }

    /// CR 903.3(c): A legendary Spacecraft with one or more power/toughness boxes
    /// is commander-eligible. Hearthhull, the Worldseed is the motivating case
    /// (Legendary Artifact — Spacecraft with printed P/T 6/7).
    #[test]
    fn commander_eligibility_accepts_legendary_spacecraft_with_pt() {
        use crate::types::ability::PtValue;

        let face = CardFace {
            name: "Hearthhull, the Worldseed".to_string(),
            card_type: crate::types::card_type::CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Spacecraft".to_string()],
            },
            power: Some(PtValue::Fixed(6)),
            toughness: Some(PtValue::Fixed(7)),
            ..CardFace::default()
        };
        assert!(is_commander_eligible(&face));

        // CR 903.3(c) explicitly requires a power/toughness box. A Spacecraft
        // without P/T is *not* eligible (no such card exists today; the guard is
        // load-bearing for forward compatibility).
        let face_no_pt = CardFace {
            power: None,
            toughness: None,
            ..face.clone()
        };
        assert!(!is_commander_eligible(&face_no_pt));
    }

    /// CR 903.3(b): A legendary Vehicle is commander-eligible regardless of
    /// whether it is currently a creature. Verifies the type-line path catches
    /// legendary Vehicles independent of MTGJSON's leadershipSkills bit.
    #[test]
    fn commander_eligibility_accepts_legendary_vehicle() {
        let face = CardFace {
            name: "Parnesse, the Subtle Brush".to_string(),
            card_type: crate::types::card_type::CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Vehicle".to_string()],
            },
            ..CardFace::default()
        };
        assert!(is_commander_eligible(&face));
    }

    /// Non-legendary Spacecraft / Vehicle must NOT be commander-eligible —
    /// the legendary supertype is required by every clause of CR 903.3.
    #[test]
    fn commander_eligibility_rejects_non_legendary_spacecraft_or_vehicle() {
        use crate::types::ability::PtValue;

        let spacecraft = CardFace {
            name: "Hypothetical Common Spacecraft".to_string(),
            card_type: crate::types::card_type::CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Spacecraft".to_string()],
            },
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            ..CardFace::default()
        };
        assert!(!is_commander_eligible(&spacecraft));

        let vehicle = CardFace {
            name: "Smuggler's Copter".to_string(),
            card_type: crate::types::card_type::CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Vehicle".to_string()],
            },
            ..CardFace::default()
        };
        assert!(!is_commander_eligible(&vehicle));
    }

    /// `face.is_commander` precomputed by synthesis (MTGJSON `leadershipSkills.commander`)
    /// must short-circuit the type-line analysis — catches cards MTGJSON has
    /// blessed that our type-line check might not yet recognize.
    #[test]
    fn commander_eligibility_honors_precomputed_field() {
        let face = CardFace {
            name: "Future Commander With No Type-Line Match".to_string(),
            is_commander: true,
            // No legendary supertype, no creature/vehicle/spacecraft, no permission text:
            // type-line analysis would reject this, but the synthesized field overrides.
            ..CardFace::default()
        };
        assert!(is_commander_eligible(&face));
    }

    #[test]
    fn cross_group_pairings_rejected() {
        use crate::types::keywords::PartnerType;
        // Generic + FriendsForever = invalid
        let a = partner_face("A", vec![Keyword::Partner(PartnerType::Generic)], vec![]);
        let b = partner_face(
            "B",
            vec![Keyword::Partner(PartnerType::FriendsForever)],
            vec![],
        );
        assert!(!are_valid_partners(&a, &b, None));

        // Generic + CharacterSelect = invalid
        let c = partner_face(
            "C",
            vec![Keyword::Partner(PartnerType::CharacterSelect)],
            vec![],
        );
        assert!(!are_valid_partners(&a, &c, None));

        // FriendsForever + CharacterSelect = invalid
        assert!(!are_valid_partners(&b, &c, None));
    }

    #[test]
    fn amy_pond_multi_keyword_pairing() {
        // Amy Pond has Doctor's Companion AND Partner with Rory Williams
        use crate::types::keywords::PartnerType;
        let amy = partner_face(
            "Amy Pond",
            vec![
                Keyword::Partner(PartnerType::DoctorsCompanion),
                Keyword::Partner(PartnerType::With("Rory Williams".to_string())),
            ],
            vec![],
        );
        // Can pair with a Doctor
        let doctor = partner_face("The Thirteenth Doctor", vec![], vec!["Time Lord", "Doctor"]);
        assert!(are_valid_partners(&amy, &doctor, None));

        // Can pair with Rory Williams
        let rory = partner_face(
            "Rory Williams",
            vec![Keyword::Partner(PartnerType::With("Amy Pond".to_string()))],
            vec![],
        );
        assert!(are_valid_partners(&amy, &rory, None));

        // Cannot pair with a random generic partner
        let random = partner_face(
            "Random",
            vec![Keyword::Partner(PartnerType::Generic)],
            vec![],
        );
        assert!(!are_valid_partners(&amy, &random, None));
    }

    // CR 702.124: the public `can_pair_commanders` seam (consumed by the WASM
    // deck-builder bridge) must resolve both names through the database and apply
    // the asymmetric Doctor's Companion rule in either selection order.
    #[test]
    fn can_pair_commanders_resolves_doctor_pairing_through_db() {
        fn card_json(
            name: &str,
            subtypes: &[&str],
            keywords: serde_json::Value,
        ) -> serde_json::Value {
            serde_json::json!({
                "name": name,
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": subtypes },
                "power": "2", "toughness": "2",
                "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": keywords,
                "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null, "legalities": {}
            })
        }
        let db_json = serde_json::json!({
            "amy pond": card_json("Amy Pond", &["Human"], serde_json::json!([{ "Partner": { "type": "DoctorsCompanion" } }])),
            "the eleventh doctor": card_json("The Eleventh Doctor", &["Time Lord", "Doctor"], serde_json::json!([])),
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();

        assert!(can_pair_commanders(
            &db,
            "Amy Pond",
            "The Eleventh Doctor",
            None
        ));
        assert!(can_pair_commanders(
            &db,
            "The Eleventh Doctor",
            "Amy Pond",
            None
        ));
        // Unknown names resolve to no pairing rather than panicking.
        assert!(!can_pair_commanders(
            &db,
            "Amy Pond",
            "Nonexistent Card",
            None
        ));
    }

    #[test]
    fn doctors_companion_rejects_non_doctor_subtypes() {
        let companion = partner_face(
            "Amy Pond",
            vec![Keyword::Partner(PartnerType::DoctorsCompanion)],
            vec![],
        );
        let human_doctor = partner_face("Not A Real Doctor", vec![], vec!["Human", "Doctor"]);
        assert!(!are_valid_partners(&companion, &human_doctor, None));

        let non_creature_doctor = CardFace {
            name: "Noncreature Doctor".to_string(),
            is_commander: true,
            card_type: crate::types::card_type::CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Time Lord".to_string(), "Doctor".to_string()],
            },
            ..CardFace::default()
        };
        assert!(!are_valid_partners(&companion, &non_creature_doctor, None));

        let unified = partner_face("The Eleventh Doctor", vec![], vec!["Time Lord Doctor"]);
        assert!(are_valid_partners(&companion, &unified, None));
    }

    #[test]
    fn no_partner_keywords_rejected() {
        let a = partner_face("A", vec![], vec![]);
        let b = partner_face("B", vec![], vec![]);
        assert!(!are_valid_partners(&a, &b, None));
    }

    // --- validate_deck_for_format tests ---

    #[test]
    fn validate_standard_rejects_non_standard_cards() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: vec!["Not Standard".to_string(); 60],
            sideboard: vec![],
            commander: vec![],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Standard)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = validate_deck_for_format(&db, &request);
        assert!(result.is_err());
        let reasons = result.unwrap_err();
        assert!(reasons.iter().any(|r| r.contains("Not Standard legal")));
    }

    #[test]
    fn validate_name_deck_for_format_rejects_non_standard_cards() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let main_deck = vec!["Not Standard".to_string(); 60];
        let result =
            validate_name_deck_for_format(&db, &main_deck, &[], &[], GameFormat::Standard, None);

        let reasons = result.expect_err("name-list validation must reject illegal AI decks");
        assert!(reasons.iter().any(|r| r.contains("Not Standard legal")));
    }

    #[test]
    fn validate_standard_accepts_legal_deck() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: vec![],
            commander: vec![],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Standard)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        assert!(validate_deck_for_format(&db, &request).is_ok());
    }

    #[test]
    fn non_oathbreaker_signature_spell_is_rejected_by_full_and_summary_validation() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: vec!["Legal Standard".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Modern)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let full = evaluate_deck_compatibility(&db, &request);
        assert_eq!(full.selected_format_compatible, Some(false));
        assert!(full
            .selected_format_reasons
            .iter()
            .any(|reason| reason == "Modern does not use a signature spell slot"));

        let summary = evaluate_deck_compatibility(
            &db,
            &DeckCompatibilityRequest {
                summary_only: true,
                ..request
            },
        );
        assert_eq!(summary.selected_format_compatible, Some(false));
        assert!(summary
            .selected_format_reasons
            .iter()
            .any(|reason| reason == "Modern does not use a signature spell slot"));
    }

    #[test]
    fn validate_ffa_accepts_any_deck() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: vec!["Not Standard".to_string(); 60],
            sideboard: vec![],
            commander: vec![],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeForAll)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        assert!(validate_deck_for_format(&db, &request).is_ok());
    }

    #[test]
    fn oathbreaker_missing_commander_does_not_spam_color_identity_errors() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Red Card", 58),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: vec!["Big Spell".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Oathbreaker)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("exactly 1 Oathbreaker")));
        assert!(!result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("outside Oathbreaker color identity")));
        assert!(!result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("signature spell is outside")));
    }

    #[test]
    fn validate_no_format_accepts_any_deck() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: vec!["Not Standard".to_string(); 60],
            sideboard: vec![],
            commander: vec![],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        assert!(validate_deck_for_format(&db, &request).is_ok());
    }

    // --- Sideboard size + combined copy-limit tests (CR 100.2a, CR 100.4a,
    // plus CR 709.2 / CR 712.1 one-physical-card canonicalization) ---

    #[test]
    fn constructed_sideboard_of_15_is_accepted() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: expand("Plains", 15),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Standard)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "reasons: {:?}",
            result.selected_format_reasons
        );
    }

    #[test]
    fn constructed_sideboard_of_16_is_rejected() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 60),
            sideboard: expand("Plains", 16),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Standard)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Sideboard has 16") && r.contains("maximum 15")));
    }

    #[test]
    fn combined_copies_over_four_rejected() {
        // 3 copies of "Legal Standard" in main + 2 in sideboard = 5 combined.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Legal Standard", 3);
        main.extend(expand("Plains", 57));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: expand("Legal Standard", 2),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Standard)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("More than 4 copies")));
    }

    #[test]
    fn combined_copies_basic_lands_exempt() {
        // 60 Plains in main + 15 Plains in sideboard — exempt.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 60),
            sideboard: expand("Plains", 15),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Standard)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn combined_copies_case_insensitive() {
        // Regression for B1: "Legal Standard" + "legal standard" must count as
        // the same card: CR 100.2a's limit is per English name, and case is not
        // part of a name, so both spellings share one copy-count bucket.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Legal Standard", 3);
        main.extend(expand("legal standard", 2)); // lowercase
        main.extend(expand("Plains", 55));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Standard)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("More than 4 copies")));
    }

    #[test]
    fn relentless_rats_allows_more_than_four() {
        // B2: cards whose Oracle text grants "A deck can have any number of
        // cards named X" are exempt from the 4-per-name rule.
        let db_json = serde_json::json!({
            "relentless rats": {
                "name": "Relentless Rats",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": ["Rat"] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": "Relentless Rats gets +1/+1 for each other creature named Relentless Rats you control. A deck can have any number of cards named Relentless Rats.",
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal",
                    "pioneer": "legal"
                }
            }
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Relentless Rats", 60),
            sideboard: expand("Relentless Rats", 15),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Standard)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn commander_singleton_now_case_insensitive() {
        // Regression for B1 applied retroactively to commander: 2x "Legal
        // Standard" with different casing used to slip past the singleton
        // check because the HashMap was keyed by raw string.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Legal Standard", 1);
        main.extend(expand("legal standard", 1));
        main.extend(expand("Plains", 97));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(!result.commander.compatible);
        assert!(result
            .commander
            .reasons
            .iter()
            .any(|r| r.contains("Singleton violations")));
    }

    #[test]
    fn commander_color_identity_uses_explicit_card_face_identity() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Plains", 98);
        main.push("Red Card".to_string());
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Grub Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert!(
            result.commander.compatible,
            "{:?}",
            result.commander.reasons
        );
    }

    #[test]
    fn commander_singleton_exempts_deck_limit_override() {
        // A commander deck running 5x Relentless Rats passes the singleton
        // check because the card grants its own deck-limit override.
        let db_json = serde_json::json!({
            "relentless rats": {
                "name": "Relentless Rats",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": ["Rat"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": "A deck can have any number of cards named Relentless Rats.",
                "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "legal commander": {
                "name": "Legal Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic"], "core_types": ["Land"], "subtypes": ["Plains"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            }
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();
        let mut main = expand("Relentless Rats", 5);
        main.extend(expand("Plains", 94));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "expected compatible, got reasons: {:?}",
            result.commander.reasons
        );
    }

    #[test]
    fn commander_accepts_ten_slime_against_humanity_copies() {
        // Issue #1138: "A deck can have any number of cards named Slime Against
        // Humanity" must override the Commander singleton default.
        let db_json = serde_json::json!({
            "slime against humanity": {
                "name": "Slime Against Humanity",
                "mana_cost": { "type": "Cost", "shards": ["Green"], "generic": 2 },
                "card_type": { "supertypes": [], "core_types": ["Sorcery"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": "Create a 0/0 green Ooze creature token with trample. Put X +1/+1 counters on it, where X is two plus the total number of cards you own in exile and in your graveyard that are Oozes or are named Slime Against Humanity.\nA deck can have any number of cards named Slime Against Humanity.",
                "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": ["Green"], "scryfall_oracle_id": null,
                "deck_copy_limit": { "type": "Unlimited" },
                "legalities": { "commander": "legal" }
            },
            "legal commander": {
                "name": "Legal Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": ["Green"], "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "forest": {
                "name": "Forest",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic"], "core_types": ["Land"], "subtypes": ["Forest"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            }
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();
        let mut main = expand("Slime Against Humanity", 10);
        main.extend(expand("Forest", 89));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "expected compatible, got reasons: {:?}",
            result.commander.reasons
        );
    }

    #[test]
    fn commander_sideboard_policy_accepts_maybeboard_entries() {
        // CR 903.5e: Phase's deck builder reuses the sideboard slot as a
        // builder-only "Maybeboard" for Commander-style formats. The
        // validator must accept extra entries (the engine drops them at game
        // load) and the deck must not be flagged as BO3-ready.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 99),
            sideboard: vec!["Plains".to_string()],
            commander: vec!["Legal Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "reasons: {:?}",
            result.selected_format_reasons
        );
        assert!(result.commander.compatible);
        assert!(!result.bo3_ready, "commander decks are never BO3-ready");
    }

    #[test]
    fn validate_deck_for_format_rejects_oversize_sideboard() {
        // S8: registration gate must reject a 16-card sideboard for Standard.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 60),
            sideboard: expand("Plains", 16),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Standard)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let err = validate_deck_for_format(&db, &request)
            .expect_err("16-card sideboard must be rejected at registration");
        assert!(err.iter().any(|r| r.contains("Sideboard has 16")));
    }

    #[test]
    fn free_for_all_bo3_requires_sideboard_but_no_size_cap() {
        // S2: Unlimited policy formats allow BO3 with arbitrarily large
        // sideboards — only the non-empty requirement applies.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();

        let no_sideboard = DeckCompatibilityRequest {
            main_deck: expand("Plains", 60),
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeForAll)),
            selected_match_type: Some(MatchType::Bo3),
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &no_sideboard);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("BO3 requires a sideboard")));

        let huge_sideboard = DeckCompatibilityRequest {
            sideboard: expand("Plains", 30),
            ..no_sideboard
        };
        let result = evaluate_deck_compatibility(&db, &huge_sideboard);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn validate_commander_rejects_non_singleton() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: vec!["Legal Standard".to_string(); 99],
            sideboard: vec![],
            commander: vec!["Test Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = validate_deck_for_format(&db, &request);
        assert!(result.is_err());
        let reasons = result.unwrap_err();
        assert!(reasons.iter().any(|r| r.contains("Singleton violations")));
    }

    /// CR 100.2a + CR 205.3i: Basic-lands exemption from singleton is driven
    /// by the Basic *supertype*, not a fixed name allowlist. Snow-Covered
    /// Plains and Wastes both carry the Basic supertype; Llanowar Elves does
    /// not.
    fn basic_supertype_test_db() -> String {
        serde_json::json!({
            "snow-covered plains": {
                "name": "Snow-Covered Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic", "Snow"],
                    "core_types": ["Land"],
                    "subtypes": ["Plains"]
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"],
                    "core_types": ["Land"],
                    "subtypes": ["Plains"]
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "wastes": {
                "name": "Wastes",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"],
                    "core_types": ["Land"],
                    "subtypes": []
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "llanowar elves": {
                "name": "Llanowar Elves",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": [],
                    "core_types": ["Creature"],
                    "subtypes": ["Elf", "Druid"]
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "legal commander": {
                "name": "Legal Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Legendary"],
                    "core_types": ["Creature"],
                    "subtypes": []
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            }
        })
        .to_string()
    }

    #[test]
    fn commander_singleton_permits_snow_covered_basic_duplicates() {
        let db = CardDatabase::from_json_str(&basic_supertype_test_db()).unwrap();
        let mut main = expand("Snow-Covered Plains", 10);
        main.extend(expand("Plains", 89));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "Snow-Covered Plains must be treated as basic; reasons: {:?}",
            result.commander.reasons
        );
    }

    #[test]
    fn commander_singleton_permits_wastes_duplicates() {
        let db = CardDatabase::from_json_str(&basic_supertype_test_db()).unwrap();
        let mut main = expand("Wastes", 10);
        main.extend(expand("Plains", 89));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "Wastes (Basic supertype) must be treated as basic; reasons: {:?}",
            result.commander.reasons
        );
    }

    #[test]
    fn commander_singleton_rejects_non_basic_duplicates() {
        let db = CardDatabase::from_json_str(&basic_supertype_test_db()).unwrap();
        let mut main = expand("Llanowar Elves", 2);
        main.extend(expand("Plains", 97));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            !result.commander.compatible,
            "duplicate non-basic must still fail singleton"
        );
        assert!(result
            .commander
            .reasons
            .iter()
            .any(|r| r.contains("Llanowar Elves")));
    }

    #[test]
    fn commander_singleton_permits_mixed_basic_variants() {
        let db = CardDatabase::from_json_str(&basic_supertype_test_db()).unwrap();
        let mut main = vec!["Plains".to_string(), "Snow-Covered Plains".to_string()];
        main.extend(expand("Wastes", 97));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "1x Plains + 1x Snow-Covered Plains must pass; reasons: {:?}",
            result.commander.reasons
        );
    }

    fn vintage_test_db() -> String {
        // Power-9-shaped fixture: a "restricted" Vintage card and a generic
        // legal Vintage filler so we can build a 60-card deck.
        serde_json::json!({
            "black lotus": {
                "name": "Black Lotus",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Artifact"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "vintage": "restricted" }
            },
            "island": {
                "name": "Island",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"], "core_types": ["Land"], "subtypes": ["Island"]
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "vintage": "legal" }
            }
        })
        .to_string()
    }

    #[test]
    fn vintage_one_copy_of_restricted_card_is_legal() {
        // CR 100.6: Tournament rules may limit a card's use. Phase's
        // `Restricted` status uses a one-copy format-policy. Regression for a
        // bug where `is_legal()` rejected the status, marking Power 9 as
        // illegal in Vintage decks.
        let db = CardDatabase::from_json_str(&vintage_test_db()).unwrap();
        let mut main = vec!["Black Lotus".to_string()];
        main.extend(expand("Island", 59));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Vintage)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "1x restricted card must be legal in Vintage; reasons: {:?}",
            result.selected_format_reasons
        );
    }

    #[test]
    fn vintage_two_copies_of_restricted_card_violate_one_copy_limit() {
        // CR 100.6: Tournament rules may limit a card's use. Two copies
        // violate Phase's restricted-card policy — the deck must be flagged,
        // but the message is "More than 1 copy of a restricted card", not
        // "banned".
        let db = CardDatabase::from_json_str(&vintage_test_db()).unwrap();
        let mut main = expand("Black Lotus", 2);
        main.extend(expand("Island", 58));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Vintage)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(
            result
                .selected_format_reasons
                .iter()
                .any(|r| r.contains("More than 1 copy of a restricted card")
                    && r.contains("Black Lotus")),
            "expected restricted-copy violation; reasons: {:?}",
            result.selected_format_reasons
        );
        assert!(
            !result
                .selected_format_reasons
                .iter()
                .any(|r| r.contains("Not Vintage legal")),
            "restricted card must not be flagged as illegal; reasons: {:?}",
            result.selected_format_reasons
        );
    }

    fn momir_request(main: Vec<String>) -> DeckCompatibilityRequest {
        DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Momir)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        }
    }

    /// The fixed Momir's Madness deck: 12 copies of each of the five snow basic
    /// lands (no Snow-Covered Wastes), totaling 60. Delegates to the engine's
    /// canonical `momir_fixed_deck_names()` so the auto-supplied deck and this
    /// validator are exercised against the same single source of truth — if they
    /// ever drift, `momir_madness_snow_basics_pass` below catches it.
    fn momir_madness_deck() -> Vec<String> {
        crate::game::deck_loading::momir_fixed_deck_names()
    }

    #[test]
    fn momir_madness_snow_basics_pass() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = momir_request(momir_madness_deck());
        let check = evaluate_momir(&db, &request, &BTreeSet::new());
        assert!(
            check.compatible,
            "12x each of the five snow basics must be a legal Momir's Madness deck, reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn momir_madness_regular_basics_fail() {
        // 60 regular (non-snow) Plains must be rejected — Momir's Madness
        // requires snow basics, not ordinary basics.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let check = evaluate_momir(&db, &momir_request(expand("Plains", 60)), &BTreeSet::new());
        assert!(
            !check.compatible,
            "60 regular (non-snow) basics must be rejected"
        );
        assert!(
            check
                .reasons
                .iter()
                .any(|r| r.contains("only contain the five snow basic lands")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn momir_madness_wrong_per_type_count_fails() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // 11 Plains + 13 Island + 12 each of the other three = 60 total, but the
        // fixed 12-per-type ratio is broken.
        let mut deck = expand("Snow-Covered Plains", 11);
        deck.extend(expand("Snow-Covered Island", 13));
        deck.extend(expand("Snow-Covered Swamp", 12));
        deck.extend(expand("Snow-Covered Mountain", 12));
        deck.extend(expand("Snow-Covered Forest", 12));
        assert_eq!(deck.len(), 60);
        let check = evaluate_momir(&db, &momir_request(deck), &BTreeSet::new());
        assert!(!check.compatible, "an off-ratio deck must be rejected");
        assert!(
            check
                .reasons
                .iter()
                .any(|r| r.contains("exactly 12") && r.contains("Plains")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn momir_madness_missing_type_fails() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // 15 each of four types = 60 total, but Forest is entirely absent. This is
        // the "iterate over expected types, not present types" guard: a naive
        // per-present-type check would pass this (every present type is off-ratio
        // too, but the danger is a 12-each-of-four + 12-extra shape; here the rule
        // must reject because Forest's count resolves to 0 != 12.
        let mut deck = expand("Snow-Covered Plains", 15);
        deck.extend(expand("Snow-Covered Island", 15));
        deck.extend(expand("Snow-Covered Swamp", 15));
        deck.extend(expand("Snow-Covered Mountain", 15));
        assert_eq!(deck.len(), 60);
        let check = evaluate_momir(&db, &momir_request(deck), &BTreeSet::new());
        assert!(
            !check.compatible,
            "a deck missing one of the five snow basic types must be rejected"
        );
        assert!(
            check
                .reasons
                .iter()
                .any(|r| r.contains("exactly 12") && r.contains("Forest")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn momir_madness_count_off_total_fails() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        for delta in [-1i32, 1] {
            let mut deck = momir_madness_deck();
            if delta > 0 {
                deck.push("Snow-Covered Plains".to_string());
            } else {
                deck.pop();
            }
            let check = evaluate_momir(&db, &momir_request(deck), &BTreeSet::new());
            assert!(
                !check.compatible,
                "a deck off the 60-card total (delta {delta}) must be rejected"
            );
            assert!(check.reasons.iter().any(|r| r.contains("exactly 60")));
        }
    }

    #[test]
    fn momir_madness_snow_covered_wastes_fails() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // Swap one Snow-Covered Plains for a Snow-Covered Wastes: still snow +
        // basic + land, but "Wastes" is not a basic land type (CR 305.6).
        let mut deck = expand("Snow-Covered Wastes", 1);
        deck.extend(expand("Snow-Covered Plains", 11));
        deck.extend(expand("Snow-Covered Island", 12));
        deck.extend(expand("Snow-Covered Swamp", 12));
        deck.extend(expand("Snow-Covered Mountain", 12));
        deck.extend(expand("Snow-Covered Forest", 12));
        assert_eq!(deck.len(), 60);
        let check = evaluate_momir(&db, &momir_request(deck), &BTreeSet::new());
        assert!(!check.compatible, "Snow-Covered Wastes must be rejected");
        assert!(
            check
                .reasons
                .iter()
                .any(|r| r.contains("only contain the five snow basic lands")),
            "Snow-Covered Wastes must be flagged as a non-snow-basic-type card; reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn momir_madness_non_basic_fails() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut deck = expand("Snow-Covered Plains", 11);
        deck.extend(expand("Snow-Covered Island", 12));
        deck.extend(expand("Snow-Covered Swamp", 12));
        deck.extend(expand("Snow-Covered Mountain", 12));
        deck.extend(expand("Snow-Covered Forest", 12));
        deck.push("Legal Standard".to_string()); // non-basic, total 60
        assert_eq!(deck.len(), 60);
        let check = evaluate_momir(&db, &momir_request(deck), &BTreeSet::new());
        assert!(!check.compatible, "a non-basic card must be rejected");
        assert!(check
            .reasons
            .iter()
            .any(|r| r.contains("only contain the five snow basic lands")));
    }

    #[test]
    fn momir_madness_non_empty_sideboard_fails() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut request = momir_request(momir_madness_deck());
        request.sideboard = vec!["Snow-Covered Plains".to_string()];
        let check = evaluate_momir(&db, &request, &BTreeSet::new());
        assert!(!check.compatible, "Momir's Madness has no sideboard");
        assert!(check.reasons.iter().any(|r| r.contains("sideboard")));
    }

    #[test]
    fn selected_format_tag_falls_back_to_the_bare_method_default() {
        let selected = SelectedFormat::Tag(GameFormat::Standard);
        assert_eq!(
            selected.rules().unwrap().default_deck_copy_limit,
            GameFormat::Standard.default_deck_copy_limit()
        );
    }

    #[test]
    fn selected_format_resolved_prefers_its_own_threaded_value() {
        let stricter = FormatConfig {
            default_deck_copy_limit: DeckCopyLimit::UpTo(1),
            ..FormatConfig::standard()
        };
        let selected = SelectedFormat::Resolved(Box::new(stricter));
        assert_eq!(
            selected.rules().unwrap().default_deck_copy_limit,
            DeckCopyLimit::UpTo(1)
        );
    }

    /// V4 (Verification Matrix): the untrusted-boundary counterpart to
    /// `SelectedFormat`'s Wire-Inertness Invariant — supersedes the deleted
    /// `deck_compatibility_request_default_deck_copy_limit_is_wire_inert`
    /// test, whose subject (`DeckCompatibilityRequest.default_deck_copy_limit`)
    /// no longer exists.
    #[test]
    fn deck_compatibility_request_selected_format_is_wire_inert() {
        // Positive control: `FormatConfig` really does serialize to a JSON
        // object, so the negative assertion below is not vacuous.
        assert!(serde_json::to_value(FormatConfig::standard())
            .unwrap()
            .is_object());

        let json = serde_json::json!({
            "selected_format": {
                "format": "Standard",
                "starting_life": 20,
                "min_players": 2,
                "max_players": 2,
                "deck_size": { "type": "Minimum", "data": 60 },
                "singleton": false,
                "command_zone": false,
                "commander_damage_threshold": null,
                "team_based": false,
                "uses_commander": false,
                "sideboard_policy": { "type": "Unlimited" },
                "default_deck_copy_limit": { "type": "Unlimited" },
            }
        });
        let result: Result<DeckCompatibilityRequest, _> = serde_json::from_value(json);
        assert!(
            result.is_err(),
            "a full FormatConfig object must never deserialize into selected_format"
        );

        let tag_json = serde_json::json!({ "selected_format": "Standard" });
        let request: DeckCompatibilityRequest = serde_json::from_value(tag_json).unwrap();
        assert_eq!(
            request.selected_format,
            Some(SelectedFormat::Tag(GameFormat::Standard))
        );
    }

    #[test]
    fn admission_and_max_deck_copies_agree_on_the_format_default_copy_limit() {
        // Filler is basic Plains (CR 205.4c-exempt, exactly as the shared
        // `legal_60_main` helper does) so the only card the copy rule can
        // bind on is "Red Card" — a non-basic filler would itself blow the
        // ceiling and make both halves pass for the wrong reason.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let format_config = FormatConfig::standard();

        assert_eq!(
            max_deck_copies(&db, "Red Card", &format_config),
            DeckCopyLimit::UpTo(4)
        );

        let mut at_limit = expand("Plains", 56);
        at_limit.extend(expand("Red Card", 4));
        assert!(validate_name_deck_for_format_full(
            &db,
            &at_limit,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &format_config,
            None,
            default_player_count(),
        )
        .is_ok());

        let mut over_limit = expand("Plains", 55);
        over_limit.extend(expand("Red Card", 5));
        match validate_name_deck_for_format_full(
            &db,
            &over_limit,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &format_config,
            None,
            default_player_count(),
        ) {
            Err(reasons) => assert!(reasons.iter().any(|r| r.contains("More than 4 copies"))),
            Ok(()) => panic!("5 copies of Red Card must exceed Standard's CR 100.2a ceiling"),
        }
    }

    /// `evaluate_standard` remains category (b): it hardcodes
    /// `&FormatConfig::standard()` for the reference-column STD badge and
    /// never reads the request's resolved rules. But `evaluate_selected_format`'s
    /// literal `GameFormat::Standard` arm is a SEPARATE call — it invokes
    /// `evaluate_constructed` fresh with this request's own `format_rules`,
    /// exactly like every other format's arm. A `Resolved` config's stricter
    /// `default_deck_copy_limit` on a literal Standard selection is therefore
    /// honored by the authoritative admission path, matching
    /// `evaluate_{planechase,archenemy}_honors_a_stricter_resolved_copy_limit`.
    #[test]
    fn validate_name_deck_for_format_full_honors_a_stricter_resolved_copy_limit_on_standard() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let stricter_config = FormatConfig {
            default_deck_copy_limit: DeckCopyLimit::UpTo(1),
            ..FormatConfig::standard()
        };
        let mut deck = expand("Plains", 58);
        deck.extend(expand("Red Card", 2));
        match validate_name_deck_for_format_full(
            &db,
            &deck,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &stricter_config,
            None,
            default_player_count(),
        ) {
            Err(reasons) => assert!(
                reasons.iter().any(|r| r.contains("More than 1 cop")),
                "reasons: {reasons:?}"
            ),
            Ok(()) => panic!(
                "the literal GameFormat::Standard admission arm must call evaluate_constructed \
                 fresh with this request's resolved format_rules, honoring a stricter \
                 default_deck_copy_limit exactly like every other format's arm"
            ),
        }
    }

    /// Commander sibling of the Standard test above — the registry's own
    /// Commander `default_deck_copy_limit` is already `UpTo(1)` (a singleton
    /// format), so `UpTo(0)` (not `UpTo(1)`) is the value that is actually
    /// STRICTER than the registry and therefore discriminates: reverting the
    /// `evaluate_selected_format` Commander arm to reuse the precomputed,
    /// registry-only `commander: &CompatibilityCheck` would let this exact
    /// fixture's single "Legal Standard" copy pass (1 <= registry's 1), so
    /// this assertion flips to `Ok` under that regression.
    #[test]
    fn validate_name_deck_for_format_full_honors_a_stricter_resolved_copy_limit_on_commander() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let stricter_config = FormatConfig {
            default_deck_copy_limit: DeckCopyLimit::UpTo(0),
            ..FormatConfig::commander()
        };
        let mut main = expand("Legal Standard", 1);
        main.extend(expand("Plains", 98));
        match validate_name_deck_for_format_full(
            &db,
            &main,
            &[],
            &["Legal Commander".to_string()],
            &[],
            &[],
            &[],
            &[],
            &[],
            &stricter_config,
            None,
            default_player_count(),
        ) {
            Err(reasons) => assert!(
                reasons.iter().any(|r| r.contains("Legal Standard")),
                "reasons: {reasons:?}"
            ),
            Ok(()) => panic!(
                "the literal GameFormat::Commander admission arm must call \
                 evaluate_commander_with_format fresh with this request's resolved \
                 format_rules, honoring a stricter default_deck_copy_limit exactly like every \
                 other format's arm"
            ),
        }
    }

    #[test]
    fn evaluate_planechase_honors_a_stricter_resolved_copy_limit() {
        let db = planechase_test_db();
        let mut main = expand("Legal Standard", 2);
        main.extend(expand("Plains", 58));
        let base = DeckCompatibilityRequest {
            main_deck: main,
            ..planechase_request(2, plane_names(20))
        };

        let unthreaded =
            evaluate_planechase(&db, &base, &BTreeSet::new(), &FormatConfig::planechase());
        assert!(unthreaded.compatible, "reasons: {:?}", unthreaded.reasons);

        let stricter_rules = FormatConfig {
            default_deck_copy_limit: DeckCopyLimit::UpTo(1),
            ..FormatConfig::planechase()
        };
        let stricter = DeckCompatibilityRequest {
            selected_format: Some(SelectedFormat::Resolved(Box::new(stricter_rules.clone()))),
            ..base
        };
        let check = evaluate_planechase(&db, &stricter, &BTreeSet::new(), &stricter_rules);
        assert!(!check.compatible, "reasons: {:?}", check.reasons);
        assert!(check.reasons.iter().any(|r| r.contains("Legal Standard")));
    }

    /// Representative sibling of `evaluate_planechase_honors_a_stricter_
    /// resolved_copy_limit`, but through `evaluate_deck_compatibility`'s
    /// `summary_only` dispatch — `evaluate_selected_format_summary` ->
    /// `quick_planechase_check` -> `evaluate_planechase`. Fix 4 threaded
    /// `format_rules` into `quick_planechase_check` as a parameter rather
    /// than a re-derivation; a caller that silently passed the WRONG value
    /// (e.g. a hardcoded `&FormatConfig::planechase()` instead of the
    /// request's own resolved `format_rules`) would still compile, so only
    /// an end-to-end assertion through the real dispatch — not the
    /// compiler — can catch that class of mistake. The other three
    /// (`Archenemy`/`TinyLeaders`/`Oathbreaker`) share the identical
    /// `quick_*_check(db, request, &format_rules)` wiring pattern at the
    /// same call site (`evaluate_selected_format_summary`) and are not
    /// separately hostile-fixture-tested this round.
    #[test]
    fn summary_planechase_honors_a_stricter_resolved_copy_limit() {
        let db = planechase_test_db();
        let mut main = expand("Legal Standard", 2);
        main.extend(expand("Plains", 58));
        let base = DeckCompatibilityRequest {
            main_deck: main,
            summary_only: true,
            ..planechase_request(2, plane_names(20))
        };

        let baseline = evaluate_deck_compatibility(&db, &base);
        assert_eq!(
            baseline.selected_format_compatible,
            Some(true),
            "reasons: {:?}",
            baseline.selected_format_reasons
        );

        let stricter_rules = FormatConfig {
            default_deck_copy_limit: DeckCopyLimit::UpTo(1),
            ..FormatConfig::planechase()
        };
        let stricter = DeckCompatibilityRequest {
            selected_format: Some(SelectedFormat::Resolved(Box::new(stricter_rules))),
            ..base
        };
        let result = evaluate_deck_compatibility(&db, &stricter);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(
            result
                .selected_format_reasons
                .iter()
                .any(|r| r.contains("Legal Standard")),
            "reasons: {:?}",
            result.selected_format_reasons
        );
    }

    #[test]
    fn evaluate_archenemy_honors_a_stricter_resolved_copy_limit() {
        let db = archenemy_test_db();
        let mut main = expand("Legal Standard", 2);
        main.extend(expand("Plains", 58));
        let base = DeckCompatibilityRequest {
            main_deck: main,
            ..archenemy_request(scheme_names(20))
        };

        let unthreaded =
            evaluate_archenemy(&db, &base, &BTreeSet::new(), &FormatConfig::archenemy());
        assert!(unthreaded.compatible, "reasons: {:?}", unthreaded.reasons);

        let stricter_rules = FormatConfig {
            default_deck_copy_limit: DeckCopyLimit::UpTo(1),
            ..FormatConfig::archenemy()
        };
        let stricter = DeckCompatibilityRequest {
            selected_format: Some(SelectedFormat::Resolved(Box::new(stricter_rules.clone()))),
            ..base
        };
        let check = evaluate_archenemy(&db, &stricter, &BTreeSet::new(), &stricter_rules);
        assert!(!check.compatible, "reasons: {:?}", check.reasons);
        assert!(check.reasons.iter().any(|r| r.contains("Legal Standard")));
    }

    #[test]
    fn evaluate_tiny_leaders_honors_a_stricter_resolved_copy_limit() {
        let db = CardDatabase::from_json_str(&tiny_leaders_test_db_json()).unwrap();
        // "Small Spell" (MV 1, colorless, not on the Tiny Leaders deck-ban
        // list — unlike "Sol Ring", which IS banned) clears the Tiny Leaders
        // MV <= 3 cost-identity cap at a single copy, so the registry-default
        // baseline below is actually fully compatible rather than merely
        // "still has a singleton violation but for an unrelated reason".
        let mut main = expand("Small Spell", 1);
        main.extend(expand("Plains", 48));
        let base = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["White Tiny Leader".to_string()],
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::TinyLeaders)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let unthreaded =
            evaluate_tiny_leaders(&db, &base, &BTreeSet::new(), &FormatConfig::tiny_leaders());
        assert!(unthreaded.compatible, "reasons: {:?}", unthreaded.reasons);

        // `UpTo(0)` is strictly stricter than the registry's `UpTo(1)`
        // (CR 903.5b's Tiny Leaders singleton default) under
        // `DeckCopyLimit::permits_no_more_than`, matching the admissible
        // "equal-or-stricter" direction `built_in_axes_no_looser_than_rules`
        // allows through — unlike `Unlimited`, which that gate would reject
        // outright as looser than the registry.
        let stricter_rules = FormatConfig {
            default_deck_copy_limit: DeckCopyLimit::UpTo(0),
            ..FormatConfig::tiny_leaders()
        };
        let stricter = DeckCompatibilityRequest {
            selected_format: Some(SelectedFormat::Resolved(Box::new(stricter_rules.clone()))),
            ..base
        };
        let check = evaluate_tiny_leaders(&db, &stricter, &BTreeSet::new(), &stricter_rules);
        assert!(
            check
                .reasons
                .iter()
                .any(|r| r.contains("Singleton violations")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn evaluate_oathbreaker_honors_a_stricter_resolved_copy_limit() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // A single copy of the non-basic "Red Card" plus 59 (copy-exempt)
        // Plains — legal under the registry's `UpTo(1)` default, unlike the
        // original 60-copy fixture, which already violated even the
        // registry default and so could never isolate a STRICTER-than-
        // registry ceiling (the direction `built_in_axes_no_looser_than_rules`
        // actually admits).
        let mut main = expand("Red Card", 1);
        main.extend(expand("Plains", 59));
        let base = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: Vec::new(),
            companion: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Oathbreaker)),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
            draft_set_codes: Vec::new(),
        };

        let unthreaded =
            evaluate_oathbreaker(&db, &base, &BTreeSet::new(), &FormatConfig::oathbreaker());
        assert!(
            !unthreaded
                .reasons
                .iter()
                .any(|r| r.contains("Singleton violations")),
            "reasons: {:?}",
            unthreaded.reasons
        );

        // `UpTo(0)` is strictly stricter than the registry's `UpTo(1)` under
        // `DeckCopyLimit::permits_no_more_than` — the admissible direction —
        // unlike the `Unlimited` this test previously used, which
        // `built_in_axes_no_looser_than_rules` would reject outright.
        let stricter_rules = FormatConfig {
            default_deck_copy_limit: DeckCopyLimit::UpTo(0),
            ..FormatConfig::oathbreaker()
        };
        let stricter = DeckCompatibilityRequest {
            selected_format: Some(SelectedFormat::Resolved(Box::new(stricter_rules.clone()))),
            ..base
        };
        let check = evaluate_oathbreaker(&db, &stricter, &BTreeSet::new(), &stricter_rules);
        assert!(
            check
                .reasons
                .iter()
                .any(|r| r.contains("Singleton violations")),
            "reasons: {:?}",
            check.reasons
        );
    }

    /// Regression: deck validation must resolve every name through
    /// `CardDatabase::lookup_key`, never through its own `//` split.
    ///
    /// The discriminating case is a single-faced card whose printed name
    /// literally contains `//` (issue #4790's `"SP//dr, Piloted by Peni"`
    /// class) whose front segment is ALSO a real card. `lookup_key` matches the
    /// exact name first, so it resolves to the whole card; a split-first
    /// ordering resolves to the front segment — the wrong card, with the wrong
    /// legality. Today's corpus has no such collision, so this synthetic DB is
    /// the only barrier keeping the two orderings from diverging again.
    #[test]
    fn deck_validation_resolves_a_double_slash_card_name_before_splitting_it() {
        let mut cards = Map::new();
        // Front segment collides with a real, differently-legal card.
        let mut fire = planechase_card_json("Fire", &[], &["Instant"]);
        fire["legalities"] = serde_json::json!({ "standard": "banned" });
        cards.insert("fire".to_string(), fire);
        insert_planechase_card(&mut cards, "Fire//Ice, the Whole Card", &[], &["Instant"]);
        insert_planechase_card(&mut cards, "Plains", &["Basic"], &["Land"]);
        let db = CardDatabase::from_json_str(&Value::Object(cards).to_string()).unwrap();

        let whole = "Fire//Ice, the Whole Card";

        // Every assertion below goes through a deck-validation wrapper, not
        // `db` directly: `CardDatabase`'s own accessors already resolved
        // through `lookup_key` before this change, so asserting on them cannot
        // fail if the local `//` split is reinstated. `card_db.rs` covers those
        // four cases in its own tests.

        // Copy counting buckets by the resolved key, so the whole card and its
        // colliding front segment stay distinct entries.
        assert_ne!(
            canonical_deck_count_key(&db, whole),
            canonical_deck_count_key(&db, "Fire"),
            "the whole card and its front segment must not share a count bucket"
        );

        // The unknown-card sweep must not report the whole card missing, and
        // must resolve the front segment to the front card rather than to it.
        assert!(card_is_known(&db, whole));
        assert_eq!(
            canonical_deck_count_key(&db, whole),
            whole.to_lowercase(),
            "the whole card must key by its own name, not its front segment"
        );

        // Legality on the dominant decklist path: a split-first ordering would
        // report the front segment's `banned`, condemning a legal deck. Asserted
        // through `evaluate_deck_compatibility`, the production entry point.
        let request = DeckCompatibilityRequest {
            main_deck: std::iter::repeat_n(whole.to_string(), 60).collect(),
            ..Default::default()
        };
        let legality = evaluate_format_legality(&db, &request);
        assert_eq!(
            legality.get("standard").map(String::as_str),
            Some("legal"),
            "legality must come from the whole card, not its front segment: {legality:?}"
        );
    }

    /// Regression: coverage must report one entry per *card*, not one per
    /// spelling. A decklist that mixes a composite name, a glued composite, and
    /// the bare front-face name refers to a single card; keying the unique set
    /// on raw strings counted it N times and gave each entry the full copy
    /// count, so the supported/total ratio the deck-builder renders was wrong.
    #[test]
    fn deck_coverage_counts_one_card_once_across_mixed_spellings() {
        let mut cards = Map::new();
        // `Fire` must be UNSUPPORTED here: `copies` is carried only by
        // `UnsupportedCard`, so a fully-supported fixture leaves the copy-count
        // assertion below with nothing to run against. Same construction as
        // `insert_unsupported_scheme_card`, which `archenemy_rejects_unsupported_scheme`
        // already pins through the same `card_face_gaps` predicate.
        let mut fire = planechase_card_json("Fire", &[], &["Instant"]);
        fire["abilities"] = serde_json::to_value(vec![crate::types::AbilityDefinition::new(
            crate::types::AbilityKind::Spell,
            crate::types::Effect::unimplemented("deck_coverage_test", "unsupported test card"),
        )])
        .unwrap();
        cards.insert("fire".to_string(), fire);
        insert_planechase_card(&mut cards, "Plains", &["Basic"], &["Land"]);
        let db = CardDatabase::from_json_str(&Value::Object(cards).to_string()).unwrap();

        let request = DeckCompatibilityRequest {
            main_deck: vec![
                "Fire // Ice".to_string(),
                "Fire//Ice".to_string(),
                "Fire".to_string(),
                "fire".to_string(),
            ],
            ..Default::default()
        };

        let coverage = evaluate_deck_coverage(&db, &request);
        assert_eq!(
            coverage.total_unique, 1,
            "four spellings of one card are one unique card"
        );
        assert_eq!(
            coverage.supported_unique + coverage.unsupported_cards.len(),
            coverage.total_unique,
            "every unique card must land in exactly one bucket"
        );
        assert_eq!(
            coverage.unsupported_cards.len(),
            1,
            "the unsupported fixture must reach the bucket the copy count lives in"
        );
        assert_eq!(
            coverage.unsupported_cards[0].copies, 4,
            "the single entry carries the whole playset, not a per-spelling share"
        );
    }

    #[test]
    fn deck_coverage_excludes_unresolvable_names_from_both_buckets() {
        // The mixed-spelling test above never reaches the `get_face_by_name ->
        // None` arm, because every fixture there is a known card. Production
        // decklists take that arm constantly (typos, un-exported cards), so
        // this fixture drives it directly: an unknown name must land in neither
        // coverage bucket AND must not inflate `total_unique`, or the deck
        // builder's supported/total ratio silently disagrees with itself.
        let mut cards = Map::new();
        insert_planechase_card(&mut cards, "Plains", &["Basic"], &["Land"]);
        let db = CardDatabase::from_json_str(&Value::Object(cards).to_string()).unwrap();

        let request = DeckCompatibilityRequest {
            main_deck: vec![
                "Plains".to_string(),
                "Totally Not A Real Card Xyzzy".to_string(),
            ],
            ..Default::default()
        };

        let coverage = evaluate_deck_coverage(&db, &request);
        assert_eq!(
            coverage.total_unique, 1,
            "the unresolvable name is not a card the engine can rate"
        );
        assert_eq!(
            coverage.supported_unique + coverage.unsupported_cards.len(),
            coverage.total_unique,
            "every counted card must land in exactly one bucket"
        );
    }

    /// A DFC commander whose two faces are each indexed under their own key,
    /// the way `data/card-data.json` stores every multi-face card (it holds no
    /// composite `"A // B"` keys at all). Both faces are mono-red so the deck's
    /// CR 903.5c identity is unambiguous.
    fn dfc_commander_db() -> CardDatabase {
        let mut cards = Map::new();
        let mut front =
            planechase_card_json("Tovolar, Dire Overlord", &["Legendary"], &["Creature"]);
        front["color_override"] = serde_json::json!(["Red"]);
        cards.insert("tovolar, dire overlord".to_string(), front);
        let mut back = planechase_card_json(
            "Tovolar, the Midnight Scourge",
            &["Legendary"],
            &["Creature"],
        );
        back["color_override"] = serde_json::json!(["Red"]);
        cards.insert("tovolar, the midnight scourge".to_string(), back);
        insert_planechase_card(&mut cards, "Mountain", &["Basic"], &["Land"]);
        CardDatabase::from_json_str(&Value::Object(cards).to_string()).unwrap()
    }

    /// CR 903.5a: the commander is one of the 100, so a decklist naming it in
    /// the command zone by its composite name and in the 99 by its front face
    /// describes ONE physical card and the deck is 100, not 101.
    ///
    /// This pins `commanders_represented_in_main`'s switch from
    /// `eq_ignore_ascii_case` to `same_card` — the composite-aware deck-size
    /// compare, and the one substantive rules change this PR makes to
    /// `deck_validation`. Restoring the raw-spelling comparison makes
    /// `represented_in_main` count 0 instead of 1, the deck reads 101, and the
    /// deck-size reason below appears.
    ///
    /// Asserted on the deck-SIZE reason specifically rather than on
    /// `reasons.is_empty()`: the singleton and netting behavior around the same
    /// listing is command-zone netting, which is a separate change.
    #[test]
    fn a_composite_named_commander_listed_in_the_99_counts_toward_the_100_once() {
        let db = dfc_commander_db();
        // Command zone spells the composite name; the main deck lists the front
        // face. One physical card, so total = main.len() + (1 - 1) = 100.
        let mut main = vec!["Tovolar, Dire Overlord".to_string()];
        main.extend(expand("Mountain", 99));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            commander: vec!["Tovolar, Dire Overlord // Tovolar, the Midnight Scourge".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            player_count: default_player_count(),
            ..Default::default()
        };

        // PREMISE: the two spellings really are different strings, so this is
        // about resolution rather than a trivially equal comparison.
        assert_ne!(request.commander[0], request.main_deck[0]);
        assert_eq!(
            commanders_represented_in_main(&db, &request),
            1,
            "the composite-named commander is the same physical card as the \
             front-face listing in the 99"
        );

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            !result
                .selected_format_reasons
                .iter()
                .any(|reason| reason.contains("exactly 100 cards")),
            "99 listings + 1 commander that is one of them is a legal 100, so no \
             deck-size reason may be reported: {:?}",
            result.selected_format_reasons
        );
    }

    /// Control for the test above: when the command-zone card is genuinely NOT
    /// in the 99, nothing is netted and the same 99 + 1 arithmetic must report
    /// a 100-card deck too — so the test above cannot pass merely because the
    /// deck-size check is inert.
    #[test]
    fn a_commander_absent_from_the_99_still_counts_toward_the_100() {
        let db = dfc_commander_db();
        // Commander absent from the 99: total = 99 + (1 - 0) = 100.
        let mut main = Vec::new();
        main.extend(expand("Mountain", 99));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            commander: vec!["Tovolar, Dire Overlord".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            player_count: default_player_count(),
            ..Default::default()
        };

        assert_eq!(
            commanders_represented_in_main(&db, &request),
            0,
            "the commander is not listed in the 99, so nothing is represented"
        );
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            !result
                .selected_format_reasons
                .iter()
                .any(|reason| reason.contains("exactly 100 cards")),
            "99 + a commander not among them is also exactly 100: {:?}",
            result.selected_format_reasons
        );
    }

    /// The commander exemption must not become a blanket amnesty: a SECOND
    /// copy of the commander in the 99 is still a CR 903.5b violation, and the
    /// deck is still 101 cards. Guards the deck-size compare in
    /// `combined_copy_counts` against over-crediting.
    #[test]
    fn a_genuine_second_copy_of_the_commander_is_still_a_violation() {
        let db = dfc_commander_db();
        let mut main = vec![
            "Tovolar, Dire Overlord".to_string(),
            "Tovolar, Dire Overlord // Tovolar, the Midnight Scourge".to_string(),
        ];
        main.extend(expand("Mountain", 99));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            commander: vec!["Tovolar, Dire Overlord".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            player_count: default_player_count(),
            ..Default::default()
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(
            result
                .selected_format_reasons
                .iter()
                .any(|reason| reason.contains("Singleton violations")),
            "two real copies must still trip CR 903.5b: {:?}",
            result.selected_format_reasons
        );
    }

    /// Regression: a DFC commander listed in the command zone by its composite
    /// name and in the 99 by its front name is one card. Comparing the raw
    /// spellings let it escape the command-zone skip and be reported as a
    /// CR 903.5c violation against its own color identity.
    #[test]
    fn commander_listed_under_two_spellings_is_not_its_own_identity_violation() {
        let mut cards = Map::new();
        let mut commander = planechase_card_json("Tovolar", &["Legendary"], &["Creature"]);
        commander["color_override"] = serde_json::json!(["Red"]);
        cards.insert("tovolar".to_string(), commander);
        insert_planechase_card(&mut cards, "Plains", &["Basic"], &["Land"]);
        let db = CardDatabase::from_json_str(&Value::Object(cards).to_string()).unwrap();

        let identity: HashSet<ManaColor> = HashSet::new();
        let violations = color_identity_violations(
            &db,
            &["Tovolar // Tovolar, the Midnight Scourge".to_string()],
            &identity,
            &BTreeSet::new(),
            |name| db.lookup_key(name) == db.lookup_key("Tovolar"),
        );
        assert!(
            violations.is_empty(),
            "the commander must be skipped under any spelling: {violations:?}"
        );

        // And when a card genuinely violates, it is reported once under its
        // resolved face name even if listed under two spellings.
        let violations = color_identity_violations(
            &db,
            &[
                "Tovolar // Tovolar, the Midnight Scourge".to_string(),
                "Tovolar".to_string(),
            ],
            &identity,
            &BTreeSet::new(),
            |_| false,
        );
        assert_eq!(
            violations.iter().cloned().collect::<Vec<_>>(),
            vec!["Tovolar".to_string()],
            "one card must produce exactly one violation entry"
        );
    }

    /// An Oathbreaker DB whose Oathbreaker and signature spell are each
    /// multi-face, with every face indexed under its own key the way
    /// `data/card-data.json` stores multi-face cards (it holds no composite
    /// `"A // B"` keys at all). Everything is mono-red so CR 903.5c and the
    /// Oathbreaker RC color-identity rule are unambiguous.
    fn dfc_oathbreaker_db() -> CardDatabase {
        let mut cards = Map::new();
        let mut ob_front = planechase_card_json("Ob Front", &["Legendary"], &["Planeswalker"]);
        ob_front["color_override"] = serde_json::json!(["Red"]);
        ob_front["is_oathbreaker"] = serde_json::json!(true);
        cards.insert("ob front".to_string(), ob_front);
        let mut ob_back = planechase_card_json("Ob Back", &["Legendary"], &["Planeswalker"]);
        ob_back["color_override"] = serde_json::json!(["Red"]);
        ob_back["is_oathbreaker"] = serde_json::json!(true);
        cards.insert("ob back".to_string(), ob_back);

        let mut sig_front = planechase_card_json("Sig Front", &[], &["Sorcery"]);
        sig_front["color_override"] = serde_json::json!(["Red"]);
        cards.insert("sig front".to_string(), sig_front);
        let mut sig_back = planechase_card_json("Sig Back", &[], &["Sorcery"]);
        sig_back["color_override"] = serde_json::json!(["Red"]);
        cards.insert("sig back".to_string(), sig_back);

        insert_planechase_card(&mut cards, "Mountain", &["Basic"], &["Land"]);
        CardDatabase::from_json_str(&Value::Object(cards).to_string()).unwrap()
    }

    /// The headline regression, driven end-to-end through
    /// `evaluate_deck_compatibility` rather than through a hand-written closure,
    /// so it actually executes the production comparison sites.
    ///
    /// A commander listed in the command zone by its composite name and in the
    /// 99 by its front name is ONE card (CR 903.5a: the deck is 100 cards
    /// *including* its commander; CR 709.2 / CR 712.1: a split or double-faced
    /// card is a single physical card, whichever of its faces a decklist names
    /// it by). Comparing raw spellings made all three CR 903.5 checks
    /// misfire at once: the deck counted 101 cards, the commander was reported
    /// as a 2-copy CR 903.5b singleton violation against itself, and it escaped
    /// the CR 903.5c command-zone skip.
    #[test]
    fn commander_listed_by_composite_and_front_name_is_one_card() {
        let db = dfc_commander_db();
        let composite = "Tovolar, Dire Overlord // Tovolar, the Midnight Scourge";

        let mut main = vec!["Tovolar, Dire Overlord".to_string()];
        main.extend(expand("Mountain", 99));
        assert_eq!(main.len(), 100, "the commander is one of the 100");

        for summary_only in [false, true] {
            let request = DeckCompatibilityRequest {
                main_deck: main.clone(),
                commander: vec![composite.to_string()],
                selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
                summary_only,
                player_count: default_player_count(),
                ..Default::default()
            };

            let result = evaluate_deck_compatibility(&db, &request);
            assert_eq!(
                result.selected_format_compatible,
                Some(true),
                "summary_only={summary_only}: deck must be legal, got: {:?}",
                result.selected_format_reasons
            );
            assert!(
                result.selected_format_reasons.is_empty(),
                "summary_only={summary_only}: {:?}",
                result.selected_format_reasons
            );
        }
    }

    /// Regression: the Oathbreaker RC deck-size check is the fourth
    /// command-zone format, and it must resolve card identity (CR 709.2 /
    /// CR 712.1: one physical card per multi-face card) like
    /// the other three. Listing the Oathbreaker in the command zone by its
    /// composite name and in the 59 by its front face describes ONE physical
    /// card; a raw-spelling compare counted it twice and rejected a legal
    /// 60-card deck as 61.
    #[test]
    fn oathbreaker_listed_by_composite_and_front_name_is_one_card() {
        let db = dfc_oathbreaker_db();

        // 58 Mountain + 1 Ob Front + 1 Sig Front = 60 physical cards.
        let mut main = vec!["Ob Front".to_string(), "Sig Front".to_string()];
        main.extend(expand("Mountain", 58));
        assert_eq!(main.len(), 60);

        // Both verdict paths: the summary twin delegates to the same
        // `evaluate_oathbreaker`, so they must agree for the same decklist.
        for summary_only in [false, true] {
            let request = DeckCompatibilityRequest {
                main_deck: main.clone(),
                commander: vec!["Ob Front // Ob Back".to_string()],
                signature_spell: vec!["Sig Front // Sig Back".to_string()],
                selected_format: Some(SelectedFormat::Tag(GameFormat::Oathbreaker)),
                summary_only,
                player_count: default_player_count(),
                ..Default::default()
            };

            let result = evaluate_deck_compatibility(&db, &request);
            assert_eq!(
                result.selected_format_compatible,
                Some(true),
                "summary_only={summary_only}: reasons: {:?}",
                result.selected_format_reasons
            );
            assert!(
                result.selected_format_reasons.is_empty(),
                "summary_only={summary_only}: {:?}",
                result.selected_format_reasons
            );
        }
    }

    /// Regression: the signature spell is a command-zone slot with the same
    /// "one physical card" identity as the Oathbreaker, so a composite/front
    /// spelling split across the two slots must not be reported as a CR 903.5b
    /// singleton violation against itself. Netting only the commander left this
    /// live even after the deck-size check was fixed.
    #[test]
    fn signature_spell_listed_under_two_spellings_is_not_its_own_singleton_violation() {
        let db = dfc_oathbreaker_db();

        let mut main = vec!["Ob Front".to_string(), "Sig Front".to_string()];
        main.extend(expand("Mountain", 58));

        let request = DeckCompatibilityRequest {
            main_deck: main,
            commander: vec!["Ob Front".to_string()],
            signature_spell: vec!["Sig Front // Sig Back".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Oathbreaker)),
            player_count: default_player_count(),
            ..Default::default()
        };

        let counts = combined_copy_counts(&db, &request, CommandZoneNetting::NetAgainstMainDeck);
        assert_eq!(
            counts.get("sig front"),
            Some(&1),
            "one physical signature spell counts once: {counts:?}"
        );

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            !result
                .selected_format_reasons
                .iter()
                .any(|reason| reason.contains("Singleton violations")),
            "{:?}",
            result.selected_format_reasons
        );
    }

    /// CR 903.5a netting corrects a DOUBLE-listing; it is not a blanket amnesty.
    /// When two command-zone slots resolve to the same card — here an
    /// Oathbreaker whose commander and signature spell are two spellings of one
    /// card — a per-entry `saturating_sub(1)` gated only on "the main deck
    /// contains this card at all" decrements twice against a single main-deck
    /// listing, netting a real 2-copy CR 903.5b violation down to 1 and hiding
    /// it. Bounding the decrement by main-deck occurrence is what keeps the
    /// violation visible.
    #[test]
    fn netting_never_credits_more_copies_than_the_main_deck_actually_lists() {
        let db = dfc_oathbreaker_db();

        let mut main = vec!["Ob Front".to_string(), "Ob Front".to_string()];
        main.extend(expand("Mountain", 58));

        let request = DeckCompatibilityRequest {
            main_deck: main,
            // Both command-zone slots resolve to the SAME card.
            commander: vec!["Ob Front".to_string()],
            signature_spell: vec!["Ob Front // Ob Back".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Oathbreaker)),
            player_count: default_player_count(),
            ..Default::default()
        };

        let counts = combined_copy_counts(&db, &request, CommandZoneNetting::NetAgainstMainDeck);
        assert_eq!(
            counts.get("ob front"),
            Some(&2),
            "three listings minus the one copy the main deck actually double-lists              leaves a genuine 2-copy singleton violation: {counts:?}"
        );
    }

    /// The signature-spell exemption must not become a blanket amnesty either:
    /// a genuine SECOND copy of the signature spell in the 58 is still a
    /// singleton violation. Guards the copy-count bucketing against
    /// over-crediting on the new slot.
    #[test]
    fn a_genuine_second_copy_of_the_signature_spell_is_still_a_violation() {
        let db = dfc_oathbreaker_db();

        let mut main = vec![
            "Ob Front".to_string(),
            "Sig Front".to_string(),
            "Sig Front // Sig Back".to_string(),
        ];
        main.extend(expand("Mountain", 57));

        let request = DeckCompatibilityRequest {
            main_deck: main,
            commander: vec!["Ob Front".to_string()],
            signature_spell: vec!["Sig Front".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Oathbreaker)),
            player_count: default_player_count(),
            ..Default::default()
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result
                .selected_format_reasons
                .iter()
                .any(|reason| reason.contains("Singleton violations")),
            "two real copies must still trip the singleton rule: {:?}",
            result.selected_format_reasons
        );
    }

    /// Regression: CR 903.5a netting belongs to command-zone formats only.
    /// Constructed has no commander concept, so a populated commander slot must
    /// not silently discount one main-deck copy and hide a real CR 100.2a
    /// violation. `CommandZoneNetting::CountVerbatim` is what keeps the two
    /// rules apart.
    #[test]
    fn constructed_copy_limit_does_not_net_out_a_commander_slot_entry() {
        let mut cards = Map::new();
        insert_planechase_card(&mut cards, "Fire", &[], &["Instant"]);
        insert_planechase_card(&mut cards, "Plains", &["Basic"], &["Land"]);
        let db = CardDatabase::from_json_str(&Value::Object(cards).to_string()).unwrap();

        let mut main = expand("Fire", 5);
        main.extend(expand("Plains", 55));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            commander: vec!["Fire".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Standard)),
            player_count: default_player_count(),
            ..Default::default()
        };

        let counts = combined_copy_counts(&db, &request, CommandZoneNetting::CountVerbatim);
        assert_eq!(
            counts.get("fire"),
            Some(&6),
            "constructed counts every listed slot verbatim: {counts:?}"
        );

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result
                .selected_format_reasons
                .iter()
                .any(|reason| reason.contains("Fire")),
            "five main-deck copies must still trip CR 100.2a: {:?}",
            result.selected_format_reasons
        );
    }

    // -----------------------------------------------------------------
    // Phase 1d: `evaluate_custom_format` / `quick_custom_format_check` /
    // `DeclaredPool` / `CardPoolAuthority`.
    // -----------------------------------------------------------------

    /// A constructed-shaped `CustomFormatRules` (no command zone, unrestricted
    /// card pool) with `deck_size`/`default_deck_copy_limit` set by the
    /// caller — the shared base every loop test below specializes.
    fn base_custom_rules(
        deck_size: DeckSizeRule,
        default_deck_copy_limit: DeckCopyLimit,
    ) -> CustomFormatRules {
        CustomFormatRules {
            id: CustomFormatId(1),
            structural: StructuralRules {
                starting_life: 20,
                min_players: 2,
                max_players: 4,
                deck_size,
                singleton: false,
                command_zone_mode: CommandZoneMode::Disabled,
                range_of_influence: None,
                team_based: false,
                sideboard_policy: SideboardPolicy::Unlimited,
                default_deck_copy_limit,
            },
            legality: LegalityRules {
                legal_sets: None,
                banned: Vec::new(),
                restricted: Vec::new(),
                legacy: LegacyRuleSet::default(),
            },
        }
    }

    fn deck_with_copies(name: &str, count: usize, total: usize) -> Vec<String> {
        let mut deck = expand(name, count);
        deck.extend(expand("Plains", total.saturating_sub(count)));
        deck
    }

    fn card_json_with_printings(name: &str, printings: &[&str]) -> Value {
        serde_json::json!({
            "name": name,
            "mana_cost": { "type": "NoCost" },
            "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
            "power": null, "toughness": null, "loyalty": null, "defense": null,
            "oracle_text": null, "non_ability_text": null, "flavor_name": null,
            "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
            "color_override": null, "scryfall_oracle_id": null, "legalities": {},
            "printings": printings
        })
    }

    fn card_json_without_printings(name: &str) -> Value {
        serde_json::json!({
            "name": name,
            "mana_cost": { "type": "NoCost" },
            "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
            "power": null, "toughness": null, "loyalty": null, "defense": null,
            "oracle_text": null, "non_ability_text": null, "flavor_name": null,
            "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
            "color_override": null, "scryfall_oracle_id": null, "legalities": {}
        })
    }

    /// A `CardDatabase` fixture carrying REAL `printings` data, unlike
    /// `test_db_json` (whose cards all omit `printings` entirely — every pool
    /// assertion against that fixture would pass vacuously even against a
    /// `printed_in_any_set` that always returned `false`). Carries:
    /// - "In Pool Card": printed in `MH3` (uppercase, as MTGJSON stores set
    ///   codes) — the POSITIVE case every negative below is paired against.
    /// - "Out Of Pool Card": printed only in `ABC`, a set no rule value below
    ///   ever declares legal.
    /// - "Delver of Secrets" / "Insectile Aberration": a two-entry DFC, each
    ///   carrying its OWN `printings: ["MH3"]` (mirroring how real card data
    ///   duplicates printings onto both faces — see `CardDatabase::
    ///   is_front_face_key`'s doc comment) — so a decklist naming the card by
    ///   its BACK face alone still resolves printings via `lookup_key`'s
    ///   exact-match step, with no front-face collapse involved.
    /// - "No Printings Card": `printings` omitted entirely, so
    ///   `from_export_entries` never populates `printings_index` for it
    ///   (`if !entry.printings.is_empty()`) — `db.printings_for` returns
    ///   `None`, the fail-closed case.
    fn custom_pool_db_json() -> String {
        let mut cards = Map::new();
        cards.insert(
            "in pool card".to_string(),
            card_json_with_printings("In Pool Card", &["MH3"]),
        );
        cards.insert(
            "out of pool card".to_string(),
            card_json_with_printings("Out Of Pool Card", &["ABC"]),
        );
        cards.insert(
            "delver of secrets".to_string(),
            card_json_with_printings("Delver of Secrets", &["MH3"]),
        );
        cards.insert(
            "insectile aberration".to_string(),
            card_json_with_printings("Insectile Aberration", &["MH3"]),
        );
        cards.insert(
            "no printings card".to_string(),
            card_json_without_printings("No Printings Card"),
        );
        Value::Object(cards).to_string()
    }

    /// Every non-negative pool assertion here is paired with a positive on the
    /// SAME `DeclaredPool` (same `rules` value), so a `printed_in_any_set`
    /// that always returned `false` would fail the positives instead of
    /// silently passing every negative.
    #[test]
    fn declared_pool_status_uses_real_printings_data() {
        let db = CardDatabase::from_json_str(&custom_pool_db_json()).unwrap();
        // F4: the RULES side declares a lowercase set code; MTGJSON-style
        // printings are uppercase ("MH3"). `eq_ignore_ascii_case`, never
        // uppercase-normalized, so this row exercises the lowercase-on-the-
        // rules-side direction.
        let rules = LegalityRules {
            legal_sets: Some(vec![SetCode("mh3".to_string())]),
            banned: Vec::new(),
            restricted: Vec::new(),
            legacy: LegacyRuleSet::default(),
        };
        let pool = DeclaredPool::resolve(&db, &rules);

        // Positive: printed in MH3, matched via the lowercase "mh3" rule.
        assert_eq!(
            pool.status(&db, "In Pool Card"),
            Some(LegalityStatus::Legal)
        );

        // Negative, paired with the positive above on the SAME rules value: a
        // card printed only in a set the rules never declare legal is
        // rejected (`None`, matching `LegalityTable`'s own "no data" `None`).
        assert_eq!(pool.status(&db, "Out Of Pool Card"), None);

        // DFC back-face name: looked up directly (its own storage key, no
        // front-face collapse via `CardDatabase::lookup_key`), and its own
        // duplicated `printings: ["MH3"]` resolves identically to the front
        // face.
        assert_eq!(
            pool.status(&db, "Insectile Aberration"),
            Some(LegalityStatus::Legal)
        );

        // Omitted printings: fails CLOSED — paired with the "In Pool Card"
        // positive above on the same rules value.
        assert_eq!(pool.status(&db, "No Printings Card"), None);
    }

    /// CR 407.3: the ante class is identified by the cards' own printed text,
    /// so this fixture gives two of them the templated clause and withholds it
    /// from the controls. All four cards are printed in sets Swedish Old
    /// School declares legal, so pool membership never confounds the verdict.
    fn ante_db_json() -> String {
        let mut cards = serde_json::Map::new();
        let ante_text = "Remove this card from your deck before playing if you're not playing for \
                         ante.";
        for (key, name, printing, oracle_text) in [
            // On the format's restricted list AND an ante card.
            (
                "contract from below",
                "Contract from Below",
                "LEA",
                Some(ante_text),
            ),
            // An ante card the format's own lists never mention.
            ("jeweled bird", "Jeweled Bird", "ARN", Some(ante_text)),
            // Restricted, not ante — the paired control that keeps the
            // restricted verdict reachable.
            ("black lotus", "Black Lotus", "LEA", None),
            // In pool, on no list, not ante.
            ("savannah lions", "Savannah Lions", "LEA", None),
        ] {
            let mut card = card_json_with_printings(name, &[printing]);
            card["oracle_text"] = match oracle_text {
                Some(text) => Value::String(text.to_string()),
                None => Value::Null,
            };
            cards.insert(key.to_string(), card);
        }
        // Basic land, printed in a Swedish-legal set, so the pipeline tests
        // below can build a real 60-card deck: `deck_with_copies`/
        // `legal_60_main` pad with Plains, and the CR 100.2a basic-land
        // exemption keeps 56 copies under the format's 4-copy ceiling.
        let mut plains = card_json_with_printings("Plains", &["LEA"]);
        plains["card_type"] = serde_json::json!({
            "supertypes": ["Basic"],
            "core_types": ["Land"],
            "subtypes": ["Plains"]
        });
        cards.insert("plains".to_string(), plains);
        Value::Object(cards).to_string()
    }

    /// `DeclaredPool` answers ONLY the format's own declared card pool. CR 407.3
    /// is not part of that — it is applied once for every format by
    /// `ante_deck_violations` — so an ante card gets whatever verdict the
    /// preset's own lists give it here, and nothing more. Pinning that keeps a
    /// future change from quietly reintroducing the rule in this authority,
    /// where the permissive formats could never see it.
    #[test]
    fn declared_pool_judges_only_the_declared_lists_not_the_ante_rule() {
        let db = CardDatabase::from_json_str(&ante_db_json()).unwrap();
        let rules = crate::types::custom_format::swedish_old_school()
            .rules
            .legality;
        let pool = DeclaredPool::resolve(&db, &rules);

        // Restricted AND an ante card: `Restricted` is the pool's honest
        // answer. The deck gate rejects it outright, one layer up.
        assert_eq!(
            pool.status(&db, "Contract from Below"),
            Some(LegalityStatus::Restricted)
        );

        // An ante card the preset's lists never mention is simply legal AS FAR
        // AS THE POOL IS CONCERNED — `validate_deck_for_format` is what keeps it
        // out of a deck.
        assert_eq!(
            pool.status(&db, "Jeweled Bird"),
            Some(LegalityStatus::Legal)
        );

        // Paired controls on the same pool value.
        assert_eq!(
            pool.status(&db, "Black Lotus"),
            Some(LegalityStatus::Restricted)
        );
        assert_eq!(
            pool.status(&db, "Savannah Lions"),
            Some(LegalityStatus::Legal)
        );
    }

    /// CR 407.3 across the routes that decide deck admission by DIFFERENT
    /// authorities: permissive formats with no card-pool check at all
    /// (FreeForAll / TwoHeadedGiant answer `true` unconditionally) and a custom
    /// format on its own `DeclaredPool`. They must agree, which is the whole
    /// reason the rule sits at the dispatch seam rather than inside one
    /// authority — the permissive routes have no authority to put it in.
    ///
    /// The `LegalityTable` route is deliberately absent: an ante card is
    /// already `NotLegal` in every sanctioned format's table, so a built-in
    /// constructed format would reject this deck with or without CR 407.3 and
    /// prove nothing. The permissive routes are where the rule is load-bearing.
    #[test]
    fn every_deck_admission_route_rejects_an_ante_card() {
        let db = CardDatabase::from_json_str(&ante_db_json()).unwrap();

        for format in [
            // The routes with no card-pool authority to hide the rule in — the
            // ones that would silently admit an ante card if CR 407.3 lived in
            // `CardPoolAuthority`.
            FormatConfig::free_for_all(),
            FormatConfig::two_headed_giant(),
            // DeclaredPool route, for contrast: a format that DOES consult a
            // card pool must reach the same verdict by the same rule.
            FormatConfig::for_custom_rules(
                &crate::types::custom_format::swedish_old_school().rules,
            ),
        ] {
            let label = format.format.label();
            let request = DeckCompatibilityRequest {
                main_deck: legal_60_main("Jeweled Bird"),
                selected_format: Some(SelectedFormat::Resolved(Box::new(format.clone()))),
                player_count: default_player_count(),
                ..Default::default()
            };
            let Err(reasons) = validate_deck_for_format(&db, &request) else {
                panic!("{label} must reject an ante card (CR 407.3)");
            };
            assert!(
                reasons.iter().any(|reason| reason.contains("Jeweled Bird")),
                "{label}: the rejection must name the ante card, got {reasons:?}"
            );

            // Same format, same deck shape, no ante card: accepted. Without
            // this the loop would pass against a validator that rejected
            // everything — and the permissive formats in particular accept
            // essentially any deck, so a spurious rejection there would
            // otherwise be invisible.
            let control = DeckCompatibilityRequest {
                main_deck: expand("Plains", 60),
                selected_format: Some(SelectedFormat::Resolved(Box::new(format))),
                player_count: default_player_count(),
                ..Default::default()
            };
            assert!(
                validate_deck_for_format(&db, &control).is_ok(),
                "{label}: the same deck without the ante card must be accepted"
            );
        }
    }

    /// CR 407.3 through the ACTUAL admission route, not the private helper:
    /// `FormatConfig::for_custom_rules` → `SelectedFormat::Resolved` →
    /// `validate_deck_for_format` → `evaluate_custom_format` →
    /// `custom_format_pool` → `DeclaredPool`. The sibling test above proves the
    /// pure function; this proves the wiring that reaches it in production, so
    /// a future refactor that stopped calling it would fail here.
    ///
    /// **Both halves of CR 407.3's boundary — "their decks or sideboards".**
    /// The sideboard reaches the check only because `construction_deck_cards`
    /// chains it; a main-deck-only regression would not notice if that stopped,
    /// and the sideboard is exactly where a player would try to hide an ante
    /// card. Uses `swedish_old_school()`'s real rules rather than a synthetic
    /// config, since the preset is what ships.
    #[test]
    fn validate_deck_for_format_rejects_ante_cards_in_deck_and_sideboard() {
        let db = CardDatabase::from_json_str(&ante_db_json()).unwrap();
        let config = FormatConfig::for_custom_rules(
            &crate::types::custom_format::swedish_old_school().rules,
        );

        let request_with = |main: Vec<String>, sideboard: Vec<String>| DeckCompatibilityRequest {
            main_deck: main,
            sideboard,
            selected_format: Some(SelectedFormat::Resolved(Box::new(config.clone()))),
            player_count: default_player_count(),
            ..Default::default()
        };

        // Paired positive control on the SAME config: a legal 60-card deck with
        // a legal sideboard is ACCEPTED. Without it, both rejections below
        // would still pass against a validator that rejected every deck for
        // some unrelated reason (deck size, pool membership, copy limit).
        assert!(
            validate_deck_for_format(&db, &request_with(expand("Plains", 60), Vec::new())).is_ok(),
            "a 60-card Swedish-legal deck must be accepted, or the rejections below prove nothing"
        );
        assert!(
            validate_deck_for_format(
                &db,
                &request_with(expand("Plains", 60), expand("Savannah Lions", 4))
            )
            .is_ok(),
            "a legal sideboard must be accepted"
        );

        // CR 407.3, main deck.
        let in_deck = validate_deck_for_format(
            &db,
            &request_with(legal_60_main("Jeweled Bird"), Vec::new()),
        )
        .expect_err("an ante card in the main deck must be rejected end-to-end");
        assert!(
            in_deck.iter().any(|reason| reason.contains("Jeweled Bird")),
            "the rejection must name the offending card, got: {in_deck:?}"
        );

        // CR 407.3, sideboard — the half a main-deck-only test would miss.
        let in_sideboard = validate_deck_for_format(
            &db,
            &request_with(expand("Plains", 60), expand("Jeweled Bird", 1)),
        )
        .expect_err("an ante card in the sideboard must be rejected end-to-end (CR 407.3)");
        assert!(
            in_sideboard
                .iter()
                .any(|reason| reason.contains("Jeweled Bird")),
            "the sideboard rejection must name the offending card, got: {in_sideboard:?}"
        );

        // An ante card that is ALSO on the restricted list is rejected
        // outright, not downgraded to "one copy is fine" — the ordering inside
        // `DeclaredPool::status`, observed from outside it. Exactly ONE copy,
        // which the restricted rule alone would permit: at four copies this
        // would be rejected for the copy limit whether or not the ante rule
        // exists, and would prove nothing about the ordering.
        let restricted_ante = validate_deck_for_format(
            &db,
            &request_with(deck_with_copies("Contract from Below", 1, 60), Vec::new()),
        )
        .expect_err(
            "one copy of a restricted ante card is legal under the restricted rule alone, so \
             rejecting it is attributable to CR 407.3",
        );
        assert!(
            restricted_ante
                .iter()
                .any(|reason| reason.contains("Contract from Below")),
            "got: {restricted_ante:?}"
        );
    }

    /// CR 407.2: playing for ante makes the class legal again.
    ///
    /// Pinned at `ante_deck_violations` rather than end-to-end, and the reason
    /// is worth recording: `AntePolicy::Enabled` cannot reach deck evaluation
    /// at all today, because `custom_format_pool`'s legacy-axis gate rejects
    /// the whole format first — declaring an ante zone the engine does not
    /// implement is refused before any card is looked at. An end-to-end
    /// assertion here would therefore be testing the gate, not this rule. When
    /// `LegacyAxis::Ante` joins `IMPLEMENTED_LEGACY_AXES`, the deck half is
    /// already correct and this test is what says so.
    #[test]
    fn ante_deck_violations_admit_the_class_when_playing_for_ante() {
        let db = CardDatabase::from_json_str(&ante_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Jeweled Bird"),
            sideboard: expand("Jeweled Bird", 1),
            ..Default::default()
        };

        let mut for_ante = crate::types::custom_format::swedish_old_school().rules;
        for_ante.legality.legacy.ante = AntePolicy::Enabled;
        assert!(
            ante_deck_violations(
                &db,
                &request,
                &BTreeSet::new(),
                &FormatConfig::for_custom_rules(&for_ante),
            )
            .is_empty(),
            "CR 407.2: a format played for ante admits the class"
        );

        // Paired control on the SAME request: the default policy flags exactly
        // that card, so the emptiness above is the policy taking effect and not
        // a request the scan never looked at.
        let excluded = FormatConfig::for_custom_rules(
            &crate::types::custom_format::swedish_old_school().rules,
        );
        assert!(
            ante_deck_violations(&db, &request, &BTreeSet::new(), &excluded)
                .contains("Jeweled Bird"),
            "the same deck under the default Excluded policy must flag the ante card"
        );
    }

    /// CR 201.3b: a banned/restricted entry naming a split/DFC's whole-card
    /// identity ("Fire // Ice") must match a decklist naming just one face
    /// ("Fire"), and banned beats restricted when a name (implausibly)
    /// appears on both lists.
    #[test]
    fn declared_pool_banned_beats_restricted_and_matches_canonical_names() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let rules = LegalityRules {
            legal_sets: None,
            banned: vec!["Legal Standard".to_string()],
            restricted: vec!["Legal Standard".to_string(), "Not Standard".to_string()],
            legacy: LegacyRuleSet::default(),
        };
        let pool = DeclaredPool::resolve(&db, &rules);

        // Banned beats restricted for a name on both lists.
        assert_eq!(
            pool.status(&db, "Legal Standard"),
            Some(LegalityStatus::Banned)
        );
        // Restricted-only name.
        assert_eq!(
            pool.status(&db, "Not Standard"),
            Some(LegalityStatus::Restricted)
        );
        // Positive, paired with the two negatives above on the same rules
        // value: a name on neither list is Legal.
        assert_eq!(pool.status(&db, "Plains"), Some(LegalityStatus::Legal));
    }

    // -----------------------------------------------------------------
    // `max_deck_copies`'s `Custom(_)` restricted-list branch. This is the
    // deck-BUILDER query authority (`pub fn`), distinct from the
    // deck-VALIDATION authority above (`DeclaredPool`/`evaluate_custom_format`)
    // — both must read the same declared restricted list, but neither calls
    // the other, so each needs its own coverage or the two can silently
    // diverge (exactly the class of divergence this function's own doc
    // comment says can never happen).
    // -----------------------------------------------------------------

    #[test]
    fn max_deck_copies_custom_restricted_list_paired_with_positive() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut rules = base_custom_rules(DeckSizeRule::Minimum(60), DeckCopyLimit::UpTo(4));
        rules.legality.restricted = vec!["Legal Standard".to_string()];
        let config = FormatConfig::for_custom_rules(&rules);

        // Restricted card under a Custom config: capped at 1, overriding the
        // format's own more permissive `default_deck_copy_limit` (UpTo(4)).
        assert_eq!(
            max_deck_copies(&db, "Legal Standard", &config),
            DeckCopyLimit::UpTo(1)
        );
        // Paired positive on the SAME config: a name on the restricted list
        // reads the declared default straight through. A stubbed-out
        // restricted check that always returned `UpTo(1)` would fail this
        // assertion instead of silently passing the one above.
        assert_eq!(
            max_deck_copies(&db, "Not Standard", &config),
            DeckCopyLimit::UpTo(4)
        );
    }

    #[test]
    fn max_deck_copies_custom_restricted_list_matches_canonical_names() {
        // CR 201.3b / CR 709.2: a restricted entry spelled as a split card's
        // front-face name ("Fire") must match a decklist naming the full
        // composite name ("Fire // Ice") — proving this reads through
        // `canonical_deck_count_key` (which resolves the composite name to
        // its front face via `CardDatabase::lookup_key`'s `//`-split), not
        // raw string equality between the two spellings.
        let mut cards = Map::new();
        cards.insert("fire".to_string(), card_json_without_printings("Fire"));
        let db = CardDatabase::from_json_str(&Value::Object(cards).to_string()).unwrap();

        let mut rules = base_custom_rules(DeckSizeRule::Minimum(60), DeckCopyLimit::UpTo(4));
        rules.legality.restricted = vec!["Fire".to_string()];
        let config = FormatConfig::for_custom_rules(&rules);

        assert_eq!(
            max_deck_copies(&db, "Fire // Ice", &config),
            DeckCopyLimit::UpTo(1),
            "a restricted entry spelled as the front face must match a decklist entry spelled \
             as the composite split-card name"
        );
    }

    #[test]
    fn max_deck_copies_custom_missing_rules_fails_closed_to_up_to_one() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // Defense-in-depth, same shape as `custom_format_pool`'s own
        // `custom_rules: None` gate: should never occur in practice
        // (`validate_custom_rules_consistency` forbids it at `Deserialize`),
        // but a trusted, non-deserialized `FormatConfig` can still construct
        // it directly.
        let config = FormatConfig {
            custom_rules: None,
            ..FormatConfig::for_custom_rules(&base_custom_rules(
                DeckSizeRule::Minimum(60),
                DeckCopyLimit::Unlimited,
            ))
        };
        assert_eq!(
            max_deck_copies(&db, "Legal Standard", &config),
            DeckCopyLimit::UpTo(1),
            "custom_rules: None must fail CLOSED to UpTo(1), never silently apply no cap"
        );
    }

    #[test]
    fn max_deck_copies_built_in_arm_is_unchanged_by_the_discriminant_dispatch() {
        // Regression guard for the discriminant-dispatch fix (F1): if the
        // match were ever flipped to dispatch on `custom_rules.is_some()`
        // instead of `format_config.format`, a built-in format's OWN
        // restricted list — read from the `LegalityFormat` table, since
        // built-ins carry no `custom_rules` at all — would silently stop
        // being consulted, because `custom_rules.is_some()` is `false` for
        // every built-in format.
        let db = CardDatabase::from_json_str(&vintage_test_db()).unwrap();
        assert_eq!(
            max_deck_copies(&db, "Black Lotus", &FormatConfig::vintage()),
            DeckCopyLimit::UpTo(1)
        );
    }

    #[test]
    fn custom_format_copy_limit_accepts_at_n_rejects_at_n_plus_one() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        for limit in [
            DeckCopyLimit::UpTo(1),
            DeckCopyLimit::UpTo(2),
            DeckCopyLimit::UpTo(4),
            DeckCopyLimit::Unlimited,
        ] {
            let rules = base_custom_rules(DeckSizeRule::Minimum(60), limit);
            let config = FormatConfig::for_custom_rules(&rules);

            let accept_n = match limit {
                DeckCopyLimit::UpTo(n) => n as usize,
                DeckCopyLimit::Unlimited => 60,
            };
            let accept_request = DeckCompatibilityRequest {
                main_deck: deck_with_copies("Legal Standard", accept_n, 60),
                selected_format: Some(SelectedFormat::Resolved(Box::new(config.clone()))),
                player_count: default_player_count(),
                ..Default::default()
            };
            assert!(
                validate_deck_for_format(&db, &accept_request).is_ok(),
                "{limit:?} must accept {accept_n} copies of one name"
            );

            if let DeckCopyLimit::UpTo(n) = limit {
                let reject_request = DeckCompatibilityRequest {
                    main_deck: deck_with_copies("Legal Standard", n as usize + 1, 60),
                    selected_format: Some(SelectedFormat::Resolved(Box::new(config))),
                    player_count: default_player_count(),
                    ..Default::default()
                };
                assert!(
                    validate_deck_for_format(&db, &reject_request).is_err(),
                    "{limit:?} must reject {} copies of one name",
                    n + 1
                );
            }
        }
    }

    #[test]
    fn custom_format_deck_size_accepts_at_floor_rejects_below_it() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        for deck_size in [
            DeckSizeRule::Minimum(40),
            DeckSizeRule::Exactly(60),
            DeckSizeRule::Minimum(100),
        ] {
            let rules = base_custom_rules(deck_size, DeckCopyLimit::Unlimited);
            let config = FormatConfig::for_custom_rules(&rules);
            let n = deck_size.min_cards() as usize;

            let accept_request = DeckCompatibilityRequest {
                main_deck: expand("Plains", n),
                selected_format: Some(SelectedFormat::Resolved(Box::new(config.clone()))),
                player_count: default_player_count(),
                ..Default::default()
            };
            assert!(
                validate_deck_for_format(&db, &accept_request).is_ok(),
                "{deck_size:?} must accept exactly {n} cards"
            );

            let reject_request = DeckCompatibilityRequest {
                main_deck: expand("Plains", n - 1),
                selected_format: Some(SelectedFormat::Resolved(Box::new(config.clone()))),
                player_count: default_player_count(),
                ..Default::default()
            };
            assert!(
                validate_deck_for_format(&db, &reject_request).is_err(),
                "{deck_size:?} must reject {} cards",
                n - 1
            );

            if let DeckSizeRule::Exactly(_) = deck_size {
                let over_request = DeckCompatibilityRequest {
                    main_deck: expand("Plains", n + 1),
                    selected_format: Some(SelectedFormat::Resolved(Box::new(config))),
                    player_count: default_player_count(),
                    ..Default::default()
                };
                assert!(
                    validate_deck_for_format(&db, &over_request).is_err(),
                    "Exactly({n}) must reject {} cards",
                    n + 1
                );
            }
        }
    }

    #[test]
    fn custom_format_rejects_every_undeclared_legacy_axis() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // Loops over every axis NOT in `IMPLEMENTED_LEGACY_AXES` — currently
        // all five, since that list is empty; a future phase populating it
        // narrows this loop automatically (each entry is still exercised
        // above by `passes_legacy_axis_gate`'s own direct assertion, so a
        // freshly-implemented axis fails loudly here instead of silently
        // dropping out).
        let non_default_rulesets = [
            LegacyRuleSet {
                mana_burn: ManaBurnPolicy::Obsolete,
                ..LegacyRuleSet::default()
            },
            LegacyRuleSet {
                damage_timing: CombatDamageTiming::OnStack,
                ..LegacyRuleSet::default()
            },
            LegacyRuleSet {
                wish_scope: WishOutsideGameScope::PreM10ReachesExile,
                ..LegacyRuleSet::default()
            },
            LegacyRuleSet {
                legend_rule_scope: LegendRuleScope::PreM14AnyController,
                ..LegacyRuleSet::default()
            },
            // CR 407.2/407.4: only `Enabled` is a declared axis — it promises
            // an ante zone and the ante action. The default `Excluded` is
            // enforced (CR 407.3) and so is deliberately NOT gated, which is
            // why it does not appear in this list.
            LegacyRuleSet {
                ante: AntePolicy::Enabled,
                ..LegacyRuleSet::default()
            },
        ];
        for legacy in non_default_rulesets {
            assert!(
                !passes_legacy_axis_gate(&legacy),
                "IMPLEMENTED_LEGACY_AXES is empty, so every non-default axis must fail the \
                 gate: {legacy:?}"
            );
            let mut rules = base_custom_rules(DeckSizeRule::Minimum(60), DeckCopyLimit::Unlimited);
            rules.legality.legacy = legacy;
            // `for_custom_rules` is total and applies no gate of its own, so
            // this exercises `evaluate_custom_format`'s OWN defense-in-depth
            // check, not `FormatConfig::deserialize`'s.
            let config = FormatConfig::for_custom_rules(&rules);
            let request = DeckCompatibilityRequest {
                main_deck: expand("Plains", 60),
                selected_format: Some(SelectedFormat::Resolved(Box::new(config))),
                player_count: default_player_count(),
                ..Default::default()
            };
            assert_eq!(
                validate_deck_for_format(&db, &request),
                Err(vec![CUSTOM_FORMAT_UNIMPLEMENTED_LEGACY_AXIS.to_string()])
            );
        }
    }

    /// Only [`CUSTOM_FORMAT_UNRESOLVED`] is downgraded to "no opinion" by
    /// `evaluate_deck_compatibility`; the other three Custom sentinels are
    /// definite verdicts about a format whose rules the engine DOES hold, and
    /// must surface as real `Some(false)` rejections.
    #[test]
    fn only_the_unresolved_sentinel_is_downgraded_to_no_opinion() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();

        let unresolved_request = DeckCompatibilityRequest {
            selected_format: Some(SelectedFormat::Tag(GameFormat::Custom(CustomFormatId(1)))),
            ..Default::default()
        };
        let hint = evaluate_deck_compatibility(&db, &unresolved_request);
        assert_eq!(hint.selected_format_compatible, None);
        assert!(hint.selected_format_reasons.is_empty());

        let mut command_zone_rules =
            base_custom_rules(DeckSizeRule::Exactly(100), DeckCopyLimit::UpTo(1));
        command_zone_rules.structural.command_zone_mode = CommandZoneMode::Enabled {
            commander_damage_threshold: Some(21),
            eligibility_rule: CommanderEligibilityRule::Standard,
        };
        let command_zone_config = FormatConfig::for_custom_rules(&command_zone_rules);
        let command_zone_request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 100),
            selected_format: Some(SelectedFormat::Resolved(Box::new(command_zone_config))),
            player_count: default_player_count(),
            ..Default::default()
        };
        let result = evaluate_deck_compatibility(&db, &command_zone_request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert_eq!(
            result.selected_format_reasons,
            vec![CUSTOM_FORMAT_COMMAND_ZONE_UNSUPPORTED.to_string()]
        );
        // The summary dispatch must agree with the full one.
        let mut summary_request = command_zone_request;
        summary_request.summary_only = true;
        let summary_result = evaluate_deck_compatibility(&db, &summary_request);
        assert_eq!(summary_result.selected_format_compatible, Some(false));
        assert_eq!(
            summary_result.selected_format_reasons,
            vec![CUSTOM_FORMAT_COMMAND_ZONE_UNSUPPORTED.to_string()]
        );

        let mut legacy_rules =
            base_custom_rules(DeckSizeRule::Minimum(60), DeckCopyLimit::Unlimited);
        legacy_rules.legality.legacy.mana_burn = ManaBurnPolicy::Obsolete;
        let legacy_config = FormatConfig::for_custom_rules(&legacy_rules);
        let legacy_request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 60),
            selected_format: Some(SelectedFormat::Resolved(Box::new(legacy_config))),
            player_count: default_player_count(),
            ..Default::default()
        };
        let result = evaluate_deck_compatibility(&db, &legacy_request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert_eq!(
            result.selected_format_reasons,
            vec![CUSTOM_FORMAT_UNIMPLEMENTED_LEGACY_AXIS.to_string()]
        );

        // Defense-in-depth: `custom_rules: None` should never occur in
        // practice (`validate_custom_rules_consistency` forbids it at
        // `Deserialize`), but a trusted, non-deserialized `FormatConfig` can
        // still construct it directly, as this row does.
        let missing_config = FormatConfig {
            custom_rules: None,
            ..FormatConfig::for_custom_rules(&base_custom_rules(
                DeckSizeRule::Minimum(60),
                DeckCopyLimit::Unlimited,
            ))
        };
        let missing_request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 60),
            selected_format: Some(SelectedFormat::Resolved(Box::new(missing_config))),
            player_count: default_player_count(),
            ..Default::default()
        };
        let result = evaluate_deck_compatibility(&db, &missing_request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert_eq!(
            result.selected_format_reasons,
            vec![CUSTOM_FORMAT_MISSING_RULES.to_string()]
        );
    }

    /// Third entry point for the bare-Tag rejection, alongside
    /// `validate_deck_for_format_rejects_custom_independently_of_the_ui_hint`
    /// and `evaluate_deck_format_gate_rejects_custom_even_for_an_otherwise_legal_deck`
    /// (which together cover `validate_deck_for_format`, `evaluate_deck_format_gate`,
    /// and the full `evaluate_deck_compatibility` dispatch) — this one is the
    /// `summary_only` dispatch. Uses `fully_legal_standard_request` so the
    /// rejection is demonstrably about the format, not a degenerate deck.
    #[test]
    fn evaluate_deck_compatibility_summary_reports_no_opinion_for_an_otherwise_legal_custom_deck() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut request = fully_legal_standard_request(GameFormat::Custom(CustomFormatId(1)));
        request.summary_only = true;
        let hint = evaluate_deck_compatibility(&db, &request);
        assert_eq!(hint.selected_format_compatible, None);
        assert!(hint.selected_format_reasons.is_empty());
    }

    /// `reprint_policy` / `printing_fidelity` are `CustomFormatDef`-only
    /// display/registry metadata — `evaluate_custom_format` only ever sees the
    /// resolved `FormatConfig` (via `FormatConfig::for_custom_rules`), which
    /// has no field for either. Two defs built from the SAME rules but
    /// different metadata resolve to byte-identical configs.
    #[test]
    fn reprint_policy_has_no_effect_on_custom_format_evaluation() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let rules = base_custom_rules(DeckSizeRule::Minimum(60), DeckCopyLimit::UpTo(4));
        let def_a = CustomFormatDef {
            rules: rules.clone(),
            label: "A".to_string(),
            short_label: "A".to_string(),
            description: "A".to_string(),
            reprint_policy: None,
            printing_fidelity: PrintingFidelity::NotApplicable,
        };
        let def_b = CustomFormatDef {
            rules,
            label: "B".to_string(),
            short_label: "B".to_string(),
            description: "B".to_string(),
            reprint_policy: Some(ReprintPolicy::AllowAnyPrinting),
            printing_fidelity: PrintingFidelity::SetCodeApproximation,
        };
        assert_eq!(
            FormatConfig::for_custom_rules(&def_a.rules),
            FormatConfig::for_custom_rules(&def_b.rules),
            "reprint_policy/printing_fidelity are CustomFormatDef-only metadata and must not \
             change the resolved FormatConfig the evaluator reads"
        );

        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 60),
            selected_format: Some(SelectedFormat::Resolved(Box::new(
                FormatConfig::for_custom_rules(&def_a.rules),
            ))),
            player_count: default_player_count(),
            ..Default::default()
        };
        assert!(validate_deck_for_format(&db, &request).is_ok());
    }

    /// Reach-guard: the `evaluate_constructed`/`quick_constructed_check`
    /// widening to `format_rules.deck_size.accepts(..)` must not change
    /// behavior for the built-in constructed formats that were already
    /// `Minimum(60)` before this phase.
    #[test]
    fn constructed_formats_still_reject_59_and_accept_60() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        for format in [GameFormat::Standard, GameFormat::Modern, GameFormat::Pauper] {
            let mut short_deck = legal_60_main("Legal Standard");
            short_deck.pop();
            let short_request = DeckCompatibilityRequest {
                main_deck: short_deck,
                selected_format: Some(SelectedFormat::Tag(format)),
                player_count: default_player_count(),
                ..Default::default()
            };
            let result = evaluate_deck_compatibility(&db, &short_request);
            assert_eq!(
                result.selected_format_compatible,
                Some(false),
                "{format:?} must reject 59 cards"
            );
            assert!(
                result
                    .selected_format_reasons
                    .iter()
                    .any(|r| r.contains("at least 60") && r.contains("found 59")),
                "{format:?} reasons: {:?}",
                result.selected_format_reasons
            );

            let full_request = DeckCompatibilityRequest {
                main_deck: legal_60_main("Legal Standard"),
                selected_format: Some(SelectedFormat::Tag(format)),
                player_count: default_player_count(),
                ..Default::default()
            };
            let result = evaluate_deck_compatibility(&db, &full_request);
            assert_eq!(
                result.selected_format_compatible,
                Some(true),
                "{format:?} must accept 60 cards, reasons: {:?}",
                result.selected_format_reasons
            );
        }
    }
}
