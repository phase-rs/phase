use engine::types::player::PlayerId;
use serde::{Deserialize, Serialize};

use crate::pick_pass::required_pick_count;
use crate::session::{concession_set_codes, session_concessions};
use crate::types::*;
// Deep-path import by design: `engine::game::mod` re-exports `deck_validation`'s
// public surface, but this phase must not edit that file.
use engine::game::deck_validation::GrantableCommanderFiller;
use engine::types::match_config::MatchConfig;

/// A single entry in the standings table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StandingEntry {
    pub seat_index: u8,
    pub display_name: String,
    pub match_wins: u8,
    pub match_losses: u8,
    pub game_wins: u8,
    pub game_losses: u8,
}

/// A pairing visible to all players for the current round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingView {
    pub round: u8,
    pub table: u8,
    pub seat_a: u8,
    pub name_a: String,
    pub seat_b: u8,
    pub name_b: String,
    pub match_id: String,
    pub status: PairingStatus,
    pub winner_seat: Option<u8>,
    /// Game wins for seat A in the current match (Bo3 tracking).
    pub score_a: Option<u8>,
    /// Game wins for seat B in the current match (Bo3 tracking).
    pub score_b: Option<u8>,
}

/// Public seat info visible to all players.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeatPublicView {
    pub seat_index: u8,
    pub display_name: String,
    pub is_bot: bool,
    pub connected: bool,
    pub has_submitted_deck: bool,
    pub pick_status: PickStatus,
    /// Engine-owned presence signal for a pack a seat is actively drafting.
    ///
    /// This deliberately exposes only `0` or `1`, never the pack's card
    /// count or identity. It is `1` precisely while the session is drafting
    /// and this seat has a nonempty current pack that it has not picked from in
    /// the current round; otherwise it is `0`.
    pub active_pack_count: u8,
    /// CR 905.2c: Draft cards that remain face up are visible to every player.
    pub face_up_draft_cards: Vec<DraftCardInstance>,
}

/// A stable, engine-defined category for a limited pool display group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftPoolGroupKind {
    White,
    Blue,
    Black,
    Red,
    Green,
    Multicolor,
    Colorless,
    Creature,
    Instant,
    Sorcery,
    Enchantment,
    Artifact,
    Planeswalker,
    Land,
    Other,
    Mythic,
    Rare,
    Uncommon,
    Common,
    /// Rarities outside the standard four (Scryfall "special" / "bonus").
    RarityOther,
    ManaValue0,
    ManaValue1,
    ManaValue2,
    ManaValue3,
    ManaValue4,
    ManaValue5,
    ManaValue6Plus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftRarityGroupKind {
    Mythic,
    Rare,
    Uncommon,
    Common,
    RarityOther,
}

/// One distinct card and the number of copies in a pool group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftPoolEntry {
    pub card: DraftCardInstance,
    pub count: usize,
    /// Every collapsed copy's instance id, in entry order. The collapse keys on
    /// the NAME, but same-name instances can differ in classification on
    /// another axis (a reprint at a different rarity), so a consumer that
    /// filters or addresses copies must key on these ids — the representative
    /// `card.instance_id` speaks for only one of them (#7546 review).
    /// `default` keeps pre-v11 serialized entries deserializable; the client
    /// normalizer upgrades an empty list to the representative id.
    #[serde(default)]
    pub instance_ids: Vec<String>,
}

/// One ordered display group in a limited pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftPoolGroup {
    pub kind: DraftPoolGroupKind,
    /// Number of physical cards before duplicate entries are collapsed.
    pub total: usize,
    pub cards: Vec<DraftPoolEntry>,
}

/// WUBRG card totals for the pool header. Multicolor cards count toward every
/// color they contain.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftPoolColorCounts {
    pub white: usize,
    pub blue: usize,
    pub black: usize,
    pub red: usize,
    pub green: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DraftWorkspaceCapabilities {
    pub rarity_group_order: Option<Vec<DraftRarityGroupKind>>,
}

impl<'de> Deserialize<'de> for DraftWorkspaceCapabilities {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireCapabilities {
            #[serde(deserialize_with = "deserialize_required_nullable")]
            rarity_group_order: Option<Vec<DraftRarityGroupKind>>,
        }

        let wire = WireCapabilities::deserialize(deserializer)?;
        Ok(Self {
            rarity_group_order: wire.rarity_group_order,
        })
    }
}

fn deserialize_required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftWorkspaceRowClassification {
    pub creature_instance_ids: Vec<String>,
    pub noncreature_instance_ids: Vec<String>,
}

/// Pre-grouped, ordered presentation data for a player's limited pool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftPoolGroups {
    pub color_groups: Vec<DraftPoolGroup>,
    pub type_groups: Vec<DraftPoolGroup>,
    pub cmc_groups: Vec<DraftPoolGroup>,
    /// Rarity is the fourth engine-owned pool axis (#7507), so a display never
    /// has to re-derive it from the instance's raw `rarity` string.
    /// `default` keeps pre-v11 serialized views (no rarity axis) deserializable.
    #[serde(default)]
    pub rarity_groups: Vec<DraftPoolGroup>,
    /// Engine-owned option list for a type-filter control: every type bucket
    /// any pool member belongs to (CR 205.2b: multi-valued), in engine order.
    /// The exclusive `type_groups` axis stays a presentation/sorting shape.
    /// `default` keeps pre-v11 serialized views deserializable.
    #[serde(default)]
    pub type_filter_options: Vec<DraftPoolGroupKind>,
    /// Engine-owned option list for a color-filter control: every color bucket
    /// any pool member belongs to (CR 105.2: one or more colors), in engine
    /// order. The exclusive `color_groups` axis stays a presentation shape.
    /// `default` keeps pre-v11 serialized views deserializable.
    #[serde(default)]
    pub color_filter_options: Vec<DraftPoolGroupKind>,
    pub color_counts: DraftPoolColorCounts,
    #[serde(default)]
    pub workspace_capabilities: DraftWorkspaceCapabilities,
    #[serde(default)]
    pub workspace_row_classification: DraftWorkspaceRowClassification,
}

/// A source description that may cross a player or spectator boundary.
///
/// This is deliberately distinct from [`DraftSource`]. A persisted Chaos
/// source contains every seat's future booster assignments; that authoritative
/// layout belongs to the host snapshot, never to an ordinary draft view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DraftSourceView {
    Set { layout: SetLayoutView },
    Cube { id: String, name: String },
}

/// The view-safe portion of a set source.
///
/// `Chaos` records the candidate intent everywhere, the viewer's currently
/// opened booster while drafting, and their completed sequence only after all
/// picks are over. It intentionally has no representation for another seat's
/// assignment or any unopened booster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetLayoutView {
    UniformByRound {
        codes: Vec<String>,
    },
    Chaos {
        candidate_codes: Vec<String>,
        current_pack_code: Option<String>,
        completed_own_pack_codes: Option<Vec<String>>,
        actual_set_codes: Option<Vec<String>>,
    },
}

impl DraftPoolGroups {
    /// Builds the engine-owned ordering, grouping, and duplicate counts for a
    /// limited pool display.
    pub fn from_pool(pool: &[DraftCardInstance], source: &DraftSource) -> Self {
        let (rarity_groups, rarity_group_order) = match source {
            DraftSource::Set { .. } => (
                source_order_groups_for(pool, &RARITY_GROUP_ORDER, rarity_group),
                Some(RARITY_CAPABILITY_ORDER.to_vec()),
            ),
            DraftSource::Cube { .. } => (Vec::new(), None),
        };
        let mut creature_instance_ids = Vec::new();
        let mut noncreature_instance_ids = Vec::new();
        for card in pool {
            if type_memberships(card).contains(&DraftPoolGroupKind::Creature) {
                creature_instance_ids.push(card.instance_id.clone());
            } else {
                noncreature_instance_ids.push(card.instance_id.clone());
            }
        }

        Self {
            color_groups: groups_for(pool, &COLOR_GROUP_ORDER, color_group, true),
            type_groups: groups_for(pool, &TYPE_GROUP_ORDER, type_group, true),
            cmc_groups: groups_for(pool, &CMC_GROUP_ORDER, mana_value_group, false),
            rarity_groups,
            type_filter_options: type_filter_options(pool),
            color_filter_options: color_filter_options(pool),
            color_counts: color_counts(pool),
            workspace_capabilities: DraftWorkspaceCapabilities { rarity_group_order },
            workspace_row_classification: DraftWorkspaceRowClassification {
                creature_instance_ids,
                noncreature_instance_ids,
            },
        }
    }
}

/// Typed filter contract for a limited-pool display (#7546 review): the
/// display sends WHAT it asks for; the engine decides WHICH instances match.
/// An empty axis does not constrain; within an axis selections OR, across
/// axes they AND; `query` is a case-insensitive name substring.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolFilter {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub types: Vec<DraftPoolGroupKind>,
    #[serde(default)]
    pub colors: Vec<DraftPoolGroupKind>,
    #[serde(default)]
    pub rarities: Vec<DraftPoolGroupKind>,
}

impl PoolFilter {
    pub fn is_active(&self) -> bool {
        !self.query.trim().is_empty()
            || !self.types.is_empty()
            || !self.colors.is_empty()
            || !self.rarities.is_empty()
    }
}

fn axis_matches(selected: &[DraftPoolGroupKind], kind: DraftPoolGroupKind) -> bool {
    selected.is_empty() || selected.contains(&kind)
}

/// The single filtering authority for a limited-pool display (#7546 review):
/// narrow `listing` (any subset of the pool — the build screen passes the pool
/// minus the cards already moved to the deck) and return the kept instance ids
/// in listing order. The display renders exactly this result; it never
/// interprets the game data itself.
///
/// Each instance is classified HERE, by the same per-card classifiers the
/// group builder uses — not looked up in wire-delivered groups. That keeps one
/// classification authority for every message vintage: a legacy (pre-v11)
/// view whose serialized groups cannot address all collapsed copies still
/// filters every copy correctly, because the groups are not consulted at all
/// (review round 3). Classification is total (every axis has a residual
/// bucket), so an unclassifiable listing entry cannot exist.
pub fn filter_pool_listing(listing: &[DraftCardInstance], filter: &PoolFilter) -> Vec<String> {
    if !filter.is_active() {
        return listing
            .iter()
            .map(|card| card.instance_id.clone())
            .collect();
    }
    let query = filter.query.trim().to_lowercase();

    listing
        .iter()
        .filter(|card| {
            (query.is_empty() || card.name.to_lowercase().contains(&query))
                && (filter.types.is_empty()
                    || type_memberships(card)
                        .iter()
                        .any(|kind| filter.types.contains(kind)))
                && (filter.colors.is_empty()
                    || color_memberships(card)
                        .iter()
                        .any(|kind| filter.colors.contains(kind)))
                && axis_matches(&filter.rarities, rarity_group(card))
        })
        .map(|card| card.instance_id.clone())
        .collect()
}

/// Filtered draft state for a specific player. Built from scratch (not a reference
/// into DraftSession) to prevent accidental hidden state leakage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftPlayerView {
    /// Current draft status
    pub status: DraftStatus,
    /// Draft kind (Quick/Premier/Traditional/Sealed)
    pub kind: DraftKind,
    /// View-safe source metadata. This is never the persisted `DraftSource`:
    /// Chaos assignment matrices remain host-only.
    pub source: DraftSourceView,
    /// Engine-authorized post-draft game launch. This is a procedure-derived
    /// capability, not a client inference from the draft-kind label: a
    /// completed pod only offers a game when the procedure says it can.
    pub launch_capability: DraftLaunchCapability,
    /// Which pack round (0, 1, 2)
    pub current_pack_number: u8,
    /// Which pick within the current pack
    pub pick_number: u8,
    /// Current pass direction
    pub pass_direction: PassDirection,
    /// The viewer's current pack (None if between packs or not their turn)
    pub current_pack: Option<Vec<DraftCardInstance>>,
    /// CR 903.13b: how many cards the viewer's next pick step takes from
    /// `current_pack` — `min(cards_per_pick, remaining pack size)`, the exact
    /// count `pick_pass::apply_pick_inner` enforces. 0 when there is no
    /// pending pack.
    ///
    /// Published so the display layer never re-derives it: the count is 1 for
    /// the four CR 905.1a kinds, 2 for `CommanderDraft`, and drops to 1 on an
    /// odd pack's final step — a distinction no per-kind lookup can make.
    pub required_pick_count: usize,
    /// Engine-owned selection interaction for this draft procedure. This stays
    /// ordered for Commander Draft's one-card final step, unlike
    /// `required_pick_count`.
    pub pick_selection_mode: PickSelectionMode,
    /// The viewer's drafted pool
    pub pool: Vec<DraftCardInstance>,
    /// Drafted cards whose effects can be activated during a later pick.
    pub draft_effects: Vec<DraftCardInstance>,
    /// Engine-defined groups for displaying the viewer's pool without client-side
    /// card classification, ordering, or deduplication.
    pub pool_groups: DraftPoolGroups,
    /// Each of the viewer's sealed packs, in opening order. Present only for
    /// sealed events so clients can present the engine-generated pulls without
    /// reconstructing packs from a flattened pool.
    pub sealed_packs: Option<Vec<Vec<DraftCardInstance>>>,
    /// Public info for all seats
    pub seats: Vec<SeatPublicView>,
    /// Cards in the booster currently being drafted (for UI progress display).
    /// Multi-set drafts mix booster sizes, so this tracks `current_pack_number`
    /// rather than describing the session as a whole — see `pack_sizes`.
    pub cards_per_pack: u8,
    /// Cards in each booster of the session, in pack order. Engine-derived so
    /// clients render per-pack progress without reconstructing pack shape.
    pub pack_sizes: Vec<u8>,
    /// The set filling each booster, in pack order. Multi-set drafts open a
    /// different set each round; single-set drafts repeat one code. Cube
    /// sources report the cube id for every pack.
    pub pack_set_codes: Vec<String>,
    /// CR 903.13b: pick STEPS in each booster, in pack order — the per-pack
    /// counterpart of `pick_steps_per_pack`. A progress display measures each
    /// booster against this, never against `pack_sizes`: the two differ
    /// whenever a kind takes more than one card per step.
    pub pack_pick_steps: Vec<u8>,
    /// CR 903.13b: how many pick STEPS the booster currently being drafted
    /// contains for this kind — `cards_per_pack.div_ceil(cards_per_pick)`.
    /// `pick_number` counts steps, not cards, so this is the denominator a
    /// progress display can actually reach: a 14-card Commander pack is 7
    /// steps, not 14. Derived from `cards_per_pack`, so it tracks
    /// `current_pack_number` for the same reason that field does — a multi-set
    /// draft whose packs differ in size also differs in step count per pack.
    ///
    /// Published so the display layer never re-derives it. A client that
    /// divided `cards_per_pack` itself would be a second authority for
    /// CR 903.13b's step rule, and it would be right for the four CR 905.1a
    /// kinds and wrong by 2x for `CommanderDraft`.
    pub pick_steps_per_pack: u8,
    /// Total pack count (for UI progress display)
    pub pack_count: u8,
    /// Minimum main deck size for this draft.
    pub min_deck_size: usize,
    /// Cards available in unlimited quantity during deck construction.
    pub addable_cards: Vec<String>,
    /// CR 903.13e: every commander filler this draft's booster sets grant, and
    /// each one's cap. EMPTY when no contained set grants one.
    ///
    /// Plural because CR 903.13e states its grants per contained set: a draft
    /// that opened Commander Masters and Battle for Baldur's Gate boosters
    /// satisfies both conditions and concedes both cards.
    ///
    /// Deliberately NOT folded into `addable_cards`, whose contract is
    /// *unlimited quantity* -- the exact property CR 903.13e denies. Engine-
    /// derived: the client must never re-derive it from the set codes.
    /// Rendered by `PoolPanel` (the grant lines) and by `LimitedDeckBuilder`
    /// (the addable list); the caps and the CR 903.13e commander-condition stay
    /// engine-enforced in `validate_limited_deck`.
    pub grantable_commander_fillers: Vec<GrantableCommanderFiller>,
    /// CR 903.13f(3): every set this draft was latched to, as OPAQUE courier
    /// tokens for the engine functions that map contained sets to a
    /// deck-construction concession (`commanderPartnerCandidates`). EMPTY for
    /// a cube and for every kind outside CR 903.13's scope.
    ///
    /// Plural for the same reason as `grantable_commander_fillers`: both rules
    /// ask what the draft CONTAINED, and a mixed-set draft contained all of
    /// them. Publishing one representative would silently drop the grants the
    /// others make.
    ///
    /// The display layer passes them back to the engine and NEVER interprets
    /// them: which sets grant what is engine knowledge, tabled once in
    /// `deck_validation::DRAFT_SET_CONCESSIONS`. In particular they must never
    /// be reconstructed from a pool card's `set_code` -- the filler cards are
    /// printed in the granting sets' own boosters, so a card's printing is
    /// evidence of the grant in neither direction.
    pub draft_set_codes: Vec<String>,
    /// Milliseconds remaining on the pick timer. Always None from the reducer;
    /// the P2P host injects the authoritative value on the wire.
    pub timer_remaining_ms: Option<u32>,
    /// Tournament standings, sorted by match_wins descending. Empty before pairings.
    pub standings: Vec<StandingEntry>,
    /// Current tournament round (0 = not started).
    pub current_round: u8,
    /// The round pairings may next be generated for. Engine-derived from the
    /// single authority (`DraftSession::next_pairing_round`) so clients never
    /// recompute it. Always >= 1. Published unconditionally, so once `status`
    /// is `Complete` it names a round that can never be generated
    /// (`apply_generate_pairings` accepts only `Deckbuilding`/`Pairing`/
    /// `RoundComplete`) — read `current_round` on a finished pod.
    pub next_pairing_round: u8,
    /// Tournament format from config.
    pub tournament_format: TournamentFormat,
    /// Pod policy from config.
    pub pod_policy: PodPolicy,
    /// Pairings for the current round.
    pub pairings: Vec<PairingView>,
    /// Resolved match configuration owned by the draft engine.
    pub match_config: MatchConfig,
}

/// Re-export view-facing types from `types` for convenience.
pub use crate::types::{DraftLaunchCapability, SpectatorVisibility};

/// Whether the draft has finished assigning boosters. Only then may a player
/// learn their completed Chaos sequence and the actual pod-wide set union.
/// Lobby, drafting, paused, and abandoned sessions can still contain unopened
/// boosters, so those states deliberately retain only the candidate intent.
fn chaos_assignments_are_complete(status: DraftStatus) -> bool {
    matches!(
        status,
        DraftStatus::Deckbuilding
            | DraftStatus::Pairing
            | DraftStatus::MatchInProgress
            | DraftStatus::RoundComplete
            | DraftStatus::Complete
    )
}

fn set_source_view_for_player(
    layout: &SetLayout,
    status: DraftStatus,
    seat_index: u8,
    current_pack_number: u8,
    current_pack_origin: Option<u8>,
) -> SetLayoutView {
    match layout {
        SetLayout::UniformByRound { codes } => SetLayoutView::UniformByRound {
            codes: codes.clone(),
        },
        SetLayout::Chaos {
            candidate_codes,
            assignments,
        } => {
            let assignments_complete = chaos_assignments_are_complete(status);
            SetLayoutView::Chaos {
                candidate_codes: candidate_codes.clone(),
                current_pack_code: (status == DraftStatus::Drafting)
                    .then(|| {
                        current_pack_origin.and_then(|origin| {
                            assignments
                                .get(usize::from(origin))
                                .and_then(|rounds| rounds.get(usize::from(current_pack_number)))
                                .cloned()
                        })
                    })
                    .flatten(),
                completed_own_pack_codes: assignments_complete
                    .then(|| assignments.get(usize::from(seat_index)).cloned())
                    .flatten(),
                actual_set_codes: assignments_complete.then(|| {
                    layout
                        .actual_set_codes()
                        .into_iter()
                        .map(str::to_string)
                        .collect()
                }),
            }
        }
    }
}

fn source_view_for_player(session: &DraftSession, seat_index: u8) -> DraftSourceView {
    match &session.config.source {
        DraftSource::Set { layout } => DraftSourceView::Set {
            layout: set_source_view_for_player(
                layout,
                session.status,
                seat_index,
                session.current_pack_number,
                current_pack_origin(session, seat_index),
            ),
        },
        DraftSource::Cube { id, name } => DraftSourceView::Cube {
            id: id.clone(),
            name: name.clone(),
        },
    }
}

/// The opening seat of a player's live booster. A Chaos assignment belongs to
/// this origin, not the seat holding the booster after a pass.
fn current_pack_origin(session: &DraftSession, seat_index: u8) -> Option<u8> {
    session
        .current_pack
        .get(usize::from(seat_index))
        .and_then(Option::as_ref)
        .filter(|pack| !pack.0.is_empty())?;
    session
        .current_pack_origins
        .get(usize::from(seat_index))
        .copied()
        .flatten()
}

fn source_view_for_spectator(session: &DraftSession) -> DraftSourceView {
    match &session.config.source {
        DraftSource::Set { layout } => DraftSourceView::Set {
            layout: match layout {
                SetLayout::UniformByRound { codes } => SetLayoutView::UniformByRound {
                    codes: codes.clone(),
                },
                SetLayout::Chaos {
                    candidate_codes, ..
                } => SetLayoutView::Chaos {
                    candidate_codes: candidate_codes.clone(),
                    current_pack_code: None,
                    completed_own_pack_codes: None,
                    actual_set_codes: None,
                },
            },
        },
        DraftSource::Cube { id, name } => DraftSourceView::Cube {
            id: id.clone(),
            name: name.clone(),
        },
    }
}

/// Legacy progress metadata that remains safe for every viewer.
///
/// Uniform drafts expose their round sequence as before. Chaos intentionally
/// returns no sequence: callers must use `DraftSourceView`'s scoped values
/// instead of accidentally treating seat zero's schedule as public.
fn visible_pack_set_codes(session: &DraftSession) -> Vec<String> {
    match &session.config.source {
        DraftSource::Set {
            layout: SetLayout::UniformByRound { .. },
        }
        | DraftSource::Cube { .. } => session.pack_set_code_sequence(),
        DraftSource::Set {
            layout: SetLayout::Chaos { .. },
        } => Vec::new(),
    }
}

fn visible_chaos_concessions(session: &DraftSession) -> bool {
    !matches!(
        &session.config.source,
        DraftSource::Set {
            layout: SetLayout::Chaos { .. }
        }
    ) || chaos_assignments_are_complete(session.status)
}

/// Filtered view for spectators watching a draft.
///
/// Public mode hides all private information (pools, packs).
/// Omniscient mode exposes all pools and current packs for all seats, except
/// Chaos sources: a spectator connection is not an authenticated host export,
/// so Chaos keeps every assignment-bearing card view private.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectatorDraftView {
    pub status: DraftStatus,
    pub kind: DraftKind,
    /// View-safe source metadata. A Chaos spectator receives candidate intent,
    /// never any seat's assignment or future booster identity.
    pub source: DraftSourceView,
    pub current_pack_number: u8,
    pub pick_number: u8,
    pub pass_direction: PassDirection,
    pub seats: Vec<SeatPublicView>,
    /// Cards in the booster currently being drafted. See
    /// [`DraftPlayerView::cards_per_pack`].
    pub cards_per_pack: u8,
    /// Cards in each booster of the session, in pack order.
    pub pack_sizes: Vec<u8>,
    /// The set filling each booster, in pack order.
    pub pack_set_codes: Vec<String>,
    /// CR 903.13b: mirrors `DraftPlayerView::pack_pick_steps`; see that field.
    pub pack_pick_steps: Vec<u8>,
    /// CR 903.13b: mirrors `DraftPlayerView::pick_steps_per_pack`; see that
    /// field. Present on both views because `DraftProgress` renders either
    /// shape through one shared prop contract.
    pub pick_steps_per_pack: u8,
    pub pack_count: u8,
    pub min_deck_size: usize,
    pub addable_cards: Vec<String>,
    /// CR 903.13e: every commander filler this draft's booster sets grant, and
    /// each one's cap. Chaos spectators receive an empty list: those grants
    /// are published only to a deckbuilding owner once all assignments finish.
    pub grantable_commander_fillers: Vec<GrantableCommanderFiller>,
    pub standings: Vec<StandingEntry>,
    pub current_round: u8,
    pub tournament_format: TournamentFormat,
    pub pod_policy: PodPolicy,
    pub pairings: Vec<PairingView>,
    /// Resolved match configuration owned by the draft engine.
    pub match_config: MatchConfig,
    /// Populated only in `Omniscient` mode. Each inner Vec is a seat's pool.
    pub pools: Option<Vec<Vec<DraftCardInstance>>>,
    /// Populated only in `Omniscient` mode. Each entry is a seat's current pack.
    pub current_packs: Option<Vec<Option<Vec<DraftCardInstance>>>>,
}

/// Generate a spectator view of the draft session.
///
/// Visibility is read from session.config.spectator_visibility (set by host at creation).
/// Public mode hides all private information (pools, packs).
/// Omniscient mode exposes all pools and current packs for all seats.
pub fn filter_for_spectator(
    session: &DraftSession,
    visibility: SpectatorVisibility,
) -> SpectatorDraftView {
    let is_drafting = session.status == DraftStatus::Drafting;

    let seats = session
        .seats
        .iter()
        .enumerate()
        .map(|(i, seat)| {
            let player_id_for_seat = match seat {
                DraftSeat::Human { player_id, .. } => Some(*player_id),
                DraftSeat::Bot { .. } => None,
            };

            let pick_status = if !is_drafting {
                PickStatus::NotDrafting
            } else if session.current_pack[i].is_some() {
                PickStatus::Pending
            } else {
                PickStatus::Picked
            };

            SeatPublicView {
                seat_index: i as u8,
                display_name: match seat {
                    DraftSeat::Human { display_name, .. } => display_name.clone(),
                    DraftSeat::Bot { name, .. } => name.clone(),
                },
                is_bot: matches!(seat, DraftSeat::Bot { .. }),
                connected: match seat {
                    // Source of truth: the runtime `connected_seats` bitmap,
                    // populated via `DraftAction::SetSeatConnected` by the host
                    // adapter on (dis)connect. Bots are always considered
                    // connected by construction. `get_or(.., true)` so an
                    // in-flight save deserialised before `ensure_len` runs
                    // shows seats as connected, not as a wall of disconnect dots.
                    DraftSeat::Human { .. } => session.connected_seats.get_or(i as u8, true),
                    DraftSeat::Bot { .. } => true,
                },
                has_submitted_deck: player_id_for_seat
                    .map(|pid| session.submitted_decks.contains_key(&pid))
                    .unwrap_or(false),
                pick_status,
                active_pack_count: u8::from(
                    is_drafting
                        && session.current_pack[i]
                            .as_ref()
                            .is_some_and(|pack| !pack.0.is_empty())
                        && !session.seats_picked_this_round.get(i as u8),
                ),
                face_up_draft_cards: face_up_draft_cards(&session.pools[i]),
            }
        })
        .collect();

    let standings = compute_standings(session);
    let pairings = compute_pairing_views(session);

    let chaos_source = matches!(
        &session.config.source,
        DraftSource::Set {
            layout: SetLayout::Chaos { .. }
        }
    );
    let (pools, current_packs) = match (visibility, chaos_source) {
        (_, true) | (SpectatorVisibility::Public, false) => (None, None),
        (SpectatorVisibility::Omniscient, false) => {
            let pools = Some(session.pools.clone());
            let packs = Some(
                session
                    .current_pack
                    .iter()
                    .map(|p| p.as_ref().map(|pack| pack.0.clone()))
                    .collect(),
            );
            (pools, packs)
        }
    };

    SpectatorDraftView {
        status: session.status,
        kind: session.kind,
        source: source_view_for_spectator(session),
        current_pack_number: session.current_pack_number,
        pick_number: session.pick_number,
        pass_direction: session.pass_direction,
        seats,
        cards_per_pack: session.cards_in_pack(session.current_pack_number),
        pack_sizes: session.pack_size_sequence(),
        pack_set_codes: visible_pack_set_codes(session),
        pack_pick_steps: session.pack_pick_step_sequence(),
        pick_steps_per_pack: session
            .kind
            .procedure()
            .pick_steps_per_pack(session.cards_in_pack(session.current_pack_number)),
        pack_count: session.config.pack_count,
        min_deck_size: session.config.min_deck_size,
        addable_cards: session.config.addable_cards.display_names(),
        // CR 903.13e: read from the latch, never re-derived here.
        grantable_commander_fillers: matches!(
            &session.config.source,
            DraftSource::Set {
                layout: SetLayout::Chaos { .. }
            }
        )
        .then(Vec::new)
        .unwrap_or_else(|| session_concessions(session).fillers),
        standings,
        current_round: session.current_round,
        tournament_format: session.config.tournament_format,
        pod_policy: session.config.pod_policy,
        pairings,
        match_config: session.kind.match_config(),
        pools,
        current_packs,
    }
}

/// Split a sealed pool back into the boosters it was opened from.
///
/// Sealed pools are stored flat, in opening order. A multi-set sealed event
/// mixes booster sizes, so the split follows the per-pack sizes the session
/// recorded rather than a single chunk width. Any remainder (a pool that does
/// not match the recorded sizes) is returned as a final pack so no card is
/// silently dropped from the display.
fn split_by_pack_size(
    pool: &[DraftCardInstance],
    session: &DraftSession,
) -> Vec<Vec<DraftCardInstance>> {
    let mut packs = Vec::with_capacity(usize::from(session.config.pack_count));
    let mut rest = pool;
    for size in session.pack_size_sequence() {
        if rest.is_empty() {
            break;
        }
        let (pack, remainder) = rest.split_at(usize::from(size).min(rest.len()));
        packs.push(pack.to_vec());
        rest = remainder;
    }
    if !rest.is_empty() {
        packs.push(rest.to_vec());
    }
    packs
}

/// Produce a filtered view of the draft session for a specific seat.
///
/// The viewer sees:
/// - Their own current pack and pool
/// - Public draft status, kind, pack/pick numbers, direction
/// - Public seat info (names, connected status, submission status)
///
/// The viewer does NOT see:
/// - Other players' packs or pools
/// - RNG seed
/// - Bot rankings or archetypes
/// - Unopened packs (packs_by_seat)
/// - Other players' deck submissions
pub fn filter_for_player(session: &DraftSession, seat_index: u8) -> DraftPlayerView {
    let idx = seat_index as usize;

    let current_pack =
        session
            .current_pack
            .get(idx)
            .and_then(|p| p.as_ref())
            .map(|p| match &session.config.source {
                DraftSource::Set { .. } => set_pack_in_rarity_order(&p.0),
                DraftSource::Cube { .. } => p.0.clone(),
            });

    let pool = session.pools.get(idx).cloned().unwrap_or_default();
    let draft_effects = face_up_draft_cards(&pool);
    // Only an all-at-once kind has unopened packs to project onto the view; a
    // pick-and-pass kind's pool is not chunked into packs. The split follows the
    // per-pack sizes the session recorded rather than a single uniform chunk
    // width, because a multi-set event's boosters differ in size.
    let sealed_packs = match session.kind.procedure().distribution {
        PackDistribution::AllAtOnce => Some(split_by_pack_size(&pool, session)),
        PackDistribution::PickAndPass => None,
    };
    let pool_groups = DraftPoolGroups::from_pool(&pool, &session.config.source);

    let is_drafting = session.status == DraftStatus::Drafting;

    let seats = session
        .seats
        .iter()
        .enumerate()
        .map(|(i, seat)| {
            let player_id_for_seat = match seat {
                DraftSeat::Human { player_id, .. } => Some(*player_id),
                DraftSeat::Bot { .. } => None,
            };

            let pick_status = if !is_drafting {
                PickStatus::NotDrafting
            } else if session.current_pack[i].is_some() {
                PickStatus::Pending
            } else {
                PickStatus::Picked
            };

            SeatPublicView {
                seat_index: i as u8,
                display_name: match seat {
                    DraftSeat::Human { display_name, .. } => display_name.clone(),
                    DraftSeat::Bot { name, .. } => name.clone(),
                },
                is_bot: matches!(seat, DraftSeat::Bot { .. }),
                connected: match seat {
                    // Source of truth: the runtime `connected_seats` bitmap,
                    // populated via `DraftAction::SetSeatConnected` by the host
                    // adapter on (dis)connect. Bots are always considered
                    // connected by construction. `get_or(.., true)` so an
                    // in-flight save deserialised before `ensure_len` runs
                    // shows seats as connected, not as a wall of disconnect dots.
                    DraftSeat::Human { .. } => session.connected_seats.get_or(i as u8, true),
                    DraftSeat::Bot { .. } => true,
                },
                has_submitted_deck: player_id_for_seat
                    .map(|pid| session.submitted_decks.contains_key(&pid))
                    .unwrap_or(false),
                pick_status,
                active_pack_count: u8::from(
                    is_drafting
                        && session.current_pack[i]
                            .as_ref()
                            .is_some_and(|pack| !pack.0.is_empty())
                        && !session.seats_picked_this_round.get(i as u8),
                ),
                face_up_draft_cards: face_up_draft_cards(&session.pools[i]),
            }
        })
        .collect();

    // Compute standings from match records
    let standings = compute_standings(session);

    // Compute pairings for the current round
    let pairings = compute_pairing_views(session);

    DraftPlayerView {
        status: session.status,
        kind: session.kind,
        source: source_view_for_player(session, seat_index),
        launch_capability: session.kind.procedure().launch_capability(),
        current_pack_number: session.current_pack_number,
        pick_number: session.pick_number,
        pass_direction: session.pass_direction,
        current_pack,
        required_pick_count: required_pick_count(session, seat_index),
        pick_selection_mode: session.kind.procedure().pick_selection_mode,
        pool,
        draft_effects,
        pool_groups,
        sealed_packs,
        seats,
        cards_per_pack: session.cards_in_pack(session.current_pack_number),
        pack_sizes: session.pack_size_sequence(),
        pack_set_codes: visible_pack_set_codes(session),
        pack_pick_steps: session.pack_pick_step_sequence(),
        pick_steps_per_pack: session
            .kind
            .procedure()
            .pick_steps_per_pack(session.cards_in_pack(session.current_pack_number)),
        pack_count: session.config.pack_count,
        min_deck_size: session.config.min_deck_size,
        addable_cards: session.config.addable_cards.display_names(),
        // CR 903.13e: read from the latch, never re-derived here.
        grantable_commander_fillers: if visible_chaos_concessions(session) {
            session_concessions(session).fillers
        } else {
            Vec::new()
        },
        // CR 903.13f(3): the same latch, published for the engine's partner
        // query. Owned strings because the view is owned.
        draft_set_codes: if visible_chaos_concessions(session) {
            concession_set_codes(session)
                .into_iter()
                .map(str::to_string)
                .collect()
        } else {
            Vec::new()
        },
        timer_remaining_ms: None,
        standings,
        current_round: session.current_round,
        next_pairing_round: session.next_pairing_round(),
        tournament_format: session.config.tournament_format,
        pod_policy: session.config.pod_policy,
        pairings,
        match_config: session.kind.match_config(),
    }
}

fn face_up_draft_cards(pool: &[DraftCardInstance]) -> Vec<DraftCardInstance> {
    pool.iter()
        .filter(|card| card.draft_effect.is_some())
        .cloned()
        .collect()
}

const COLOR_GROUP_ORDER: [DraftPoolGroupKind; 7] = [
    DraftPoolGroupKind::White,
    DraftPoolGroupKind::Blue,
    DraftPoolGroupKind::Black,
    DraftPoolGroupKind::Red,
    DraftPoolGroupKind::Green,
    DraftPoolGroupKind::Multicolor,
    DraftPoolGroupKind::Colorless,
];

const TYPE_GROUP_ORDER: [DraftPoolGroupKind; 8] = [
    DraftPoolGroupKind::Creature,
    DraftPoolGroupKind::Instant,
    DraftPoolGroupKind::Sorcery,
    DraftPoolGroupKind::Enchantment,
    DraftPoolGroupKind::Artifact,
    DraftPoolGroupKind::Planeswalker,
    DraftPoolGroupKind::Land,
    DraftPoolGroupKind::Other,
];

const RARITY_GROUP_ORDER: [DraftPoolGroupKind; 5] = [
    DraftPoolGroupKind::Mythic,
    DraftPoolGroupKind::Rare,
    DraftPoolGroupKind::Uncommon,
    DraftPoolGroupKind::Common,
    DraftPoolGroupKind::RarityOther,
];

const RARITY_CAPABILITY_ORDER: [DraftRarityGroupKind; 5] = [
    DraftRarityGroupKind::Mythic,
    DraftRarityGroupKind::Rare,
    DraftRarityGroupKind::Uncommon,
    DraftRarityGroupKind::Common,
    DraftRarityGroupKind::RarityOther,
];

const CMC_GROUP_ORDER: [DraftPoolGroupKind; 7] = [
    DraftPoolGroupKind::ManaValue0,
    DraftPoolGroupKind::ManaValue1,
    DraftPoolGroupKind::ManaValue2,
    DraftPoolGroupKind::ManaValue3,
    DraftPoolGroupKind::ManaValue4,
    DraftPoolGroupKind::ManaValue5,
    DraftPoolGroupKind::ManaValue6Plus,
];

fn groups_for(
    pool: &[DraftCardInstance],
    order: &[DraftPoolGroupKind],
    classify: fn(&DraftCardInstance) -> DraftPoolGroupKind,
    sort_by_cmc: bool,
) -> Vec<DraftPoolGroup> {
    order
        .iter()
        .filter_map(|kind| {
            let cards: Vec<_> = pool
                .iter()
                .filter(|card| classify(card) == *kind)
                .cloned()
                .collect();
            let total = cards.len();
            (!cards.is_empty()).then(|| DraftPoolGroup {
                kind: *kind,
                total,
                cards: sorted_entries(cards, sort_by_cmc),
            })
        })
        .collect()
}

fn source_order_groups_for(
    pool: &[DraftCardInstance],
    order: &[DraftPoolGroupKind],
    classify: fn(&DraftCardInstance) -> DraftPoolGroupKind,
) -> Vec<DraftPoolGroup> {
    order
        .iter()
        .filter_map(|kind| {
            let cards: Vec<_> = pool
                .iter()
                .filter(|card| classify(card) == *kind)
                .cloned()
                .collect();
            let total = cards.len();
            (!cards.is_empty()).then(|| DraftPoolGroup {
                kind: *kind,
                total,
                cards: source_order_entries(cards),
            })
        })
        .collect()
}

fn source_order_entries(cards: Vec<DraftCardInstance>) -> Vec<DraftPoolEntry> {
    let mut entries: Vec<DraftPoolEntry> = Vec::new();
    for card in cards {
        if let Some(entry) = entries
            .last_mut()
            .filter(|entry| entry.card.name == card.name)
        {
            entry.count += 1;
            entry.instance_ids.push(card.instance_id.clone());
        } else {
            let instance_ids = vec![card.instance_id.clone()];
            entries.push(DraftPoolEntry {
                card,
                count: 1,
                instance_ids,
            });
        }
    }
    entries
}

fn set_pack_in_rarity_order(pack: &[DraftCardInstance]) -> Vec<DraftCardInstance> {
    RARITY_GROUP_ORDER
        .iter()
        .flat_map(|kind| {
            pack.iter()
                .filter(move |card| rarity_group(card) == *kind)
                .cloned()
        })
        .collect()
}

fn sorted_entries(mut cards: Vec<DraftCardInstance>, sort_by_cmc: bool) -> Vec<DraftPoolEntry> {
    cards.sort_by(|left, right| {
        if sort_by_cmc {
            left.cmc
                .cmp(&right.cmc)
                .then_with(|| left.name.cmp(&right.name))
        } else {
            left.name.cmp(&right.name)
        }
    });

    let mut entries: Vec<DraftPoolEntry> = Vec::new();
    for card in cards {
        if let Some(entry) = entries
            .last_mut()
            .filter(|entry| entry.card.name == card.name)
        {
            entry.count += 1;
            entry.instance_ids.push(card.instance_id.clone());
        } else {
            let instance_ids = vec![card.instance_id.clone()];
            entries.push(DraftPoolEntry {
                card,
                count: 1,
                instance_ids,
            });
        }
    }
    entries
}

fn color_group(card: &DraftCardInstance) -> DraftPoolGroupKind {
    match card.colors.as_slice() {
        [] => DraftPoolGroupKind::Colorless,
        [_color, _second, ..] => DraftPoolGroupKind::Multicolor,
        [color] => match color.as_str() {
            "W" => DraftPoolGroupKind::White,
            "U" => DraftPoolGroupKind::Blue,
            "B" => DraftPoolGroupKind::Black,
            "R" => DraftPoolGroupKind::Red,
            "G" => DraftPoolGroupKind::Green,
            _ => DraftPoolGroupKind::Colorless,
        },
    }
}

/// EVERY color bucket `card` belongs to, in `COLOR_GROUP_ORDER` — CR 105.2:
/// "an object can be one or more of the five colors", so a white-blue card is
/// a member of White AND Blue AND (CR 105.2b) Multicolor; a colorless card is
/// a member of Colorless (CR 105.2c). This is the FILTERING membership; the
/// exclusive `color_group` stays the sorted display's one-bucket-per-card
/// shape (a multicolor card sorts under Multicolor alone).
fn color_memberships(card: &DraftCardInstance) -> Vec<DraftPoolGroupKind> {
    if card.colors.is_empty() {
        return vec![DraftPoolGroupKind::Colorless];
    }
    let mut memberships: Vec<DraftPoolGroupKind> = [
        (DraftPoolGroupKind::White, "W"),
        (DraftPoolGroupKind::Blue, "U"),
        (DraftPoolGroupKind::Black, "B"),
        (DraftPoolGroupKind::Red, "R"),
        (DraftPoolGroupKind::Green, "G"),
    ]
    .into_iter()
    .filter(|(_, symbol)| card.colors.iter().any(|color| color == symbol))
    .map(|(kind, _)| kind)
    .collect();
    if card.colors.len() >= 2 {
        memberships.push(DraftPoolGroupKind::Multicolor);
    }
    if memberships.is_empty() {
        // Colors outside WUBRG cannot occur in real data; classify totally
        // rather than silently dropping the card from the axis.
        memberships.push(DraftPoolGroupKind::Colorless);
    }
    memberships
}

/// Every color bucket ANY pool member belongs to, in `COLOR_GROUP_ORDER` —
/// the engine-owned option list a color-filter control offers. A pool of
/// white-blue cards offers White, Blue AND Multicolor chips even though its
/// sorted display has only a Multicolor group.
fn color_filter_options(pool: &[DraftCardInstance]) -> Vec<DraftPoolGroupKind> {
    let mut present: Vec<DraftPoolGroupKind> = Vec::new();
    for card in pool {
        for kind in color_memberships(card) {
            if !present.contains(&kind) {
                present.push(kind);
            }
        }
    }
    COLOR_GROUP_ORDER
        .iter()
        .copied()
        .filter(|kind| present.contains(kind))
        .collect()
}

/// Every rarity bucket any pool member belongs to, in `RARITY_GROUP_ORDER` —
/// the engine-owned option list a rarity-filter control offers. Rarity is
/// single-valued per printing, so this equals the non-empty `rarity_groups`
/// kinds; carried here so a legacy view's controls can be rebuilt from the
/// pool alone.
fn rarity_filter_options(pool: &[DraftCardInstance]) -> Vec<DraftPoolGroupKind> {
    RARITY_GROUP_ORDER
        .iter()
        .copied()
        .filter(|kind| pool.iter().any(|card| rarity_group(card) == *kind))
        .collect()
}

/// The complete engine-owned option lists for a limited-pool filter control,
/// computable from the pool instances alone. The stateless path a display
/// uses when its delivered view predates the option fields (review round 5:
/// legacy controls must come from the engine, not from the lossy exclusive
/// presentation buckets, and never be reconstructed in the display layer).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolFilterOptions {
    pub types: Vec<DraftPoolGroupKind>,
    pub colors: Vec<DraftPoolGroupKind>,
    pub rarities: Vec<DraftPoolGroupKind>,
}

pub fn pool_filter_options(pool: &[DraftCardInstance]) -> PoolFilterOptions {
    PoolFilterOptions {
        types: type_filter_options(pool),
        colors: color_filter_options(pool),
        rarities: rarity_filter_options(pool),
    }
}

/// The EXCLUSIVE presentation bucket for the sorted pool display — a card
/// appears in exactly one group, so the priority chain picks its most salient
/// type. Filtering must NOT use this: see [`type_memberships`].
fn type_group(card: &DraftCardInstance) -> DraftPoolGroupKind {
    *type_memberships(card)
        .first()
        .expect("type membership is total — the Other bucket catches the rest")
}

/// EVERY type bucket `card` belongs to, in `TYPE_GROUP_ORDER` — CR 205.2b:
/// an object with more than one card type "satisfies the criteria for any
/// effect that applies to any of their card types", so an Artifact Creature
/// is a member of BOTH the Artifact and the Creature bucket. This is the FILTERING membership
/// (review round 4: the exclusive bucket silently excluded multi-type cards
/// from every non-primary type selection); the exclusive presentation bucket
/// is its first element, keeping the two views of the same card consistent
/// by construction.
fn type_memberships(card: &DraftCardInstance) -> Vec<DraftPoolGroupKind> {
    let type_line = card.type_line.to_ascii_lowercase();
    let memberships: Vec<DraftPoolGroupKind> = [
        (DraftPoolGroupKind::Creature, "creature"),
        (DraftPoolGroupKind::Instant, "instant"),
        (DraftPoolGroupKind::Sorcery, "sorcery"),
        (DraftPoolGroupKind::Enchantment, "enchantment"),
        (DraftPoolGroupKind::Artifact, "artifact"),
        (DraftPoolGroupKind::Planeswalker, "planeswalker"),
        (DraftPoolGroupKind::Land, "land"),
    ]
    .into_iter()
    .filter(|(_, needle)| type_line.contains(needle))
    .map(|(kind, _)| kind)
    .collect();
    if memberships.is_empty() {
        vec![DraftPoolGroupKind::Other]
    } else {
        memberships
    }
}

/// Every type bucket ANY pool member belongs to, in `TYPE_GROUP_ORDER` — the
/// engine-owned option list a type-filter control offers. Distinct from the
/// exclusive `type_groups` axis: a pool of Artifact Creatures offers BOTH
/// chips even though its sorted display has only a Creature group.
fn type_filter_options(pool: &[DraftCardInstance]) -> Vec<DraftPoolGroupKind> {
    let mut present: Vec<DraftPoolGroupKind> = Vec::new();
    for card in pool {
        for kind in type_memberships(card) {
            if !present.contains(&kind) {
                present.push(kind);
            }
        }
    }
    TYPE_GROUP_ORDER
        .iter()
        .copied()
        .filter(|kind| present.contains(kind))
        .collect()
}

/// Buckets the instance's raw rarity string into the standard four, with
/// everything else ("special", "bonus", unknown) collected under `RarityOther`
/// rather than silently dropped from the axis.
fn rarity_group(card: &DraftCardInstance) -> DraftPoolGroupKind {
    match card.rarity.to_ascii_lowercase().as_str() {
        "mythic" => DraftPoolGroupKind::Mythic,
        "rare" => DraftPoolGroupKind::Rare,
        "uncommon" => DraftPoolGroupKind::Uncommon,
        "common" => DraftPoolGroupKind::Common,
        _ => DraftPoolGroupKind::RarityOther,
    }
}

fn mana_value_group(card: &DraftCardInstance) -> DraftPoolGroupKind {
    match card.cmc {
        0 => DraftPoolGroupKind::ManaValue0,
        1 => DraftPoolGroupKind::ManaValue1,
        2 => DraftPoolGroupKind::ManaValue2,
        3 => DraftPoolGroupKind::ManaValue3,
        4 => DraftPoolGroupKind::ManaValue4,
        5 => DraftPoolGroupKind::ManaValue5,
        _ => DraftPoolGroupKind::ManaValue6Plus,
    }
}

fn color_counts(pool: &[DraftCardInstance]) -> DraftPoolColorCounts {
    let mut counts = DraftPoolColorCounts::default();
    for card in pool {
        for color in &card.colors {
            match color.as_str() {
                "W" => counts.white += 1,
                "U" => counts.blue += 1,
                "B" => counts.black += 1,
                "R" => counts.red += 1,
                "G" => counts.green += 1,
                _ => {}
            }
        }
    }
    counts
}

fn compute_standings(session: &DraftSession) -> Vec<StandingEntry> {
    if session.pairings.is_empty() {
        return Vec::new();
    }

    let mut entries: Vec<StandingEntry> = session
        .seats
        .iter()
        .enumerate()
        .map(|(i, seat)| {
            let pid = match seat {
                DraftSeat::Human { player_id, .. } => *player_id,
                DraftSeat::Bot { .. } => PlayerId(i as u8),
            };
            let record = session.match_records.get(&pid);
            StandingEntry {
                seat_index: i as u8,
                display_name: match seat {
                    DraftSeat::Human { display_name, .. } => display_name.clone(),
                    DraftSeat::Bot { name } => name.clone(),
                },
                match_wins: record.map_or(0, |r| r.match_wins),
                match_losses: record.map_or(0, |r| r.match_losses),
                game_wins: record.map_or(0, |r| r.wins),
                game_losses: record.map_or(0, |r| r.losses),
            }
        })
        .collect();

    entries.sort_by_key(|e| std::cmp::Reverse(e.match_wins));
    entries
}

fn compute_pairing_views(session: &DraftSession) -> Vec<PairingView> {
    let current_round = session.current_round;
    if current_round == 0 {
        return Vec::new();
    }

    // Build a PlayerId -> (seat_index, name) lookup
    let player_seat_map: std::collections::HashMap<PlayerId, (u8, String)> = session
        .seats
        .iter()
        .enumerate()
        .map(|(i, seat)| {
            let (pid, name) = match seat {
                DraftSeat::Human {
                    player_id,
                    display_name,
                    ..
                } => (*player_id, display_name.clone()),
                DraftSeat::Bot { name } => (PlayerId(i as u8), name.clone()),
            };
            (pid, (i as u8, name))
        })
        .collect();

    session
        .pairings
        .iter()
        .filter(|p| p.round == current_round)
        .map(|p| {
            let (seat_a, name_a) = player_seat_map
                .get(&p.players[0])
                .cloned()
                .unwrap_or((0, "Unknown".to_string()));
            let (seat_b, name_b) = player_seat_map
                .get(&p.players[1])
                .cloned()
                .unwrap_or((0, "Unknown".to_string()));

            let winner_seat = p.result_winner(&session.match_records).and_then(|winner| {
                if winner == p.players[0] {
                    Some(seat_a)
                } else if winner == p.players[1] {
                    Some(seat_b)
                } else {
                    None
                }
            });

            PairingView {
                round: p.round,
                table: p.table,
                seat_a,
                name_a,
                seat_b,
                name_b,
                match_id: p.match_id.clone(),
                status: p.status,
                winner_seat,
                score_a: None,
                score_b: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_source::FixturePackSource;
    use crate::session;

    use engine::types::player::PlayerId;

    fn test_session(pod_size: u8) -> (DraftSession, FixturePackSource) {
        let config = DraftConfig {
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            pod_size,
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats: Vec<DraftSeat> = (0..pod_size)
            .map(|i| DraftSeat::Human {
                player_id: PlayerId(i),
                display_name: format!("Player {i}"),
            })
            .collect();
        let source = FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        let s = DraftSession::new(config, seats, "TEST-001".to_string());
        (s, source)
    }

    fn start_and_pick(session: &mut DraftSession, source: &FixturePackSource) {
        session::apply(session, DraftAction::StartDraft, Some(source)).unwrap();
        // Make a pick for seat 0 so they have something in their pool
        let card_id = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        session::apply(
            session,
            DraftAction::Pick {
                seat: 0,
                card_instance_ids: vec![card_id],
            },
            None,
        )
        .unwrap();
    }

    fn draft_card(name: &str, colors: &[&str], cmc: u8, type_line: &str) -> DraftCardInstance {
        DraftCardInstance {
            instance_id: name.to_string(),
            name: name.to_string(),
            set_code: "TST".to_string(),
            collector_number: "1".to_string(),
            rarity: "common".to_string(),
            colors: colors.iter().map(ToString::to_string).collect(),
            cmc,
            type_line: type_line.to_string(),
            draft_effect: None,
        }
    }

    #[test]
    fn view_contains_viewers_current_pack() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();
        let actual_pack = session.current_pack[0].as_ref().unwrap().0.clone();

        let view = filter_for_player(&session, 0);
        let pack = view.current_pack.unwrap();
        assert_eq!(pack.len(), 14);
        assert_eq!(session.current_pack[0].as_ref().unwrap().0, actual_pack);
        assert!(actual_pack.iter().all(|card| pack
            .iter()
            .any(|projected| projected.instance_id == card.instance_id)));
    }

    #[test]
    fn view_contains_viewers_pool() {
        let (mut session, source) = test_session(8);
        start_and_pick(&mut session, &source);

        let view = filter_for_player(&session, 0);
        assert_eq!(view.pool.len(), 1);
        assert_eq!(view.pool[0].instance_id, session.pools[0][0].instance_id);
    }

    #[test]
    fn view_exposes_other_players_face_up_draft_cards_without_their_pool() {
        let (mut session, source) = test_session(2);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let face_up = DraftCardInstance {
            instance_id: "cogwork-1".to_string(),
            name: "Cogwork Librarian".to_string(),
            set_code: "CNS".to_string(),
            collector_number: "58".to_string(),
            rarity: "common".to_string(),
            colors: Vec::new(),
            cmc: 4,
            type_line: "Artifact Creature — Construct".to_string(),
            draft_effect: Some(engine::types::card::DraftEffect::AdditionalPick),
        };
        let hidden = draft_card("Hidden Pool Card", &[], 2, "Creature");
        session.pools[1] = vec![face_up.clone(), hidden.clone()];

        let view = filter_for_player(&session, 0);

        assert_eq!(view.seats[1].face_up_draft_cards, vec![face_up]);
        assert!(view.draft_effects.is_empty());
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains(&hidden.instance_id));
    }

    #[test]
    fn sealed_view_preserves_the_viewers_pack_boundaries() {
        let (mut session, source) = test_session(2);
        session.kind = DraftKind::Sealed;
        session.config.kind = DraftKind::Sealed;
        session.config.pack_count = 6;
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_player(&session, 0);
        let sealed_packs = view
            .sealed_packs
            .expect("sealed view includes opening packs");

        assert_eq!(sealed_packs.len(), 6);
        assert!(sealed_packs.iter().all(|pack| pack.len() == 14));
        assert_eq!(sealed_packs.concat(), view.pool);
    }

    #[test]
    fn pool_groups_are_engine_ordered_and_deduplicated() {
        let pool = vec![
            draft_card("Adept", &["W"], 2, "Artifact Creature — Wizard"),
            draft_card("Adept", &["W"], 2, "Artifact Creature — Wizard"),
            draft_card("Bolt", &["R"], 1, "Instant"),
            draft_card("Charm", &["U", "R"], 3, "Sorcery"),
            draft_card("Field", &[], 0, "Land"),
        ];

        let groups = DraftPoolGroups::from_pool(&pool, &DraftSource::single_set("TST"));

        assert_eq!(
            groups
                .color_groups
                .iter()
                .map(|group| group.kind)
                .collect::<Vec<_>>(),
            vec![
                DraftPoolGroupKind::White,
                DraftPoolGroupKind::Red,
                DraftPoolGroupKind::Multicolor,
                DraftPoolGroupKind::Colorless,
            ]
        );
        assert_eq!(groups.color_groups[0].cards[0].count, 2);
        assert_eq!(groups.color_groups[0].total, 2);
        assert_eq!(
            groups
                .type_groups
                .iter()
                .map(|group| group.kind)
                .collect::<Vec<_>>(),
            vec![
                DraftPoolGroupKind::Creature,
                DraftPoolGroupKind::Instant,
                DraftPoolGroupKind::Sorcery,
                DraftPoolGroupKind::Land,
            ]
        );
        assert_eq!(groups.type_groups[0].cards[0].card.name, "Adept");
        assert_eq!(groups.type_groups[0].cards[0].count, 2);
        assert_eq!(groups.color_counts.white, 2);
        assert_eq!(groups.color_counts.red, 2);
    }

    #[test]
    fn rarity_groups_bucket_the_standard_four_and_collect_the_rest() {
        let mut mythic = draft_card("Dragon", &["R"], 6, "Creature — Dragon");
        mythic.rarity = "mythic".to_string();
        let mut rare = draft_card("Relic", &[], 2, "Artifact");
        rare.rarity = "Rare".to_string(); // case-insensitive bucketing
        let mut special = draft_card("Oddity", &["U"], 3, "Sorcery");
        special.rarity = "special".to_string();
        let common_a = draft_card("Adept", &["W"], 2, "Creature — Wizard");
        let common_b = draft_card("Adept", &["W"], 2, "Creature — Wizard");

        let groups = DraftPoolGroups::from_pool(
            &[mythic, rare, special, common_a, common_b],
            &DraftSource::single_set("TST"),
        );

        assert_eq!(
            groups
                .rarity_groups
                .iter()
                .map(|group| group.kind)
                .collect::<Vec<_>>(),
            vec![
                DraftPoolGroupKind::Mythic,
                DraftPoolGroupKind::Rare,
                DraftPoolGroupKind::Common,
                DraftPoolGroupKind::RarityOther,
            ],
            "engine order, empty buckets omitted, non-standard rarities collected"
        );
        assert_eq!(groups.rarity_groups[2].cards[0].count, 2);
        assert_eq!(groups.rarity_groups[2].total, 2);
    }

    #[test]
    fn set_and_cube_workspace_capabilities_are_source_owned() {
        let mut adept_first = draft_card("Adept", &["W"], 2, "Artifact Creature — Wizard");
        adept_first.instance_id = "Adept-1".to_string();
        let bolt = draft_card("Bolt", &["R"], 1, "Instant");
        let mut adept_second = draft_card("Adept", &["W"], 2, "Artifact Creature — Wizard");
        adept_second.instance_id = "Adept-2".to_string();
        let mut relic = draft_card("Relic", &[], 2, "Artifact");
        relic.rarity = "rare".to_string();
        let mut dragon = draft_card("Dragon", &["R"], 6, "Creature — Dragon");
        dragon.rarity = "mythic".to_string();
        let mut charm = draft_card("Charm", &["U"], 3, "Instant");
        charm.rarity = "uncommon".to_string();
        let mut oddity = draft_card("Oddity", &["U"], 4, "Sorcery");
        oddity.rarity = "special".to_string();
        let field = draft_card("Field", &[], 0, "Land");
        let pack = vec![
            adept_first.clone(),
            relic.clone(),
            bolt.clone(),
            oddity.clone(),
            dragon.clone(),
            charm.clone(),
        ];
        let pool = vec![
            adept_first,
            bolt,
            adept_second,
            relic,
            dragon,
            charm,
            oddity,
            field,
        ];

        let (mut set_session, _) = test_session(1);
        set_session.current_pack[0] = Some(DraftPack(pack.clone()));
        set_session.pools[0] = pool.clone();
        let set_view = filter_for_player(&set_session, 0);

        assert_eq!(
            set_view
                .current_pack
                .as_ref()
                .unwrap()
                .iter()
                .map(|card| card.instance_id.as_str())
                .collect::<Vec<_>>(),
            vec!["Dragon", "Relic", "Charm", "Adept-1", "Bolt", "Oddity"]
        );
        assert_eq!(set_session.current_pack[0].as_ref().unwrap().0, pack);
        assert_eq!(
            set_view
                .pool_groups
                .workspace_capabilities
                .rarity_group_order,
            Some(RARITY_CAPABILITY_ORDER.to_vec())
        );
        assert_eq!(
            set_view
                .pool_groups
                .rarity_groups
                .iter()
                .map(|group| group.kind)
                .collect::<Vec<_>>(),
            RARITY_GROUP_ORDER
        );
        let common = set_view
            .pool_groups
            .rarity_groups
            .iter()
            .find(|group| group.kind == DraftPoolGroupKind::Common)
            .unwrap();
        assert_eq!(
            common
                .cards
                .iter()
                .map(|entry| entry.card.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Adept", "Bolt", "Adept", "Field"]
        );
        assert_eq!(common.cards[0].instance_ids, vec!["Adept-1"]);
        assert_eq!(common.cards[2].instance_ids, vec!["Adept-2"]);
        assert_eq!(
            common
                .cards
                .iter()
                .flat_map(|entry| entry.instance_ids.iter().map(String::as_str))
                .collect::<Vec<_>>(),
            vec!["Adept-1", "Bolt", "Adept-2", "Field"]
        );
        assert_eq!(
            set_view
                .pool_groups
                .workspace_row_classification
                .creature_instance_ids,
            vec!["Adept-1", "Adept-2", "Dragon"]
        );
        assert_eq!(
            set_view
                .pool_groups
                .workspace_row_classification
                .noncreature_instance_ids,
            vec!["Bolt", "Relic", "Charm", "Oddity", "Field"]
        );

        let mut cube_session = set_session;
        cube_session.config.source = DraftSource::Cube {
            id: "cube-1".to_string(),
            name: "Test Cube".to_string(),
        };
        let cube_view = filter_for_player(&cube_session, 0);
        assert_eq!(cube_view.current_pack.unwrap(), pack);
        assert!(cube_view.pool_groups.rarity_groups.is_empty());
        assert_eq!(
            cube_view
                .pool_groups
                .workspace_capabilities
                .rarity_group_order,
            None
        );
        assert_eq!(
            cube_view.pool_groups.workspace_row_classification,
            set_view.pool_groups.workspace_row_classification
        );
    }

    #[test]
    fn filter_pool_listing_is_the_single_filtering_authority() {
        let pool = vec![
            draft_card("Adept", &["W"], 2, "Creature — Wizard"),
            draft_card("Adept", &["W"], 2, "Creature — Wizard"),
            draft_card("Bolt", &["R"], 1, "Instant"),
            draft_card("Charm", &["U", "R"], 3, "Sorcery"),
        ];
        // Distinct instance ids for the duplicate copies.
        let mut pool = pool;
        pool[0].instance_id = "adept-1".to_string();
        pool[1].instance_id = "adept-2".to_string();
        pool[3].rarity = "rare".to_string();

        // Inactive filter: the whole listing, in order.
        assert_eq!(
            filter_pool_listing(&pool, &PoolFilter::default()),
            vec!["adept-1", "adept-2", "Bolt", "Charm"]
        );

        // One axis narrows and covers every duplicate copy.
        let creatures = PoolFilter {
            types: vec![DraftPoolGroupKind::Creature],
            ..PoolFilter::default()
        };
        assert_eq!(
            filter_pool_listing(&pool, &creatures),
            vec!["adept-1", "adept-2"]
        );

        // OR within an axis, AND across axes.
        let across = PoolFilter {
            colors: vec![DraftPoolGroupKind::Red, DraftPoolGroupKind::Multicolor],
            rarities: vec![DraftPoolGroupKind::Rare],
            ..PoolFilter::default()
        };
        assert_eq!(filter_pool_listing(&pool, &across), vec!["Charm"]);

        // Case-insensitive name query on top of an axis.
        let query = PoolFilter {
            query: "aDePt".to_string(),
            types: vec![DraftPoolGroupKind::Creature],
            ..PoolFilter::default()
        };
        assert_eq!(
            filter_pool_listing(&pool, &query),
            vec!["adept-1", "adept-2"]
        );

        // Classification is total: an instance added to the listing after the
        // wire groups were built still classifies (here: a common stray joins
        // the commons) — no membership lookup exists to go stale.
        let mut with_stray = pool.clone();
        with_stray.push(draft_card("Stray", &[], 1, "Instant"));
        let commons = PoolFilter {
            rarities: vec![DraftPoolGroupKind::Common],
            ..PoolFilter::default()
        };
        assert_eq!(
            filter_pool_listing(&with_stray, &commons),
            vec!["adept-1", "adept-2", "Bolt", "Stray"]
        );
    }

    #[test]
    fn filter_pool_listing_keeps_each_same_name_copy_its_own_rarity() {
        // A reprint at a different rarity: the copies share a NAME but sit in
        // different rarity groups; each rarity selection keeps exactly ITS
        // copy (#7546 review — a name-keyed lookup hid the wrong card).
        let mut common = draft_card("Adept", &["W"], 2, "Creature — Wizard");
        common.instance_id = "adept-common".to_string();
        let mut rare = draft_card("Adept", &["W"], 2, "Creature — Wizard");
        rare.instance_id = "adept-rare".to_string();
        rare.rarity = "rare".to_string();
        let pool = vec![common, rare];

        let rare_only = PoolFilter {
            rarities: vec![DraftPoolGroupKind::Rare],
            ..PoolFilter::default()
        };
        assert_eq!(filter_pool_listing(&pool, &rare_only), vec!["adept-rare"]);
        let common_only = PoolFilter {
            rarities: vec![DraftPoolGroupKind::Common],
            ..PoolFilter::default()
        };
        assert_eq!(
            filter_pool_listing(&pool, &common_only),
            vec!["adept-common"]
        );
        // The shared axis still covers both copies.
        let creatures = PoolFilter {
            types: vec![DraftPoolGroupKind::Creature],
            ..PoolFilter::default()
        };
        assert_eq!(
            filter_pool_listing(&pool, &creatures),
            vec!["adept-common", "adept-rare"]
        );
    }

    #[test]
    fn multi_type_cards_match_every_type_they_carry() {
        // CR 205.2a: card types are multi-valued. Review round 4: the
        // exclusive presentation bucket (Creature-first priority) must not be
        // the filtering membership — an Artifact Creature belongs to BOTH
        // selections, and the option list offers both chips.
        let artifact_creature = draft_card("Golem", &[], 3, "Artifact Creature — Golem");
        let enchantment_creature = draft_card("Nymph", &["G"], 2, "Enchantment Creature — Nymph");
        let artifact_land = draft_card("Tomb", &[], 0, "Artifact Land");
        let plain_instant = draft_card("Bolt", &["R"], 1, "Instant");
        let pool = vec![
            artifact_creature,
            enchantment_creature,
            artifact_land,
            plain_instant,
        ];

        let by = |kind: DraftPoolGroupKind| {
            filter_pool_listing(
                &pool,
                &PoolFilter {
                    types: vec![kind],
                    ..PoolFilter::default()
                },
            )
        };
        assert_eq!(
            by(DraftPoolGroupKind::Artifact),
            vec!["Golem", "Tomb"],
            "the Artifact selection reaches the Artifact Creature AND the Artifact Land"
        );
        assert_eq!(
            by(DraftPoolGroupKind::Creature),
            vec!["Golem", "Nymph"],
            "both multi-type creatures stay reachable through Creature"
        );
        assert_eq!(by(DraftPoolGroupKind::Enchantment), vec!["Nymph"]);
        assert_eq!(by(DraftPoolGroupKind::Land), vec!["Tomb"]);

        // The engine-owned option list offers every membership, in engine
        // order — while the exclusive presentation axis keeps one bucket per
        // card (the Artifact Land sorts under Artifact, not Land).
        let groups = DraftPoolGroups::from_pool(&pool, &DraftSource::single_set("TST"));
        assert_eq!(
            groups.type_filter_options,
            vec![
                DraftPoolGroupKind::Creature,
                DraftPoolGroupKind::Instant,
                DraftPoolGroupKind::Enchantment,
                DraftPoolGroupKind::Artifact,
                DraftPoolGroupKind::Land,
            ]
        );
        assert_eq!(
            groups
                .type_groups
                .iter()
                .map(|group| group.kind)
                .collect::<Vec<_>>(),
            vec![
                DraftPoolGroupKind::Creature,
                DraftPoolGroupKind::Instant,
                DraftPoolGroupKind::Artifact,
            ],
            "the sorted display keeps its exclusive one-bucket-per-card shape"
        );
    }

    #[test]
    fn multi_color_cards_match_every_color_they_carry() {
        // CR 105.2 + CR 105.2b + CR 105.2c: a white-blue card IS white and IS
        // blue (and multicolored); a colorless card is colorless. The filter
        // membership must say so — the exclusive Multicolor display bucket is
        // a sorting shape, not the card's colors.
        let azorius = draft_card("Charm", &["W", "U"], 2, "Instant");
        let mono = draft_card("Pacifism", &["W"], 2, "Enchantment — Aura");
        let artifact = draft_card("Sphere", &[], 1, "Artifact");
        let pool = vec![azorius, mono, artifact];

        let by = |kind: DraftPoolGroupKind| {
            filter_pool_listing(
                &pool,
                &PoolFilter {
                    colors: vec![kind],
                    ..PoolFilter::default()
                },
            )
        };
        assert_eq!(
            by(DraftPoolGroupKind::White),
            vec!["Charm", "Pacifism"],
            "the White selection reaches the white-blue card too"
        );
        assert_eq!(by(DraftPoolGroupKind::Blue), vec!["Charm"]);
        assert_eq!(by(DraftPoolGroupKind::Multicolor), vec!["Charm"]);
        assert_eq!(by(DraftPoolGroupKind::Colorless), vec!["Sphere"]);

        // The option list offers every membership; the sorted display keeps
        // its exclusive shape (Charm sorts under Multicolor alone).
        let groups = DraftPoolGroups::from_pool(&pool, &DraftSource::single_set("TST"));
        assert_eq!(
            groups.color_filter_options,
            vec![
                DraftPoolGroupKind::White,
                DraftPoolGroupKind::Blue,
                DraftPoolGroupKind::Multicolor,
                DraftPoolGroupKind::Colorless,
            ]
        );
        assert_eq!(
            groups
                .color_groups
                .iter()
                .map(|group| group.kind)
                .collect::<Vec<_>>(),
            vec![
                DraftPoolGroupKind::White,
                DraftPoolGroupKind::Multicolor,
                DraftPoolGroupKind::Colorless,
            ]
        );
    }

    #[test]
    fn pool_filter_options_rebuild_every_membership_from_the_pool_alone() {
        // Review round 5: a legacy view's controls come from THIS stateless
        // path — the exclusive display buckets would hide the Artifact chip
        // of an Artifact Creature pool and the White/Blue chips of a
        // white-blue pool.
        let pool = vec![draft_card("Golem", &[], 3, "Artifact Creature — Golem"), {
            let mut charm = draft_card("Charm", &["W", "U"], 2, "Instant");
            charm.rarity = "rare".to_string();
            charm
        }];
        assert_eq!(
            pool_filter_options(&pool),
            PoolFilterOptions {
                types: vec![
                    DraftPoolGroupKind::Creature,
                    DraftPoolGroupKind::Instant,
                    DraftPoolGroupKind::Artifact,
                ],
                colors: vec![
                    DraftPoolGroupKind::White,
                    DraftPoolGroupKind::Blue,
                    DraftPoolGroupKind::Multicolor,
                    DraftPoolGroupKind::Colorless,
                ],
                rarities: vec![DraftPoolGroupKind::Rare, DraftPoolGroupKind::Common],
            }
        );
    }

    #[test]
    fn a_legacy_view_filters_every_collapsed_copy() {
        // Review round 3: a persisted/replayed v10 view collapses duplicates
        // into a `count: 2` entry whose wire shape carries no instance ids.
        // Filtering must not depend on that shape: both copies pass a
        // constrained axis, because each instance is classified here rather
        // than looked up in the legacy groups.
        let mut first = draft_card("Adept", &["W"], 2, "Creature — Wizard");
        first.instance_id = "adept-1".to_string();
        let mut second = draft_card("Adept", &["W"], 2, "Creature — Wizard");
        second.instance_id = "adept-2".to_string();
        let listing = vec![first, second];

        // The legacy groups deserialize (see the shape test below) but are
        // NOT an input to the filter — there is no path for them to drop a
        // copy.
        let creatures = PoolFilter {
            types: vec![DraftPoolGroupKind::Creature],
            ..PoolFilter::default()
        };
        assert_eq!(
            filter_pool_listing(&listing, &creatures),
            vec!["adept-1", "adept-2"]
        );
        let commons = PoolFilter {
            rarities: vec![DraftPoolGroupKind::Common],
            ..PoolFilter::default()
        };
        assert_eq!(
            filter_pool_listing(&listing, &commons),
            vec!["adept-1", "adept-2"]
        );
    }

    #[test]
    fn pre_v11_pool_group_json_still_deserializes() {
        // A v10 wire shape: no `rarity_groups`, entries without `instance_ids`.
        let old = r#"{
            "color_groups": [],
            "type_groups": [{
                "kind": "creature",
                "total": 1,
                "cards": [{
                    "card": {
                        "instance_id": "a", "name": "Adept", "set_code": "TST",
                        "collector_number": "1", "rarity": "common",
                        "colors": ["W"], "cmc": 2, "type_line": "Creature"
                    },
                    "count": 1
                }]
            }],
            "cmc_groups": [],
            "color_counts": {"white": 1, "blue": 0, "black": 0, "red": 0, "green": 0}
        }"#;
        let groups: DraftPoolGroups = serde_json::from_str(old).expect("old shape deserializes");
        assert!(groups.rarity_groups.is_empty());
        assert!(groups.type_groups[0].cards[0].instance_ids.is_empty());
        assert_eq!(
            groups.workspace_capabilities,
            DraftWorkspaceCapabilities::default()
        );
        assert_eq!(
            groups.workspace_row_classification,
            DraftWorkspaceRowClassification::default()
        );
    }

    #[test]
    fn same_name_instances_keep_their_own_rarity_group() {
        // A reprint at a different rarity: same NAME, distinct instances. The
        // name-keyed collapse must not merge them across groups, and each
        // group's entry must carry ITS copies' instance ids (#7546 review).
        let mut common = draft_card("Adept", &["W"], 2, "Creature — Wizard");
        common.instance_id = "adept-common".to_string();
        let mut rare = draft_card("Adept", &["W"], 2, "Creature — Wizard");
        rare.instance_id = "adept-rare".to_string();
        rare.rarity = "rare".to_string();

        let groups = DraftPoolGroups::from_pool(&[common, rare], &DraftSource::single_set("TST"));

        assert_eq!(
            groups
                .rarity_groups
                .iter()
                .map(|group| (group.kind, group.cards[0].instance_ids.clone()))
                .collect::<Vec<_>>(),
            vec![
                (DraftPoolGroupKind::Rare, vec!["adept-rare".to_string()]),
                (DraftPoolGroupKind::Common, vec!["adept-common".to_string()]),
            ],
            "each rarity group addresses exactly its own copy"
        );
        // The shared-classification axis still collapses both copies into one
        // entry — and that entry addresses BOTH instances.
        assert_eq!(
            groups.type_groups[0].cards[0].instance_ids,
            vec!["adept-common".to_string(), "adept-rare".to_string()]
        );
        assert_eq!(groups.type_groups[0].cards[0].count, 2);
        assert_eq!(
            groups.workspace_row_classification.creature_instance_ids,
            vec!["adept-common", "adept-rare"]
        );
        assert!(groups
            .workspace_row_classification
            .noncreature_instance_ids
            .is_empty());
    }

    #[test]
    fn the_player_view_publishes_the_shape_and_set_of_every_booster() {
        let (mut session, source) = test_session(2);
        session.config.source = DraftSource::Set {
            layout: SetLayout::UniformByRound {
                codes: vec!["AAA".to_string(), "BBB".to_string(), "AAA".to_string()],
            },
        };
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();
        // Pretend the table has moved on to the second booster.
        session.pack_sizes = vec![15, 14, 15];
        session.current_pack_number = 1;

        let view = filter_for_player(&session, 0);

        assert_eq!(view.pack_sizes, vec![15, 14, 15]);
        assert_eq!(
            view.pack_set_codes,
            vec!["AAA".to_string(), "BBB".to_string(), "AAA".to_string()]
        );
        // `cards_per_pack` tracks the booster in play, not a session-wide size.
        assert_eq!(view.cards_per_pack, 14);
    }

    #[test]
    fn chaos_source_views_publish_only_viewer_scoped_assignment_metadata() {
        let (mut session, source) = test_session(2);
        session.config.source = DraftSource::Set {
            layout: SetLayout::Chaos {
                candidate_codes: vec!["AAA".to_string(), "BBB".to_string()],
                assignments: vec![
                    vec!["AAA".to_string(), "BBB".to_string(), "AAA".to_string()],
                    vec!["BBB".to_string(), "AAA".to_string(), "BBB".to_string()],
                ],
            },
        };

        let lobby = filter_for_player(&session, 0);
        assert!(lobby.pack_set_codes.is_empty());
        assert!(matches!(
            lobby.source,
            DraftSourceView::Set {
                layout: SetLayoutView::Chaos {
                    current_pack_code: None,
                    completed_own_pack_codes: None,
                    actual_set_codes: None,
                    ..
                }
            }
        ));

        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();
        session.current_pack_number = 1;
        let player_zero = filter_for_player(&session, 0);
        let player_one = filter_for_player(&session, 1);
        let source_for = |view: DraftPlayerView| match view.source {
            DraftSourceView::Set {
                layout:
                    SetLayoutView::Chaos {
                        candidate_codes,
                        current_pack_code,
                        completed_own_pack_codes,
                        actual_set_codes,
                    },
            } => (
                candidate_codes,
                current_pack_code,
                completed_own_pack_codes,
                actual_set_codes,
            ),
            _ => panic!("expected a Chaos source view"),
        };
        let (candidate_codes, current_pack_code, completed_codes, actual_codes) =
            source_for(player_zero);
        assert_eq!(candidate_codes, vec!["AAA".to_string(), "BBB".to_string()]);
        assert_eq!(current_pack_code, Some("BBB".to_string()));
        assert_eq!(completed_codes, None);
        assert_eq!(actual_codes, None);
        assert_eq!(source_for(player_one).1, Some("AAA".to_string()));

        session.current_pack[0] = None;
        session.current_pack_origins[0] = None;
        let after_pick = filter_for_player(&session, 0);
        assert!(matches!(
            after_pick.source,
            DraftSourceView::Set {
                layout: SetLayoutView::Chaos {
                    current_pack_code: None,
                    ..
                }
            }
        ));
        session.current_pack[0] = session.current_pack[1].clone();
        session.current_pack_origins[0] = session.current_pack_origins[1];

        let spectator = filter_for_spectator(&session, SpectatorVisibility::Public);
        assert!(spectator.pack_set_codes.is_empty());
        assert!(matches!(
            spectator.source,
            DraftSourceView::Set {
                layout: SetLayoutView::Chaos {
                    current_pack_code: None,
                    completed_own_pack_codes: None,
                    actual_set_codes: None,
                    ..
                }
            }
        ));
        let omniscient = filter_for_spectator(&session, SpectatorVisibility::Omniscient);
        assert!(omniscient.pools.is_none());
        assert!(omniscient.current_packs.is_none());
        assert!(matches!(
            omniscient.source,
            DraftSourceView::Set {
                layout: SetLayoutView::Chaos {
                    current_pack_code: None,
                    completed_own_pack_codes: None,
                    actual_set_codes: None,
                    ..
                }
            }
        ));

        session.status = DraftStatus::Deckbuilding;
        let deckbuilder = filter_for_player(&session, 0);
        match &deckbuilder.source {
            DraftSourceView::Set {
                layout:
                    SetLayoutView::Chaos {
                        completed_own_pack_codes,
                        actual_set_codes,
                        ..
                    },
            } => {
                assert_eq!(
                    completed_own_pack_codes,
                    &Some(vec![
                        "AAA".to_string(),
                        "BBB".to_string(),
                        "AAA".to_string()
                    ])
                );
                assert_eq!(
                    actual_set_codes,
                    &Some(vec!["AAA".to_string(), "BBB".to_string()])
                );
            }
            _ => panic!("expected a Chaos source view"),
        }

        let serialized = serde_json::to_value(deckbuilder).expect("serialize player view");
        assert!(
            serialized
                .pointer("/source/data/layout/Chaos/assignments")
                .is_none(),
            "the source view must have no assignment matrix to leak"
        );
    }

    #[test]
    fn chaos_commander_concessions_wait_for_the_deckbuilding_view() {
        let (mut session, _) = test_session(2);
        session.kind = DraftKind::CommanderDraft;
        session.config.kind = DraftKind::CommanderDraft;
        session.config.source = DraftSource::Set {
            layout: SetLayout::Chaos {
                candidate_codes: vec!["CMM".to_string(), "CLB".to_string()],
                assignments: vec![
                    vec!["CMM".to_string(), "CLB".to_string(), "CMM".to_string()],
                    vec!["CLB".to_string(), "CMM".to_string(), "CLB".to_string()],
                ],
            },
        };
        session.status = DraftStatus::Drafting;

        let drafting = filter_for_player(&session, 0);
        assert!(drafting.draft_set_codes.is_empty());
        assert!(drafting.grantable_commander_fillers.is_empty());

        session.status = DraftStatus::Deckbuilding;
        let deckbuilding = filter_for_player(&session, 0);
        assert_eq!(
            deckbuilding.draft_set_codes,
            vec!["CMM".to_string(), "CLB".to_string()]
        );
        assert_eq!(
            deckbuilding.grantable_commander_fillers,
            engine::game::deck_validation::draft_set_concessions_for(["CMM", "CLB"]).fillers
        );

        let spectator = filter_for_spectator(&session, SpectatorVisibility::Public);
        assert!(
            spectator.grantable_commander_fillers.is_empty(),
            "a spectator is not a deckbuilding owner"
        );
    }

    #[test]
    fn chaos_current_pack_code_follows_the_packs_opening_seat_after_a_pass() {
        let (mut session, source) = test_session(2);
        session.config.source = DraftSource::Set {
            layout: SetLayout::Chaos {
                candidate_codes: vec!["AAA".to_string(), "BBB".to_string()],
                assignments: vec![
                    vec!["AAA".to_string(), "AAA".to_string(), "AAA".to_string()],
                    vec!["BBB".to_string(), "BBB".to_string(), "BBB".to_string()],
                ],
            },
        };
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let current_code = |view: DraftPlayerView| match view.source {
            DraftSourceView::Set {
                layout:
                    SetLayoutView::Chaos {
                        current_pack_code: Some(code),
                        ..
                    },
            } => code,
            _ => panic!("expected a current Chaos pack code"),
        };
        assert_eq!(current_code(filter_for_player(&session, 0)), "AAA");
        assert_eq!(current_code(filter_for_player(&session, 1)), "BBB");

        for seat in 0..2 {
            let card_instance_id = session.current_pack[seat]
                .as_ref()
                .expect("each seat begins with a pack")
                .0[0]
                .instance_id
                .clone();
            session::apply(
                &mut session,
                DraftAction::Pick {
                    seat: seat as u8,
                    card_instance_ids: vec![card_instance_id],
                },
                None,
            )
            .expect("the second pick passes both boosters");
        }

        assert_eq!(session.current_pack_origins, vec![Some(1), Some(0)]);
        assert_eq!(
            current_code(filter_for_player(&session, 0)),
            "BBB",
            "seat 0 now holds the booster that seat 1 opened"
        );
        assert_eq!(
            current_code(filter_for_player(&session, 1)),
            "AAA",
            "seat 1 now holds the booster that seat 0 opened"
        );
    }

    #[test]
    fn a_mixed_size_sealed_pool_splits_back_into_the_boosters_it_came_from() {
        let (mut session, source) = test_session(2);
        session.kind = DraftKind::Sealed;
        session.config.kind = DraftKind::Sealed;
        session.config.pack_count = SEALED_PACK_COUNT;
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();
        // Sealed pools are stored flat; only the recorded sizes say where one
        // booster ended and the next began.
        session.pack_sizes = vec![20, 20, 20, 8, 8, 8];

        let view = filter_for_player(&session, 0);

        let packs = view
            .sealed_packs
            .expect("sealed events publish their packs");
        assert_eq!(
            packs.iter().map(Vec::len).collect::<Vec<_>>(),
            [20, 20, 20, 8, 8, 8]
        );
    }

    #[test]
    fn rarity_group_kinds_match_the_wire_contract() {
        let values = [
            (DraftPoolGroupKind::Mythic, "mythic"),
            (DraftPoolGroupKind::Rare, "rare"),
            (DraftPoolGroupKind::Uncommon, "uncommon"),
            (DraftPoolGroupKind::Common, "common"),
            (DraftPoolGroupKind::RarityOther, "rarity_other"),
        ];

        for (kind, expected) in values {
            assert_eq!(serde_json::to_value(kind).unwrap(), expected);
        }
    }

    #[test]
    fn mana_value_group_kinds_match_the_wire_contract() {
        let values = [
            (DraftPoolGroupKind::ManaValue0, "mana_value0"),
            (DraftPoolGroupKind::ManaValue1, "mana_value1"),
            (DraftPoolGroupKind::ManaValue2, "mana_value2"),
            (DraftPoolGroupKind::ManaValue3, "mana_value3"),
            (DraftPoolGroupKind::ManaValue4, "mana_value4"),
            (DraftPoolGroupKind::ManaValue5, "mana_value5"),
            (DraftPoolGroupKind::ManaValue6Plus, "mana_value6_plus"),
        ];

        for (kind, expected) in values {
            assert_eq!(serde_json::to_value(kind).unwrap(), expected);
        }
    }

    #[test]
    fn view_contains_public_status_fields() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_player(&session, 0);
        assert_eq!(view.status, DraftStatus::Drafting);
        assert_eq!(view.kind, DraftKind::Premier);
        assert_eq!(view.current_pack_number, 0);
        assert_eq!(view.pick_number, 0);
        assert_eq!(view.pass_direction, PassDirection::Left);
        assert_eq!(view.cards_per_pack, 14);
        assert_eq!(view.pack_count, 3);
    }

    #[test]
    fn view_does_not_contain_other_players_packs() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_player(&session, 0);
        let json = serde_json::to_string(&view).unwrap();

        // Check that no other seat's card instance IDs appear in the view
        for seat in 1..8u8 {
            let other_pack = session.current_pack[seat as usize].as_ref().unwrap();
            for card in &other_pack.0 {
                assert!(
                    !json.contains(&card.instance_id),
                    "view for seat 0 leaks seat {seat}'s card {}",
                    card.instance_id
                );
            }
        }
    }

    #[test]
    fn view_does_not_contain_other_players_pools() {
        let (mut session, source) = test_session(8);
        start_and_pick(&mut session, &source);

        // Make a pick for seat 1 too
        let card_id = session.current_pack[1].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 1,
                card_instance_ids: vec![card_id],
            },
            None,
        )
        .unwrap();

        let view = filter_for_player(&session, 0);
        let json = serde_json::to_string(&view).unwrap();

        // Seat 1's pool card should not appear
        for card in &session.pools[1] {
            assert!(
                !json.contains(&card.instance_id),
                "view for seat 0 leaks seat 1's pool card {}",
                card.instance_id
            );
        }
    }

    #[test]
    fn view_does_not_contain_rng_seed() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_player(&session, 0);
        let json = serde_json::to_string(&view).unwrap();

        // The seed is 42, check it doesn't appear as "rng_seed" anywhere
        assert!(
            !json.contains("rng_seed"),
            "view should not contain rng_seed field"
        );
    }

    #[test]
    fn view_shows_seat_public_info() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_player(&session, 0);
        assert_eq!(view.seats.len(), 8);

        for (i, seat_view) in view.seats.iter().enumerate() {
            assert_eq!(seat_view.seat_index, i as u8);
            assert_eq!(seat_view.display_name, format!("Player {i}"));
            assert!(!seat_view.is_bot);
            assert!(seat_view.connected);
            assert!(!seat_view.has_submitted_deck);
        }
    }

    #[test]
    fn view_shows_submission_status_without_deck_contents() {
        let (mut session, _) = test_session(2);
        session.status = DraftStatus::Deckbuilding;

        // Give seat 0 a pool
        session.pools[0] = (0..42)
            .map(|i| DraftCardInstance {
                instance_id: format!("card-{i}"),
                name: format!("Card {i}"),
                set_code: "TST".to_string(),
                collector_number: format!("{i}"),
                rarity: "common".to_string(),
                colors: Vec::new(),
                cmc: 0,
                type_line: String::new(),
                draft_effect: None,
            })
            .collect();
        session.pools[1] = session.pools[0].clone();

        // Seat 0 submits a deck
        let mut main_deck: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
        main_deck.extend(std::iter::repeat_n("Plains".to_string(), 17));

        session::apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: main_deck.clone(),
                commanders: Vec::new(),
            },
            None,
        )
        .unwrap();

        // View from seat 1 should show seat 0 has submitted
        let view = filter_for_player(&session, 1);
        assert!(view.seats[0].has_submitted_deck);
        assert!(!view.seats[1].has_submitted_deck);

        // But the view should not contain the deck card names as a "main_deck" field
        let json = serde_json::to_string(&view).unwrap();
        assert!(
            !json.contains("main_deck"),
            "view should not contain submitted deck contents"
        );
    }

    #[test]
    fn view_does_not_contain_unopened_packs() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_player(&session, 0);
        let json = serde_json::to_string(&view).unwrap();

        // packs_by_seat should not appear in the view
        assert!(
            !json.contains("packs_by_seat"),
            "view should not contain unopened packs"
        );

        // Verify unopened packs exist in the session but not in the view
        assert!(!session.packs_by_seat[0].is_empty());
    }

    #[test]
    fn view_bot_seat_shows_as_bot() {
        let config = DraftConfig {
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Quick,
            pod_size: 8,
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let mut seats = vec![DraftSeat::Human {
            player_id: PlayerId(0),
            display_name: "Human".to_string(),
        }];
        for i in 1..8u8 {
            seats.push(DraftSeat::Bot {
                name: format!("Bot {i}"),
            });
        }
        let session = DraftSession::new(config, seats, "BOT-TEST".to_string());

        let view = filter_for_player(&session, 0);
        assert!(!view.seats[0].is_bot);
        assert!(view.seats[0].connected);
        for i in 1..8 {
            assert!(view.seats[i].is_bot);
            assert!(view.seats[i].connected); // bots always connected
        }
    }

    #[test]
    fn view_pick_status_during_drafting() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        // During drafting, all seats with packs show as Pending
        let view = filter_for_player(&session, 0);
        for seat in &view.seats {
            assert_eq!(seat.pick_status, PickStatus::Pending);
        }

        // After seat 0 picks, the pack still exists (with one fewer card).
        // Picks only resolve when ALL seats pick, so individual pick status
        // during a round is tracked by the P2P host, not the session reducer.
        let card_id = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_ids: vec![card_id],
            },
            None,
        )
        .unwrap();

        let view = filter_for_player(&session, 0);
        // Seat 0 still has a current_pack (13 cards remain), so shows as Pending
        assert_eq!(view.seats[0].pick_status, PickStatus::Pending);
    }

    #[test]
    fn view_pick_status_not_drafting() {
        let (session, _) = test_session(8);
        // Lobby status
        let view = filter_for_player(&session, 0);
        for seat in &view.seats {
            assert_eq!(seat.pick_status, PickStatus::NotDrafting);
        }
    }

    #[test]
    fn public_active_pack_count_tracks_the_actual_pick_and_pass_round() {
        let (mut session, source) = test_session(2);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let assert_counts = |session: &DraftSession, expected: &[u8]| {
            assert_eq!(
                filter_for_player(session, 0)
                    .seats
                    .iter()
                    .map(|seat| seat.active_pack_count)
                    .collect::<Vec<_>>(),
                expected,
            );
            assert_eq!(
                filter_for_spectator(session, SpectatorVisibility::Public)
                    .seats
                    .iter()
                    .map(|seat| seat.active_pack_count)
                    .collect::<Vec<_>>(),
                expected,
            );
        };

        assert_counts(&session, &[1, 1]);

        let seat_zero_card = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_ids: vec![seat_zero_card],
            },
            None,
        )
        .unwrap();
        // The pack remains until every seat picks, but seat 0 no longer has an
        // active pack in this round.
        assert_counts(&session, &[0, 1]);

        let seat_one_card = session.current_pack[1].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 1,
                card_instance_ids: vec![seat_one_card],
            },
            None,
        )
        .unwrap();
        // The real pass resets the per-seat picked flags for the next round.
        assert_counts(&session, &[1, 1]);

        // A present-but-empty pack is not active either.
        session.current_pack[0].as_mut().unwrap().0.clear();
        assert_counts(&session, &[0, 1]);

        // A stale current_pack is not an active pack once drafting has ended.
        session.status = DraftStatus::Deckbuilding;
        for seat in filter_for_player(&session, 0).seats {
            assert_eq!(seat.active_pack_count, 0);
        }
        for seat in filter_for_spectator(&session, SpectatorVisibility::Public).seats {
            assert_eq!(seat.active_pack_count, 0);
        }
    }

    #[test]
    fn view_standings_after_pairings() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        // Generate pairings
        session::apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let winner_pid = session
            .pairings
            .iter()
            .find(|p| p.match_id == "r1-t0")
            .unwrap()
            .players[0];

        session::apply(
            &mut session,
            DraftAction::ReportMatchResult {
                match_id: "r1-t0".to_string(),
                winner_seat: Some(winner_pid.0),
            },
            None,
        )
        .unwrap();

        let view = filter_for_player(&session, 0);
        assert!(!view.standings.is_empty());

        let winner_standing = view
            .standings
            .iter()
            .find(|s| s.seat_index == winner_pid.0)
            .unwrap();
        assert_eq!(winner_standing.match_wins, 1);
        assert_eq!(winner_standing.match_losses, 0);

        // Standings should be sorted by match_wins descending
        for window in view.standings.windows(2) {
            assert!(window[0].match_wins >= window[1].match_wins);
        }
    }

    #[test]
    fn view_standings_include_bot_seats() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;
        session.seats[7] = DraftSeat::Bot {
            name: "Bot 7".to_string(),
        };

        session::apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let view = filter_for_player(&session, 0);
        let bot_standing = view
            .standings
            .iter()
            .find(|standing| standing.seat_index == 7)
            .unwrap();
        assert_eq!(bot_standing.display_name, "Bot 7");
    }

    #[test]
    fn view_standings_empty_before_pairings() {
        let (session, _) = test_session(8);
        let view = filter_for_player(&session, 0);
        assert!(view.standings.is_empty());
    }

    #[test]
    fn view_has_config_fields() {
        let (session, _) = test_session(8);
        let view = filter_for_player(&session, 0);
        assert_eq!(view.tournament_format, TournamentFormat::Swiss);
        assert_eq!(view.pod_policy, PodPolicy::Competitive);
        assert_eq!(view.current_round, 0);
        // Pins the engine's `>= 1` guarantee at the lobby state. NOT
        // discriminating on its own: a hard-coded `1` satisfies it too.
        assert_eq!(view.next_pairing_round, 1);
        assert!(view.timer_remaining_ms.is_none());
    }

    #[test]
    fn view_pairings_for_current_round() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        session::apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let view = filter_for_player(&session, 0);
        assert_eq!(view.pairings.len(), 4);
        assert_eq!(view.current_round, 1);
        assert_eq!(view.next_pairing_round, 2);
        for pv in &view.pairings {
            assert_eq!(pv.round, 1);
            assert_eq!(pv.status, PairingStatus::Pending);
            assert!(pv.winner_seat.is_none());
        }
    }

    #[test]
    fn view_pairing_winner_seat_uses_pairing_result() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        session::apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let pairing = session
            .pairings
            .iter()
            .find(|p| p.match_id == "r1-t0")
            .unwrap()
            .clone();

        session::apply(
            &mut session,
            DraftAction::ReportMatchResult {
                match_id: pairing.match_id.clone(),
                winner_seat: Some(pairing.players[1].0),
            },
            None,
        )
        .unwrap();

        let view = filter_for_player(&session, 0);
        let pairing_view = view
            .pairings
            .iter()
            .find(|p| p.match_id == pairing.match_id)
            .unwrap();
        assert_eq!(pairing_view.winner_seat, Some(pairing.players[1].0));
    }

    #[test]
    fn view_pairing_winner_seat_infers_legacy_completed_result() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        session::apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let pairing = session
            .pairings
            .iter()
            .find(|p| p.match_id == "r1-t0")
            .unwrap()
            .clone();

        session
            .pairings
            .iter_mut()
            .find(|p| p.match_id == pairing.match_id)
            .unwrap()
            .status = PairingStatus::Complete;
        session.match_records.insert(
            pairing.players[1],
            DraftMatchRecord {
                player: pairing.players[1],
                wins: 1,
                losses: 0,
                draws: 0,
                match_wins: 1,
                match_losses: 0,
            },
        );

        let view = filter_for_player(&session, 0);
        let pairing_view = view
            .pairings
            .iter()
            .find(|p| p.match_id == pairing.match_id)
            .unwrap();
        assert_eq!(pairing_view.winner_seat, Some(pairing.players[1].0));
    }

    #[test]
    fn pairing_view_score_fields_default_to_none() {
        // BO3-06: PairingView score_a/score_b are None when match not started.
        // This test deliberately references score_a/score_b to create a compile
        // error until Plan 01 adds these fields to PairingView.
        let view = PairingView {
            round: 1,
            table: 1,
            seat_a: 0,
            name_a: "Alice".to_string(),
            seat_b: 1,
            name_b: "Bob".to_string(),
            match_id: "m1".to_string(),
            status: PairingStatus::Pending,
            winner_seat: None,
            score_a: None, // Compile-fails until Plan 01 adds this field
            score_b: None, // Compile-fails until Plan 01 adds this field
        };
        assert_eq!(view.score_a, None);
        assert_eq!(view.score_b, None);
    }

    #[test]
    fn spectator_public_view_hides_pools_and_packs() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_spectator(&session, SpectatorVisibility::Public);
        assert!(view.pools.is_none());
        assert!(view.current_packs.is_none());
        assert_eq!(view.seats.len(), 8);
        assert_eq!(view.status, DraftStatus::Drafting);
        assert_eq!(view.kind, DraftKind::Premier);
    }

    #[test]
    fn spectator_omniscient_view_exposes_all_pools() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_spectator(&session, SpectatorVisibility::Omniscient);
        assert!(view.pools.is_some());
        assert_eq!(view.pools.as_ref().unwrap().len(), 8);
        assert!(view.current_packs.is_some());
        assert_eq!(view.current_packs.as_ref().unwrap().len(), 8);
        // All seats should have a current pack during drafting
        for pack in view.current_packs.as_ref().unwrap() {
            assert!(pack.is_some());
        }
    }

    #[test]
    fn spectator_public_view_has_standings_and_pairings() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        session::apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let view = filter_for_spectator(&session, SpectatorVisibility::Public);
        assert_eq!(view.pairings.len(), 4);
        assert!(view.pools.is_none());
    }

    /// U7 row 14 -- CR 903.13e: the grant is PUBLISHED on both views, on one
    /// axis with opposite verdicts.
    ///
    /// Both builders are asserted because one correctly-wired builder must not
    /// be able to vouch for the other. The `Some` half is the reach guard:
    /// without it, "the field is `None`" is satisfied by a builder that
    /// hard-codes `None`, which is exactly the mis-wiring this row exists to
    /// catch. And the `Some` half asserts EQUALITY WITH THE TABLE rather than
    /// naming a card, which additionally proves the builder reads the latch
    /// instead of constructing its own value.
    ///
    /// Rendering landed in phase 8 -- `PoolPanel`'s grant line and
    /// `LimitedDeckBuilder`'s addable list -- while this test still pins the
    /// publishing half.
    #[test]
    fn both_views_publish_the_latched_commander_filler() {
        fn commander_draft_session(set_code: &str) -> DraftSession {
            let (mut session, _) = test_session(4);
            session.kind = DraftKind::CommanderDraft;
            session.config.kind = DraftKind::CommanderDraft;
            session.config.source = DraftSource::single_set(set_code);
            session
        }

        let granting = commander_draft_session("CMM");
        let expected = engine::game::deck_validation::draft_set_concessions("CMM").fillers;
        assert!(
            !expected.is_empty(),
            "reach guard: CR 903.13e names Commander Masters as a granting set"
        );

        assert_eq!(
            filter_for_player(&granting, 0).grantable_commander_fillers,
            expected
        );
        assert_eq!(
            filter_for_spectator(&granting, SpectatorVisibility::default())
                .grantable_commander_fillers,
            expected
        );

        let non_granting = commander_draft_session("NEO");
        assert!(filter_for_player(&non_granting, 0)
            .grantable_commander_fillers
            .is_empty());
        assert!(
            filter_for_spectator(&non_granting, SpectatorVisibility::default())
                .grantable_commander_fillers
                .is_empty()
        );

        // CR 903.13e: a mixed-set draft publishes EVERY contained set's grant.
        // Both builders read one latch, so both must carry the union -- a
        // builder that published only the first would red on this pair.
        let mut mixed = commander_draft_session("CMM");
        mixed.config.source = DraftSource::Set {
            layout: SetLayout::UniformByRound {
                codes: vec!["CMM".to_string(), "CLB".to_string()],
            },
        };
        let union =
            engine::game::deck_validation::draft_set_concessions_for(["CMM", "CLB"]).fillers;
        assert_eq!(
            union.len(),
            2,
            "reach guard: CR 903.13e names DIFFERENT cards for CMM and CLB"
        );
        assert_eq!(
            filter_for_player(&mixed, 0).grantable_commander_fillers,
            union
        );
        assert_eq!(
            filter_for_spectator(&mixed, SpectatorVisibility::default())
                .grantable_commander_fillers,
            union
        );
    }

    /// V4 -- CR 903.13f(3): `DraftPlayerView.draft_set_codes` publishes the
    /// LATCHED concession set codes, and publishes them only for a Commander
    /// Draft whose source is a set.
    ///
    /// Four rows on one axis, because a single non-empty row is satisfied by
    /// `session.config.source.set_code()` -- which returns the CUBE ID for a
    /// cube, a code for every kind, and the JOINED `"CMM+CLB"` label for a
    /// mixed draft -- and would publish a grant CR 903.13 does not make, or a
    /// token no set-code lookup can match. Row (i) carries a reach guard
    /// (`grantable_commander_fillers` is non-empty) so the two empty rows
    /// cannot be vacuous greens from a fixture that concedes nothing in the
    /// first place.
    #[test]
    fn publishes_the_latched_concession_set_codes_only_for_a_commander_draft_from_a_set() {
        fn session_with(kind: DraftKind, source: DraftSource) -> DraftSession {
            let (mut session, _) = test_session(4);
            session.kind = kind;
            session.config.kind = kind;
            session.config.source = source;
            session
        }

        // (i) Commander Draft from a granting set: the latch is published.
        let from_set = session_with(DraftKind::CommanderDraft, DraftSource::single_set("CMM"));
        let from_set_view = filter_for_player(&from_set, 0);
        assert!(
            !from_set_view.grantable_commander_fillers.is_empty(),
            "reach guard: CR 903.13e names Commander Masters as a granting set, \
             so this fixture really is a conceding session"
        );
        assert_eq!(from_set_view.draft_set_codes, vec!["CMM".to_string()]);

        // (i-b) A mixed Commander Draft publishes EVERY set it contained, as
        // separate codes. The `"CMM+CLB"` label `DraftSource::set_code()`
        // builds is a DISPLAY string that no concession lookup can match, so
        // publishing it here would silently disable both grants.
        let mixed = session_with(
            DraftKind::CommanderDraft,
            DraftSource::Set {
                layout: SetLayout::UniformByRound {
                    codes: vec!["CMM".to_string(), "CLB".to_string(), "CMM".to_string()],
                },
            },
        );
        assert_eq!(
            filter_for_player(&mixed, 0).draft_set_codes,
            vec!["CMM".to_string(), "CLB".to_string()],
            "CR 903.13e/f ask what the draft CONTAINED, and it contained both"
        );

        // (ii) A cube contains no draft boosters from any set. `set_code()`
        // would answer with the cube ID here, which is the wrong answer.
        let from_cube = session_with(
            DraftKind::CommanderDraft,
            DraftSource::Cube {
                id: "CMM".to_string(),
                name: "Test Cube".to_string(),
            },
        );
        assert!(filter_for_player(&from_cube, 0).draft_set_codes.is_empty());

        // (iii) CR 903.13 scopes both concessions to Commander Draft.
        let sealed = session_with(DraftKind::Sealed, DraftSource::single_set("CMM"));
        assert!(filter_for_player(&sealed, 0).draft_set_codes.is_empty());
    }
    /// VM row 3 — PF3 / U25. CR 903.13b: the published pick-step count, folded
    /// over every kind in the procedure table.
    ///
    /// `pick_number` counts STEPS, not cards, so a 14-card Commander pack is
    /// SEVEN steps. Revert the publication and the field is gone (a compile
    /// error); publish `cards_per_pack` instead and the CommanderDraft row
    /// reds at 14 against an expected 7.
    ///
    /// The fold is its own reach-guard: it asserts a nonzero, per-kind value
    /// for all five kinds, so an all-zeros field cannot pass it. The four
    /// CR 905.1a kinds are the reach-guard against a field that is only
    /// correct for CommanderDraft — for them the value EQUALS `cards_per_pack`,
    /// so a field that merely echoed `cards_per_pack` would pass 4/5 and fail
    /// only on the fifth.
    #[test]
    fn the_published_pick_step_count_is_per_kind_and_matches_the_procedure_table() {
        for kind in DraftKind::ALL {
            let (mut session, _) = test_session(4);
            session.kind = kind;
            session.config.kind = kind;

            let procedure = kind.procedure();
            let cards_per_pack = session.config.cards_per_pack;

            // The divisor invariant §Rust Idioms relies on, pinned rather than
            // defended with a `.max(1)`: a future table row that set `0` here
            // reds this assertion instead of dividing by zero.
            assert!(
                procedure.cards_per_pick >= 1,
                "{kind:?}: every kind takes at least one card per pick step"
            );

            let expected = cards_per_pack.div_ceil(procedure.cards_per_pick);
            assert!(expected >= 1, "{kind:?}: a pack is at least one step");

            assert_eq!(
                filter_for_player(&session, 0).pick_steps_per_pack,
                expected,
                "{kind:?}: the player view must publish the engine's own step count"
            );
            assert_eq!(
                filter_for_spectator(&session, SpectatorVisibility::Public).pick_steps_per_pack,
                expected,
                "{kind:?}: the spectator view publishes the same count"
            );
        }

        // The values the fold above computes, stated as literals so a reader
        // can see WHAT is being asserted and not merely that two expressions
        // agree. A 14-card pack is 14 steps at one card per step (CR 905.1a)
        // and 7 at two (CR 903.13b).
        assert_eq!(DraftKind::Premier.procedure().pick_steps_per_pack(14), 14);
        assert_eq!(
            DraftKind::CommanderDraft
                .procedure()
                .pick_steps_per_pack(14),
            7
        );
        // Rounds UP: an odd pack's final step takes the remainder.
        assert_eq!(
            DraftKind::CommanderDraft
                .procedure()
                .pick_steps_per_pack(15),
            8
        );
    }

    #[test]
    fn player_view_publishes_the_procedure_owned_launch_capability() {
        let (mut session, _) = test_session(4);
        session.kind = DraftKind::Quick;
        session.config.kind = DraftKind::Quick;
        assert_eq!(
            filter_for_player(&session, 0).launch_capability,
            DraftLaunchCapability::None,
            "complete-immediately alone must not authorize a multiplayer pod game"
        );

        session.kind = DraftKind::CommanderDraft;
        session.config.kind = DraftKind::CommanderDraft;
        assert_eq!(
            filter_for_player(&session, 0).launch_capability,
            DraftLaunchCapability::CommanderMultiplayer,
            "the Commander procedure's post-draft and designation axes authorize its pod game"
        );

        session.kind = DraftKind::Premier;
        session.config.kind = DraftKind::Premier;
        assert_eq!(
            filter_for_player(&session, 0).launch_capability,
            DraftLaunchCapability::None,
            "in-session tournament pairings must not expose an external game launch"
        );
    }

    /// CR 903.13b, per pack. `pack_pick_steps` is the per-pack counterpart of
    /// the scalar above, and it exists because BOTH axes vary independently: a
    /// multi-set draft's boosters differ in size, and the kind's procedure
    /// decides how many cards one step takes.
    ///
    /// CommanderDraft is the discriminating kind. At two cards per step
    /// (CR 903.13b), boosters of 20/14/16 cards are 10/7/8 steps — a triple
    /// that no competing implementation reproduces:
    ///   - publishing `pack_sizes` gives [20, 14, 16] (cards, not steps);
    ///   - broadcasting the scalar `pick_steps_per_pack` gives [7, 7, 7]
    ///     (the current pack's count, applied to every pack);
    ///   - halving a session-wide `config.cards_per_pack` gives [7, 7, 7] too.
    ///
    /// Each of those reds here while passing every single-set fixture.
    #[test]
    fn the_published_per_pack_step_counts_track_each_boosters_own_size() {
        let (mut session, _) = test_session(4);
        session.kind = DraftKind::CommanderDraft;
        session.config.kind = DraftKind::CommanderDraft;
        session.config.pack_count = 3;
        // A multi-set Commander draft: three boosters, three sizes.
        session.pack_sizes = vec![20, 14, 16];

        let player = filter_for_player(&session, 0);
        assert_eq!(
            player.pack_pick_steps,
            vec![10, 7, 8],
            "each booster is measured in its own pick steps, not its card count"
        );
        assert_eq!(
            filter_for_spectator(&session, SpectatorVisibility::Public).pack_pick_steps,
            vec![10, 7, 8],
            "the spectator view publishes the same per-pack counts"
        );

        // The array and the scalar are one contract: the scalar is the entry
        // for the booster in play, so a display reading either agrees.
        for pack in 0..session.config.pack_count {
            session.current_pack_number = pack;
            let view = filter_for_player(&session, 0);
            assert_eq!(
                view.pick_steps_per_pack,
                view.pack_pick_steps[usize::from(pack)],
                "pack {pack}: the scalar must equal this pack's entry"
            );
        }

        // Reach-guard for the CR 905.1a kinds: at one card per step the steps
        // ARE the sizes, so the field still tracks per-pack shape rather than
        // collapsing to a single number.
        session.kind = DraftKind::Premier;
        session.config.kind = DraftKind::Premier;
        assert_eq!(
            filter_for_player(&session, 0).pack_pick_steps,
            vec![20, 14, 16],
            "one card per step means steps equal cards — still per pack"
        );
    }
}
