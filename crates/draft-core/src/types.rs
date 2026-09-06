use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::validation::{LimitedDeckError, STANDARD_BASIC_LANDS};
use engine::types::card::DraftEffect;
use engine::types::match_config::{MatchConfig, MatchType};
use engine::types::player::PlayerId;

/// Tournament pairing format for the draft event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TournamentFormat {
    /// Swiss: 3 rounds, pair within win-bracket, all players play every round.
    #[default]
    Swiss,
    /// Single-elimination: 3 rounds (8-player bracket), losers eliminated.
    SingleElimination,
}

/// Controls timer, disconnect handling, and round-advancement behavior.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PodPolicy {
    /// Timed picks, auto-pick on timeout, 10s disconnect grace period, auto-advance rounds.
    #[default]
    Competitive,
    /// No timer, no auto-pick, host controls round advancement, host notified on disconnect.
    Casual,
}

/// Controls what spectators can see during a draft. Defaults to Public.
/// Competitive pods MUST use Public. Casual pods allow host to set Omniscient at creation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpectatorVisibility {
    /// Battlefield, standings, pairings visible. Pools and packs hidden.
    #[default]
    Public,
    /// All pools and current packs visible. Host must explicitly enable for Casual pods.
    /// Chaos sources still redact them because an ordinary spectator socket is
    /// not an authenticated host export.
    Omniscient,
}

/// Per-seat pick status during the draft phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PickStatus {
    /// Seat has a pack and hasn't picked yet.
    Pending,
    /// Seat has picked and pack has passed.
    Picked,
    /// Seat timed out (set by P2P host, not derivable from session state).
    TimedOut,
    /// Not in drafting phase (deckbuilding, match play, etc.).
    NotDrafting,
}

/// The kind of draft event, modeled after Arena's three draft modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DraftKind {
    /// Quick Draft: 1 human + 7 bots, Bo1 matches.
    Quick,
    /// Premier Draft: 8 humans, Bo1 matches.
    Premier,
    /// Traditional Draft: 8 humans, Bo3 matches.
    Traditional,
    /// Sealed: each player receives six unopened packs directly, Bo1 matches.
    Sealed,
    /// Commander Draft (CR 903.13a): a 4-seat pod drafts three Commander
    /// Legends-style packs two cards at a time, then plays one multiplayer
    /// Commander game. 1 human + 3 bots by default.
    CommanderDraft,
}

/// How a draft kind's packs reach the seats.
///
/// This is the axis the `kind == DraftKind::Sealed` equality tests were really
/// testing. Consumers match on it exhaustively, so a new kind must *declare*
/// which shape it uses instead of silently falling into an `else` branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PackDistribution {
    /// Packs are opened one at a time and passed around the pod.
    /// CR 905.1a describes this shape (one card per step, pass the remainder).
    PickAndPass,
    /// Every pack is handed to its own seat unopened; there is no pick step.
    AllAtOnce,
}

/// What happens to the draft session once every seat has submitted a deck.
///
/// The axis behind three compiler-invisible kind-identity predicates: two
/// `matches!(kind, Premier | Traditional | Sealed)` whitelists in the reducer
/// and one `kind != DraftKind::Quick` blacklist at the `CreateDraft` wire.
/// Those spellings agreed on the four kinds that existed when they were
/// written and disagree on any fifth, so the axis is named here instead.
/// Not persisted: `DraftProcedure` is computed from `kind`, never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PostDraftPlay {
    /// The draft session ends at `DraftStatus::Complete`; play is arranged
    /// outside it. CR 903.13a: Commander Draft is "a draft ... followed by a
    /// multiplayer game" — not an in-session Swiss/single-elimination bracket.
    CompleteImmediately,
    /// Swiss / single-elimination pairings run inside the draft session.
    /// Tournament structure is MTR policy, not Comprehensive Rules — there is
    /// deliberately no CR citation on this variant.
    TournamentPairings,
}

/// What game, if any, a completed draft procedure authorizes the host to launch.
///
/// This is deliberately a typed capability instead of a `DraftKind` check at a
/// display boundary. `PostDraftPlay::CompleteImmediately` alone is too broad:
/// Quick Draft also completes immediately but has no multiplayer pod game. The
/// procedure's commander-designation axis distinguishes the multiplayer
/// Commander launch without teaching UI code which kind happens to own it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DraftLaunchCapability {
    None,
    CommanderMultiplayer,
}

/// How a player chooses cards for one draft pick step.
///
/// This describes selection interaction, not the number of cards currently
/// required. Commander Draft remains ordered on its one-card final step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PickSelectionMode {
    /// Selecting a card replaces the current selection immediately.
    Direct,
    /// Selecting cards preserves their order and rolls the oldest selection out.
    Ordered,
}

/// The per-kind draft procedure: the single authority for every axis that
/// previously leaked to call sites as a literal.
///
/// Every field below replaces at least one live literal measured in the tree.
/// `cards_per_pick` is the CR 903.13b axis ("drafts two cards"), and it has two
/// consumers: `pick_pass::required_pick_count` reads it per seat to size one
/// pick step, and [`DraftProcedure::pick_steps_per_pack`] reads it to count how
/// many such steps a pack contains. [`MAX_CARDS_PER_PICK`] is derived from it by
/// `max_cards_per_pick_matches_procedure_table`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftProcedure {
    /// Seats at the table. Was: `default_pod_size()`'s unconditional `8`.
    pub pod_size: u8,
    /// Seats occupied by humans; the remainder are bots. Was: `human_seats()`.
    pub human_seats: u8,
    /// Smallest pod a client may request for this kind.
    pub min_pod_size: u8,
    /// Smallest local cube pod for this kind. This is not a remote contract.
    pub local_cube_min_pod_size: u8,
    /// Largest pod a client may request for this kind.
    pub max_pod_size: u8,
    /// Largest pod a local cube event may create for this kind. This is not
    /// exported to remote hosts or accepted by public/server preflight.
    pub local_cube_max_pod_size: u8,
    /// Packs each seat consumes over the whole event.
    pub packs_per_player: u8,
    /// Cards taken per pick step. The per-kind value is this table's, never a
    /// literal at a call site: CR 905.1a drafts "one card" per step, and
    /// CR 903.13b drafts "two cards" per step for Commander Draft. Only
    /// meaningful under [`PackDistribution::PickAndPass`]; fixed at `1` under
    /// [`PackDistribution::AllAtOnce`], which has no pick step at all.
    /// [`MAX_CARDS_PER_PICK`] is derived from this axis by
    /// `max_cards_per_pick_matches_procedure_table`.
    pub cards_per_pick: u8,
    /// How the client selects cards for this procedure's pick steps.
    pub pick_selection_mode: PickSelectionMode,
    /// How packs reach the seats.
    pub distribution: PackDistribution,
    /// CR 100.2b: limited decks have a 40-card minimum deck size.
    pub min_deck_size: usize,
    /// Smallest deck size a cube request may select for this procedure.
    /// Ordinary cube drafts permit any positive size. Commander Draft keeps
    /// the CR 903.13f(1) minimum of 60 cards.
    pub cube_min_deck_size: usize,
    /// CR 903.3: how many commanders each deck built from this kind's pool must
    /// designate. `0` for the four CR 905.1a kinds; `1` for CommanderDraft, whose
    /// decks are Commander decks (CR 903.13f routes deck construction through
    /// CR 903.5). Not a bool: CR 903.13f(3) + CR 702.124 admit a second commander,
    /// so the count is the axis, not the presence.
    pub commanders_required: u8,
    /// What the session does once every seat has submitted a deck: end at
    /// `Complete`, or run in-session tournament pairings.
    pub post_draft_play: PostDraftPlay,
    /// Match configuration for this draft kind. Was: `match_config()`.
    pub match_config: MatchConfig,
}

impl DraftProcedure {
    /// The engine-authorized game launch for a completed draft procedure.
    ///
    /// This joins two procedure axes rather than exposing a `DraftKind` check
    /// to a transport or display consumer. A procedure that completes
    /// immediately but does not designate commanders is a local draft, not a
    /// multiplayer pod game.
    pub fn launch_capability(self) -> DraftLaunchCapability {
        match (self.post_draft_play, self.commanders_required) {
            (PostDraftPlay::CompleteImmediately, 1..) => {
                DraftLaunchCapability::CommanderMultiplayer
            }
            (PostDraftPlay::CompleteImmediately | PostDraftPlay::TournamentPairings, 0) => {
                DraftLaunchCapability::None
            }
            (PostDraftPlay::TournamentPairings, 1..) => DraftLaunchCapability::None,
        }
    }

    /// The engine-owned allowed seat range for this procedure and tournament
    /// shape. Tournament pairings require a full bracket for single
    /// elimination; procedures that complete immediately retain their normal
    /// range even if the host selected that presentation value.
    pub fn allowed_pod_size_range(
        self,
        tournament_format: TournamentFormat,
    ) -> std::ops::RangeInclusive<u8> {
        if self.post_draft_play == PostDraftPlay::TournamentPairings
            && tournament_format == TournamentFormat::SingleElimination
        {
            self.max_pod_size..=self.max_pod_size
        } else {
            self.min_pod_size..=self.max_pod_size
        }
    }

    /// The complete engine-owned selectable seat set for this procedure and
    /// tournament format. The reducer validates the same range; this only
    /// transports that authority to the display layer.
    pub fn allowed_pod_sizes(self, tournament_format: TournamentFormat) -> Vec<u8> {
        self.allowed_pod_size_range(tournament_format).collect()
    }

    /// Whether `pod_size` is legal for this complete engine procedure.
    pub fn allows_pod_size(self, tournament_format: TournamentFormat, pod_size: u8) -> bool {
        self.allowed_pod_size_range(tournament_format)
            .contains(&pod_size)
    }

    /// Local cube events retain their procedure-owned local ceiling without
    /// expanding the public/remote pod-size contract.
    pub fn allows_local_cube_pod_size(
        self,
        tournament_format: TournamentFormat,
        pod_size: u8,
    ) -> bool {
        if self.post_draft_play == PostDraftPlay::TournamentPairings
            && tournament_format == TournamentFormat::SingleElimination
        {
            pod_size == self.max_pod_size
        } else {
            (self.local_cube_min_pod_size..=self.local_cube_max_pod_size).contains(&pod_size)
        }
    }

    /// Applies this procedure's engine-owned cube floor to a requested size.
    pub fn effective_cube_min_deck_size(self, requested: usize) -> usize {
        requested.max(self.cube_min_deck_size)
    }

    /// CR 903.13b: how many pick steps a pack of `cards_per_pack` contains for
    /// this kind. `pick_number` counts STEPS, not cards, so this is the
    /// denominator a progress display can actually reach. Rounds up: an odd
    /// pack's final step takes the remainder, which is the same boundary
    /// `pick_pass::required_pick_count` reports per step.
    ///
    /// `cards_per_pack` is a parameter rather than a field because it is a
    /// [`DraftConfig`] value while `cards_per_pick` is a procedure axis — the
    /// method joins the two without either owning the other.
    pub fn pick_steps_per_pack(self, cards_per_pack: u8) -> u8 {
        cards_per_pack.div_ceil(self.cards_per_pick)
    }
}

/// The largest `DraftProcedure::cards_per_pick` over every `DraftKind`.
///
/// The session-independent half of the `DraftAction::Pick` payload bound in
/// `server-core`'s `guard_draft_action_payload`, which receives only the action
/// and can never consult the session (so it cannot check the exact per-kind
/// count — `apply_pick_inner` owns that). Derived from the procedure table, not
/// chosen: `max_cards_per_pick_matches_procedure_table` folds over
/// [`DraftKind::ALL`] and fails if this drifts.
pub const MAX_CARDS_PER_PICK: usize = 2; // CR 903.13b, the CommanderDraft row

/// CR 702.124g: "no partner ability or combination of partner abilities can
/// ever let a player have more than two commanders."
///
/// The session-independent bound on `DraftAction::SubmitDeck.commanders`, which
/// `server-core`'s `guard_draft_action_payload` can check without consulting a
/// session -- exactly the role [`MAX_CARDS_PER_PICK`] plays for
/// `DraftAction::Pick`. It is NOT the lobby transport's
/// `MAX_COMMANDER_ENTRIES`, which is a different (larger) bound on a different
/// list; the two coexist with different values on purpose.
pub const MAX_COMMANDER_DESIGNATIONS: usize = 2;

impl DraftKind {
    /// Every `DraftKind`, in declaration order.
    ///
    /// Folded over by `max_cards_per_pick_matches_procedure_table` (to derive
    /// [`MAX_CARDS_PER_PICK`]) and by `procedure_matches_legacy_accessors`.
    ///
    /// Hand-written, and the guarantees are worth stating precisely because
    /// they are narrower than "compiler-enforced": the wildcard-free `match` in
    /// `draft_kind_all_lists_every_variant` makes a sixth variant an `E0004`
    /// **there**, which enforces the *arm set*; the array type `[DraftKind; 5]`
    /// enforces the *length*; and the sorted-index assertion catches
    /// *duplication*. A future variant's **membership in this array** is
    /// enforced by nothing — a sixth variant that adds its `index_of` arm but
    /// is left out of `ALL` compiles and passes. The `E0004` lands the author
    /// beside this array, and that proximity is the actual guarantee.
    pub const ALL: [DraftKind; 5] = [
        DraftKind::Quick,
        DraftKind::Premier,
        DraftKind::Traditional,
        DraftKind::Sealed,
        DraftKind::CommanderDraft,
    ];

    /// The single authority for this kind's procedure.
    ///
    /// One exhaustive `match` with no wildcard and no `..Default::default()`
    /// spread: adding a variant is an `E0004` here, and the author must state
    /// a value for every axis rather than inheriting one silently.
    pub fn procedure(self) -> DraftProcedure {
        match self {
            DraftKind::Quick => DraftProcedure {
                pod_size: 8,
                human_seats: 1,
                min_pod_size: 2,
                local_cube_min_pod_size: 1,
                max_pod_size: 8,
                // Quick Draft also backs the local cube entry point, which
                // intentionally supports large bot-filled pods.
                local_cube_max_pod_size: u8::MAX,
                packs_per_player: 3,
                cards_per_pick: 1,
                pick_selection_mode: PickSelectionMode::Direct,
                distribution: PackDistribution::PickAndPass,
                min_deck_size: 40,
                cube_min_deck_size: 1,
                commanders_required: 0,
                // A local single-player event: the session ends when the deck
                // is submitted and the client starts a game from it. No CR —
                // a local event is not a Comprehensive Rules concept.
                post_draft_play: PostDraftPlay::CompleteImmediately,
                match_config: MatchConfig {
                    match_type: MatchType::Bo1,
                    ..MatchConfig::default()
                },
            },
            DraftKind::Premier => DraftProcedure {
                pod_size: 8,
                human_seats: 8,
                min_pod_size: 2,
                local_cube_min_pod_size: 2,
                max_pod_size: 8,
                local_cube_max_pod_size: 8,
                packs_per_player: 3,
                cards_per_pick: 1,
                pick_selection_mode: PickSelectionMode::Direct,
                distribution: PackDistribution::PickAndPass,
                min_deck_size: 40,
                cube_min_deck_size: 1,
                commanders_required: 0,
                post_draft_play: PostDraftPlay::TournamentPairings,
                match_config: MatchConfig {
                    match_type: MatchType::Bo1,
                    ..MatchConfig::default()
                },
            },
            DraftKind::Traditional => DraftProcedure {
                pod_size: 8,
                human_seats: 8,
                min_pod_size: 2,
                local_cube_min_pod_size: 2,
                max_pod_size: 8,
                local_cube_max_pod_size: 8,
                packs_per_player: 3,
                cards_per_pick: 1,
                pick_selection_mode: PickSelectionMode::Direct,
                distribution: PackDistribution::PickAndPass,
                min_deck_size: 40,
                cube_min_deck_size: 1,
                commanders_required: 0,
                post_draft_play: PostDraftPlay::TournamentPairings,
                match_config: MatchConfig {
                    match_type: MatchType::Bo3,
                    ..MatchConfig::default()
                },
            },
            DraftKind::Sealed => DraftProcedure {
                pod_size: 8,
                human_seats: 8,
                min_pod_size: 2,
                local_cube_min_pod_size: 2,
                max_pod_size: 8,
                local_cube_max_pod_size: 8,
                packs_per_player: 6,
                cards_per_pick: 1,
                pick_selection_mode: PickSelectionMode::Direct,
                distribution: PackDistribution::AllAtOnce,
                min_deck_size: 40,
                cube_min_deck_size: 1,
                commanders_required: 0,
                post_draft_play: PostDraftPlay::TournamentPairings,
                match_config: MatchConfig {
                    match_type: MatchType::Bo1,
                    ..MatchConfig::default()
                },
            },
            // CR 903.13a: "a draft ... followed by a multiplayer game." WotC's
            // Commander Limited product page gives the 4-player pod as the
            // format's default increment; CR 903.13 does not fix a pod size, so
            // 4 is a product default, not an invariant. 1 human + 3 bots
            // mirrors DraftKind::Quick's bot-filled shape and likewise carries
            // no CR.
            DraftKind::CommanderDraft => DraftProcedure {
                pod_size: 4,
                human_seats: 1,
                // CR 903.13a + CR 800.1: Commander Draft is "a draft ...
                // followed by a multiplayer game", and "a multiplayer game is a
                // game that begins with more than two players" — so three seats
                // is the smallest pod that can still deliver the game the
                // format is defined as. This field is the floor below which a
                // client's requested pod is rejected, not the table default:
                // the 4-player pod is `pod_size` above.
                min_pod_size: 3,
                local_cube_min_pod_size: 3,
                max_pod_size: 8,
                local_cube_max_pod_size: 8,
                // CR 903.13b: three draft rounds.
                packs_per_player: 3,
                // CR 903.13b: "drafts two cards".
                cards_per_pick: 2,
                pick_selection_mode: PickSelectionMode::Ordered,
                // CR 903.13b: "passes the remaining cards".
                distribution: PackDistribution::PickAndPass,
                // CR 903.13f(1): "at least 60 cards" — the limited-pool floor
                // for `validate_limited_deck`. Format LEGALITY is
                // `GameFormat::CommanderDraft`'s job, not this field's.
                min_deck_size: 60,
                // CR 903.13f(1): cube settings cannot lower Commander Draft's
                // 60-card deck-construction minimum.
                cube_min_deck_size: 60,
                // CR 903.3 as routed by CR 903.13f: a Commander Draft deck is a
                // Commander deck, so it designates a commander. `1` rather than
                // `2`: CR 903.13f(3)'s partner grant needs the draft to have
                // contained Commander Masters boosters, which the session does
                // not model.
                commanders_required: 1,
                // CR 903.13a: the pod plays one multiplayer game; the draft
                // session itself runs no bracket.
                post_draft_play: PostDraftPlay::CompleteImmediately,
                match_config: MatchConfig {
                    match_type: MatchType::Bo1,
                    ..MatchConfig::default()
                },
            },
        }
    }

    /// Default pod size for Arena-style drafts.
    pub fn default_pod_size(self) -> u8 {
        self.procedure().pod_size
    }

    /// Number of human seats. Quick Draft has 1 human + 7 bots.
    pub fn human_seats(self) -> u8 {
        self.procedure().human_seats
    }

    /// CR 903.3: how many commanders a deck built from this kind's pool must
    /// designate. `0` for the four CR 905.1a kinds.
    pub fn commanders_required(self) -> u8 {
        self.procedure().commanders_required
    }

    /// Match configuration for this draft kind.
    pub fn match_config(self) -> MatchConfig {
        self.procedure().match_config
    }
}

/// Boosters a Sealed event opens per player. Fixed by the event format itself,
/// not by the player's set selection — the reducer rejects any other count.
pub const SEALED_PACK_COUNT: u8 = 6;

/// Upper bound on the length of a draft's pack sequence.
///
/// A sequence names one set per booster, and every entry costs a distinct pool
/// to load and ship. No `DraftKind` opens more than [`SEALED_PACK_COUNT`]
/// boosters, so this leaves headroom for a future kind while keeping an
/// untrusted wire sequence bounded before any pool lookup runs.
pub const MAX_PACK_COUNT: u8 = 8;

/// Resolve the entry of a pack-ordered sequence that describes pack
/// `pack_number`.
///
/// Sequences shorter than the session's pack count repeat their last entry, so
/// a single-source draft is a one-element sequence rather than the same value
/// copied once per pack. Returns `None` only for an empty sequence.
pub fn entry_for_pack<T>(sequence: &[T], pack_number: u8) -> Option<&T> {
    sequence.get(usize::from(pack_number).min(sequence.len().checked_sub(1)?))
}

/// The way a set-backed draft assigns its boosters to seats and rounds.
///
/// `UniformByRound` preserves the original block-draft model: every seat gets
/// the same set in a round and a short sequence repeats its final entry.
/// `Chaos` records the host-created result for every `(seat, round)` pair, not
/// merely the candidate pool. That makes a resumed draft replay the exact
/// boosters it originally assigned rather than re-rolling them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum SetLayout {
    /// One set per round for the entire pod.
    UniformByRound {
        #[serde(alias = "code", deserialize_with = "deserialize_set_codes")]
        codes: Vec<String>,
    },
    /// Persisted Chaos assignments. The outer vector is seat order; each inner
    /// vector is pack-round order and must be exactly `pack_count` long.
    Chaos {
        candidate_codes: Vec<String>,
        assignments: Vec<Vec<String>>,
    },
}

/// Strict wire forms used solely while deserializing [`SetLayout`]. An
/// untagged enum normally accepts unknown fields, which lets a redacted Chaos
/// source containing both `candidate_codes` and `codes` silently become a
/// Uniform layout. These structs make the two persisted layouts disjoint.
#[derive(Deserialize)]
#[serde(untagged)]
enum SetLayoutWire {
    Uniform(UniformByRoundLayout),
    Chaos(ChaosLayout),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UniformByRoundLayout {
    #[serde(alias = "code", deserialize_with = "deserialize_set_codes")]
    codes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChaosLayout {
    candidate_codes: Vec<String>,
    assignments: Vec<Vec<String>>,
}

impl<'de> Deserialize<'de> for SetLayout {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match SetLayoutWire::deserialize(deserializer)? {
            SetLayoutWire::Uniform(UniformByRoundLayout { codes }) => {
                Ok(Self::UniformByRound { codes })
            }
            SetLayoutWire::Chaos(ChaosLayout {
                candidate_codes,
                assignments,
            }) => Ok(Self::Chaos {
                candidate_codes,
                assignments,
            }),
        }
    }
}

impl SetLayout {
    /// Codes actually assigned to boosters, deduplicated in first-appearance
    /// order. Candidate codes that the deterministic Chaos draw did not select
    /// are deliberately absent: a later rules consumer must learn what the
    /// draft contained from assignments, never from what it could have drawn.
    pub fn actual_set_codes(&self) -> Vec<&str> {
        match self {
            SetLayout::UniformByRound { codes } => distinct_set_codes(codes.iter()),
            SetLayout::Chaos { assignments, .. } => {
                distinct_set_codes(assignments.iter().flatten())
            }
        }
    }

    /// The set assigned to this seat's booster. Uniform layouts retain their
    /// repeat-final shorthand; Chaos assignments are intentionally exact and
    /// never repeat past their stored dimensions.
    pub fn set_code_for_seat_and_pack(&self, seat: u8, pack_number: u8) -> Option<&str> {
        match self {
            SetLayout::UniformByRound { codes } => {
                entry_for_pack(codes, pack_number).map(String::as_str)
            }
            SetLayout::Chaos { assignments, .. } => assignments
                .get(usize::from(seat))
                .and_then(|rounds| rounds.get(usize::from(pack_number)))
                .map(String::as_str),
        }
    }

    /// Check persisted dimensions and that Chaos cannot name a pool it was not
    /// configured to select. Pool-data and pack-size validation belongs at the
    /// source boundary; this protects stored session shape independently.
    pub fn validate_for_draft(&self, seat_count: u8, pack_count: u8) -> Result<(), String> {
        match self {
            SetLayout::UniformByRound { codes } => {
                if codes.is_empty() {
                    return Err("a draft must name at least one set".to_string());
                }
                Ok(())
            }
            SetLayout::Chaos {
                candidate_codes,
                assignments,
            } => {
                if candidate_codes.is_empty() {
                    return Err("a Chaos draft must name at least one candidate set".to_string());
                }
                if assignments.len() != usize::from(seat_count) {
                    return Err(format!(
                        "Chaos assignments must contain {seat_count} seats, got {}",
                        assignments.len()
                    ));
                }
                for (seat, rounds) in assignments.iter().enumerate() {
                    if rounds.len() != usize::from(pack_count) {
                        return Err(format!(
                            "Chaos assignments for seat {seat} must contain {pack_count} rounds, got {}",
                            rounds.len()
                        ));
                    }
                    for code in rounds {
                        if !candidate_codes
                            .iter()
                            .any(|candidate| candidate.eq_ignore_ascii_case(code))
                        {
                            return Err(format!(
                                "Chaos assignment '{code}' for seat {seat} is not a candidate set"
                            ));
                        }
                    }
                }
                Ok(())
            }
        }
    }
}

/// Preserve the first spelling of every case-insensitively distinct set code.
/// Both Uniform and Chaos source layouts need this same assignment-aware union.
fn distinct_set_codes<'a>(codes: impl Iterator<Item = &'a String>) -> Vec<&'a str> {
    let mut distinct = Vec::new();
    for code in codes {
        if !distinct
            .iter()
            .any(|held: &&str| held.eq_ignore_ascii_case(code))
        {
            distinct.push(code.as_str());
        }
    }
    distinct
}

/// Origin of the draft card pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DraftSource {
    Set {
        #[serde(flatten)]
        layout: SetLayout,
    },
    Cube {
        id: String,
        name: String,
    },
}

/// Accept both the pack-ordered `codes` array and the single `code` string that
/// pre-multi-set snapshots and wire frames wrote, so an in-flight draft — and a
/// peer that predates multi-set drafts — survives the upgrade.
///
/// Public because the same two shapes reach the engine from a second boundary:
/// `server_core::protocol::ClientMessage::CreateDraftWithSettings` carries the
/// host's chosen sequence, and a pre-multi-set client sends the single string
/// there. One deserializer owns both spellings for every boundary that sees
/// them, rather than each re-deciding what a legacy frame means.
pub fn deserialize_set_codes<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SetCodes {
        Single(String),
        Sequence(Vec<String>),
    }

    Ok(match SetCodes::deserialize(deserializer)? {
        SetCodes::Single(code) => vec![code],
        SetCodes::Sequence(codes) => codes,
    })
}

impl DraftSource {
    /// A set-backed source whose every booster comes from one set.
    pub fn single_set(code: impl Into<String>) -> Self {
        DraftSource::Set {
            layout: SetLayout::UniformByRound {
                codes: vec![code.into()],
            },
        }
    }

    /// Identifier for the source as a whole, used as the session's `set_code`
    /// label and for display. Multi-set drafts join their distinct set codes in
    /// first-appearance order (`"ISD+DKA+AVR"`) so one string still names the
    /// whole source; per-pack identity lives in [`DraftSource::set_code_for_pack`].
    pub fn set_code(&self) -> String {
        match self {
            DraftSource::Set { layout } => layout.actual_set_codes().join("+"),
            DraftSource::Cube { id, .. } => id.clone(),
        }
    }

    /// All set codes that actually fill this draft's boosters. Chaos layouts
    /// return the assignment union, excluding merely selectable candidate sets.
    pub fn actual_set_codes(&self) -> Vec<&str> {
        match self {
            DraftSource::Set { layout } => layout.actual_set_codes(),
            DraftSource::Cube { .. } => Vec::new(),
        }
    }

    /// The set filling a particular seat's booster.
    pub fn set_code_for_seat_and_pack(&self, seat: u8, pack_number: u8) -> String {
        match self {
            DraftSource::Set { layout } => layout
                .set_code_for_seat_and_pack(seat, pack_number)
                .unwrap_or_default()
                .to_string(),
            DraftSource::Cube { id, .. } => id.clone(),
        }
    }

    /// The set filling booster `pack_number`. Cube sources have no per-pack
    /// set, so every pack reports the cube id.
    pub fn set_code_for_pack(&self, pack_number: u8) -> String {
        self.set_code_for_seat_and_pack(0, pack_number)
    }
}

impl Default for DraftSource {
    fn default() -> Self {
        DraftSource::single_set("UNKNOWN")
    }
}

/// Which non-drafted cards are available in unlimited quantity while building
/// a Limited deck.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeckAddableCardPolicy {
    #[default]
    StandardBasics,
    CustomOnly,
    StandardBasicsPlusCustom,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckAddableCards {
    pub policy: DeckAddableCardPolicy,
    #[serde(default)]
    pub custom: Vec<String>,
}

impl DeckAddableCards {
    pub fn standard_basics() -> Self {
        Self {
            policy: DeckAddableCardPolicy::StandardBasics,
            custom: Vec::new(),
        }
    }

    pub fn is_addable(&self, name: &str) -> bool {
        let standard = STANDARD_BASIC_LANDS.contains(&name);
        let custom = self.custom.iter().any(|card| card == name);
        match self.policy {
            DeckAddableCardPolicy::StandardBasics => standard,
            DeckAddableCardPolicy::CustomOnly => custom,
            DeckAddableCardPolicy::StandardBasicsPlusCustom => standard || custom,
        }
    }

    pub fn display_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        match self.policy {
            DeckAddableCardPolicy::StandardBasics => {
                names.extend(STANDARD_BASIC_LANDS.iter().map(|name| (*name).to_string()));
            }
            DeckAddableCardPolicy::CustomOnly => names.extend(self.custom.iter().cloned()),
            DeckAddableCardPolicy::StandardBasicsPlusCustom => {
                names.extend(STANDARD_BASIC_LANDS.iter().map(|name| (*name).to_string()));
                names.extend(self.custom.iter().cloned());
            }
        }
        names.sort();
        names.dedup();
        names
    }
}

/// Direction packs are passed around the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PassDirection {
    Left,
    Right,
}

impl PassDirection {
    /// Standard MTG draft pass direction: pack 1 left, pack 2 right, pack 3 left, etc.
    pub fn for_pack(pack_number: u8) -> Self {
        if pack_number.is_multiple_of(2) {
            PassDirection::Left
        } else {
            PassDirection::Right
        }
    }

    /// Calculate the next seat index in this pass direction, wrapping around the pod.
    pub fn next_seat(self, current: u8, pod_size: u8) -> u8 {
        match self {
            PassDirection::Left => (current + 1) % pod_size,
            PassDirection::Right => (current + pod_size - 1) % pod_size,
        }
    }
}

/// Overall status of a draft session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DraftStatus {
    Lobby,
    Drafting,
    Paused,
    Deckbuilding,
    Pairing,
    MatchInProgress,
    RoundComplete,
    Complete,
    Abandoned,
}

/// A single card instance in a draft pack or pool.
/// Lightweight collation type — NOT engine CardFace.
/// Enriched with colors/cmc/type_line for bot AI color preference (Medium+),
/// frontend sorting (PoolPanel by color/type/CMC), and ManaCurve rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftCardInstance {
    pub instance_id: String,
    pub name: String,
    pub set_code: String,
    pub collector_number: String,
    pub rarity: String,
    /// Color identity letters, e.g. ["W", "U"]. Populated at pack generation from set pool data.
    #[serde(default)]
    pub colors: Vec<String>,
    /// Converted mana cost. Populated at pack generation from set pool data.
    #[serde(default)]
    pub cmc: u8,
    /// Full type line, e.g. "Creature — Human Wizard". Populated at pack generation from set pool data.
    #[serde(default)]
    pub type_line: String,
    /// Draft-time effect parsed from the card's Oracle text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_effect: Option<DraftEffect>,
}

/// A pack of cards, newtype wrapper over Vec<DraftCardInstance>.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftPack(pub Vec<DraftCardInstance>);

/// A seat in the draft pod — either a human player or a bot.
///
/// Runtime connection state lives in `DraftSession.connected_seats` — do NOT
/// add a `connected: bool` field here. The seat enum only describes who
/// occupies the slot; presence/absence is tracked separately so the view
/// layer has one authoritative source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DraftSeat {
    Human {
        player_id: PlayerId,
        display_name: String,
    },
    Bot {
        name: String,
    },
}

/// Per-seat bitmap indexed by seat. Length grows to `pod_size` on first
/// access via [`SeatFlags::ensure_len`], which uses [`Vec::resize`] semantics
/// (preserves existing entries on grow; pads new slots with `default`).
/// Does NOT shrink — pod size is immutable mid-session.
///
/// All seats — including bots — occupy a slot for index alignment with
/// [`DraftSession::seats`]. Bot slots are not consulted by the view layer
/// (it short-circuits to `true`), but are written-through to keep the
/// index invariant intact.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SeatFlags(Vec<bool>);

impl SeatFlags {
    pub fn all_true(pod_size: u8) -> Self {
        Self(vec![true; pod_size as usize])
    }

    pub fn all_false(pod_size: u8) -> Self {
        Self(vec![false; pod_size as usize])
    }

    /// Grow to `pod_size` if shorter, padding with `default`. Never shrinks.
    /// Existing entries are preserved on grow.
    pub fn ensure_len(&mut self, pod_size: u8, default: bool) {
        if self.0.len() < pod_size as usize {
            self.0.resize(pod_size as usize, default);
        }
    }

    pub fn get(&self, seat: u8) -> bool {
        self.0.get(seat as usize).copied().unwrap_or(false)
    }

    /// Like [`SeatFlags::get`] but returns `default` for out-of-bounds reads.
    ///
    /// Use this when "absence of an entry" should mean something specific —
    /// e.g. `connected_seats` reads in the view layer pass `true` so an
    /// in-flight save deserialised from pre-fix code (empty bitmap before
    /// `ensure_len` runs) renders human seats as connected, not as a wall
    /// of disconnect dots.
    pub fn get_or(&self, seat: u8, default: bool) -> bool {
        self.0.get(seat as usize).copied().unwrap_or(default)
    }

    pub fn set(&mut self, seat: u8, value: bool) {
        if let Some(slot) = self.0.get_mut(seat as usize) {
            *slot = value;
        }
    }

    pub fn clear(&mut self) {
        for flag in &mut self.0 {
            *flag = false;
        }
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Typed reason for a draft pause, used over the wire and on the i18n key path.
///
/// Spelling note: every other enum in this file uses default PascalCase variant
/// serialization (`DraftAction`, `DraftDelta`, `DraftStatus`, etc.). We keep
/// that convention here — wire shape is `"PlayerDisconnected"` etc. The TS
/// i18n key path also uses PascalCase (`pauseReason.PlayerDisconnected`) so
/// wire = lookup with no boundary conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DraftPauseReason {
    PlayerDisconnected,
    PausedByHost,
    DisconnectGraceExpired,
}

/// Actions that can be performed on a draft session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DraftAction {
    StartDraft,
    /// One whole CR 903.13b pick step: a seat takes every card it drafts this
    /// step in a single action.
    ///
    /// The count is not free — `apply_pick_inner` requires exactly
    /// `min(kind.procedure().cards_per_pick, remaining_pack_len)` ids, which is
    /// `1` for the four CR 905.1a kinds and `2` for CommanderDraft, dropping to
    /// the remainder on an odd final pick. `DraftDelta::CardPicked` stays
    /// singular: one delta per card.
    Pick {
        seat: u8,
        card_instance_ids: Vec<String>,
    },
    PickWithDraftEffect {
        seat: u8,
        effect_card_instance_id: String,
        card_instance_ids: Vec<String>,
    },
    SubmitDeck {
        seat: u8,
        main_deck: Vec<String>,
        /// CR 903.3 + CR 903.13e: the card names this seat designates as its
        /// commander(s). `main_deck` is the COMPLETE submitted list and every
        /// designated name is a member of it (CR 903.5a: "including its
        /// commander"), so a designation is a label on a deck card and never
        /// an extra card beside the deck.
        ///
        /// Empty for every non-commander draft kind, which is why
        /// `#[serde(default)]` here is semantics rather than a compatibility
        /// shim: an empty designation list is the correct and meaningful value
        /// for a Quick/Premier/Traditional/Sealed submission.
        ///
        /// CR 702.124g caps the list at
        /// [`MAX_COMMANDER_DESIGNATIONS`](crate::types::MAX_COMMANDER_DESIGNATIONS).
        #[serde(default)]
        commanders: Vec<String>,
    },
    /// Generate the next round's pairings. Carries no round: the reducer is the
    /// single authority for which round that is (`DraftSession::next_pairing_round`).
    GeneratePairings,
    ReportMatchResult {
        match_id: String,
        /// None = draw.
        winner_seat: Option<u8>,
    },
    AdvanceRound,
    /// Casual mode: host replaces a human seat with a bot.
    ReplaceSeatWithBot {
        seat: u8,
        #[serde(default)]
        name: Option<String>,
    },
    /// Host-side runtime: mark a human seat as connected or disconnected.
    /// The bitmap drives `DraftPlayerView.seats[*].connected`. Rejects bot seats.
    SetSeatConnected {
        seat: u8,
        connected: bool,
    },
}

/// State changes produced by applying a DraftAction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DraftDelta {
    DraftStarted,
    CardPicked {
        seat: u8,
        card_instance_id: String,
    },
    PackPassed,
    PackExhausted {
        new_pack_number: u8,
    },
    DeckSubmitted {
        seat: u8,
    },
    TransitionedTo {
        status: DraftStatus,
    },
    PairingsGenerated {
        round: u8,
    },
    MatchResultRecorded {
        match_id: String,
        winner_seat: Option<u8>,
    },
    RoundAdvanced {
        new_round: u8,
    },
    SeatReplacedWithBot {
        seat: u8,
    },
    SeatConnectionChanged {
        seat: u8,
        connected: bool,
    },
}

/// Errors that can occur during draft operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum DraftError {
    #[error("invalid transition from {from:?}: {action}")]
    InvalidTransition { from: DraftStatus, action: String },
    #[error("seat {seat} out of range for pod size {pod_size}")]
    SeatOutOfRange { seat: u8, pod_size: u8 },
    #[error("card '{card_instance_id}' not found in pack")]
    CardNotInPack { card_instance_id: String },
    #[error("draft effect card '{card_instance_id}' is not in the player's pool")]
    DraftEffectCardNotInPool { card_instance_id: String },
    #[error("draft effect requires {expected_cards} cards, got {actual_cards}")]
    InvalidDraftEffectSelection {
        expected_cards: usize,
        actual_cards: usize,
    },
    #[error("seat {seat} has no pending pack")]
    NoPendingPack { seat: u8 },
    #[error("deck validation failed")]
    ValidationFailed { errors: Vec<LimitedDeckError> },
    #[error("pairing not found: {match_id}")]
    PairingNotFound { match_id: String },
    #[error("pairing {match_id} is not in current round {current_round}")]
    PairingNotInCurrentRound { match_id: String, current_round: u8 },
    #[error("single-elimination match {match_id} requires a winner")]
    MatchWinnerRequired { match_id: String },
    #[error("seat {seat} is not in pairing {match_id}")]
    SeatNotInPairing { seat: u8, match_id: String },
    #[error("{format:?} requires {required} seats, got {actual}")]
    UnsupportedTournamentSize {
        format: TournamentFormat,
        required: u8,
        actual: u8,
    },
    #[error("draft source has {available} cards, but {required} cards are required")]
    InsufficientCards { available: usize, required: usize },
    #[error("seat {seat} must pick {expected} card(s) from the current pack, got {actual}")]
    WrongPickCardCount {
        seat: u8,
        expected: usize,
        actual: usize,
    },
    #[error("seat {seat} picked card {card_instance_id} more than once")]
    DuplicatePickCardId { seat: u8, card_instance_id: String },
    #[error("seat {seat} has already picked this round")]
    SeatAlreadyPickedThisRound { seat: u8 },
    #[error("seat {seat} is a bot — operation not applicable")]
    SeatIsBot { seat: u8 },
    #[error("sealed events require a set source")]
    SealedRequiresSetSource,
    #[error("invalid pack sequence: {reason}")]
    InvalidPackSequence { reason: String },
    #[error("invalid sealed configuration: {reason}")]
    InvalidSealedConfiguration { reason: String },
    #[error("invalid sealed snapshot: {reason}")]
    InvalidSealedSnapshot { reason: String },
    /// CR 903.13a + CR 800.1: Commander Draft is "a draft ... followed by a
    /// multiplayer game", and "a multiplayer game is a game that begins with
    /// more than two players" — so a kind's `min_pod_size` is the smallest pod
    /// that can still deliver the game that kind is defined as. Carries the
    /// `kind`, never a `TournamentFormat`: the floor is a per-kind rule, and
    /// reporting it through `UnsupportedTournamentSize` would re-introduce the
    /// kind-blindness this guard exists to remove.
    #[error("{kind:?} pods require at least {required} seats, got {actual}")]
    PodBelowMinimumSize {
        kind: DraftKind,
        required: u8,
        actual: u8,
    },
}

/// Configuration for a draft session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftConfig {
    #[serde(default)]
    pub source: DraftSource,
    pub set_code: String,
    pub kind: DraftKind,
    #[serde(default = "default_pod_size")]
    pub pod_size: u8,
    /// Nominal booster size, used by sources that generate uniform packs (cube)
    /// and as the fallback for snapshots written before per-pack sizes were
    /// recorded. A multi-set draft mixes MTGJSON booster sizes, so the
    /// authority for how many cards a given booster holds is
    /// [`DraftSession::cards_in_pack`] — never this field.
    pub cards_per_pack: u8,
    pub pack_count: u8,
    #[serde(default = "default_min_deck_size")]
    pub min_deck_size: usize,
    #[serde(default = "DeckAddableCards::standard_basics")]
    pub addable_cards: DeckAddableCards,
    pub rng_seed: u64,
    #[serde(default)]
    pub tournament_format: TournamentFormat,
    #[serde(default)]
    pub pod_policy: PodPolicy,
    #[serde(default)]
    pub spectator_visibility: SpectatorVisibility,
}

// The two `serde` defaults below are a second authority for axes that
// `DraftKind::procedure()` now owns, and neither can see `kind`. They agree with
// every kind that exists today (all four are pod 8 / 40 cards), so nothing is
// wrong now; they are recorded because a kind whose procedure states a different
// value silently resolves to the literal here whenever serde fills the field.
// `procedure()` is the authority — read it, do not copy these numbers.

/// Not `DraftKind::default_pod_size`, which correctly delegates to
/// `procedure()`. This is the `serde` fallback for a `DraftConfig` that omits
/// `pod_size`, and it cannot consult `kind`. `DraftKind::CommanderDraft` now
/// exists and specifies a 4-seat pod, so a payload naming it while omitting
/// this field lands on `8`.
///
/// That is unreachable from any in-tree producer: `DraftConfig` derives
/// `Serialize` with no `skip_serializing_if` on any field, so every serializer
/// in this repo emits `pod_size`, and a pre-`CommanderDraft` save — the case
/// this fallback exists for — cannot name a kind that did not exist when it was
/// written. The residual is a hand-crafted payload at a system boundary, which
/// is not worth a kind-aware custom `Deserialize`.
fn default_pod_size() -> u8 {
    8
}

/// The `serde` fallback for a `DraftConfig` that omits `min_deck_size`; it
/// cannot consult `kind`. CR 100.2b's 40-card limited minimum is correct for
/// four of the five kinds, but CR 903.13f(1) requires *at least 60* for
/// Commander Draft, which now exists and lands on `40` through this path.
///
/// Unreachable for the same measured reason as `default_pod_size` above: no
/// in-tree producer can emit a `DraftConfig` missing this field, and no save
/// old enough to rely on the fallback can name `CommanderDraft`.
fn default_min_deck_size() -> usize {
    40
}

/// A player's submitted deck for limited play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftDeckSubmission {
    pub seat: u8,
    pub main_deck: Vec<String>,
    /// CR 903.3 + CR 903.13e: the commander designation, SNAPSHOTTED at
    /// submission. A later pool change must never silently re-designate, so
    /// this is stored beside `main_deck` rather than re-derived; it is
    /// invalidated only by resubmission. Every name here is a member of
    /// `main_deck` as a multiset (CR 702.124h), enforced at submission by
    /// `validate_limited_deck`.
    #[serde(default)]
    pub commanders: Vec<String>,
}

/// Win/loss record for a player in the draft event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftMatchRecord {
    pub player: PlayerId,
    pub wins: u8,
    pub losses: u8,
    pub draws: u8,
    pub match_wins: u8,
    pub match_losses: u8,
}

/// Status of a pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingStatus {
    Pending,
    InProgress,
    Complete,
}

/// A pairing between two players for a match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftPairing {
    pub round: u8,
    pub table: u8,
    pub players: [PlayerId; 2],
    pub match_id: String,
    pub status: PairingStatus,
    #[serde(default)]
    pub winner: Option<PlayerId>,
}

impl DraftPairing {
    pub fn result_winner(&self, records: &HashMap<PlayerId, DraftMatchRecord>) -> Option<PlayerId> {
        self.winner
            .or_else(|| self.infer_winner_from_records(records))
    }

    fn infer_winner_from_records(
        &self,
        records: &HashMap<PlayerId, DraftMatchRecord>,
    ) -> Option<PlayerId> {
        if self.status != PairingStatus::Complete {
            return None;
        }

        let w0 = records.get(&self.players[0]).map_or(0, |r| r.match_wins);
        let w1 = records.get(&self.players[1]).map_or(0, |r| r.match_wins);

        match w0.cmp(&w1) {
            std::cmp::Ordering::Greater => Some(self.players[0]),
            std::cmp::Ordering::Less => Some(self.players[1]),
            std::cmp::Ordering::Equal => None,
        }
    }
}

/// The full state of a draft session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftSession {
    pub draft_code: String,
    pub set_code: String,
    pub kind: DraftKind,
    pub status: DraftStatus,
    pub config: DraftConfig,
    pub seats: Vec<DraftSeat>,
    pub current_pack_number: u8,
    /// Cards each booster held when it was opened, in pack order. Recorded at
    /// [`DraftAction::StartDraft`] from the packs the source actually
    /// generated: a multi-set draft mixes booster sizes, and picking consumes
    /// the packs themselves, so neither a session-wide scalar nor the live
    /// packs can answer "how big was pack 2?". Empty on snapshots written
    /// before this field existed — read through
    /// [`DraftSession::cards_in_pack`], which falls back to the uniform
    /// `config.cards_per_pack`.
    #[serde(default)]
    pub pack_sizes: Vec<u8>,
    pub pick_number: u8,
    /// Per-seat flag, `true` once that seat has submitted a pick for the
    /// current pick number. Cleared when the round advances. Replaces the
    /// pre-fix `picks_this_round: u8` counter, which did not track seat
    /// identity and allowed a single seat to force pack-passing.
    #[serde(default)]
    pub seats_picked_this_round: SeatFlags,
    /// Runtime per-seat connection flag set via [`DraftAction::SetSeatConnected`].
    /// Defaults to all-true at session creation. Bots occupy a slot for index
    /// alignment but are short-circuited to `true` by [`crate::view::filter_for_player`].
    #[serde(default)]
    pub connected_seats: SeatFlags,
    pub pass_direction: PassDirection,
    pub packs_by_seat: Vec<Vec<DraftPack>>,
    pub current_pack: Vec<Option<DraftPack>>,
    /// The seat that opened each currently held booster. Packs pass between
    /// seats, while Chaos set assignments belong to the opening seat and pack
    /// round, so this provenance travels with the pack rather than its holder.
    /// Empty legacy snapshots deserialize as `None` origins and therefore omit
    /// the optional Chaos pack label rather than guessing one.
    #[serde(default)]
    pub current_pack_origins: Vec<Option<u8>>,
    pub pools: Vec<Vec<DraftCardInstance>>,
    pub submitted_decks: HashMap<PlayerId, DraftDeckSubmission>,
    pub match_records: HashMap<PlayerId, DraftMatchRecord>,
    pub pairings: Vec<DraftPairing>,
    pub current_round: u8,
    pub created_at: u64,
    pub updated_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_kind_default_pod_size() {
        assert_eq!(DraftKind::Quick.default_pod_size(), 8);
        assert_eq!(DraftKind::Premier.default_pod_size(), 8);
        assert_eq!(DraftKind::Traditional.default_pod_size(), 8);
        assert_eq!(DraftKind::Sealed.default_pod_size(), 8);
    }

    #[test]
    fn draft_kind_human_seats() {
        assert_eq!(DraftKind::Quick.human_seats(), 1);
        assert_eq!(DraftKind::Premier.human_seats(), 8);
        assert_eq!(DraftKind::Traditional.human_seats(), 8);
        assert_eq!(DraftKind::Sealed.human_seats(), 8);
    }

    #[test]
    fn draft_kind_match_config() {
        assert_eq!(DraftKind::Quick.match_config().match_type, MatchType::Bo1);
        assert_eq!(DraftKind::Premier.match_config().match_type, MatchType::Bo1);
        assert_eq!(
            DraftKind::Traditional.match_config().match_type,
            MatchType::Bo3
        );
        assert_eq!(DraftKind::Sealed.match_config().match_type, MatchType::Bo1);
    }

    /// `procedure()` is the single authority for the per-kind draft axes.
    ///
    /// The structural half is this phase's only genuinely discriminating
    /// assertion: a pure refactor has no observable behavior delta, so the
    /// witness that the duplicated literal is gone must be structural. Measured
    /// before the refactor, the scan below matched exactly two sites.
    #[test]
    fn draft_procedure_is_single_authority() {
        assert_eq!(DraftKind::Sealed.procedure().packs_per_player, 6);
        // Non-vacuity sibling: without this, the assertion above also passes
        // against a `procedure()` that ignores `self` and returns one record.
        assert_ne!(
            DraftKind::Sealed.procedure().packs_per_player,
            DraftKind::Premier.procedure().packs_per_player
        );

        // Needle assembly, modelled on `crates/engine/src/source_census.rs:273`:
        // a real character moves into the `format!` argument, so no line of this
        // file's own source contains either assembled needle contiguously.
        let needle_ternary = format!("pack_count: i{}", 'f');
        let needle_kind = format!("DraftKind::{}", "Sealed");

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/draft-core sits two levels under the workspace root");

        let mut files: Vec<std::path::PathBuf> = Vec::new();
        let mut stack = vec![root.join("crates")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}")) {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|name| name == "target") {
                        continue;
                    }
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    files.push(path);
                }
            }
        }
        files.sort();

        let mut total_bytes = 0usize;
        let mut saw_wasm_bridge = false;
        let mut saw_server = false;
        let mut offenders: Vec<String> = Vec::new();
        for path in &files {
            let rel = path
                .strip_prefix(root)
                .expect("under the workspace root")
                .to_string_lossy()
                .replace('\\', "/");
            // Self-exclusion, modelled on `source_census.rs:291-293`: this file
            // defines the predicate and never legitimately carries the ternary.
            if rel == "crates/draft-core/src/types.rs" {
                continue;
            }
            saw_wasm_bridge |= rel == "crates/draft-wasm/src/lib.rs";
            saw_server |= rel == "crates/phase-server/src/main.rs";
            let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
            total_bytes += text.len();
            for (index, line) in text.lines().enumerate() {
                if line.contains(&needle_ternary) && line.contains(&needle_kind) {
                    offenders.push(format!("{rel}:{}", index + 1));
                }
            }
        }

        // Paired positive reach-guard: a path walk that silently resolved to
        // nothing would make the scan pass vacuously. This answers a walk that
        // finds NOTHING; the two mitigations above answer one that finds ITSELF.
        assert!(
            total_bytes > 0,
            "reach-guard: the walk read no source at all"
        );
        assert!(
            saw_wasm_bridge && saw_server,
            "reach-guard: the walk missed the two files that carried the ternary \
             (draft-wasm seen: {saw_wasm_bridge}, phase-server seen: {saw_server})"
        );

        assert!(
            offenders.is_empty(),
            "the per-kind pack total must be read from DraftKind::procedure(), \
             but a duplicated ternary survives at: {offenders:?}"
        );
    }

    /// Every `procedure()` arm reproduces the values previously duplicated at
    /// call sites — the refactor's actual behavior-preservation claim.
    ///
    /// That claim is carried by the per-kind **literal** assertions below, not
    /// by the loop. Stated honestly: since the accessors became one-line
    /// delegations to `procedure()`, each loop assertion compares
    /// `procedure().f` against `procedure().f`, so it cannot fail for any kind
    /// and it does **not** catch a mistyped arm. What it still pins is the
    /// *delegation* — that no accessor has reacquired an independent authority
    /// for a per-kind number. It starts discriminating again only if someone
    /// re-introduces a second `match`, which is exactly the regression the
    /// procedure table removed and this test is named for.
    #[test]
    fn procedure_matches_legacy_accessors() {
        // Tautological by construction today (see the doc above): pins that
        // each accessor below remains a delegation, not that its value is right.
        for kind in DraftKind::ALL {
            let procedure = kind.procedure();
            assert_eq!(procedure.pod_size, kind.default_pod_size(), "{kind:?}");
            assert_eq!(procedure.human_seats, kind.human_seats(), "{kind:?}");
            assert_eq!(procedure.match_config, kind.match_config(), "{kind:?}");
            assert_eq!(
                procedure.commanders_required,
                kind.commanders_required(),
                "{kind:?}"
            );
        }

        // CR 100.2b: the 40-card limited minimum the four deleted `DraftConfig`
        // literals each hardcoded. CR 903.13f(1) puts CommanderDraft at 60, so
        // these are per-kind rather than loop-invariant.
        assert_eq!(DraftKind::Quick.procedure().min_deck_size, 40);
        assert_eq!(DraftKind::Premier.procedure().min_deck_size, 40);
        assert_eq!(DraftKind::Traditional.procedure().min_deck_size, 40);
        assert_eq!(DraftKind::Sealed.procedure().min_deck_size, 40);
        assert_eq!(DraftKind::CommanderDraft.procedure().min_deck_size, 60);

        // CR 905.1a: one card per pick step for the four Arena-style kinds.
        // CR 903.13b: two for CommanderDraft.
        assert_eq!(DraftKind::Quick.procedure().cards_per_pick, 1);
        assert_eq!(DraftKind::Premier.procedure().cards_per_pick, 1);
        assert_eq!(DraftKind::Traditional.procedure().cards_per_pick, 1);
        assert_eq!(DraftKind::Sealed.procedure().cards_per_pick, 1);
        assert_eq!(DraftKind::CommanderDraft.procedure().cards_per_pick, 2);

        assert_eq!(
            DraftKind::Quick.procedure().pick_selection_mode,
            PickSelectionMode::Direct
        );
        assert_eq!(
            DraftKind::Premier.procedure().pick_selection_mode,
            PickSelectionMode::Direct
        );
        assert_eq!(
            DraftKind::Traditional.procedure().pick_selection_mode,
            PickSelectionMode::Direct
        );
        assert_eq!(
            DraftKind::Sealed.procedure().pick_selection_mode,
            PickSelectionMode::Direct
        );
        assert_eq!(
            DraftKind::CommanderDraft.procedure().pick_selection_mode,
            PickSelectionMode::Ordered
        );

        // The values the deleted `pack_count` ternaries produced.
        assert_eq!(DraftKind::Quick.procedure().packs_per_player, 3);
        assert_eq!(DraftKind::Premier.procedure().packs_per_player, 3);
        assert_eq!(DraftKind::Traditional.procedure().packs_per_player, 3);
        assert_eq!(DraftKind::Sealed.procedure().packs_per_player, 6);

        // Public and remote pods share a 2-seat floor; local Quick Cube keeps
        // its distinct procedure-owned one-seat capability.
        assert_eq!(DraftKind::Quick.procedure().min_pod_size, 2);
        assert_eq!(DraftKind::Quick.procedure().local_cube_min_pod_size, 1);
        assert_eq!(DraftKind::Premier.procedure().min_pod_size, 2);
        assert_eq!(DraftKind::Traditional.procedure().min_pod_size, 2);
        assert_eq!(DraftKind::Sealed.procedure().min_pod_size, 2);

        // Sealed is the sole `AllAtOnce` kind — the fact every converted
        // equality test against `DraftKind::Sealed` silently depended on.
        assert_eq!(
            DraftKind::Sealed.procedure().distribution,
            PackDistribution::AllAtOnce
        );
        assert_eq!(
            DraftKind::Quick.procedure().distribution,
            PackDistribution::PickAndPass
        );
        assert_eq!(
            DraftKind::Premier.procedure().distribution,
            PackDistribution::PickAndPass
        );
        assert_eq!(
            DraftKind::Traditional.procedure().distribution,
            PackDistribution::PickAndPass
        );
    }

    /// Every field of the Commander Draft procedure, against CR 903.13.
    ///
    /// One assertion per field of `DraftProcedure`, because a preset row is
    /// data: the only way to pin it is to state every value.
    #[test]
    fn commander_draft_procedure_matches_cr_903_13() {
        let procedure = DraftKind::CommanderDraft.procedure();

        // Product defaults, deliberately carrying no CR citation.
        assert_eq!(procedure.pod_size, 4);
        assert_eq!(procedure.human_seats, 1);
        assert_eq!(procedure.match_config.match_type, MatchType::Bo1);

        // CR 903.13a + CR 800.1: three seats is the smallest pod that still
        // delivers the multiplayer game the format is defined as. This is the
        // wire rejection floor, NOT the 4-seat product default above.
        assert_eq!(procedure.min_pod_size, 3);
        assert_eq!(procedure.max_pod_size, 8);

        // CR 903.13b.
        assert_eq!(procedure.packs_per_player, 3);
        assert_eq!(procedure.cards_per_pick, 2);
        assert_eq!(procedure.pick_selection_mode, PickSelectionMode::Ordered);
        assert_eq!(procedure.distribution, PackDistribution::PickAndPass);

        // CR 903.13f(1): the limited-pool floor, not format legality.
        assert_eq!(procedure.min_deck_size, 60);
        assert_eq!(procedure.cube_min_deck_size, 60);

        // CR 903.3 as routed by CR 903.13f: a Commander Draft deck is a
        // Commander deck and designates a commander. `1`, not `2` — the
        // CR 903.13f(3) partner grant is conditioned on the draft having
        // contained Commander Masters boosters, which is not modelled.
        assert_eq!(procedure.commanders_required, 1);

        // CR 903.13a: one multiplayer game, not an in-session bracket.
        assert_eq!(
            procedure.post_draft_play,
            PostDraftPlay::CompleteImmediately
        );
    }

    #[test]
    fn procedure_enforces_only_its_cube_minimum_deck_size() {
        for kind in [
            DraftKind::Quick,
            DraftKind::Premier,
            DraftKind::Traditional,
            DraftKind::Sealed,
        ] {
            let ordinary = kind.procedure();
            assert_eq!(ordinary.cube_min_deck_size, 1, "{kind:?}");
            assert_eq!(ordinary.effective_cube_min_deck_size(73), 73, "{kind:?}");
        }

        let commander = DraftKind::CommanderDraft.procedure();
        assert_eq!(commander.effective_cube_min_deck_size(1), 60);
        assert_eq!(commander.effective_cube_min_deck_size(75), 75);
    }

    #[test]
    fn procedure_owns_allowed_pod_size_policy_for_every_kind() {
        for kind in DraftKind::ALL {
            let procedure = kind.procedure();
            assert!(
                procedure.allows_pod_size(TournamentFormat::Swiss, procedure.min_pod_size),
                "Swiss accepts the procedure floor for {kind:?}"
            );
            assert!(
                procedure.allows_pod_size(TournamentFormat::Swiss, procedure.max_pod_size),
                "Swiss accepts the procedure ceiling for {kind:?}"
            );
            if procedure.max_pod_size < u8::MAX {
                assert!(
                    !procedure
                        .allows_pod_size(TournamentFormat::Swiss, procedure.max_pod_size + 1,),
                    "Swiss rejects above the procedure ceiling for {kind:?}"
                );
            }
        }

        for kind in [
            DraftKind::Premier,
            DraftKind::Traditional,
            DraftKind::Sealed,
        ] {
            let procedure = kind.procedure();
            assert!(
                !procedure.allows_pod_size(TournamentFormat::SingleElimination, 7),
                "tournament pairings require the full bracket for {kind:?}"
            );
            assert!(
                procedure.allows_pod_size(TournamentFormat::SingleElimination, 8),
                "tournament pairings admit the full bracket for {kind:?}"
            );
        }

        let commander = DraftKind::CommanderDraft.procedure();
        assert!(commander.allows_pod_size(TournamentFormat::SingleElimination, 3));
        assert!(commander.allows_pod_size(TournamentFormat::SingleElimination, 8));
    }

    /// [`MAX_CARDS_PER_PICK`] is derived from the procedure table, not chosen.
    ///
    /// `server-core`'s payload guard cannot consult a session, so it bounds a
    /// `Pick` by this constant. If a future kind raised `cards_per_pick`
    /// without updating it, the wire would reject that kind's legitimate picks.
    #[test]
    fn max_cards_per_pick_matches_procedure_table() {
        let derived = DraftKind::ALL
            .into_iter()
            .map(|kind| usize::from(kind.procedure().cards_per_pick))
            .max()
            .expect("DraftKind::ALL is never empty");
        assert_eq!(
            derived, MAX_CARDS_PER_PICK,
            "MAX_CARDS_PER_PICK must equal the largest cards_per_pick in the procedure table"
        );
    }

    /// `DraftKind::ALL` must list every variant exactly once.
    ///
    /// What this actually enforces, stated narrowly: the `match` below is
    /// wildcard-free, so a sixth `DraftKind` is an `E0004` **here**, which
    /// lands the author beside the array that must list it; the array type
    /// `[DraftKind; 5]` enforces the length; and the sorted-index equality
    /// catches a duplicated entry. It does **not** enforce that a sixth variant
    /// is added to `ALL` — a variant that adds its arm below and is omitted
    /// from the array still compiles and still passes. The `E0004`'s proximity
    /// is the guarantee, not the assertion.
    #[test]
    fn draft_kind_all_lists_every_variant() {
        fn index_of(kind: DraftKind) -> usize {
            match kind {
                DraftKind::Quick => 0,
                DraftKind::Premier => 1,
                DraftKind::Traditional => 2,
                DraftKind::Sealed => 3,
                DraftKind::CommanderDraft => 4,
            }
        }
        let mut indices: Vec<usize> = DraftKind::ALL.into_iter().map(index_of).collect();
        indices.sort_unstable();
        assert_eq!(
            indices,
            [0, 1, 2, 3, 4],
            "DraftKind::ALL must list every variant exactly once"
        );
    }

    #[test]
    fn pass_direction_for_pack() {
        assert_eq!(PassDirection::for_pack(0), PassDirection::Left);
        assert_eq!(PassDirection::for_pack(1), PassDirection::Right);
        assert_eq!(PassDirection::for_pack(2), PassDirection::Left);
        assert_eq!(PassDirection::for_pack(3), PassDirection::Right);
    }

    /// CR 903.13c: "In the first and third draft rounds, booster packs are
    /// passed to each player's left. In the second draft round ... right."
    ///
    /// Assert-only: `PassDirection::for_pack` is already correct and this phase
    /// does not modify it, so this pins existing behavior for a pod of 4 (which
    /// no prior test exercised — every existing rotation test uses pod 8)
    /// rather than discriminating any change of P2's.
    #[test]
    fn commander_draft_passes_left_right_left() {
        assert_eq!(PassDirection::for_pack(0), PassDirection::Left);
        assert_eq!(PassDirection::for_pack(1), PassDirection::Right);
        assert_eq!(PassDirection::for_pack(2), PassDirection::Left);

        // Pod-4 wraparound in both directions.
        let pod_size = DraftKind::CommanderDraft.procedure().pod_size;
        assert_eq!(pod_size, 4);
        assert_eq!(PassDirection::Left.next_seat(3, pod_size), 0);
        assert_eq!(PassDirection::Right.next_seat(0, pod_size), 3);
    }

    #[test]
    fn pass_direction_next_seat_left() {
        assert_eq!(PassDirection::Left.next_seat(0, 8), 1);
        assert_eq!(PassDirection::Left.next_seat(7, 8), 0);
        assert_eq!(PassDirection::Left.next_seat(3, 8), 4);
    }

    #[test]
    fn pass_direction_next_seat_right() {
        assert_eq!(PassDirection::Right.next_seat(0, 8), 7);
        assert_eq!(PassDirection::Right.next_seat(1, 8), 0);
        assert_eq!(PassDirection::Right.next_seat(5, 8), 4);
    }

    #[test]
    fn serde_roundtrip_draft_kind() {
        for kind in [
            DraftKind::Quick,
            DraftKind::Premier,
            DraftKind::Traditional,
            DraftKind::Sealed,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: DraftKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn serde_roundtrip_draft_status() {
        let statuses = [
            DraftStatus::Lobby,
            DraftStatus::Drafting,
            DraftStatus::Paused,
            DraftStatus::Deckbuilding,
            DraftStatus::Pairing,
            DraftStatus::MatchInProgress,
            DraftStatus::RoundComplete,
            DraftStatus::Complete,
            DraftStatus::Abandoned,
        ];
        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let back: DraftStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    #[test]
    fn serde_roundtrip_pass_direction() {
        for dir in [PassDirection::Left, PassDirection::Right] {
            let json = serde_json::to_string(&dir).unwrap();
            let back: PassDirection = serde_json::from_str(&json).unwrap();
            assert_eq!(dir, back);
        }
    }

    #[test]
    fn serde_roundtrip_tournament_format() {
        for fmt in [TournamentFormat::Swiss, TournamentFormat::SingleElimination] {
            let json = serde_json::to_string(&fmt).unwrap();
            let back: TournamentFormat = serde_json::from_str(&json).unwrap();
            assert_eq!(fmt, back);
        }
    }

    #[test]
    fn serde_roundtrip_pod_policy() {
        for policy in [PodPolicy::Competitive, PodPolicy::Casual] {
            let json = serde_json::to_string(&policy).unwrap();
            let back: PodPolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(policy, back);
        }
    }

    #[test]
    fn serde_roundtrip_pick_status() {
        for status in [
            PickStatus::Pending,
            PickStatus::Picked,
            PickStatus::TimedOut,
            PickStatus::NotDrafting,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let back: PickStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    #[test]
    fn serde_roundtrip_spectator_visibility() {
        for vis in [SpectatorVisibility::Public, SpectatorVisibility::Omniscient] {
            let json = serde_json::to_string(&vis).unwrap();
            let back: SpectatorVisibility = serde_json::from_str(&json).unwrap();
            assert_eq!(vis, back);
        }
    }

    #[test]
    fn spectator_visibility_default_is_public() {
        assert_eq!(SpectatorVisibility::default(), SpectatorVisibility::Public);
    }

    #[test]
    fn displayed_addable_cards_match_the_selected_policy() {
        let custom = "Watery Grave";
        for (policy, should_display_custom) in [
            (DeckAddableCardPolicy::StandardBasics, false),
            (DeckAddableCardPolicy::CustomOnly, true),
            (DeckAddableCardPolicy::StandardBasicsPlusCustom, true),
        ] {
            let addable_cards = DeckAddableCards {
                policy,
                custom: vec![custom.to_string()],
            };

            assert_eq!(
                addable_cards
                    .display_names()
                    .iter()
                    .any(|name| name == custom),
                should_display_custom,
            );
            assert_eq!(addable_cards.is_addable(custom), should_display_custom);
        }
    }

    #[test]
    fn a_pre_multi_set_source_snapshot_restores_as_a_one_element_sequence() {
        // Snapshots written before multi-set drafts carried a single `code`.
        // A one-element sequence repeats for every pack, which is exactly what
        // that snapshot meant.
        let json = r#"{"type":"Set","data":{"code":"blb"}}"#;
        let source: DraftSource = serde_json::from_str(json).unwrap();

        assert_eq!(source, DraftSource::single_set("blb"));
        for pack in [0u8, 1, 2, 5] {
            assert_eq!(source.set_code_for_pack(pack), "blb");
        }
    }

    #[test]
    fn set_source_restores_both_legacy_code_spellings() {
        for json in [
            r#"{"type":"Set","data":{"code":"blb"}}"#,
            r#"{"type":"Set","data":{"codes":["blb"]}}"#,
        ] {
            let source: DraftSource = serde_json::from_str(json).unwrap();
            assert_eq!(source, DraftSource::single_set("blb"));
        }
    }

    #[test]
    fn set_layout_rejects_hybrid_and_unknown_shapes() {
        for json in [
            r#"{"type":"Set","data":{"candidate_codes":["TST"],"codes":["TST"]}}"#,
            r#"{"type":"Set","data":{"codes":["TST"],"unexpected":true}}"#,
            r#"{"type":"Set","data":{"candidate_codes":["TST"]}}"#,
        ] {
            assert!(serde_json::from_str::<DraftSource>(json).is_err(), "{json}");
        }
    }

    #[test]
    fn cube_source_serde_is_unchanged() {
        let source = DraftSource::Cube {
            id: "my-cube".to_string(),
            name: "My Cube".to_string(),
        };
        let json = serde_json::to_string(&source).unwrap();
        assert_eq!(serde_json::from_str::<DraftSource>(&json).unwrap(), source);
    }

    #[test]
    fn chaos_uses_actual_assignment_union_not_unselected_candidates() {
        let source = DraftSource::Set {
            layout: SetLayout::Chaos {
                candidate_codes: vec!["AAA".to_string(), "BBB".to_string()],
                assignments: vec![
                    vec!["BBB".to_string(), "BBB".to_string()],
                    vec!["bbb".to_string(), "BBB".to_string()],
                ],
            },
        };

        assert_eq!(source.actual_set_codes(), vec!["BBB"]);
        assert_eq!(source.set_code(), "BBB");
        assert_eq!(source.set_code_for_seat_and_pack(1, 0), "bbb");
        assert_eq!(
            serde_json::from_str::<DraftSource>(&serde_json::to_string(&source).unwrap()).unwrap(),
            source
        );
    }

    #[test]
    fn a_pack_sequence_source_reports_the_set_filling_each_pack() {
        let source = DraftSource::Set {
            layout: SetLayout::UniformByRound {
                codes: vec!["ISD".to_string(), "DKA".to_string(), "ISD".to_string()],
            },
        };

        assert_eq!(source.set_code_for_pack(0), "ISD");
        assert_eq!(source.set_code_for_pack(1), "DKA");
        assert_eq!(source.set_code_for_pack(2), "ISD");
        // Past the sequence, the last entry repeats.
        assert_eq!(source.set_code_for_pack(3), "ISD");
    }

    #[test]
    fn a_multi_set_source_label_lists_its_distinct_sets_in_pack_order() {
        let source = DraftSource::Set {
            layout: SetLayout::UniformByRound {
                codes: vec![
                    "ISD".to_string(),
                    "DKA".to_string(),
                    "ISD".to_string(),
                    "AVR".to_string(),
                ],
            },
        };

        assert_eq!(source.set_code(), "ISD+DKA+AVR");
        assert_eq!(DraftSource::single_set("BLB").set_code(), "BLB");
    }

    #[test]
    fn serde_roundtrip_multi_set_source() {
        let source = DraftSource::Set {
            layout: SetLayout::UniformByRound {
                codes: vec!["ISD".to_string(), "DKA".to_string(), "ISD".to_string()],
            },
        };
        let json = serde_json::to_string(&source).unwrap();
        assert_eq!(
            json,
            r#"{"type":"Set","data":{"codes":["ISD","DKA","ISD"]}}"#
        );
        let back: DraftSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, back);
    }

    #[test]
    fn entry_for_pack_repeats_the_last_entry_and_rejects_an_empty_sequence() {
        assert_eq!(entry_for_pack(&[10, 20], 0), Some(&10));
        assert_eq!(entry_for_pack(&[10, 20], 1), Some(&20));
        assert_eq!(entry_for_pack(&[10, 20], 9), Some(&20));
        assert_eq!(entry_for_pack::<u8>(&[], 0), None);
    }

    #[test]
    fn draft_config_missing_spectator_visibility_defaults_to_public() {
        // Backward compatibility: configs serialized before this field was added
        // should deserialize with Public visibility.
        let json = r#"{
            "set_code": "TST",
            "kind": "Premier",
            "cards_per_pack": 14,
            "pack_count": 3,
            "rng_seed": 42
        }"#;
        let config: DraftConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.spectator_visibility, SpectatorVisibility::Public);
    }
}
