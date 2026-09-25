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
    /// Seat is in a [`PackDistribution::SharedStackPiles`] draft and is NOT the
    /// active seat: it owes no decision until the turn passes to it.
    ///
    /// A distinct status rather than a reuse of `Picked`, because the
    /// pick-and-pass pair cannot describe a shared-stack seat at all: no seat
    /// ever holds a `current_pack` under this distribution, so the
    /// `current_pack[i].is_some()` test that separates `Pending` from `Picked`
    /// is `false` for EVERY seat and would report the whole pod as `Picked`
    /// while a turn is live. The active seat is `Pending`; every other seat is
    /// this.
    Waiting,
    /// Seat timed out (set by P2P host, not derivable from session state).
    TimedOut,
    /// Not in drafting phase (deckbuilding, match play, etc.).
    NotDrafting,
}

impl PickStatus {
    /// Every status, in declaration order. The [`DraftKind::ALL`] idiom: the
    /// serde round-trip folds this instead of a hand-written array, so a status
    /// added later cannot ship with its wire encoding uncovered.
    pub const ALL: [PickStatus; 5] = [
        PickStatus::Pending,
        PickStatus::Picked,
        PickStatus::Waiting,
        PickStatus::TimedOut,
        PickStatus::NotDrafting,
    ];
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
    /// Winston Draft: a two-player (up to four) draft from one shared
    /// face-down stack dealt through three take-or-decline piles. Any seat may
    /// be a bot: a bot seat's turn is decided from the same projection a human
    /// sees (`draft_wasm::bot_ai::winston_decision`) and driven by
    /// `draft_wasm::resolve_shared_stack_bot_turns`, and its decisions are
    /// adjudicated by `shared_stack::refusal_for` like anyone's.
    ///
    /// NO Comprehensive Rules section exists for this format; the procedural
    /// authority is Wizards of the Coast, "Casual Formats" (2008-08-11),
    /// <https://magic.wizards.com/en/news/feature/casual-formats-2008-08-11>.
    /// See [`PackDistribution::SharedStackPiles`] for the full grep-verified
    /// statement of why CR 905 and CR 903.13 do not apply.
    Winston,
}

/// How a draft kind's packs reach the seats.
///
/// This is the axis the `kind == DraftKind::Sealed` equality tests were really
/// testing. Consumers match on it exhaustively, so a new kind must *declare*
/// which shape it uses instead of silently falling into an `else` branch.
// `Deserialize` as well as `Serialize` because the player view carries this
// now, and every view type on this boundary round-trips: a persisted snapshot
// and a peer's frame both parse back into the same struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackDistribution {
    /// Packs are opened one at a time and passed around the pod.
    /// CR 905.1a describes this shape (one card per step, pass the remainder).
    PickAndPass,
    /// Every pack is handed to its own seat unopened; there is no pick step.
    AllAtOnce,
    /// One shared face-down main stack dealt through `pile_count`
    /// take-or-decline piles, one active seat at a time. Winston Draft is its
    /// first consumer; the axis is named for the SHAPE, so a future variant
    /// that differs only in the number of piles is a different `pile_count`
    /// rather than a sibling variant.
    ///
    /// Winston Draft has NO Comprehensive Rules section. Grep-verified against
    /// docs/MagicCompRules.txt: CR 905 is Conspiracy Draft and CR 903.13 is
    /// Commander Draft; neither covers a shared face-down stack dealt through
    /// take-or-decline piles, and no other section does. The procedural
    /// authority is Wizards of the Coast, "Casual Formats" (2008-08-11),
    /// <https://magic.wizards.com/en/news/feature/casual-formats-2008-08-11> --
    /// deliberately no CR citation, the same discipline
    /// [`PostDraftPlay::TournamentPairings`] applies to MTR tournament
    /// structure. Deck construction is the one part the CR does cover:
    /// CR 100.2b (40-card limited minimum) and CR 100.4b (the rest of the pool
    /// is the sideboard).
    ///
    /// The pile count lives in the payload rather than on [`DraftProcedure`]
    /// so that "a pile count exists iff piles exist" is a type-level fact: a
    /// flat field would be readable from `DraftKind::Premier.procedure()`,
    /// where it can never mean anything.
    SharedStackPiles { pile_count: u8 },
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
    /// [`PackDistribution::AllAtOnce`] and
    /// [`PackDistribution::SharedStackPiles`], neither of which has a pick step
    /// at all. `procedure()` is the authority for which shape a kind uses --
    /// read it rather than re-deriving the partition from this sentence.
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
    /// designate. `0` for every kind whose decks are not Commander decks -- the
    /// four CR 905.1a kinds and `Winston`; `1` for CommanderDraft, whose
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

    /// Can this distribution express a per-`(seat, round)` set assignment?
    ///
    /// [`SetLayout::Chaos`] says two things at once: each booster is drawn from
    /// its own set, and WHICH set reached WHICH `(seat, round)` stays private to
    /// the host. Under [`PackDistribution::SharedStackPiles`] every booster is
    /// opened unlooked-at and shuffled into one shared stack before the first
    /// decision, so no seat ever holds the packs generated for it — the first
    /// half of that statement names a distinction the shuffle has already
    /// erased, and the second leaves the players unable to learn what pool they
    /// are drafting. The same mixed pool remains available through
    /// [`SetLayout::UniformByRound`], which states its sets openly and is what
    /// a shared-stack pod takes.
    ///
    /// No CR: Winston Draft is a WotC casual format with no Comprehensive Rules
    /// section. See [`PackDistribution::SharedStackPiles`].
    ///
    /// This is the SHAPE-ONLY half of [`Self::validate_source`], for an
    /// admission boundary that holds a host's intent and has not resolved a
    /// [`DraftSource`] yet — a server that asks here refuses before it draws
    /// entropy and before it registers a lobby nothing could later start.
    /// Exhaustive deliberately: a future distribution must decide this question
    /// rather than inherit a permissive fallback.
    pub fn allows_chaos_layout(self) -> bool {
        self.allowed_set_layouts().contains(&SetLayoutKind::Chaos)
    }

    /// Every [`SetLayoutKind`] this procedure admits, most permissive first.
    ///
    /// THE SINGLE AUTHORITY for source-layout legality, and the published one:
    /// `DraftProcedure`'s DTO carries this list to the client, which renders it
    /// rather than deriving anything. [`Self::allows_chaos_layout`] and
    /// [`Self::validate_source`] both read it, so the refusal a host sees at
    /// admission, the refusal the reducer raises at `StartDraft`, and the
    /// options the setup page offers cannot disagree.
    ///
    /// Exhaustive over [`PackDistribution`] so a new distribution must state its
    /// answer rather than inherit one.
    pub fn allowed_set_layouts(self) -> &'static [SetLayoutKind] {
        match self.distribution {
            // A pack per seat per round is exactly what a Chaos assignment
            // addresses, so both shapes are expressible.
            PackDistribution::PickAndPass | PackDistribution::AllAtOnce => {
                &[SetLayoutKind::UniformByRound, SetLayoutKind::Chaos]
            }
            // A shared stack opens every booster into ONE pile before the first
            // decision, so there is no per-seat, per-round slot for a Chaos
            // assignment to fill and nothing a player could observe if there
            // were.
            PackDistribution::SharedStackPiles { .. } => &[SetLayoutKind::UniformByRound],
        }
    }

    /// Refuse a resolved pack source this distribution cannot express.
    ///
    /// The whole-source half of [`Self::allows_chaos_layout`], which carries the
    /// reasoning. A cube source generates packs exactly as a set source does and
    /// names no seats, so every distribution takes one.
    pub fn validate_source(self, source: &DraftSource) -> Result<(), DraftError> {
        let layout = match source {
            DraftSource::Set { layout } => layout,
            DraftSource::Cube { .. } => return Ok(()),
        };
        match layout {
            SetLayout::Chaos { .. } if !self.allows_chaos_layout() => {
                Err(DraftError::InvalidSharedStackConfiguration {
                    reason: CHAOS_LAYOUT_REFUSAL.to_string(),
                })
            }
            SetLayout::Chaos { .. } | SetLayout::UniformByRound { .. } => Ok(()),
        }
    }
}

/// The one sentence every boundary that refuses a Chaos layout says.
///
/// Shared so the reducer's refusal and an admission boundary's pre-resolution
/// refusal cannot drift into two different explanations of one rule. See
/// [`DraftProcedure::allows_chaos_layout`].
pub const CHAOS_LAYOUT_REFUSAL: &str =
    "a shared-stack draft shuffles every booster into one stack, so it takes a named pack \
     sequence rather than a Chaos assignment";

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

/// The largest `PackDistribution::SharedStackPiles::pile_count` over every
/// [`DraftKind`].
///
/// The session-independent half of the `DraftAction::SharedStackDecision`
/// payload bound in `server-core`'s `guard_draft_action_payload`, which
/// receives only the action and can never consult the session (so it cannot
/// check that the index is the ACTIVE pile -- `shared_stack::refusal_for` owns
/// that). Derived from the procedure table, not chosen: exactly the role
/// [`MAX_CARDS_PER_PICK`] plays for `DraftAction::Pick`, and
/// `max_shared_stack_piles_matches_procedure_table` folds [`DraftKind::ALL`]
/// and fails if this drifts.
///
/// No CR: Winston Draft has no Comprehensive Rules section (see
/// [`PackDistribution::SharedStackPiles`]). The `3` is WotC "Casual Formats"'s
/// "three stacks face down on the table", by way of the procedure table.
pub const MAX_SHARED_STACK_PILES: usize = 3;

/// How many [`SharedStackDecisionRecord`]s [`SharedStackState::history`] keeps.
///
/// 128 covers an entire 2-seat 90-card draft outright (MEASURED in this
/// worktree, driving the reducer to completion over four seeds x both walk
/// policies: 72 decisions declining first, 93 taking first -- the count is
/// policy-dependent and seed-independent), and the large majority of a 4-seat
/// 180-card one (MEASURED the same way: 138 and 183).
///
/// Deliberately NOT unbounded, and the bound is the point: a 4-seat cube
/// Winston admits thousands of decisions (`CubeDraftSettings::cards_per_pack`
/// is client-supplied with no product ceiling -- the same reason
/// [`SharedStackState::inspected`] is a `usize`), and this history rides every
/// `SharedStackView` broadcast and every persisted snapshot. This constant is
/// the knob that bounds that payload.
///
/// MEASURED wire cost at this capacity, rather than argued: a full 128-record
/// history serializes to 7142 JSON bytes, and a whole 3-pile `SharedStackView`
/// carrying one is 26536 bytes against 19396 without it -- 7140 bytes
/// attributable, about 27% of that broadcast.
/// `shared_stack::tests::history_at_capacity_has_a_measured_wire_size` prints
/// those three figures; re-run it before changing this number.
pub const SHARED_STACK_HISTORY_CAPACITY: usize = 128;

impl DraftKind {
    /// Every `DraftKind`, in declaration order.
    ///
    /// Folded over by `max_cards_per_pick_matches_procedure_table` (to derive
    /// [`MAX_CARDS_PER_PICK`]) and by `procedure_matches_legacy_accessors`.
    ///
    /// Hand-written, and the guarantees are worth stating precisely because
    /// they are narrower than "compiler-enforced": the wildcard-free `match` in
    /// `draft_kind_all_lists_every_variant` makes a seventh variant an `E0004`
    /// **there**, which enforces the *arm set*; the array type `[DraftKind; 6]`
    /// enforces the *length*; and the sorted-index assertion catches
    /// *duplication*. A future variant's **membership in this array** is
    /// enforced by nothing — a seventh variant that adds its `index_of` arm but
    /// is left out of `ALL` compiles and passes. The `E0004` lands the author
    /// beside this array, and that proximity is the actual guarantee.
    pub const ALL: [DraftKind; 6] = [
        DraftKind::Quick,
        DraftKind::Premier,
        DraftKind::Traditional,
        DraftKind::Sealed,
        DraftKind::CommanderDraft,
        DraftKind::Winston,
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
            // Winston Draft has NO Comprehensive Rules section. Grep-verified
            // against docs/MagicCompRules.txt: CR 905 is Conspiracy Draft and
            // CR 903.13 is Commander Draft; neither covers a shared face-down
            // stack dealt through take-or-decline piles, and no other section
            // does. The procedural authority for every axis below that is not
            // marked with a CR is Wizards of the Coast, "Casual Formats"
            // (2008-08-11),
            // https://magic.wizards.com/en/news/feature/casual-formats-2008-08-11
            // -- deliberately no CR citation, the same discipline
            // `PostDraftPlay::TournamentPairings` applies to MTR tournament
            // structure.
            DraftKind::Winston => DraftProcedure {
                // WotC: "the two players each supply three booster packs".
                pod_size: 2,
                // Equals `pod_size`, and this scalar ENFORCES NOTHING. A
                // shared-stack pod admits bot seats, exactly as
                // Premier/Traditional/Sealed do, through the host's opt-in
                // bot-fill toggle; what the value records is that a Winston pod
                // is DESIGNED around humans, the way `Quick`'s `1` records that
                // Quick is designed around one human plus bots.
                //
                // It is not a seat-composition rule and must not be read as
                // one -- it is a per-kind constant that stops matching the seat
                // count the moment a 4-seat pod is created. MEASURED
                // (`grep -rn human_seats crates/ client/src`): outside this
                // table its only consumers are `draft_procedure_dto`'s copy,
                // that DTO's TypeScript mirror, and tests; no production branch
                // dispatches on it, because the host dispatches on the
                // DISTRIBUTION instead.
                human_seats: 2,
                min_pod_size: 2,
                local_cube_min_pod_size: 2,
                // The requester's ceiling: "playable by up to four".
                max_pod_size: 4,
                // No widened local allowance: the local-cube ceiling exists for
                // pods a single player fills out with bots (Quick uses
                // `u8::MAX`), and the requester's ceiling for this format is
                // four seats however they are occupied.
                local_cube_max_pod_size: 4,
                // WotC: "the two players each supply three booster packs".
                packs_per_player: 3,
                // THIS AXIS DOES NOT DESCRIBE A WINSTON TURN. A take moves
                // however many cards the pile holds and a decline moves one or
                // zero; the turn's card count is `SharedStackState`'s, never
                // this field's. `1` keeps `MAX_CARDS_PER_PICK` at 2 (which
                // bounds `DraftAction::Pick`, an action Winston never sends)
                // and keeps `pick_steps_per_pack`'s divisor >= 1.
                cards_per_pick: 1,
                // THIS AXIS DOES NOT DESCRIBE A WINSTON TURN either: you take a
                // whole pile, you do not select cards from it. Winston
                // publishes no pick step, so the axis has no consumer here.
                pick_selection_mode: PickSelectionMode::Direct,
                // WotC: "Player A sets the top three cards from the main pile
                // in three stacks face down on the table."
                distribution: PackDistribution::SharedStackPiles { pile_count: 3 },
                // CR 100.2b: limited decks have a 40-card minimum.
                min_deck_size: 40,
                // Ordinary cube floor, same as Premier: Winston-from-cube is
                // permitted.
                cube_min_deck_size: 1,
                // CR 903.3 is scoped to the Commander variant by CR 903.13f;
                // Winston is outside it, so its decks designate nothing.
                commanders_required: 0,
                // Not `CompleteImmediately`: that runs no in-session pairings,
                // and `commanders_required: 0` authorizes no Commander pod
                // launch, so the pod would reach `Deckbuilding` with no path to
                // a match at all. `TournamentPairings` reuses the existing
                // pairing machinery exactly as a 2-seat Premier pod does.
                post_draft_play: PostDraftPlay::TournamentPairings,
                // WotC does not specify. `Bo1` is stated here as the in-tree
                // default (Quick/Premier/Sealed/CommanderDraft), not as a rules
                // claim.
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
    /// designate. `0` for every kind whose decks are not Commander decks -- the
    /// four CR 905.1a kinds and `Winston`.
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

/// Which SHAPES of [`SetLayout`] a procedure admits, as a published capability.
///
/// The discriminant of `SetLayout` without its payload, so a client can be told
/// what it may offer without being told how to build it. `DraftProcedure`
/// publishes the list (`allowed_set_layouts`), the setup page renders exactly
/// that list, and nothing outside the engine decides which layouts a kind takes.
///
/// This replaced the client asking `isSharedStackDistribution(distribution)` and
/// concluding "then no Chaos". That was a second authority over source legality
/// -- correct, but derived from the wrong input and free to drift the moment a
/// distribution is added or the rule stops tracking the distribution at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetLayoutKind {
    /// One set per round for the entire pod.
    UniformByRound,
    /// A private per-`(seat, round)` assignment drawn from a candidate pool.
    Chaos,
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

/// Live state of a [`PackDistribution::SharedStackPiles`] draft.
///
/// One cohesive sub-struct rather than six loose fields on [`DraftSession`],
/// because the fields share one invariant and are meaningless individually: a
/// pile is refilled the instant it is taken, so WHILE THE MAIN STACK IS
/// NON-EMPTY EVERY PILE IS NON-EMPTY. State with a joint invariant is
/// constructed and mutated together.
///
/// No CR: Winston Draft has no Comprehensive Rules section. See
/// [`PackDistribution::SharedStackPiles`] for the grep-verified statement and
/// the WotC procedural authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedStackState {
    /// The face-down main stack. **The LAST element is the top**, so a draw is
    /// `pop()` -- O(1), and "the top card of the main stack" has exactly one
    /// spelling in `crate::shared_stack`.
    ///
    /// Its ORDER is never published to anyone: it is the `rng_seed`'s secret,
    /// and publishing it would make every pile predictable.
    pub main_stack: Vec<DraftCardInstance>,
    /// The piles, index 0 leftmost. Length is always the distribution's
    /// `pile_count`.
    pub piles: Vec<Vec<DraftCardInstance>>,
    /// The seat that drafted first, LATCHED at `StartDraft`. Never re-derived:
    /// `active_seat` moves every turn, so it cannot answer who started. This is
    /// what `play_first_chooser` is derived from.
    pub starting_seat: u8,
    /// The seat whose decision the reducer will accept.
    pub active_seat: u8,
    /// The pile `active_seat` must decide on: 0 at turn start, +1 per decline
    /// THAT ADVANCES TO A LATER PILE. A final-pile decline does not advance
    /// it -- that decline takes the forced draw and ENDS the turn, which
    /// resets this to 0 along with `inspected`. Stated precisely because the
    /// increment site is inside the "a later pile exists" branch, and "+1 per
    /// decline" alone would describe a different reducer.
    pub cursor: u8,
    /// Cards of `piles[i]` that `active_seat` has already looked at THIS TURN.
    /// Declines APPEND, so the seen set is always a prefix and a count
    /// suffices.
    ///
    /// CONTRACT, and it has exactly TWO write sites, both mandatory:
    ///   1. turn start -- zeroed, then `inspected[0] = piles[0].len()`;
    ///   2. cursor advance (every decline that does not end the turn) --
    ///      `inspected[new_cursor] = piles[new_cursor].len()`.
    ///
    /// Omitting (2) leaves the active seat's view publishing an empty
    /// `revealed` for the pile it is inspecting.
    ///
    /// STORED, not derived, and the near-miss is where a hidden-information bug
    /// would live: for `i < cursor` the revealed prefix is `piles[i].len()`
    /// minus the cards a decline added, and a decline adds one only while the
    /// stack lasts -- so if the stack is empty now, how many of this turn's
    /// declines drew is NOT recoverable from current state, and that is exactly
    /// the endgame the adjudication governs.
    ///
    /// `usize`, matching `Vec::len`, NOT `u8`: `CubeDraftSettings.cards_per_pack`
    /// is client-supplied with no product ceiling, so a 4-seat cube Winston
    /// admits up to 4 x 3 x 255 cards and a pile grows by one per decline
    /// without bound. There is no `<= 255` bound to prove, so there is no
    /// narrowing to do.
    pub inspected: Vec<usize>,
    /// Decisions applied since `StartDraft` -- EVERY applied decision, not
    /// every completed turn. Monotone, and moved by exactly one site: a
    /// refused decision returns before it and leaves the session
    /// byte-identical.
    ///
    /// ONE MEANING, deliberately. This is the client's acknowledgement
    /// predicate (`after.shared_stack.decisions > before.shared_stack.decisions`)
    /// for a decision that names no cards, and a NON-FINAL DECLINE is exactly
    /// that case: it adds nothing to any pool, so this counter is the only
    /// thing an acknowledging store can watch. It is therefore NOT a turn
    /// number -- a turn costs between one and `pile_count` decisions, so the
    /// two can never be the same field.
    ///
    /// A progress display that wants "turn N" must derive it separately (a
    /// turn counter of its own, or `pools` totals); that is a named deferral,
    /// not a second meaning of this counter. The host timer's
    /// decision-window identity is a second named deferral, and it is the
    /// per-DECISION reading it needs -- which is why this is a `u32` and not a
    /// `u8`.
    pub decisions: u32,
    /// The most recent [`SHARED_STACK_HISTORY_CAPACITY`] applied decisions,
    /// OLDEST FIRST. Public table information, and nothing else: see
    /// [`SharedStackDecisionRecord`] for what is in a record and, more
    /// importantly, what must never be added to one.
    ///
    /// BOUNDED, because a 4-seat cube Winston admits thousands of decisions
    /// (`cards_per_pack` is client-supplied with no product ceiling -- see
    /// [`Self::inspected`]'s own note) and this rides every view and every
    /// snapshot. Dropping the oldest is not lossy for its consumers: a
    /// pile-size read is a read on what a seat is doing NOW, and the
    /// pass-colour reconstruction discards any record older than the last
    /// `Take` on that pile anyway.
    ///
    /// `#[serde(default)]` so a snapshot persisted before this field existed
    /// loads with an empty history instead of failing `import_draft_session`.
    /// Only local snapshots can predate it -- the wire versions carrying it are
    /// unreleased upstream.
    #[serde(default)]
    pub history: Vec<SharedStackDecisionRecord>,
    /// Per seat, the card that seat's most recent FORCED DRAW gave it, retained
    /// until that seat decides again. `None` for a seat that has not taken one
    /// since its last decision. Indexed by seat; length is the pod size.
    ///
    /// THE ONE CARD A PLAYER RECEIVES WITHOUT SEEING IT. Every other card a
    /// seat drafts was face up in the pile it took, so the seat watched it
    /// arrive; the card a final-pile decline draws off the top of the main
    /// stack is, by the format's own rule, taken "no matter what it is" --
    /// sight unseen. Without a record of it the player is never told what they
    /// got, and could only find it by hunting their pool for a card they do not
    /// remember.
    ///
    /// STORED rather than derived, and the near-miss is instructive: a client
    /// CAN diff its pool across the acknowledgement and find the added card,
    /// but that makes the display layer compute a fact about the game, and it
    /// answers nothing for a view that arrives any other way -- a reconnect, a
    /// restored snapshot, or the host deciding for a timed-out seat. The engine
    /// knows which card it drew; nobody else should have to work it out.
    ///
    /// NOT PUBLIC, unlike every other field on this type. This is the only
    /// shared-stack state that is private to ONE seat: the drawn card goes
    /// straight into that seat's pool, and a pool is not public. The projection
    /// (`view::shared_stack_view`) publishes a seat's entry to that seat alone.
    ///
    /// `#[serde(default)]` so a snapshot persisted before this field existed
    /// loads with no pending notices rather than failing `import_draft_session`;
    /// `validate_persisted_snapshot` then holds it to the pod's seat count.
    #[serde(default)]
    pub forced_draws: Vec<Option<DraftCardInstance>>,
}

/// Take the pile, or put it back.
///
/// NOT a `bool`: a decision axis gets a named type, and a named axis is what a
/// refusal, a delta and an i18n key can all key on. Two sibling actions
/// (`TakePile` / `DeclinePile`) is the same raw-bool smell wearing an enum's
/// clothes, and would make the legality predicate answer two functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SharedStackPileDecision {
    Take,
    Decline,
}

impl SharedStackPileDecision {
    /// Every decision, in declaration order. The [`DraftKind::ALL`] idiom:
    /// folded by `shared_stack::forced_decision` and by the view's totality
    /// test, so neither can go narrow when the axis grows.
    pub const ALL: [SharedStackPileDecision; 2] = [
        SharedStackPileDecision::Take,
        SharedStackPileDecision::Decline,
    ];
}

/// One seat's decision on one pile, and how tall that pile was when they made
/// it.
///
/// PUBLIC INFORMATION: at a physical table everyone watches a player pick a
/// pile up, weigh it and put it back, and every pile's HEIGHT is visible across
/// the table (the same reason `SharedStackPileView::total` is published to
/// every viewer).
///
/// WHAT IS NOT HERE, and must never be added: the pile's CONTENTS. This record
/// is the one place a future author might reach for them, and they are the
/// format's only secret. A consumer that wants to know WHICH cards a seat
/// passed reconstructs them from ITS OWN published `revealed` prefix plus
/// `pile_size` -- exactly the information a player at the table has.
///
/// Every field is `Copy`, so the record is a plain value and a fold over
/// [`SharedStackState::history`] never clones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedStackDecisionRecord {
    /// The seat that decided.
    pub seat: u8,
    /// The pile it decided on, addressed by the engine's own pile index (the
    /// same index `SharedStackPileView::index` publishes), never by a position
    /// in some vector.
    pub pile: u8,
    /// Take it, or put it back.
    pub decision: SharedStackPileDecision,
    /// The pile's height at the moment of the decision, captured BEFORE the
    /// decision mutated the pile. For a `Decline` that is exactly the prefix
    /// the deciding seat had looked at, which is what makes the record useful
    /// without it carrying a card.
    pub pile_size: usize,
}

/// Every reason a shared-stack decision can be refused.
///
/// ONE vocabulary for the reducer's refusal and the view's publication, so the
/// two can never disagree. `shared_stack::refusal_for` is the single authority
/// that produces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SharedStackRefusal {
    /// Not this seat's turn, or not the pile the cursor is on.
    PileNotActive,
    /// An empty pile cannot be taken.
    PileEmpty,
    /// A decline must still guarantee THIS seat a card this turn, and neither a
    /// later pile nor two main-stack cards remain.
    NoGuaranteedCard,
}

/// Actions that can be performed on a draft session.
///
/// STANDING REFACTOR NOTE: `Pick`, `PickWithDraftEffect` and
/// `SharedStackDecision` are three shapes of "this seat commits to cards this
/// step". A FOURTH pick-shaped action should open a `PickSelection`
/// parameterization round rather than add a fifth sibling -- three is the
/// sibling-cluster threshold, and the third is the last one that is cheap.
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
    ///
    /// `Winston` reports `1` on that axis but takes NO PICK STEP AT ALL: under
    /// [`PackDistribution::SharedStackPiles`] a turn is a whole-pile decision,
    /// so this action is refused and [`DraftAction::SharedStackDecision`] is
    /// the one that moves cards.
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
    /// One whole shared-stack turn decision.
    ///
    /// The forced "take the top card of the main stack" that follows a
    /// final-pile decline is NOT an action -- it is the engine-owned
    /// consequence of that decline.
    ///
    /// No CR: see [`PackDistribution::SharedStackPiles`].
    SharedStackDecision {
        seat: u8,
        /// The pile the client believes it is deciding on. The cursor is the
        /// ENGINE's; this is an optimistic-concurrency check, not a selector.
        /// Without it, a double-send during a laggy turn (two `Decline` frames
        /// from one click) would apply the second to the NEXT pile silently;
        /// with it, the second frame is refused
        /// [`SharedStackRefusal::PileNotActive`] and nothing moves.
        pile: u8,
        decision: SharedStackPileDecision,
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
    /// One shared-stack turn decision was applied. Names no cards: a decline
    /// moves one or zero, and a take moves however many the pile held, so the
    /// card movement is read from the session/view rather than replayed here.
    SharedStackDecisionApplied {
        seat: u8,
        pile: u8,
        decision: SharedStackPileDecision,
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
    #[error("invalid shared-stack configuration: {reason}")]
    InvalidSharedStackConfiguration { reason: String },
    /// The reducer's rendering of `shared_stack::refusal_for`'s verdict. The
    /// predicate is the single authority; this error never re-derives it.
    #[error("seat {seat} may not act on pile {pile}: {reason:?}")]
    SharedStackDecisionRefused {
        seat: u8,
        pile: u8,
        reason: SharedStackRefusal,
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
    /// Original source name multiset, including copies and entries never dealt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub booster_pack_pool: Option<Vec<String>>,
    /// Shared-stack pile state. `None` for every distribution without piles,
    /// which is why `skip_serializing_if` is SEMANTICS and not only a
    /// compatibility shim -- the same shape `booster_pack_pool` above uses.
    ///
    /// Read through [`DraftSession::shared_stack`], which turns `None` into a
    /// `DraftError` so no call site unwraps. RETAINED, not cleared, at the
    /// `Drafting -> Deckbuilding` transition: it is the conservation evidence a
    /// completed draft's audits read, `validate_persisted_snapshot` keys its
    /// rules on it, and `pick_pass::finish_pick` sets the precedent by leaving
    /// `current_pack` as `Some(empty)` at the same transition. The VIEW is what
    /// is status-gated, not the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_stack: Option<SharedStackState>,
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
    /// THE PUBLISHED CAPABILITY AND THE REFUSAL ARE ONE ANSWER.
    ///
    /// `allowed_set_layouts` is what the setup page renders; `validate_source`
    /// is what the reducer enforces at `StartDraft`; `allows_chaos_layout` is
    /// what the server's admission guard asks before it draws entropy. Three
    /// readers, and a client that offers a layout either of the other two would
    /// refuse produces a lobby players can join and never start -- which is the
    /// exact failure the pre-entropy guard exists to prevent.
    ///
    /// Walked over every kind, so a kind added later cannot skip the agreement.
    /// The counters are measured, not assumed: some kind must admit Chaos and
    /// some kind must refuse it, or this test is passing vacuously over a table
    /// that says the same thing everywhere.
    #[test]
    fn every_kind_publishes_the_layout_capability_its_reducer_enforces() {
        let mut admitting = 0usize;
        let mut refusing = 0usize;

        for kind in super::DraftKind::ALL {
            let procedure = kind.procedure();
            let published = procedure.allowed_set_layouts();

            // Uniform is the floor: a procedure that admits NO layout could
            // never start at all, and the setup page would render an empty
            // choice.
            assert!(
                published.contains(&super::SetLayoutKind::UniformByRound),
                "{kind:?} publishes no uniform layout, so no source could start it"
            );

            let admits_chaos = published.contains(&super::SetLayoutKind::Chaos);
            assert_eq!(
                admits_chaos,
                procedure.allows_chaos_layout(),
                "{kind:?}: the published list and `allows_chaos_layout` disagree"
            );

            // And the reducer itself, through a real source rather than a flag.
            let chaos = super::DraftSource::Set {
                layout: super::SetLayout::Chaos {
                    candidate_codes: vec!["TST".to_string()],
                    assignments: vec![vec!["TST".to_string(); 3]; usize::from(procedure.pod_size)],
                },
            };
            assert_eq!(
                procedure.validate_source(&chaos).is_ok(),
                admits_chaos,
                "{kind:?}: the reducer and the published list disagree"
            );

            // The uniform paired positive, so "refuses everything" cannot pass.
            let uniform = super::DraftSource::Set {
                layout: super::SetLayout::UniformByRound {
                    codes: vec!["TST".to_string()],
                },
            };
            assert!(
                procedure.validate_source(&uniform).is_ok(),
                "{kind:?} refuses a uniform source"
            );

            if admits_chaos {
                admitting += 1;
            } else {
                refusing += 1;
            }
        }

        assert!(
            admitting > 0,
            "no kind admits Chaos, so the agreement is vacuous"
        );
        assert!(
            refusing > 0,
            "no kind refuses Chaos, so the agreement is vacuous"
        );
    }

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
    /// wildcard-free, so a seventh `DraftKind` is an `E0004` **here**, which
    /// lands the author beside the array that must list it; the array type
    /// `[DraftKind; 6]` enforces the length; and the sorted-index equality
    /// catches a duplicated entry. It does **not** enforce that a new variant
    /// is added to `ALL` — a variant that adds its arm below and is omitted
    /// from the array still compiles and still passes. The `E0004`'s proximity
    /// is the guarantee, not the assertion.
    ///
    /// The index assertion itself is DERIVED from `DraftKind::ALL.len()`, not a
    /// literal: a hand-written expectation is a sentinel the next widening
    /// invalidates silently.
    #[test]
    fn draft_kind_all_lists_every_variant() {
        fn index_of(kind: DraftKind) -> usize {
            match kind {
                DraftKind::Quick => 0,
                DraftKind::Premier => 1,
                DraftKind::Traditional => 2,
                DraftKind::Sealed => 3,
                DraftKind::CommanderDraft => 4,
                DraftKind::Winston => 5,
            }
        }
        let mut indices: Vec<usize> = DraftKind::ALL.into_iter().map(index_of).collect();
        indices.sort_unstable();
        // DERIVED, not a literal: a hand-written `[0, 1, 2, 3, 4]` is a
        // sentinel the next widening invalidates silently. Expressing the
        // expectation as the array's own length means this class of literal
        // cannot recur.
        assert_eq!(
            indices,
            (0..DraftKind::ALL.len()).collect::<Vec<_>>(),
            "DraftKind::ALL must list every variant exactly once"
        );
    }

    /// V1. A table-ROW shape test by nature; V3-V7 in `shared_stack` carry the
    /// behavior. Every axis is asserted against the value Fork 7's table states
    /// with its own reason, so a transposed pair (`pod_size` 2 <-> 3, a 60-card
    /// minimum) reddens here.
    ///
    /// WITHIN-ROW COLLISION, named rather than papered over: `pod_size`,
    /// `human_seats` and `min_pod_size` are all `2` in this row, so this test
    /// alone cannot catch a transposition among those three.
    /// `draft_procedure_dto_copies_every_axis_unmoved`'s fold over
    /// `DraftKind::ALL` is what does, because the columns stay pairwise
    /// distinct over the whole table.
    #[test]
    fn winston_procedure_row_states_the_format_rules() {
        let procedure = DraftKind::Winston.procedure();
        assert_eq!(procedure.pod_size, 2);
        assert_eq!(procedure.human_seats, 2);
        assert_eq!(procedure.min_pod_size, 2);
        assert_eq!(procedure.local_cube_min_pod_size, 2);
        assert_eq!(procedure.max_pod_size, 4);
        assert_eq!(procedure.local_cube_max_pod_size, 4);
        assert_eq!(procedure.packs_per_player, 3);
        assert_eq!(procedure.cards_per_pick, 1);
        assert_eq!(procedure.pick_selection_mode, PickSelectionMode::Direct);
        assert_eq!(
            procedure.distribution,
            PackDistribution::SharedStackPiles { pile_count: 3 }
        );
        assert_eq!(procedure.min_deck_size, 40);
        assert_eq!(procedure.cube_min_deck_size, 1);
        assert_eq!(procedure.commanders_required, 0);
        assert_eq!(procedure.post_draft_play, PostDraftPlay::TournamentPairings);
        assert_eq!(procedure.match_config.match_type, MatchType::Bo1);
        // Non-vacuity sibling: `procedure()` must actually read `self`.
        assert_ne!(
            DraftKind::Winston.procedure().distribution,
            DraftKind::Premier.procedure().distribution
        );
    }

    /// V2. `MAX_SHARED_STACK_PILES` is DERIVED from the procedure table, not
    /// chosen: hardcoding it would let a future 4-pile variant silently exceed
    /// the wire bound `guard_draft_action_payload` enforces.
    #[test]
    fn max_shared_stack_piles_matches_procedure_table() {
        let mut observed_pile_counts = 0usize;
        let mut max_pile_count = 0usize;
        for kind in DraftKind::ALL {
            match kind.procedure().distribution {
                PackDistribution::SharedStackPiles { pile_count } => {
                    observed_pile_counts += 1;
                    max_pile_count = max_pile_count.max(usize::from(pile_count));
                }
                PackDistribution::PickAndPass | PackDistribution::AllAtOnce => {}
            }
        }
        // Reach-guard: a fold that observed no pile-bearing kind would assert
        // `0 == MAX_SHARED_STACK_PILES` against a constant nobody set.
        assert!(
            observed_pile_counts >= 1,
            "the fold must observe at least one shared-stack kind"
        );
        assert_eq!(max_pile_count, MAX_SHARED_STACK_PILES);
    }

    /// V22 (the action/kind half). The serialized shapes the wire and the i18n
    /// key path both depend on.
    #[test]
    fn winston_action_and_kind_round_trip() {
        assert_eq!(
            serde_json::to_string(&DraftKind::Winston).unwrap(),
            "\"Winston\""
        );
        for decision in SharedStackPileDecision::ALL {
            let json = serde_json::to_string(&decision).unwrap();
            let back: SharedStackPileDecision = serde_json::from_str(&json).unwrap();
            assert_eq!(decision, back);
        }
        // PascalCase with no boundary conversion: wire == lookup key.
        assert_eq!(
            serde_json::to_string(&SharedStackPileDecision::Take).unwrap(),
            "\"Take\""
        );
        assert_eq!(
            serde_json::to_string(&SharedStackPileDecision::Decline).unwrap(),
            "\"Decline\""
        );
        for refusal in [
            SharedStackRefusal::PileNotActive,
            SharedStackRefusal::PileEmpty,
            SharedStackRefusal::NoGuaranteedCard,
        ] {
            let json = serde_json::to_string(&refusal).unwrap();
            let back: SharedStackRefusal = serde_json::from_str(&json).unwrap();
            assert_eq!(refusal, back);
        }
        let action = DraftAction::SharedStackDecision {
            seat: 1,
            pile: 2,
            decision: SharedStackPileDecision::Decline,
        };
        let json = serde_json::to_string(&action).unwrap();
        let back: DraftAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, back);
        let delta = DraftDelta::SharedStackDecisionApplied {
            seat: 1,
            pile: 2,
            decision: SharedStackPileDecision::Take,
        };
        let delta_json = serde_json::to_string(&delta).unwrap();
        let delta_back: DraftDelta = serde_json::from_str(&delta_json).unwrap();
        assert_eq!(delta, delta_back);
        // The distribution's externally tagged struct-variant shape, which the
        // TypeScript union mirrors. Unit variants are unchanged.
        assert_eq!(
            serde_json::to_string(&PackDistribution::SharedStackPiles { pile_count: 3 }).unwrap(),
            "{\"SharedStackPiles\":{\"pile_count\":3}}"
        );
        assert_eq!(
            serde_json::to_string(&PackDistribution::PickAndPass).unwrap(),
            "\"PickAndPass\""
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
        // Folds `DraftKind::ALL` rather than a hand-written array. The previous
        // hand-written `[Quick, Premier, Traditional, Sealed]` was already
        // non-total at base (it omitted `CommanderDraft`), which is this
        // class's silent failure mode: the test compiles, passes, and stops
        // covering the new wire value.
        for kind in DraftKind::ALL {
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
        // Folds `PickStatus::ALL` rather than a hand-written array: the
        // hand-written form was already the shape that silently goes narrow at
        // the next widening (§C7), and `Waiting` is that next widening.
        for status in PickStatus::ALL {
            let json = serde_json::to_string(&status).unwrap();
            let back: PickStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
        // Reach-guard: the fold must actually observe every declared variant,
        // so a truncated `ALL` cannot make this test pass by covering nothing.
        assert_eq!(PickStatus::ALL.len(), 5);
        assert_eq!(
            serde_json::to_string(&PickStatus::Waiting).unwrap(),
            "\"Waiting\""
        );
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
