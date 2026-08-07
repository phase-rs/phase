// engine-citation-gate: symbol anchors only
//! Engine-authored presentation projections over `GameState`.
//!
//! These "derived views" are computed just-in-time at serialization
//! boundaries (the WASM getter, the server-core broadcast) and sent to
//! clients alongside the raw state. Display consumers (React components)
//! consume the pre-grouped shape directly and never compute game logic
//! themselves — per CLAUDE.md's "engine owns all logic" invariant.
//!
//! Contrast with `crates/engine/src/game/derived.rs`, which contains
//! engine-internal state derivation (summoning sickness, commander damage
//! aggregation, etc.). This module is a thin presentation-facing wrapper
//! that composes those helpers into a client-ready shape.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::analysis::resource::ResourceAxis;
use crate::game::ability_utils::flatten_targets_in_chain;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::game_object::AttachTarget;
use crate::game::stack::{effective_stack_ability, stack_display_groups, StackDisplayGroup};
use crate::types::ability::{
    ContinuousModification, Duration, GameRestriction, KeywordAction, ProhibitedActivity,
    RestrictionExpiry, RestrictionPlayerScope, TargetFilter, TargetRef,
};
use crate::types::attribution::EffectRef;
use crate::types::card::TokenImageRef;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::format::GameFormat;
use crate::types::game_state::{
    CastingVariant, GameState, StackEntry, StackEntryKind, StackPaidSnapshot,
    SyntheticTriggerProvenance,
};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::layers::Layer;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;
use crate::types::statics::{StaticMode, StaticModeKind};
use crate::types::zones::Zone;

fn is_false(value: &bool) -> bool {
    !*value
}

/// A single commander-damage badge the HUD renders: which victim received
/// `damage` from `commander` (the ObjectId is stable across zone changes
/// because commanders live in `state.objects` for the life of the game).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommanderDamageView {
    pub victim: PlayerId,
    pub commander: ObjectId,
    pub damage: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackTargetDisplay {
    pub target: TargetRef,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum StackPaidFactView {
    XValue { value: u32 },
    ManaSpent { amount: u32 },
    ColorsSpent { distinct: u32 },
    Kicked { count: usize },
    AdditionalCostPaid,
    CastVariant { variant: String },
    Convoked { count: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerContextDisplay {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_id: Option<ObjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player: Option<PlayerId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackEntryDisplay {
    pub source_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_image_ref: Option<TokenImageRef>,
    pub kind_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ability_description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_mode_labels: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_pending: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<StackTargetDisplay>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paid: Vec<StackPaidFactView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trigger_context: Vec<TriggerContextDisplay>,
    /// Typed synthesized-trigger presentation provenance. This is the only
    /// stack provenance surface consumed by the frontend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<SyntheticTriggerProvenance>,
}

/// A single player-affecting condition the HUD surfaces as a status icon.
///
/// **Presentation-only discriminant.** `kind` selects an icon + i18n key; it
/// deliberately spans multiple CR sections (CR 104.2b, CR 119.7/.8, CR 118.3,
/// CR 101.2 / CR 702.50b) because the display layer groups "conditions
/// afflicting a player" regardless of which rules section produced them. The
/// categorical-boundary rule governs *rules-primitive* enums; this lives in
/// the `DerivedViews` presentation layer alongside `StackPaidFactView`, so
/// the cross-section span is correct here, not a sibling-cluster smell. The
/// authoritative rules state remains in `StaticMode`, `GameRestriction`, and
/// `EpicEffect` — this enum never feeds game logic, only rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum PlayerConditionKind {
    /// CR 104.2b: effect-based win attempts targeting this player are no-ops.
    CantWin,
    /// CR 119.7: life-gain events affecting this player are replaced with nothing.
    CantGainLife,
    /// CR 119.8: life-loss events affecting this player are replaced with nothing.
    CantLoseLife,
    /// CR 118.3: this player can't pay life as a cost.
    CantPayLifeAsCost,
    /// CR 101.2 / CR 702.50b: this player can't cast spells (Epic lock or a
    /// temporary `ProhibitActivity::CastSpells`, possibly spell-filtered — the
    /// `source` card identifies the specifics for the tooltip).
    CantCastSpells,
    /// CR 101.2 + CR 602.5: this player can't activate abilities (mana abilities
    /// may still be exempt — the `source` card identifies the specifics).
    CantActivateAbilities,
    /// CR 101.2 + CR 601.2a: this player may cast spells only from the listed zones.
    CastOnlyFromZones { allowed_zones: Vec<Zone> },
}

/// One rendered row of player status: which `player` is under a `kind` of
/// condition, and the permanent `source` imposing it (when known).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerStatusView {
    pub player: PlayerId,
    pub kind: PlayerConditionKind,
    /// The permanent imposing the condition, when the engine surfaces it.
    /// `None` for the statics-scanned life/cost conditions whose authority
    /// predicate returns a bare `bool` — recovering the granting permanent
    /// would require a second scan, so the FE tooltip falls back to the
    /// condition name. `Some` for stored `GameRestriction`/`EpicEffect` rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ObjectId>,
}

/// One rendered `∞` HUD row: a detected/forced unbounded loop pumps `axis`, and
/// the engine attributes the badge to `player` (the HUD it attaches to). `axis` is the
/// engine-provided identity, and the engine also owns the display family it groups into
/// ([`family_of`], published per seat as [`UnboundedFamilyView`]) — the display layer decides
/// neither attribution, nor which axes are unbounded, nor whether a collapse is coming.
///
/// `player` is computed by [`attribution_player`] (NOT the raw producing
/// controller): a payload-keyed axis (`Life(p)`/`DamageDealt(p)`/`LibraryDelta(p)`)
/// routes to the player it names (the drain/mill victim or the lifegain/self-mill
/// beneficiary), while aggregate axes route to the loop's controller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnboundedResourceView {
    pub player: PlayerId,
    pub axis: ResourceAxis,
}

/// The display family an unbounded [`ResourceAxis`] groups into. No CR governs a display
/// grouping (cf. `game/filter.rs`'s `context_free_prop_matches_face` Kleene `AnyOf` arm).
///
/// NOT `analysis::corpus::ResourceFamily` — different module, lossy
/// family→representative-axis map, no total inverse, no `Poison`→counters variant.
///
/// `rename_all` so the wire strings ARE the client's family literals — one type, no mirror map.
/// Pinned against the client's `Record<ResourceAxisTag, UnboundedFamily>` by the
/// `unbounded-family-tags.json` golden, which carries all 17 axis-tag→family pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnboundedFamily {
    Mana,
    Life,
    Damage,
    Mill,
    Counters,
    Tokens,
    Cards,
    Casts,
    Combats,
    Turns,
    Triggers,
}

/// Whether the boundary can still fail to apply this scheduled collapse. No CR governs it — it is
/// derived from `engine_resolution_choices::materialization_certainty`, which reads the boundary's
/// own non-push-exit census, never a copy of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CollapseCertainty {
    Committed,
    Conditional,
}

impl CollapseCertainty {
    /// `Conditional` wins: a family is only as certain as its least certain member. No CR governs
    /// this — it is a meet over a display promise, not a rules behavior (cf. `game/filter.rs`'s
    /// `context_free_prop_matches_face` Kleene `AnyOf` arm).
    fn weaker(self, other: Self) -> Self {
        match (self, other) {
            (Self::Committed, Self::Committed) => Self::Committed,
            _ => Self::Conditional,
        }
    }
}

/// This family's collapse coverage. `Scheduled` displays
/// `GameState::pending_unbounded_materialization` — growth whose count was fixed at accept and
/// which is in flight along CR 732.2c's advance to the proposal's ending point, that point being a
/// priority window per CR 732.2a and not the CR 500.5 boundary the stash is cashed out at
/// (`types/game_state.rs`'s `scheduled_collapse_axes` doc, and this file's
/// `THE WINDOW'S TIMING IS CR 732.2c'S ADVANCE` block). An earlier version of this doc called that
/// stash unlicensed; it is not — see `scheduled_collapse_axes` for the four-position reading.
/// `Scheduled` is nonetheless a WEAKER claim than `Committed`, and that distinction is the point of
/// this enum: `Committed` is what licenses the `∞→N` badge, which is a promise about what will
/// land, and the engine makes that promise only where it can keep it. `Mixed` is a join result.
///
/// `Unscheduled` is the one variant a CR describes, and only in the SHAPED sense the rest of this
/// crate uses (this file's `IS AN ENGINE-STATE ARGUMENT, NOT A RULES ONE` block): CR 732.1b's
/// antecedent is a state "in which a set of actions could be repeated indefinitely", and an ∞ axis
/// with nothing staged is exactly that — a legal, ordinary game state, pre-proposal. The rule's
/// PERMISSION clause is not what is cited and is never authority for engine conduct.
///
/// NOTE — distinct from `types/game_state.rs`'s `LoopCollapseAxis::Mixed`, which means "the stash
/// spans ≥2 axis KINDS" and only labels the finite-count prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum FamilyCollapseState {
    Unscheduled,
    Mixed,
    Scheduled(CollapseCertainty),
}

impl FamilyCollapseState {
    /// Join: `Scheduled(a) ⊔ Scheduled(b) = Scheduled(weaker)`, `Unscheduled ⊔ Scheduled(_) =
    /// Mixed`, `Mixed` is top. Commutative + associative + idempotent because it IS a join —
    /// load-bearing: the FE fold it replaces documented a last-wins order hazard, and its open
    /// question ("what would make the over-report reachable") is settled here rather than avoided:
    /// `Mixed` is REPRESENTABLE, so a mixed family renders a bare `∞` instead of a wrong `∞→N`.
    /// Witnessed by `mixed_family_is_not_scheduled` and
    /// `two_controllers_draining_one_victim_do_not_cross_schedule`.
    ///
    /// No CR governs this — it is a join over a display projection, not a rules behavior
    /// (cf. `game/filter.rs`'s `context_free_prop_matches_face` Kleene `AnyOf` arm).
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Mixed, _) | (_, Self::Mixed) => Self::Mixed,
            (Self::Unscheduled, Self::Unscheduled) => Self::Unscheduled,
            (Self::Unscheduled, Self::Scheduled(_)) | (Self::Scheduled(_), Self::Unscheduled) => {
                Self::Mixed
            }
            (Self::Scheduled(a), Self::Scheduled(b)) => Self::Scheduled(a.weaker(b)),
        }
    }
}

/// One HUD badge's engine-owned collapse state, keyed per seat and per display family.
///
/// COMPUTED HERE, AT THE PRODUCING CONTROLLER KEY, and never by joining two channels downstream.
/// The primary reason is layering — a join is a computation over game state, which the display
/// layer does not perform (see `CLAUDE.md`). The supporting reason is that a downstream join is not
/// even well-defined: the row channel keys on [`attribution_player`], so a victim-attributed axis
/// (`Life(p)`/`DamageDealt(p)`/`LibraryDelta(p)`/`Poison(p)`) names its VICTIM; two controllers
/// draining the same victim would collide on `(victim, Life(victim))` and any `(player, axis)` join
/// would mark the wrong controller's row scheduled. The controller key is not recoverable after
/// attribution, so only the producing loop can answer it. That is exactly why the state below is
/// resolved on the controller key, BEFORE attribution runs.
///
/// WHY `(player, FAMILY)` AND NOT `(player, axis)`: the badge is per family, so a single glyph
/// would have to say two things when two same-family axes disagree. It says the true weaker one
/// instead — `Scheduled(_) ⊔ Unscheduled = Mixed`, which renders a bare `∞`. Witnessed by
/// `two_controllers_draining_one_victim_do_not_cross_schedule`.
///
/// `GameState::pending_unbounded_materialization` remains THE authority for the accepted-collapse
/// contract, and it is what the boundary reads to cash the collapse out. It holds an accepted
/// shortcut's results in flight along CR 732.2c's advance to the proposal's ending point — a
/// priority window per CR 732.2a, not the CR 500.5 boundary itself
/// (`types/game_state.rs`'s `scheduled_collapse_axes` doc). It is still not a GUARANTEE of the
/// final amount: the boundary's growth re-check and the controller's CR 732.2a count choice can
/// both reduce what lands, which is why the display carries certainty rather than a
/// number. A second channel mirroring
/// the stash is no longer "a contract with no reader", which it genuinely was when that objection
/// was written:
/// THIS is the reader — `usePlayerDesignations` → `UnboundedBadge`, pinned on the wire by
/// `unbounded-declined-wire.json`.
///
/// SAME-FRAME ASYMMETRY — UNCHANGED AND LIVE. Carried forward from the `scheduled` flag this
/// channel replaced, because retyping the flag as [`FamilyCollapseState`] did not answer the
/// objection, and a reader still sees it on screen. Only THIS channel carries a collapse state.
/// `unbounded_pile` (card groups) and `unbounded_counters` (counter pills) are `ObjectId`-keyed and
/// carry no collapse projection at all, so during the accept→boundary window one loop can show
/// `∞→N` on the badge and a plain `∞` on its own token group and counter pill in the SAME frame.
/// Witnessed rather than asserted:
/// `kilo_live_offer_from_real_dump::kilo_accept_marks_pentad_charge_as_unbounded_display_target`
/// pins `unbounded_counters[Pentad] == [charge]` — a bare `∞` pill — in the exact frame whose
/// golden family state is `Scheduled(Committed)`.
///
/// THE ANSWER, not a disclosure: this is not the `Mana(_)` false-promise case. The collapse really
/// IS scheduled for that axis, so the quiet surfaces under-announce; none of them promises a bound
/// it will not keep. Announcing it on the object-keyed channels would require a
/// `(player, family)` → `ObjectId` join that the engine does not put on the wire, and computing
/// that join downstream is precisely the display-layer computation this channel exists to remove
/// (see `CLAUDE.md`). `Mana(_)` is different in kind — its promise is false the moment it is made —
/// and it is handled by exclusion upstream at `scheduled_display_axes`, not by this asymmetry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnboundedFamilyView {
    pub player: PlayerId,
    pub family: UnboundedFamily,
    pub state: FamilyCollapseState,
}

/// The display family a pumped [`ResourceAxis`] groups into. Exhaustive by design (no wildcard) —
/// a new `ResourceAxis` variant must make a deliberate grouping choice here. Payload-independent
/// by construction: only the variant tag decides the family, exactly as the client's
/// `Record<ResourceAxisTag, UnboundedFamily>` keys on the tag. No CR governs a display grouping.
pub fn family_of(axis: ResourceAxis) -> UnboundedFamily {
    match axis {
        ResourceAxis::Mana(_) => UnboundedFamily::Mana,
        ResourceAxis::Life(_) => UnboundedFamily::Life,
        ResourceAxis::DamageDealt(_) => UnboundedFamily::Damage,
        ResourceAxis::LibraryDelta(_) => UnboundedFamily::Mill,
        ResourceAxis::Counter(_, _) | ResourceAxis::Poison(_) => UnboundedFamily::Counters,
        ResourceAxis::TokensCreated => UnboundedFamily::Tokens,
        ResourceAxis::CardsDrawn => UnboundedFamily::Cards,
        ResourceAxis::Casts => UnboundedFamily::Casts,
        ResourceAxis::CombatPhases => UnboundedFamily::Combats,
        ResourceAxis::ExtraTurns => UnboundedFamily::Turns,
        ResourceAxis::Trigger(_)
        | ResourceAxis::LandfallTriggers
        | ResourceAxis::DeathTriggers
        | ResourceAxis::EtbTriggers
        | ResourceAxis::LtbTriggers
        | ResourceAxis::SacTriggers => UnboundedFamily::Triggers,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanechaseView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_plane: Option<ObjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planar_controller: Option<PlayerId>,
    pub planar_deck_count: usize,
    pub current_roll_cost: ManaCost,
    pub can_roll: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchenemyView {
    pub archenemy: PlayerId,
    pub scheme_deck_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_scheme_ids: Vec<ObjectId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hero_player_ids: Vec<PlayerId>,
}

/// Display-only turn-order chip row. `turns_from_now == 0` is the current
/// turn; `1` is the next actual turn that would begin; larger values count
/// future turn starts after that. Duplicate players are intentional when an
/// extra turn causes the same player to appear in adjacent slots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnOrderSlotView {
    pub player: PlayerId,
    pub slot_index: u8,
    pub turns_from_now: u8,
    /// One-based display position in the projected turn order. Kept here so
    /// clients do not turn the engine's zero-based distance into an ordinal.
    pub turn_number: u8,
    /// Whether this row belongs to the viewing player. Clients consume this
    /// display classification rather than comparing player IDs themselves.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_viewer: bool,
    /// Whether this projected slot is the game's starting player. It is only
    /// true while that player is also the current turn representative.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_starting_player: bool,
}

/// Engine-authored projections used by the display layer. Keep this struct
/// small — every field becomes mandatory payload on every state snapshot
/// the client receives. Add a new field only when the frontend would
/// otherwise have to compute game logic (a CLAUDE.md violation).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedViews {
    /// The sole player currently authorized to answer the live prompt. Omitted
    /// when there is no actor or multiple distinct authorized submitters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique_authorized_submitter: Option<PlayerId>,
    /// The live (post-layer) keyword badges each battlefield permanent should
    /// display. The engine classifies the complete keyword list so the client
    /// can render the compact strip without reinterpreting keyword timing.
    /// Keyed by object ID; absent when a permanent has no display-relevant
    /// keyword.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub battlefield_keyword_badges: HashMap<ObjectId, Vec<Keyword>>,

    /// CR 509.1b + CR 611.2c: creatures with a live, temporary
    /// `CantBeBlocked` grant. The optional value is the granting source only
    /// while that source remains a public, phased-in battlefield object; `None`
    /// retains the badge without exposing an unavailable source.
    ///
    /// Keyed by recipient ObjectId; absent when no such grant is active.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub temporary_cant_be_blocked: HashMap<ObjectId, Option<ObjectId>>,
    /// CR 509.1g: public blocker-to-attacker relationships, flattened as
    /// `(blocker, attacker)` pairs for combat-line rendering. This is sorted
    /// deterministically so equivalent combat states have stable wire output.
    /// The filtered-view projection is explicitly derived from authoritative
    /// combat state because a transport filter may clear raw combat details.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocker_assignment_pairs: Vec<(ObjectId, ObjectId)>,

    /// CR 613.2a + CR 707.2: battlefield permanents whose copiable values are
    /// currently supplied by a copy effect (Layer 1a) — Clone, Phantasmal
    /// Image, Vesuvan Doppelganger, and every "enters as a copy" permanent.
    ///
    /// Such a permanent renders pixel-identical to what it copied (the copy
    /// effect overrides `printed_ref`, so even image lookup follows the copied
    /// card), leaving the player unable to tell the copy from the original on
    /// the board. Nothing already serialized distinguishes them: `is_copy`
    /// means "not represented by a card" (CR 707.10) and is cleared once a copy
    /// resolves onto the battlefield, and the copy modification lives on a
    /// transient continuous effect rather than the object. So the engine
    /// classifies it here rather than leaving the client to infer it.
    ///
    /// CR 708.2: face-down permanents are excluded — their characteristics are
    /// only those the face-down rules grant, so surfacing "copy" on one would
    /// leak hidden information.
    ///
    /// Sorted for stable serialization; absent when nothing is a copy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub copied_permanents: Vec<ObjectId>,

    /// Commander damage grouped by the attacking commander's current
    /// controller. Each inner entry preserves per-commander identity so
    /// partner commanders under one controller render as separate badges.
    /// Empty in non-Commander formats (see `derive_views` JIT short-circuit).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub commander_damage_by_attacker: BTreeMap<PlayerId, Vec<CommanderDamageView>>,

    /// Engine-authored coalesced view of the stack. Adjacent entries with
    /// the same (source, kind, description, targets) signature collapse
    /// into one `StackDisplayGroup` with a `count`. Empty when the stack
    /// is empty (JIT short-circuit). The frontend renders one card + ×N
    /// badge per group and never re-implements the grouping rule.
    /// Authoritative grouping lives in `game::stack::stack_display_groups`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stack_display_groups: Vec<StackDisplayGroup>,

    /// Display-ready facts for each stack entry: chosen targets, ability labels,
    /// paid cast facts, and public trigger context. Empty when the stack is empty.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub stack_entry_details: HashMap<ObjectId, StackEntryDisplay>,

    /// CR 702.40a: copy counts for Storm spells in the viewing player's hand.
    /// Keyed only by that viewer's hand object ids so hidden opponents' card
    /// abilities and the table-wide spell ledger cannot leak through the view.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub prospective_storm_counts: HashMap<ObjectId, u32>,

    /// CR 303.4 + CR 702.5: Auras attached to each player (Curse cycle,
    /// Faith's Fetters-class). Players have no `attachments` back-link
    /// because they aren't `GameObject`s — this projection is the engine's
    /// answer to "which Auras enchant player X" so the HUD can render them
    /// tucked next to each player's avatar without scanning the battlefield
    /// itself. Mirrors the Object-host case (`GameObject::attachments`)
    /// shape-for-shape: the value list contains battlefield ObjectIds whose
    /// `attached_to` resolves to the keyed PlayerId. Empty entries omitted
    /// — a player with no enchanting Auras simply has no key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub auras_attached_to_player: BTreeMap<PlayerId, Vec<ObjectId>>,

    /// CR 702.188a + 604.1: web-slinging alt-cost the VIEWING player may pay for each
    /// qualifying card in their OWN hand (incl. statically-granted web-slinging). Keyed by
    /// hand ObjectId. Populated ONLY for the `viewer` passed to derive_views and ONLY from
    /// that viewer's hand — never another player's — so it cannot leak which opponent/AI
    /// cards qualify, even on the unfiltered get_game_state() path. Empty when no viewer,
    /// no granting static, or no qualifying card.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub web_slinging_costs: HashMap<ObjectId, ManaCost>,

    /// Player-affecting continuous conditions (CR 104.2b / 119.7 / 119.8 /
    /// 118.3 / 101.2 / 702.50b) the HUD renders as status icons. Aggregates
    /// the statics-scanned `player_has_*` authorities and the stored
    /// `restrictions`/`epic_effects` so the frontend never re-scans static
    /// abilities to learn that a player "can't gain life" or "can't cast".
    /// Empty (and omitted) in the dominant case where no player is afflicted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub player_status: Vec<PlayerStatusView>,

    /// CR 118.3a + CR 601.2g: during the viewer's own manual mana payment for a
    /// spell, the portion of the locked cost still UNPAID by the pool units they
    /// have pinned (selected) so far — the cost reduced against a pool of ONLY
    /// those pinned units. Lets the payment UI show the cost shrinking as the
    /// player picks mana, and "covered" (`NoCost`) when their selection alone
    /// pays the whole cost. `None` outside a non-convoke spell `ManaPayment` the
    /// viewer controls. Viewer-scoped — one caster's private in-progress choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_payment_remaining: Option<ManaCost>,

    /// CR 901: Engine-authored Planechase presentation state. The frontend
    /// renders this directly instead of deriving the active plane from command
    /// zone objects or recomputing planar-die legality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planechase: Option<PlanechaseView>,

    /// CR 904: Engine-authored Archenemy presentation state. The frontend
    /// renders this directly instead of deriving active schemes from command
    /// zone objects or recomputing side membership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archenemy: Option<ArchenemyView>,

    /// CR 101.4 + CR 103.1 + CR 500.7 + CR 614.10 + CR 805.4: Compact
    /// multiplayer turn-order slots. Empty for one-on-one games because the
    /// existing active-player ring is sufficient; populated by engine-owned
    /// projection so the frontend never computes extra/skipped/controlled turn
    /// order from raw state.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turn_order: Vec<TurnOrderSlotView>,

    /// One-based projected turn position for the viewing player. This keeps
    /// player-specific turn-order interpretation in the engine while allowing
    /// the client to render "You take turn N" directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer_turn_number: Option<u8>,

    /// CR 732.2a: the `∞` HUD rows — one per (attributed player, pumped axis) of
    /// every unbounded-resource loop in `GameState::unbounded_resources`. The
    /// engine decides the axis identity, the player attribution
    /// ([`attribution_player`]) and the display family ([`family_of`], published as
    /// `unbounded_families` below); the frontend renders what it is handed.
    /// Empty (and omitted) in the dominant case where no loop is active.
    ///
    /// NOT a straight projection of the mark: a TOKEN-axis row is withheld when its entire
    /// registered pile has left the battlefield ([`object_growth_backing`]), so this can carry
    /// FEWER axes than `GameState::unbounded_resources` marks. The mark and the accepted stash
    /// are both unaffected by that — it is a display decision, never a cancellation of agreed
    /// growth (CR 732.2c). A withheld row therefore does NOT mean the collapse was cancelled;
    /// `pending_unbounded_materialization` still carries it and the boundary still applies it,
    /// which `combo_infinite_pile::object_growth_infinity_row_dies_with_its_last_pile_member`
    /// asserts at the store level.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unbounded_resources: Vec<UnboundedResourceView>,

    /// The engine-owned per-seat, per-display-family collapse state behind each `∞` badge —
    /// the channel that replaced the client's row-flag OR-fold. One row per
    /// `(attributed player, family)` actually rendered; see [`UnboundedFamilyView`] for why the
    /// state is resolved on the PRODUCING CONTROLLER key and joined at family granularity.
    /// Empty (and omitted) whenever `unbounded_resources` is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unbounded_families: Vec<UnboundedFamilyView>,

    /// CR 732.2a / CR 110.1: the battlefield objects forming an accepted
    /// object-growth loop's "∞ pile" — the winning controller's tapped fodder-class
    /// members (projected from `GameState::unbounded_loop_pile`, filtered to objects
    /// still on the battlefield). A per-object membership channel mirroring
    /// `battlefield_keyword_badges`: the frontend renders `∞` (not `×N`) on any
    /// battlefield group whose members are all in this set. Public board state — no
    /// viewer filtering. Empty (and omitted) when no object-growth loop is active.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unbounded_pile: Vec<ObjectId>,

    /// CR 732.2a / CR 701.34a: the per-object `∞` COUNTER channel — for each
    /// battlefield object, the counter types whose preserved `Generic` counters an
    /// accepted counter-growth loop (proliferate charge on Pentad Prism, burden on
    /// The One Ring) pumps unboundedly (projected from
    /// `GameState::unbounded_counter_targets`, filtered to objects still on the
    /// battlefield). The counter analog of `unbounded_pile`: object-growth marks whole
    /// objects, but a counter-growth loop's unbounded axis is object-agnostic, so this
    /// keys the specific pumped counter so the frontend renders `∞` (not `×N`) on that
    /// counter pill and nothing else. Keyed by ObjectId; DISPLAY-only (the real counter
    /// count is unchanged). Public board state — no viewer filtering. Empty (and
    /// omitted) when no counter-growth loop is active — the dominant case.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub unbounded_counters: HashMap<ObjectId, Vec<CounterType>>,
}

/// Serialize-only wrapper: the WASM getter passes `&GameState` by reference
/// to avoid an O(n) clone of `state.objects` and other owned collections
/// (GameState is not rpds-backed at the top level). The wire shape is
/// `{ state: <GameState>, derived: <DerivedViews> }`.
#[derive(Debug)]
pub struct ClientGameStateRef<'a> {
    pub state: &'a GameState,
    pub derived: DerivedViews,
}

impl Serialize for ClientGameStateRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct ClientGameStateEnvelope<'a> {
            state: &'a serde_json::Value,
            derived: &'a DerivedViews,
        }

        let state = client_state_wire_value(self.state).map_err(serde::ser::Error::custom)?;
        ClientGameStateEnvelope {
            state: &state,
            derived: &self.derived,
        }
        .serialize(serializer)
    }
}

/// Produces the client-only state representation without changing the trusted
/// persistence schema of [`GameState`]. Delayed-trigger receipts and their
/// allocators authorize replay/transition handling; clients receive neither
/// those private capabilities nor the resolved journal that contains them.
fn client_state_wire_value(state: &GameState) -> serde_json::Result<serde_json::Value> {
    let mut value = serde_json::to_value(state)?;
    let Some(root) = value.as_object_mut() else {
        return Ok(value);
    };

    root.remove("next_delayed_trigger_token");
    root.remove("next_delayed_trigger_instance");
    root.remove("pending_trigger_firing");
    root.remove("stack_trigger_firings");
    root.remove("resolving_trigger_firing");
    root.remove("resolved_rules_journal");

    redact_private_trigger_firing(&mut value);

    Ok(value)
}

/// Removes every private firing/provenance carrier recursively. Resolution
/// frames evolve frequently, so redacting only named queue paths would leak a
/// newly nested continuation without any compiler signal.
fn redact_private_trigger_firing(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for key in [
                "provenance",
                "firing",
                "firing_classification",
                "trigger_firing",
                "stack_trigger_firings",
                "resolving_trigger_firing",
            ] {
                object.remove(key);
            }
            for child in object.values_mut() {
                redact_private_trigger_firing(child);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                redact_private_trigger_firing(child);
            }
        }
        _ => {}
    }
}

impl<'a> ClientGameStateRef<'a> {
    /// Wrap an unfiltered borrowed `GameState` with its derived projections.
    /// Viewer-filtered paths must use [`Self::wrap_filtered`] so redaction cannot
    /// erase an authoritative decision projection.
    pub fn wrap(state: &'a GameState, viewer: Option<PlayerId>) -> Self {
        Self {
            state,
            derived: derive_views(state, viewer),
        }
    }

    /// Wrap a viewer-filtered state while deriving rules-authoritative fields
    /// from the pre-filter state. Filtering may redact control/session records;
    /// it must not change who can submit the current decision.
    pub fn wrap_filtered(
        authoritative_state: &GameState,
        filtered_state: &'a GameState,
        viewer: Option<PlayerId>,
    ) -> Self {
        Self {
            state: filtered_state,
            derived: derive_filtered_views(authoritative_state, filtered_state, viewer),
        }
    }
}

/// Owned counterpart for deserialize paths (round-trip tests, any future
/// state-restore flow that ingests the wire format). The JSON shape matches
/// `ClientGameStateRef` exactly — fields named identically, no
/// `#[serde(flatten)]` — so serialize/deserialize round-trip is lossless.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientGameState {
    pub state: GameState,
    pub derived: DerivedViews,
}

/// Compute all engine-authored projections over `state`. Runs in O(damage
/// entries) per call; the JIT short-circuit for non-Commander formats
/// (where `commander_damage_threshold` is `None`) keeps the cost at exactly
/// zero for the overwhelmingly common case.
///
/// CR 903.10a: commander damage is public information tracked per commander
/// — no viewer-based redaction is applied here, and the grouping runs
/// unconditionally for every Commander-format game regardless of who is
/// viewing. Partner commanders under the same controller each get their
/// own `CommanderDamageView` entry, not a summed total.
/// CR 118.3a + CR 601.2g: the cost still unpaid by `viewer`'s pinned pool units
/// during their own manual mana payment for a spell. Reduces the locked spell
/// cost against a pool containing ONLY the pinned units (so the residual is
/// exactly what the player has chosen to spend), under the same spend-restriction
/// context (`PaymentContext::Spell`) the finalize spend uses. Returns `None`
/// unless the viewer is mid (non-convoke) spell `ManaPayment` with a pending
/// cast — activated-ability mana payment keeps its full-cost display, and
/// convoke/improvise/delve pay via board taps tracked by their own staged UI.
///
/// KNOWN LIMITATION: reduces with `any_color = false` and no life-for-color
/// permissions, so under an any-color spend permission (Chromatic Orrery) or a
/// K'rrik-style life-as-colored-mana grant the displayed residual can over-state
/// the cost (a colorless unit pinned toward `{R}` reads as not covering it).
/// This is deliberately consistent with the pin-eligibility gate
/// (`mana_unit_eligible_for_cost`), which is also `any_color`-blind and would
/// reject such a pin — both layers agree on the stricter behavior, and the
/// common cases (generic + plain colored costs) are exact. Threading the real
/// permission bundle through both sites is the follow-up to lift this.
fn pending_payment_remaining(state: &GameState, viewer: PlayerId) -> Option<ManaCost> {
    use crate::types::game_state::WaitingFor;
    use crate::types::mana::{ManaPool, PaymentContext};

    let WaitingFor::ManaPayment {
        player,
        convoke_mode,
    } = &state.waiting_for
    else {
        return None;
    };
    if *player != viewer || convoke_mode.is_some() {
        return None;
    }

    let pending = state.pending_cast.as_ref()?;
    // The mana portion the spend funnel reduces is `pending.cost` for both spells
    // and activations; live-shrink is scoped to spell casts, where that cost is
    // exactly what the payment panel displays (no activated-ability cost mismatch).
    if pending.activation_ability_index.is_some() {
        return None;
    }
    let cost = pending.cost.clone();

    // Scratch pool of ONLY the pinned units = the player's current selection.
    let player_obj = state.players.iter().find(|p| p.id == viewer)?;
    let mut selected = ManaPool::default();
    for unit in &player_obj.mana_pool.mana {
        if pending.pinned_pool_units.contains(&unit.pip_id) {
            selected.add(unit.clone());
        }
    }

    // CR 106.6: reduce under the SAME spend-restriction context the finalize
    // spend uses, so restricted mana the spell can't accept stays in the residual.
    let spell_meta = crate::game::casting::build_spell_meta(state, viewer, pending.object_id);
    let ctx = spell_meta.as_ref().map(PaymentContext::Spell);
    Some(crate::game::mana_payment::reduce_cost_by_pool(
        &selected,
        &cost,
        ctx.as_ref(),
        false,
        None,
    ))
}

/// CR 613.2a + CR 707.2: true when a live copy effect is currently supplying
/// `object_id`'s copiable values.
///
/// A copy of a permanent is expressed as a transient continuous effect carrying
/// `ContinuousModification::CopyValues`, applied in Layer 1a — never as a flag
/// on the object — so membership is decided by asking the same question the
/// layer engine asks: does this effect's `affected` filter match the object?
/// Reusing `matches_target_filter` (rather than special-casing the
/// `SpecificObject` filter copy effects usually carry) keeps the projection
/// correct for any filter shape a future copy effect might use.
fn object_has_copy_effect(state: &GameState, object_id: ObjectId) -> bool {
    let merge_layer_effect_id = state
        .objects
        .get(&object_id)
        .and_then(|object| object.merge_layer_effect_id);

    state.transient_continuous_effects.iter().any(|effect| {
        // A lapsed effect stays stored until it is swept, so "is it in the list"
        // is not the same question as "is it applying". Zygon Infiltrator's copy
        // ends the instant its target untaps while the effect remains stored —
        // badging that permanent would claim a copy the layer engine has already
        // stopped applying. Gated on the layer engine's own predicate so the two
        // cannot drift apart.
        crate::game::layers::transient_effect_is_live(state, effect)
            // CR 730.2a: merge uses a private Layer 1 `CopyValues` effect to
            // represent its top component, but a merged permanent is not a
            // copy. Exclude only that recorded representation: a merged object
            // may still acquire an independent copy effect.
            && Some(effect.id) != merge_layer_effect_id
            && effect
                .modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::CopyValues { .. }))
            && matches_target_filter(
                state,
                object_id,
                &effect.affected,
                &FilterContext::from_source(state, effect.source_id),
            )
    })
}

/// CR 509.1b + CR 611.2c + CR 613.1f: return the public source for the first
/// live, until-end-of-turn `CantBeBlocked` modification attributed to this
/// recipient in the current ability layer. `Some(None)` means the grant is
/// active but its source is no longer a public, phased-in battlefield object.
///
/// Attribution is the layer engine's authoritative record of modifications
/// that actually applied. Reading its indexed `EffectRef` avoids re-scanning
/// raw effect filters, which could disagree with the resolved layer recipient.
fn temporary_cant_be_blocked_source(
    state: &GameState,
    object_id: ObjectId,
) -> Option<Option<ObjectId>> {
    let effects = state
        .attribution
        .get(&object_id)?
        .by_layer
        .get(&Layer::Ability)?;
    effects.iter().find_map(|effect_ref| {
        let EffectRef::Transient { id, mod_index } = effect_ref else {
            return None;
        };
        let effect = state
            .transient_continuous_effects
            .iter()
            .find(|effect| effect.id == *id)?;
        (effect.duration == Duration::UntilEndOfTurn
            && crate::game::layers::transient_effect_is_live(state, effect)
            && matches!(
                effect.modifications.get(*mod_index),
                Some(ContinuousModification::AddStaticMode {
                    mode: StaticMode::CantBeBlocked
                })
            ))
        .then(|| {
            state
                .objects
                .get(&effect.source_id)
                .filter(|source| source.zone == Zone::Battlefield && source.is_phased_in())
                .map(|_| effect.source_id)
        })
    })
}

pub fn derive_views(state: &GameState, viewer: Option<PlayerId>) -> DerivedViews {
    let mut views = DerivedViews {
        unique_authorized_submitter: unique_authorized_submitter(state),
        blocker_assignment_pairs: blocker_assignment_pairs(state),
        ..DerivedViews::default()
    };

    // JIT short-circuit: grouping an empty stack is free, but this also
    // avoids the per-entry allocation path entirely for the dominant case
    // (no spells/abilities in flight).
    if !state.stack.is_empty() {
        views.stack_display_groups = stack_display_groups(state);
        views.stack_entry_details = stack_entry_details(state);
    }

    // CR 303.4 + CR 702.5: Walk the battlefield once and bucket Player-host
    // attachments by their host PlayerId. Object-host attachments are skipped
    // here — those are surfaced through `GameObject::attachments` on the host
    // itself and consumed by `PermanentCard`'s recursive render. The walk is
    // O(battlefield size); the BTreeMap stays empty (and `skip_serializing_if`
    // omits the field) when no Auras are enchanting any player, which is the
    // dominant case.
    for &obj_id in &state.battlefield {
        let Some(obj) = state.objects.get(&obj_id) else {
            continue;
        };
        if obj.zone != Zone::Battlefield {
            continue;
        }
        let badges: Vec<Keyword> = obj
            .keywords
            .iter()
            .filter(|keyword| keyword.is_battlefield_display_relevant())
            .cloned()
            .collect();
        if !badges.is_empty() {
            views.battlefield_keyword_badges.insert(obj_id, badges);
        }
        if let Some(source_id) = temporary_cant_be_blocked_source(state, obj_id) {
            views.temporary_cant_be_blocked.insert(obj_id, source_id);
        }
        // CR 613.2a + CR 707.2 / CR 708.2: see `copied_permanents`. Matched
        // through the same `matches_target_filter` the layer engine uses to
        // pick a continuous effect's recipients, so this projection and the
        // effect that actually rewrote the object's characteristics can never
        // disagree about who is a copy.
        if !obj.face_down && object_has_copy_effect(state, obj_id) {
            views.copied_permanents.push(obj_id);
        }
        if let Some(AttachTarget::Player(host)) = obj.attached_to {
            views
                .auras_attached_to_player
                .entry(host)
                .or_default()
                .push(obj_id);
        }
    }
    // Collected in `state.battlefield` order above, which reorders as permanents
    // enter and leave; sort so the serialized payload depends only on WHICH
    // permanents are copies, not on battlefield churn that never changed the
    // answer.
    views.copied_permanents.sort_unstable();

    // CR 702.40a: viewer-scoped prospective Storm copy counts (own hand only → leak-proof).
    if let Some(viewer) = viewer {
        if let Some(player) = state.players.iter().find(|player| player.id == viewer) {
            // `effective_spell_keywords` evaluates every keyword-grant source. Most
            // snapshots have neither a Storm card nor a possible Storm grant, so only
            // take that expensive path when one exists. The fallback remains necessary
            // for CR 604.1 / CR 611.2c / CR 601.2f grants.
            let may_have_granted_storm =
                (crate::game::functioning_abilities::static_kind_present(
                    state,
                    StaticModeKind::CastWithKeyword,
                ) && crate::game::functioning_abilities::game_active_statics(state).any(
                    |(_, definition)| {
                        matches!(
                            &definition.mode,
                            StaticMode::CastWithKeyword {
                                keyword: Keyword::Storm
                            }
                        )
                    },
                )) || state.transient_continuous_effects.iter().any(|effect| {
                    matches!(&effect.affected, TargetFilter::SpecificPlayer { id } if *id == viewer)
                        && effect.modifications.iter().any(|modification| {
                            matches!(
                                modification,
                                ContinuousModification::GrantStaticAbility { definition }
                                    if matches!(
                                        &definition.mode,
                                        StaticMode::CastWithKeyword {
                                            keyword: Keyword::Storm
                                        }
                                    )
                            )
                        })
                }) || state.pending_next_spell_modifiers.iter().any(|modifier| {
                    matches!(
                        modifier,
                        crate::types::game_state::PendingNextSpellModifier {
                            player,
                            modifier: crate::types::game_state::NextSpellModifier::HasKeyword {
                                keyword: Keyword::Storm
                            },
                            ..
                        } if *player == viewer
                    )
                });
            let mut copy_count = None;
            for &hand_id in player.hand.iter() {
                let has_printed_storm = state.objects.get(&hand_id).is_some_and(|object| {
                    object
                        .keywords
                        .iter()
                        .any(|keyword| matches!(keyword, Keyword::Storm))
                });
                if has_printed_storm
                    || (may_have_granted_storm
                        && crate::game::casting::effective_spell_keywords(state, viewer, hand_id)
                            .iter()
                            .any(|keyword| matches!(keyword, Keyword::Storm)))
                {
                    let copy_count = *copy_count.get_or_insert_with(|| {
                        state
                            .spells_cast_this_turn_by_player
                            .values()
                            .map(|records| records.len())
                            .sum::<usize>() as u32
                    });
                    views.prospective_storm_counts.insert(hand_id, copy_count);
                }
            }
        }

        // CR 702.188a + 604.1: viewer-scoped web-slinging costs (own hand only → leak-proof).
        let has_web_slinging_static =
            crate::game::functioning_abilities::game_active_statics(state).any(|(_, def)| {
                matches!(
                    def.mode,
                    StaticMode::CastWithKeyword {
                        keyword: Keyword::WebSlinging(_)
                    }
                )
            });
        if has_web_slinging_static {
            if let Some(player) = state.players.iter().find(|p| p.id == viewer) {
                for &hand_id in player.hand.iter() {
                    if let Some(cost) =
                        crate::game::keywords::effective_web_slinging_cost(state, viewer, hand_id)
                    {
                        views.web_slinging_costs.insert(hand_id, cost);
                    }
                }
            }
        }
    }

    // CR 118.3a + CR 601.2g: viewer-scoped remaining cost after the caster's
    // pinned (selected) pool mana — drives the payment UI's live-shrinking cost.
    if let Some(viewer) = viewer {
        views.pending_payment_remaining = pending_payment_remaining(state, viewer);
    }

    if state.format_config.format == GameFormat::Planechase {
        let roll_player = crate::game::turn_control::priority_seat(state);
        let can_viewer_roll = viewer.is_some_and(|viewer| {
            crate::game::turn_control::authorized_submitter_for_player(state, roll_player) == viewer
                && crate::game::planechase::can_roll_planar_die(state, roll_player)
        });
        views.planechase = Some(PlanechaseView {
            active_plane: crate::game::planechase::active_plane(state),
            planar_controller: state.planar_controller,
            planar_deck_count: state.planar_deck.len(),
            current_roll_cost: crate::game::planechase::planar_die_roll_cost(state, roll_player),
            can_roll: can_viewer_roll,
        });
    }

    if state.format_config.format == GameFormat::Archenemy {
        if let Some(archenemy) = crate::game::topology::archenemy(state) {
            let hero_player_ids = state
                .seat_order
                .iter()
                .copied()
                .find(|&player| player != archenemy)
                .map(|hero| crate::game::topology::team_members(state, hero))
                .unwrap_or_default();
            views.archenemy = Some(ArchenemyView {
                archenemy,
                scheme_deck_count: state.scheme_deck.len(),
                active_scheme_ids: crate::game::archenemy::active_schemes(state),
                hero_player_ids,
            });
        }
    }

    // CR 104.2b / 119.7 / 119.8 / 118.3 / 101.2 / 702.50b: aggregate
    // player-affecting conditions so the HUD can render status icons without
    // re-scanning static abilities. Runs in every format (not gated by the
    // Commander short-circuit below).
    views.player_status = player_status_views(state);

    let (turn_order, viewer_turn_number) = turn_order_views(state, viewer);
    views.turn_order = turn_order;
    views.viewer_turn_number = viewer_turn_number;

    // WHY THE THREE ∞ SURFACE CHANNELS BELOW ARE UNCONDITIONAL — the accept→boundary window.
    //
    // THE WINDOW'S TIMING IS CR 732.2c'S ADVANCE, NOT A DEVIATION FROM IT. CR 732.2c: once the
    // last player accepts, "the game advances to the last proposed ending point, with all game
    // choices contained in the shortcut proposal having been taken". Per CR 732.2a that ending
    // point "must be a place where a player has priority" — so it is NOT the CR 500.5 step/phase
    // end, which is a turn-based action, with priority arriving at the beginning of the next step
    // (CR 117.3a). The count is resolved at accept (`pending_materialization_count`), and the
    // growth lands at the CR 500.5 boundary while the game is still advancing toward that priority
    // window. State AT the ending point is the proposed state. An earlier version of this block
    // called the whole window unlicensed; that conceded rules the code satisfies. The full
    // four-position reading lives at `types/game_state.rs`'s `scheduled_collapse_axes` doc.
    //
    // WHY THE CHANNELS BELOW STILL CARRY `FamilyCollapseState` RATHER THAN A BARE FLAG: being
    // rules-correct about WHEN the loop closes is not the same as knowing WHAT NUMBER will land.
    // The boundary re-checks whether the growth is still observed, and the controller names the
    // count at the ending point (CR 732.2a's "specified number of times"), so the final amount is
    // not knowable while the badge is on screen. `∞→N` is a promise; the engine makes it only for
    // a family whose amount it can already stand behind, and shows `∞→?` otherwise.
    //
    // The two CRs this code does rely on, each for what it actually governs:
    //  • CR 732.2c — the shortcut is taken at the count every player accepted, so the collapse may
    //    not EXCEED it. `turns.rs`' `max:` reads the recorded bound for exactly that reason and
    //    `SubmitPayAmount` rejects an over-collapse. That is a CEILING on the collapse; it says
    //    nothing about what the display may show, and this projection does not read it.
    //  • CR 500.5 — the TIMING LANDMARK only: it defines the step/phase end, at which
    //    until-end-of-step effects expire and unspent mana empties. That mana drain is the one
    //    thing here CR 500.5 genuinely governs (`turns::drain_pending_phase_transition_progress`,
    //    and it is why a `Mana(_)` ∞ ends there). It does NOT license CASHING OUT the deferred
    //    token/life/counter growth at that moment — the engine chose that landmark as the point
    //    along CR 732.2c's advance at which the elided loop closes (CR 732.1b), as described above.
    //
    // WHY `∞` IS RIGHT HERE IS AN ENGINE-STATE ARGUMENT, NOT A RULES ONE. Throughout the window the
    // loop's enabling permanents are still on the battlefield and `unbounded_resources` still
    // carries the mark, so the controller really does still hold a set of actions that could be
    // repeated indefinitely — a CR-732.1b-SHAPED capability, which is the same sense the rest of
    // this crate cites CR 732.1b in. `∞` renders that live mark honestly.
    //
    // WHAT THIS PROJECTION DOES **NOT** CLAIM: that the mark is REVOCABLE for this class. The
    // zone-exit defuse (`zones::apply_zone_exit_cleanup`) is gated on a NON-EMPTY
    // `unbounded_loop_enablers`, and the only production writer of that map is the Interactive
    // Path-C arm (`engine.rs`'s `register_unbounded_loop_enablers` call).
    // `materialize_object_growth_shortcut` never registers enablers, so for the OBJECT-GROWTH class
    // — which is exactly the token and counter families this projection displays — the defuse gate
    // never matches and is INERT. `engine_resolution_choices.rs` documents that gap in those words
    // and tracks it as a pre-existing deferred follow-up; it is not introduced here.
    //
    // CONSEQUENCE, STATED RATHER THAN BURIED: because that defuse is inert for this class, an
    // enabler leaving the battlefield between accept and boundary leaves a STALE `∞` in the STORE.
    // The store is deliberately NOT filtered (the defuse and the boundary both read it), so the
    // live-authority check lives HERE, at the projection: `object_growth_backing` drops a row whose
    // whole registered display set has left the battlefield, exactly as the pile and counter loops
    // already drop individual departed members. That is a DISPLAY revocation only — it never
    // touches `pending_unbounded_materialization`, so the growth the table accepted still lands.
    // Registering enablers instead would route this through `clear_unbounded_loop`, a SIX-map wipe
    // that also destroys the accepted collapse stash and its CR 732.2c bound; see
    // `types::game_state::clear_unbounded_loop`. What ends the MARK for this class is still the
    // boundary, below.
    //
    // And hiding it is strictly worse on display coherence, which is what the old "the badge is a
    // lie" comment was really about. The BASE gate filtered the PROJECTION while the STORE still
    // said `∞` — a HUD contradicting its own engine — and it also suppressed an already-
    // materialized `Mana(_)` axis that `mana_payment::refill_infinite_mana` keeps topping back up,
    // i.e. it hid a badge beside a pool the player can visibly keep spending.
    //
    // NO SURFACE IS FILTERED BY THE SCHEDULE — that, and only that, is the invariant here. Which
    // rows/groups/pills EXIST is decided by the `∞` stores and the LIVE battlefield alone; nothing
    // below hides a surface because a collapse is scheduled. The schedule is read to ANNOTATE, not
    // to filter: the row loop accumulates a per-`(player, family)` `FamilyCollapseState` emitted as
    // a SEPARATE channel (`unbounded_families`), and NO row carries a flag. Still additive.
    //
    // Phrased as an invariant rather than a census of readers, because the census version has now
    // outlived the code beneath it THREE times:
    //   1. "the surface loops read only their own stores" — falsified by `object_growth_backing`,
    //      which deliberately cross-reads the pile and counter-target stores, because whether a
    //      ROW is still live is a question about those backing sets, not about its own.
    //   2. "no surface reads `scheduled_collapse_axes`" — falsified when the `scheduled` flag
    //      moved into the row loop.
    //   3. "…and the tag loop projects the schedule" — falsified when that channel was removed
    //      for having no consumer; and falsified AGAIN, in the opposite direction, when a separate
    //      schedule channel was REINSTATED because a consumer now exists. The reader is
    //      `unbounded_families`, consumed by `usePlayerDesignations` → `UnboundedBadge` and pinned
    //      on the wire by `unbounded-declined-wire.json`.
    //   4. "…and the schedule rides on each row as a `scheduled` flag" — falsified when that flag
    //      was deleted. A per-FAMILY badge cannot render a per-ROW flag honestly: two same-family
    //      axes that disagree need a third answer, which is what `FamilyCollapseState::Mixed` is.
    // Naming WHO reads the schedule is a claim every future consumer can break; naming what the
    // schedule may not DO is not. The stores are not filtered either:
    // `unbounded_resources` keeps the mark until the boundary applies the growth. (`unbounded_loop_enablers` is held in
    // lockstep with it as an ENGINE-STATE invariant required by no CR — but see the inertness note
    // above: for the object-growth class that map is EMPTY, so the lockstep is vacuously satisfied
    // here and is load-bearing only for the Interactive Path-C class that populates it.)
    //
    // What ends each `∞` is the boundary, never this projection:
    // `clear_collapsed_materializations` drops the collapsed axes once the growth is applied, and
    // `turns::drain_pending_phase_transition_progress` clears a `Mana(_)` axis when the step or
    // phase ends (CR 500.5).

    // CR 732.2a: project every unbounded-resource loop into per-(player, axis)
    // `∞` HUD rows, and accumulate the per-(player, family) collapse state the badge
    // actually renders. Runs in every format (placed BEFORE the Commander
    // short-circuit below) and stays empty (both fields omitted) when no loop is
    // active — the dominant case. The engine owns attribution
    // (`attribution_player`) AND the display family (`family_of`).
    let mut families: BTreeMap<(PlayerId, UnboundedFamily), FamilyCollapseState> = BTreeMap::new();
    for (&controller, axes) in &state.unbounded_resources {
        // Which axes THIS controller has an accepted-but-unapplied collapse for, and how certain
        // each one is. Resolved once per controller, on the controller key, BEFORE attribution
        // rewrites `player` — that ordering is the whole point. After `attribution_player` runs, a
        // victim-attributed axis no longer carries the identity of the loop that produced it, so no
        // downstream consumer (engine or frontend) can answer this correctly; two controllers
        // draining one victim would collide. This reads the ENGINE'S DEFERRAL STASH, which no CR
        // licenses (see `FamilyCollapseState`) — it is not a projection of CR 732.2c.
        let scheduled_axes = scheduled_display_axes(state, controller);
        for &axis in axes {
            // CR 732.2a + CR 110.1: an object-growth ∞ whose ENTIRE registered display set
            // has left the battlefield has no live board backing left — drop the row rather
            // than render an ∞ beside an already-empty ∞ pile. `None` (never registered a
            // backing set, e.g. a mana engine) keeps the badge; see `object_growth_backing`
            // for why that asymmetry is typed rather than collapsed into a bool.
            if object_growth_backing(state, controller, axis) == Some(false) {
                continue;
            }
            let player = attribution_player(axis, controller);
            let state_for_axis = match scheduled_axes.get(&axis) {
                Some(&certainty) => FamilyCollapseState::Scheduled(certainty),
                None => FamilyCollapseState::Unscheduled,
            };
            families
                .entry((player, family_of(axis)))
                .and_modify(|acc| *acc = acc.merge(state_for_axis))
                .or_insert(state_for_axis);
            views
                .unbounded_resources
                .push(UnboundedResourceView { player, axis });
        }
    }
    // Emitted HERE, above the Commander short-circuit below, for the same reason the row loop is:
    // that `return` would drop this channel in every non-Commander format.
    views.unbounded_families = families
        .into_iter()
        .map(|((player, family), state)| UnboundedFamilyView {
            player,
            family,
            state,
        })
        .collect();

    // CR 732.2a / CR 110.1: project the accepted object-growth loop's ∞ pile — the
    // winning controller's tapped fodder-class members — dropping any that have since
    // left the battlefield (stale member). Public board state (no viewer filtering);
    // the frontend renders `∞` on any group whose members are all pile members.
    //
    // Unconditional while a collapse is merely scheduled — see the CR 732 timing block above.
    for ids in state.unbounded_loop_pile.values() {
        for id in ids {
            if state.battlefield.contains(id) {
                views.unbounded_pile.push(*id);
            }
        }
    }

    // CR 732.2a / CR 701.34a: project the accepted counter-growth loop's per-object ∞
    // counter targets — the objects whose PRESERVED Generic counters (charge / burden)
    // the certified-unbounded loop pumps each cycle — dropping any that have since left
    // the battlefield (stale member). Display-only per-object channel mirroring
    // `unbounded_pile`; the frontend renders `∞` (not `×N`) on any counter pill whose
    // type is in this set. Runs in every format (BEFORE the Commander short-circuit).
    //
    // Unconditional while a collapse is merely scheduled — see the CR 732 timing block above.
    for targets in state.unbounded_counter_targets.values() {
        for (id, ct) in targets {
            if !state.battlefield.contains(id) {
                continue;
            }
            views
                .unbounded_counters
                .entry(*id)
                .or_default()
                .push(ct.clone());
        }
    }

    if state.format_config.commander_damage_threshold.is_none() {
        return views;
    }
    for &victim in &state.seat_order {
        for (attacker, entries) in super::derived::commander_damage_received(state, victim) {
            views
                .commander_damage_by_attacker
                .entry(attacker)
                .or_default()
                .extend(
                    entries
                        .into_iter()
                        .map(|(commander, damage)| CommanderDamageView {
                            victim,
                            commander,
                            damage,
                        }),
                );
        }
    }
    views
}

/// Derive a viewer-safe presentation from `filtered_state`, retaining only the
/// decision-authority projection from the pre-filter rules state. This keeps
/// rules state pure and makes repeated filtering idempotent.
pub fn derive_filtered_views(
    authoritative_state: &GameState,
    filtered_state: &GameState,
    viewer: Option<PlayerId>,
) -> DerivedViews {
    let mut views = derive_views(filtered_state, viewer);
    views.unique_authorized_submitter = unique_authorized_submitter(authoritative_state);
    // CR 509.1g: blocking relationships are public information. Preserve this
    // display projection even when a viewer-safe state intentionally omits raw
    // combat records unrelated to rendering.
    views.blocker_assignment_pairs = blocker_assignment_pairs(authoritative_state);
    views
}

/// CR 509.1g: flatten each blocking creature's chosen attacking creatures into
/// stable public display pairs. One blocker may legitimately appear more than
/// once when an effect permits it to block multiple attackers.
fn blocker_assignment_pairs(state: &GameState) -> Vec<(ObjectId, ObjectId)> {
    let mut pairs = state
        .combat
        .as_ref()
        .into_iter()
        .flat_map(|combat| {
            combat
                .blocker_to_attacker
                .iter()
                .flat_map(|(&blocker, attackers)| {
                    attackers
                        .iter()
                        .copied()
                        .map(move |attacker| (blocker, attacker))
                })
        })
        .filter(|&(blocker, attacker)| {
            is_live_battlefield_object(state, blocker)
                && is_live_battlefield_object(state, attacker)
        })
        .collect::<Vec<_>>();
    pairs.sort_unstable();
    pairs
}

fn is_live_battlefield_object(state: &GameState, object_id: ObjectId) -> bool {
    state.battlefield.contains(&object_id)
        && state
            .objects
            .get(&object_id)
            .is_some_and(|object| object.zone == Zone::Battlefield)
}

fn unique_authorized_submitter(state: &GameState) -> Option<PlayerId> {
    let mut submitters = crate::game::turn_control::authorized_submitters(state);
    submitters.sort_unstable_by_key(|player| player.0);
    submitters.dedup();
    (submitters.len() == 1).then(|| submitters[0])
}

fn turn_order_views(
    state: &GameState,
    viewer: Option<PlayerId>,
) -> (Vec<TurnOrderSlotView>, Option<u8>) {
    let mut seen = BTreeSet::new();
    let representatives: Vec<PlayerId> = state
        .seat_order
        .iter()
        .copied()
        .filter(|&player| crate::game::players::is_alive(state, player))
        .filter_map(|player| {
            let representative =
                crate::game::topology::normalize_shared_turn_recipient(state, player);
            seen.insert(representative).then_some(representative)
        })
        .collect();

    if representatives.len() <= 2 {
        return (Vec::new(), None);
    }

    let turn_order: Vec<_> = crate::game::turns::projected_turn_order(state, representatives.len())
        .into_iter()
        .enumerate()
        .map(|(index, player)| {
            let slot_index = index as u8;
            TurnOrderSlotView {
                player,
                slot_index,
                turns_from_now: slot_index,
                turn_number: slot_index + 1,
                is_viewer: viewer == Some(player),
                is_starting_player: slot_index == 0 && player == state.current_starting_player,
            }
        })
        .collect();
    let viewer_turn_number = turn_order
        .iter()
        .find(|slot| slot.is_viewer)
        .map(|slot| slot.turn_number);

    (turn_order, viewer_turn_number)
}

/// The axes `controller` has an accepted-but-unapplied collapse for, as the HUD should announce
/// them, each carrying how CERTAIN that collapse is.
///
/// This reads the stash of growth in flight along CR 732.2c's advance to the proposal's ending
/// point — a priority window per CR 732.2a, reached after the CR 500.5 boundary where the growth
/// lands. What it announces is therefore a real accepted result, not a parking spot; the reason it
/// announces CERTAINTY rather than a number is that the boundary re-checks whether the growth is
/// still observed and the controller names the count at the ending point (CR 732.2a). See
/// `FamilyCollapseState`, the `THE WINDOW'S TIMING IS CR 732.2c'S ADVANCE` block above, and
/// `types/game_state.rs`'s `scheduled_collapse_axes` doc for the reading.
///
/// Named rather than inlined into its one caller because the SCOPE LIMIT below is a rule, not a
/// line of the row loop, and it has already proved it drifts when written twice: an earlier cut of
/// this change had a second consumer (a `scheduled_collapse` tag channel, since removed for having
/// no reader) and the guard lived in that consumer alone, so mana rows shipped flagged while the
/// tag omitted them. Any future second consumer calls THIS, and inherits the limit.
///
/// SCOPE LIMIT — `Mana(_)` is excluded. This is about what the badge would TELL the player, not
/// about which code path ends the axis, and it is scoped to THE WINDOW THE BADGE RENDERS IN
/// (accept → CR 500.5 boundary). Inside that window:
/// - The pool is already unbounded and spendable — `mana_payment::refill_infinite_mana` re-tops it
///   off the store after every action — so the chosen `N` does not bound what the player may
///   spend. "A finite amount will be chosen" is false for this axis while it is true for every
///   deferred one, and the badge is only on screen here.
/// - CR 500.5 ends the badge at the step/phase end when the pool empties, on a schedule the
///   accepted count does not move.
///
/// ACROSS the boundary it is not unconditional, and the sentence above is deliberately not written
/// as if it were: a `DriveSequence` collapse replays the captured sequence `N` times after that
/// empty, so the post-collapse pool genuinely is bounded by `N`. The badge is gone by then.
///
/// THE OTHER OVER-PROMISE — NOW TYPED, NOT DISCLOSED. `Counters` and `Life` axes can be scheduled
/// here and then NOT collapse: the boundary re-runs the observed-growth firewall
/// (`engine_resolution_choices`) and DECLINES the batched apply if a counter/life observer (Heliod,
/// Corpsejack) appeared during the accept→boundary window, and a `Tokens` axis can park on a
/// replacement choice instead. Each kind's answer comes from
/// `engine_resolution_choices::materialization_certainty`, which reads that loop's own non-push-exit
/// census rather than a copy of it, and it lands here as [`CollapseCertainty`]: `Conditional` for
/// the three kinds with a hold, `Committed` only for `DriveSequence`, which cannot park. Witnessed
/// by `combo_infinite_pile::real_4p_counter_observer_drift_in_window_declines_batched_counter_but_still_mints_tokens`.
///
/// It is NOT fixable at flag time — the observer can appear after this projection ran, so no value
/// computed here can be right for the whole window — and THAT is precisely why `Conditional` is a
/// variant rather than an apology: the badge stops promising a bound it cannot keep and says
/// "collapse pending; this may stay unbounded" instead. `Mana(_)` is excluded above because its
/// promise is false the moment it is made; this one can only become false later, which is why the
/// two are handled differently.
///
/// Note this exclusion is also NOT "no materialization touches mana": a `DriveSequence` names the
/// loop's whole `proposal.unbounded` set, so `clear_collapsed_materializations` really does drop
/// the `Mana(_)` axis when that collapse applies. (On the production path that drop is usually a
/// no-op, because `turns::drain_pending_phase_transition_progress` has already removed the mana
/// axis at CR 500.5 before the prompt that reaches it; it stays live for `debug_infinite_mana`
/// seats, which that clear excludes.) The badge still must not promise a bound the player's
/// spendable pool never had.
fn scheduled_display_axes(
    state: &GameState,
    controller: PlayerId,
) -> BTreeMap<ResourceAxis, CollapseCertainty> {
    let mut axes: BTreeMap<ResourceAxis, CollapseCertainty> = BTreeMap::new();
    let Some(items) = state.pending_unbounded_materialization.get(&controller) else {
        return axes;
    };
    for item in items {
        // Per ITEM, so each axis inherits the certainty of the kind that actually scheduled it;
        // two items naming the same axis merge to the weaker answer.
        let certainty = crate::game::engine_resolution_choices::materialization_certainty(item);
        for axis in state.scheduled_collapse_axes(std::slice::from_ref(item)) {
            if matches!(axis, ResourceAxis::Mana(_)) {
                continue;
            }
            axes.entry(axis)
                .and_modify(|acc| *acc = acc.weaker(certainty))
                .or_insert(certainty);
        }
    }
    axes
}

/// CR 732.2a: which player's HUD a pumped `axis` belongs to, given the loop's
/// `controller`. Exhaustive by design (no wildcard) — a new `ResourceAxis`
/// variant must make a deliberate attribution choice here, never silently inherit
/// a default.
///
/// A payload-keyed axis names the player it acts on, so the badge follows the
/// payload, NOT permanent control:
/// - CR 119.3 + CR 704.5a: `Life(p)` — CR 119.3 makes `p` the player whose life total the
///   effect adjusts, and CR 704.5a is why that matters (the afflicted player reaching 0 life
///   loses). A drain drives an opponent's total down and lifegain raises the controller's own;
///   either way the badge belongs on `p`'s HUD.
/// - CR 120: `DamageDealt(p)` — damage accrues to the player it is dealt to, so an
///   opponent-burn loop shows `∞` on the victim's HUD.
/// - CR 704.5b: `LibraryDelta(p)` — a mill drives an opponent's library toward the
///   empty-draw loss and a self-mill the controller's own; the badge follows `p`.
///
/// - CR 704.5c: `Poison(p)` — a poison ∞ drives the afflicted player toward the
///   10-poison loss, so the badge belongs on the VICTIM's HUD.
///
/// Every aggregate axis carries no victim PlayerId and is attributed to the loop's
/// `controller` (the player generating the unbounded resource).
fn attribution_player(axis: ResourceAxis, controller: PlayerId) -> PlayerId {
    match axis {
        ResourceAxis::Life(p)
        | ResourceAxis::DamageDealt(p)
        | ResourceAxis::LibraryDelta(p)
        | ResourceAxis::Poison(p) => p,
        ResourceAxis::Mana(_)
        | ResourceAxis::Counter(_, _)
        | ResourceAxis::Trigger(_)
        | ResourceAxis::TokensCreated
        | ResourceAxis::CardsDrawn
        | ResourceAxis::Casts
        | ResourceAxis::LandfallTriggers
        | ResourceAxis::CombatPhases
        | ResourceAxis::ExtraTurns
        | ResourceAxis::DeathTriggers
        | ResourceAxis::EtbTriggers
        | ResourceAxis::LtbTriggers
        | ResourceAxis::SacTriggers => controller,
    }
}

/// CR 732.2a: whether the object-growth `∞` display set the accept registered for `axis`
/// still has LIVE authority — i.e. at least one registered member is still on the
/// battlefield (CR 110.1: a permanent stops being one as it moves to another zone).
///
/// The `Option` is the whole point, and the two negative answers are NOT the same thing:
///
/// - `Some(false)` = the axis HAS a registered board backing and every member of it has
///   left the battlefield. The `∞` has no live authority behind it ⇒ **drop the row**,
///   rather than render an `∞` badge beside an already-empty `∞` pile.
/// - `None` = the axis NEVER registered a backing set. A mana engine registers no pile at
///   all, and an untapped-growth loop's pile seed is a no-op on an empty set
///   (`register_unbounded_loop_pile` early-returns). There is no live authority to consult
///   ⇒ **badge unchanged**. Collapsing this into a `bool` would silently hide every
///   unbacked `∞`, which is the opposite of the intended fix.
///
/// This is the SINGLE authority for "is this object-growth display set still on the board".
/// The pile and counter-target loops in `derive_views` apply the same
/// `state.battlefield.contains` test at MEMBER level; this is its SET-level closure, so all
/// three read the same board in the same frame and none can be staler than another.
///
/// GRANULARITY — the rule that decides which axes may consult a backing store at all:
///
/// > A CONTROLLER-keyed backing store can answer an AXIS-scoped question if and only if the
/// > axis is a UNIT variant.
///
/// `TokensCreated` is a unit variant, so a controller can hold at most one of it and
/// `unbounded_loop_pile[controller]` IS that axis' backing — a bijection, no granularity is
/// assumed that the store does not have. `Counter(CounterClass, ObjectClass)` is a DATA
/// variant: `mark_unbounded_loop` unions arbitrarily many per controller (`entry.extend`), so a
/// controller-keyed store is strictly coarser than the axis, and it returns `None` here.
///
/// An earlier revision of this function did read `unbounded_counter_targets` for `Counter(..)`,
/// and its doc claimed the error direction was safe — "over-KEEPS a badge, never over-drops
/// one". That was FALSE, and measured so: one accepted proposal can carry both
/// `Counter(Plus1Plus1, Creature)` (`analysis::corpus`'s `ResourceFamily::Counters`) and the
/// display channel's object-agnostic `Counter(Other, Other)`, while only the latter's targets
/// are ever registered — so when those targets left the battlefield the guard dropped EVERY
/// counter row, including the one whose backing it had never consulted.
///
/// Re-keying the store by `(controller, ResourceAxis)` would not fix it. The targets are
/// axis-blind at the DERIVATION, not just at the key: `register_unbounded_counter_targets` is
/// fed by `game::engine::current_period_counter_targets` →
/// `analysis::resource::grown_generic_counter_targets`, which takes no axis argument and
/// returns one undifferentiated `Generic`-only set for the whole proposal. A per-axis key would
/// assert a scope nothing derives. Revoking a counter row needs an axis-scoped authority to
/// exist first; until one does, this refuses rather than guesses.
///
/// Read-only: recomputed from live state on every `derive_views` call, nothing is stored,
/// so nothing can go stale. Deliberately not a `clear_unbounded_loop` from the zone-exit
/// defuse — that call drops six maps including `pending_unbounded_materialization` and its
/// CR 732.2c bound, i.e. it would cancel growth the table has already unanimously accepted
/// ("the shortcut is taken" the moment the last player accepts). Revoking the BADGE is a
/// display decision; revoking the agreed GROWTH is not ours to make here.
fn object_growth_backing(
    state: &GameState,
    controller: PlayerId,
    axis: ResourceAxis,
) -> Option<bool> {
    match axis {
        // The ∞ pile IS the registered backing set for the token axis
        // (`register_unbounded_loop_pile`, written at accept by
        // `materialize_object_growth_shortcut`).
        ResourceAxis::TokensCreated => state
            .unbounded_loop_pile
            .get(&controller)
            .map(|pile| pile.iter().any(|id| state.battlefield.contains(id))),
        // No registered board backing exists for these axes — no live authority to consult,
        // badge unchanged. Exhaustive on purpose: a future ResourceAxis variant must decide
        // which side it lands on rather than silently defaulting to "unbacked"; the
        // unit-variant rule in this function's doc is the criterion for choosing.
        //
        // `Counter(..)` is here rather than reading `unbounded_counter_targets` because that
        // store cannot answer a per-axis question — see the GRANULARITY note above. Witnessed
        // by `counter_rows_are_not_revoked_by_a_controller_keyed_backing_set`.
        ResourceAxis::Counter(..)
        | ResourceAxis::Mana(_)
        | ResourceAxis::Life(_)
        | ResourceAxis::DamageDealt(_)
        | ResourceAxis::LibraryDelta(_)
        | ResourceAxis::Poison(_)
        | ResourceAxis::Trigger(_)
        | ResourceAxis::CardsDrawn
        | ResourceAxis::Casts
        | ResourceAxis::LandfallTriggers
        | ResourceAxis::CombatPhases
        | ResourceAxis::ExtraTurns
        | ResourceAxis::DeathTriggers
        | ResourceAxis::EtbTriggers
        | ResourceAxis::LtbTriggers
        | ResourceAxis::SacTriggers => None,
    }
}

/// Aggregate player-affecting conditions into render-ready rows.
///
/// Two sources, neither of which introduces new game logic:
/// 1. **Statics-scanned** life/cost conditions — delegate verbatim to the
///    single-authority `player_has_*` predicates in `static_abilities`
///    (CR 104.2b / 119.7 / 119.8 / 118.3). `source` is `None` because those
///    predicates return a bare `bool`.
/// 2. **Stored state** — read `restrictions` and `epic_effects` as-is
///    (CR 101.2 / 602.5 / 601.2a / 702.50b); `source` is the imposing card.
///
/// Deliberately excluded: `GameRestriction::DamagePreventionDisabled` has no
/// per-player axis (it scopes by source/target, CR 614.16) so it is not a
/// player condition; `player_ignores_hexproof` / `player_has_protection_from_everything`
/// are beneficial capabilities, not afflictions; `player_cant_sacrifice_as_cost`
/// is an object-parameterized per-payment query, not a player-level status.
fn player_status_views(state: &GameState) -> Vec<PlayerStatusView> {
    use crate::game::static_abilities::{
        player_cant_pay_life_as_cost, player_has_cant_gain_life, player_has_cant_lose_life,
        player_has_cant_win,
    };

    let mut views = Vec::new();

    // Source 1: statics-scanned, player-scoped life/cost conditions. Each
    // predicate is the sole authority for its CR rule; calling them keeps the
    // logic single-sourced. Cost is O(players × active statics) — bounded by
    // the (typically tiny) set of permanents with static abilities.
    for player in &state.players {
        let pid = player.id;
        let conditions = [
            (
                player_has_cant_win(state, pid),
                PlayerConditionKind::CantWin,
            ),
            (
                player_has_cant_gain_life(state, pid),
                PlayerConditionKind::CantGainLife,
            ),
            (
                player_has_cant_lose_life(state, pid),
                PlayerConditionKind::CantLoseLife,
            ),
            (
                player_cant_pay_life_as_cost(state, pid),
                PlayerConditionKind::CantPayLifeAsCost,
            ),
        ];
        for (active, kind) in conditions {
            if active {
                views.push(PlayerStatusView {
                    player: pid,
                    kind,
                    source: None,
                });
            }
        }
    }

    // Source 2a: stored activity prohibitions, read as-is from GameState.
    for restriction in &state.restrictions {
        let GameRestriction::ProhibitActivity {
            source,
            affected_players,
            activity,
            expiry,
        } = restriction
        else {
            // DamagePreventionDisabled has no per-player axis — see fn docs.
            continue;
        };
        // CR 514.2 + CR 500.7: a `UntilEndOfNextTurnOf` prohibition (Kang's "during
        // that [extra] turn, power-up abilities can't be activated") is created
        // pre-armed and only takes force during the granted turn, after the untap
        // step converts it to `EndOfTurn` (turns.rs). Suppress the HUD status badge
        // while it is still dormant so this display seam agrees with the activation
        // gate (`is_blocked_by_cant_activate_abilities`) — they share the expiry
        // variant as the single source of truth.
        if matches!(expiry, RestrictionExpiry::UntilEndOfNextTurnOf { .. }) {
            continue;
        }
        let kind = match activity {
            ProhibitedActivity::CastSpells { .. } => PlayerConditionKind::CantCastSpells,
            ProhibitedActivity::ActivateAbilities { .. } => {
                PlayerConditionKind::CantActivateAbilities
            }
            ProhibitedActivity::CastOnlyFromZones { allowed_zones } => {
                PlayerConditionKind::CastOnlyFromZones {
                    allowed_zones: allowed_zones.clone(),
                }
            }
            // CR 508.1c: a "can't attack" prohibition is enforced only at the
            // declare-attackers gate; it has no cast/activate HUD badge, so no
            // player-status row is produced for it.
            ProhibitedActivity::Attack { .. } => continue,
            // CR 116.2a: "can't play cards from <zone>" is enforced at the cast
            // and play-land gates; no dedicated HUD badge yet, so no status row.
            ProhibitedActivity::ProhibitPlayFromZone { .. } => continue,
            // CR 305.1: "can't play [matching] lands" is enforced at the
            // play-land gate; no dedicated HUD badge yet, so no status row —
            // mirrors `ProhibitPlayFromZone` above.
            ProhibitedActivity::PlayLands { .. } => continue,
        };
        for pid in restriction_affected_players(state, affected_players, *source) {
            views.push(PlayerStatusView {
                player: pid,
                kind: kind.clone(),
                source: Some(*source),
            });
        }
    }

    // Source 2b: CR 702.50b — a resolved Epic locks its controller out of casting.
    for epic in &state.epic_effects {
        views.push(PlayerStatusView {
            player: epic.controller,
            kind: PlayerConditionKind::CantCastSpells,
            source: Some(epic.prototype_id),
        });
    }

    views
}

/// Resolve a restriction's `RestrictionPlayerScope` to the concrete players it
/// afflicts at display time. The `TargetedPlayer` / `ParentTargetedPlayer`
/// placeholders are resolved to `SpecificPlayer` at resolution time
/// (CR 608.2c); if one survives to the display layer it can't be attributed,
/// so it contributes no rows.
fn restriction_affected_players(
    state: &GameState,
    scope: &RestrictionPlayerScope,
    source: ObjectId,
) -> Vec<PlayerId> {
    match scope {
        RestrictionPlayerScope::AllPlayers => state.players.iter().map(|p| p.id).collect(),
        RestrictionPlayerScope::SpecificPlayer(pid) => vec![*pid],
        RestrictionPlayerScope::OpponentsOfSourceController => {
            match state.objects.get(&source).map(|obj| obj.controller) {
                Some(controller) => state
                    .players
                    .iter()
                    .map(|p| p.id)
                    .filter(|&pid| pid != controller)
                    .collect(),
                None => Vec::new(),
            }
        }
        // CR 109.5: `add_restriction` resolves the scoped player to
        // `SpecificPlayer` when the restriction is created, so a stored
        // restriction never carries an unresolved placeholder scope here.
        // CR 109.4: `ParentObjectTargetController` is likewise resolved to
        // `SpecificPlayer` by `add_restriction` at creation time.
        RestrictionPlayerScope::TargetedPlayer
        | RestrictionPlayerScope::ParentTargetedPlayer
        | RestrictionPlayerScope::ParentObjectTargetController
        | RestrictionPlayerScope::ScopedPlayer => Vec::new(),
        // CR 508.5a: `add_restriction` resolves the defending player to
        // `SpecificPlayer` when the restriction is created, so a stored
        // restriction never carries an unresolved `DefendingPlayer` scope here.
        RestrictionPlayerScope::DefendingPlayer => Vec::new(),
    }
}

fn stack_entry_details(state: &GameState) -> HashMap<ObjectId, StackEntryDisplay> {
    state
        .stack
        .iter()
        .map(|entry| (entry.id, stack_entry_detail(state, entry)))
        .collect()
}

fn stack_entry_detail(state: &GameState, entry: &StackEntry) -> StackEntryDisplay {
    let source_name = stack_source_name(state, entry);
    let effective_ability = effective_stack_ability(state, entry);
    let (kind_label, ability_description) = match &entry.kind {
        StackEntryKind::Spell { ability, .. } => (
            "Spell".to_string(),
            ability
                .as_ref()
                .and_then(|ability| ability.description.clone()),
        ),
        StackEntryKind::ActivatedAbility { ability, .. } => (
            ability
                .ability_index
                .map(|idx| format!("Activated ability {}", idx + 1))
                .unwrap_or_else(|| "Activated ability".to_string()),
            ability.description.clone(),
        ),
        StackEntryKind::TriggeredAbility {
            ability,
            description,
            ..
        } => (
            "Triggered ability".to_string(),
            description.clone().or_else(|| ability.description.clone()),
        ),
        StackEntryKind::KeywordAction { action } => (keyword_action_label(action), None),
    };

    StackEntryDisplay {
        source_name,
        token_image_ref: stack_source_token_image_ref(state, entry),
        kind_label,
        ability_description,
        selected_mode_labels: effective_ability
            .ability
            .map(|ability| ability.selected_mode_labels.clone())
            .unwrap_or_default(),
        is_pending: effective_ability.is_pending,
        targets: stack_entry_targets(state, entry),
        paid: stack_paid_facts(state.stack_paid_facts.get(&entry.id)),
        trigger_context: stack_trigger_context(state, entry),
        provenance: match &entry.kind {
            StackEntryKind::TriggeredAbility { provenance, .. } => provenance.clone(),
            StackEntryKind::Spell { .. }
            | StackEntryKind::ActivatedAbility { .. }
            | StackEntryKind::KeywordAction { .. } => None,
        },
    }
}

fn stack_source_token_image_ref(state: &GameState, entry: &StackEntry) -> Option<TokenImageRef> {
    state
        .objects
        .get(&entry.source_id)
        .and_then(|obj| obj.token_image_ref.clone())
        .or_else(|| {
            state
                .lki_cache
                .get(&entry.source_id)
                .and_then(|lki| lki.token_image_ref.clone())
        })
}

fn stack_source_name(state: &GameState, entry: &StackEntry) -> String {
    match &entry.kind {
        StackEntryKind::TriggeredAbility { source_name, .. } if !source_name.is_empty() => {
            source_name.clone()
        }
        _ => state
            .objects
            .get(&entry.source_id)
            .map(|obj| obj.name.clone())
            .unwrap_or_else(|| "Unknown".to_string()),
    }
}

fn keyword_action_label(action: &KeywordAction) -> String {
    match action {
        KeywordAction::Equip { .. } => "Equip".to_string(),
        KeywordAction::Crew { .. } => "Crew".to_string(),
        KeywordAction::Saddle { .. } => "Saddle".to_string(),
        KeywordAction::Station { .. } => "Station".to_string(),
    }
}

fn stack_entry_targets(state: &GameState, entry: &StackEntry) -> Vec<StackTargetDisplay> {
    let targets = match &entry.kind {
        StackEntryKind::KeywordAction { action } => keyword_action_targets(action),
        _ => effective_stack_ability(state, entry)
            .ability
            .map(flatten_targets_in_chain)
            .unwrap_or_default(),
    };
    targets
        .into_iter()
        .map(|target| StackTargetDisplay {
            label: target_label(state, &target),
            target,
        })
        .collect()
}

fn keyword_action_targets(action: &KeywordAction) -> Vec<TargetRef> {
    match action {
        KeywordAction::Equip {
            target_creature_id, ..
        } => vec![TargetRef::Object(*target_creature_id)],
        KeywordAction::Crew { .. }
        | KeywordAction::Saddle { .. }
        | KeywordAction::Station { .. } => Vec::new(),
    }
}

fn target_label(state: &GameState, target: &TargetRef) -> String {
    match target {
        TargetRef::Object(object_id) => state
            .objects
            .get(object_id)
            .map(|obj| obj.name.clone())
            .unwrap_or_else(|| format!("Object {}", object_id.0)),
        TargetRef::Player(player_id) => player_label(state, *player_id),
    }
}

fn player_label(state: &GameState, player: PlayerId) -> String {
    state
        .log_player_names
        .get(player.0 as usize)
        .filter(|name| !name.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("Player {}", player.0))
}

fn stack_paid_facts(snapshot: Option<&StackPaidSnapshot>) -> Vec<StackPaidFactView> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let mut facts = Vec::new();
    if let Some(value) = snapshot.x_value {
        facts.push(StackPaidFactView::XValue { value });
    }
    if snapshot.actual_mana_spent > 0 {
        facts.push(StackPaidFactView::ManaSpent {
            amount: snapshot.actual_mana_spent,
        });
    }
    if snapshot.distinct_colors_spent > 0 {
        facts.push(StackPaidFactView::ColorsSpent {
            distinct: snapshot.distinct_colors_spent,
        });
    }
    if snapshot.kickers_paid > 0 {
        facts.push(StackPaidFactView::Kicked {
            count: snapshot.kickers_paid,
        });
    }
    if snapshot.additional_cost_paid {
        facts.push(StackPaidFactView::AdditionalCostPaid);
    }
    if snapshot.casting_variant != CastingVariant::Normal {
        facts.push(StackPaidFactView::CastVariant {
            variant: format!("{:?}", snapshot.casting_variant),
        });
    }
    if snapshot.convoked_creatures > 0 {
        facts.push(StackPaidFactView::Convoked {
            count: snapshot.convoked_creatures,
        });
    }
    facts
}

fn stack_trigger_context(state: &GameState, entry: &StackEntry) -> Vec<TriggerContextDisplay> {
    let mut events: Vec<&GameEvent> = state
        .stack_trigger_event_batches
        .get(&entry.id)
        .map(|batch| batch.iter().collect())
        .unwrap_or_default();
    if events.is_empty() {
        if let StackEntryKind::TriggeredAbility {
            trigger_event: Some(event),
            ..
        } = &entry.kind
        {
            events.push(event);
        }
    }
    events
        .into_iter()
        .filter_map(|event| trigger_event_display(state, event))
        .collect()
}

fn trigger_event_display(state: &GameState, event: &GameEvent) -> Option<TriggerContextDisplay> {
    match event {
        GameEvent::HiddenSearchViewed { .. } => None,
        GameEvent::ZoneChanged {
            object_id,
            record,
            from,
            to,
        } => Some(TriggerContextDisplay {
            label: format!(
                "{} moved {} -> {}",
                visible_zone_change_object_name(state, *object_id, &record.name, *from, *to),
                zone_label(*from),
                zone_label(Some(*to))
            ),
            object_id: Some(*object_id),
            player: Some(record.controller),
        }),
        GameEvent::CardsRevealed {
            player, card_ids, ..
        } => Some(TriggerContextDisplay {
            label: if card_ids.len() == 1 {
                format!(
                    "{} revealed {}",
                    player_label(state, *player),
                    target_label(state, &TargetRef::Object(card_ids[0]))
                )
            } else {
                format!(
                    "{} revealed {} cards",
                    player_label(state, *player),
                    card_ids.len()
                )
            },
            object_id: card_ids.first().copied(),
            player: Some(*player),
        }),
        GameEvent::SpellCast {
            object_id,
            controller,
            ..
        } => Some(TriggerContextDisplay {
            label: format!(
                "{} cast {}",
                player_label(state, *controller),
                target_label(state, &TargetRef::Object(*object_id))
            ),
            object_id: Some(*object_id),
            player: Some(*controller),
        }),
        GameEvent::AbilityActivated {
            player_id,
            source_id,
            ..
        } => Some(TriggerContextDisplay {
            label: format!(
                "{} ability activated",
                target_label(state, &TargetRef::Object(*source_id))
            ),
            object_id: Some(*source_id),
            player: Some(*player_id),
        }),
        GameEvent::VehicleCrewed {
            vehicle_id,
            creatures,
        } => Some(TriggerContextDisplay {
            label: format!(
                "{} crewed by {} creature{}",
                target_label(state, &TargetRef::Object(*vehicle_id)),
                creatures.len(),
                if creatures.len() == 1 { "" } else { "s" }
            ),
            object_id: Some(*vehicle_id),
            player: state.objects.get(vehicle_id).map(|obj| obj.controller),
        }),
        GameEvent::Saddled {
            mount_id,
            creatures,
        } => Some(TriggerContextDisplay {
            label: format!(
                "{} saddled by {} creature{}",
                target_label(state, &TargetRef::Object(*mount_id)),
                creatures.len(),
                if creatures.len() == 1 { "" } else { "s" }
            ),
            object_id: Some(*mount_id),
            player: state.objects.get(mount_id).map(|obj| obj.controller),
        }),
        _ => None,
    }
}

fn visible_zone_change_object_name(
    state: &GameState,
    object_id: ObjectId,
    fallback: &str,
    from: Option<Zone>,
    to: Zone,
) -> String {
    if let Some(obj) = state.objects.get(&object_id) {
        return obj.name.clone();
    }
    if matches!(from, Some(Zone::Hand | Zone::Library)) || matches!(to, Zone::Hand | Zone::Library)
    {
        return "Hidden Card".to_string();
    }
    fallback.to_string()
}

fn zone_label(zone: Option<Zone>) -> &'static str {
    match zone {
        Some(Zone::Battlefield) => "battlefield",
        Some(Zone::Hand) => "hand",
        Some(Zone::Library) => "library",
        Some(Zone::Graveyard) => "graveyard",
        Some(Zone::Exile) => "exile",
        Some(Zone::Stack) => "stack",
        Some(Zone::Command) => "command",
        None => "nowhere",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::CombatState;
    use crate::game::game_object::DisplaySource;
    use crate::game::triggers::{PendingTrigger, PendingTriggerContext};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        DelayedTriggerCondition, Duration, Effect, ModalChoice, ResolvedAbility, RestrictionExpiry,
        StaticCondition, TargetFilter, TargetRef,
    };
    use crate::types::card_type::CoreType;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{
        CommanderDamageEntry, DelayedTrigger, PendingCast, StackEntry, StackEntryKind,
        StackPaidSnapshot, TriggerOrderGroup, WaitingFor, ZoneChangeRecord,
    };
    use crate::types::identifiers::{
        CardId, DelayedTriggerInstanceId, DelayedTriggerOrigin, DelayedTriggerToken, TriggerFiring,
    };
    use crate::types::mana::ManaCost;
    use crate::types::phase::Phase;
    use crate::types::statics::ActivationExemption;
    use crate::types::zones::Zone;
    use std::collections::HashMap;

    fn setup_commander_game(num_players: u8) -> GameState {
        let mut state = GameState::new(FormatConfig::commander(), num_players, 42);
        for player_idx in 0..num_players {
            for i in 0..5 {
                create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }
        state
    }

    /// JIT short-circuit: non-Commander formats must return an empty view
    /// without walking `state.commander_damage`. Verifies the map is empty
    /// even when the flat list has entries (defensive; this shouldn't
    /// happen in practice, but the early-return must not depend on the
    /// data being empty).
    #[test]
    fn derive_views_empty_for_non_commander_format() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        // Push a phantom entry to prove the short-circuit doesn't inspect it.
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(0),
            commander: ObjectId(1),
            damage: 21,
        });

        let views = derive_views(&state, None);
        assert!(
            views.commander_damage_by_attacker.is_empty(),
            "non-Commander format must short-circuit regardless of stored damage entries"
        );
    }

    /// A controller-keyed backing store can answer an axis-scoped question only when the axis is
    /// a UNIT variant. `TokensCreated` is one — at most one per controller, so
    /// `unbounded_loop_pile[controller]` IS that axis' backing. `Counter(CounterClass,
    /// ObjectClass)` is not: `mark_unbounded_loop` unions arbitrary axes for one controller, and
    /// the backing derivation (`current_period_counter_targets` → `grown_generic_counter_targets`)
    /// accepts NO axis — it diffs every shared object's growable `Generic` counters and returns
    /// ONE undifferentiated set for the whole proposal.
    ///
    /// So the controller-keyed `Some(false)` this PR first shipped revoked EVERY counter row at
    /// once, including axes whose backing was never in that set: a certified proposal can carry
    /// both `Counter(Plus1Plus1, Creature)` (`analysis::corpus`'s `ResourceFamily::Counters`) and
    /// the display channel's object-agnostic `Counter(Other, Other)`, while only the latter's
    /// Generic targets are ever registered. That is an over-DROP — the opposite of the
    /// "conservative, over-keeps only" claim the first revision shipped with.
    ///
    /// Two-sided on ONE assertion (are both rows on the wire?): restoring the controller-keyed
    /// `Some(false)` arm reds the SUBJECT — both rows vanish, including the axis whose backing was
    /// never consulted. The CONTROL runs FIRST as the non-vacuity anchor: it proves this wire can
    /// carry two counter rows at all, which a "rows survived" assertion alone cannot establish.
    #[test]
    fn counter_rows_are_not_revoked_by_a_controller_keyed_backing_set() {
        use crate::analysis::resource::{CounterClass, ObjectClass, ResourceAxis};
        use crate::game::zones::move_to_zone;
        use crate::types::counter::CounterType;
        use crate::types::events::GameEvent;

        // The two axes one accepted counter-growth proposal can carry at once. Only the
        // object-agnostic one is the display channel's, and only ITS targets get registered.
        let plus1_axis = ResourceAxis::Counter(CounterClass::Plus1Plus1, ObjectClass::Creature);
        let generic_axis = ResourceAxis::Counter(CounterClass::Other, ObjectClass::Other);

        let build = || {
            let mut state = GameState::new_two_player(42);
            let target = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Pentad Prism".to_string(),
                Zone::Battlefield,
            );
            state.mark_unbounded_loop(PlayerId(0), &[plus1_axis, generic_axis]);
            // CR 701.34a: the registered backing is the GENERIC channel's, derived axis-blind.
            // Nothing here backs `plus1_axis` — that is the whole point.
            state.register_unbounded_counter_targets(
                PlayerId(0),
                vec![(target, CounterType::Generic("charge".to_string()))],
            );
            (state, target)
        };

        let rows = |state: &GameState| -> Vec<ResourceAxis> {
            derive_views(state, Some(PlayerId(0)))
                .unbounded_resources
                .iter()
                .map(|r| r.axis)
                .collect()
        };

        // CONTROL first — the reach anchor: both marked axes reach the wire.
        let (control, _kept) = build();
        let control_rows = rows(&control);
        assert!(
            control_rows.contains(&plus1_axis) && control_rows.contains(&generic_axis),
            "THE assertion (control): both marked counter axes reach the wire, got {control_rows:?}"
        );

        // SUBJECT: the only registered (Generic) target leaves the battlefield.
        let (mut subject, target) = build();
        let mut events: Vec<GameEvent> = Vec::new();
        move_to_zone(&mut subject, target, Zone::Graveyard, &mut events);
        assert!(
            !subject.battlefield.contains(&target),
            "precondition: the registered target really left the battlefield"
        );
        let subject_rows = rows(&subject);
        assert!(
            subject_rows.contains(&plus1_axis) && subject_rows.contains(&generic_axis),
            "THE assertion (subject): a controller-keyed backing set must not revoke ANY counter \
             row — least of all `plus1_axis`, whose backing was never registered, got {subject_rows:?}"
        );
    }

    #[test]
    fn blocker_assignment_pairs_are_sorted_and_exclude_stale_objects() {
        let mut state = GameState::new_two_player(42);
        let blocker = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Blocker".to_string(),
            Zone::Battlefield,
        );
        let first_attacker = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "First Attacker".to_string(),
            Zone::Battlefield,
        );
        let second_attacker = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Second Attacker".to_string(),
            Zone::Battlefield,
        );
        let stale_blocker = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Stale Blocker".to_string(),
            Zone::Graveyard,
        );
        let stale_attacker = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Stale Attacker".to_string(),
            Zone::Graveyard,
        );
        let absorbed_component = create_object(
            &mut state,
            CardId(6),
            PlayerId(1),
            "Absorbed Component".to_string(),
            Zone::Battlefield,
        );
        state.battlefield.retain(|&id| id != absorbed_component);
        state.combat = Some(CombatState {
            blocker_to_attacker: HashMap::from([
                (
                    blocker,
                    vec![second_attacker, stale_attacker, first_attacker],
                ),
                (stale_blocker, vec![first_attacker]),
                (absorbed_component, vec![second_attacker]),
            ]),
            ..CombatState::default()
        });
        let expected = vec![(blocker, first_attacker), (blocker, second_attacker)];

        let views = derive_views(&state, Some(PlayerId(0)));
        assert_eq!(views.blocker_assignment_pairs, expected);
    }

    #[test]
    fn blocker_assignment_pairs_survive_filtered_client_wire_serialization() {
        let mut state = GameState::new_two_player(42);
        let blocker = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Blocker".to_string(),
            Zone::Battlefield,
        );
        let attacker = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        state.combat = Some(CombatState {
            blocker_to_attacker: HashMap::from([(blocker, vec![attacker])]),
            ..CombatState::default()
        });

        let filtered = crate::game::visibility::filter_state_for_viewer(&state, PlayerId(0));
        let json = serde_json::to_string(&ClientGameStateRef::wrap_filtered(
            &state,
            &filtered,
            Some(PlayerId(0)),
        ))
        .expect("serialize filtered client state");
        let client: ClientGameState =
            serde_json::from_str(&json).expect("deserialize filtered client state");
        assert_eq!(
            client.derived.blocker_assignment_pairs,
            vec![(blocker, attacker)],
            "the authoritative public blocking pair survives the filtered viewer wire path"
        );
    }

    #[test]
    fn empty_blocker_assignment_pairs_omit_the_wire_key() {
        let empty_json =
            serde_json::to_string(&DerivedViews::default()).expect("empty derived views serialize");
        assert!(
            !empty_json.contains("blocker_assignment_pairs"),
            "empty blocker pairs omit their wire key"
        );
        let empty_round_trip: DerivedViews =
            serde_json::from_str(&empty_json).expect("empty derived views deserialize");
        assert!(empty_round_trip.blocker_assignment_pairs.is_empty());
    }

    #[test]
    fn derive_views_flags_a_permanent_under_a_live_copy_effect() {
        // CR 613.2a + CR 707.2: a copy of a permanent is expressed as a Layer 1a
        // `CopyValues` continuous effect, not a flag on the object — so the
        // projection has to read the effect list. Issue #5932: a Phantasmal
        // Image copying a Reveillark renders identically to the real one, and
        // nothing already serialized told the client which was which.
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let original = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Reveillark".into(),
            Zone::Battlefield,
        );
        let clone = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Phantasmal Image".into(),
            Zone::Battlefield,
        );

        let values = crate::game::printed_cards::intrinsic_copiable_values(
            state.objects.get(&original).unwrap(),
        );
        state.add_transient_continuous_effect(
            clone,
            PlayerId(0),
            Duration::Permanent,
            TargetFilter::SpecificObject { id: clone },
            vec![ContinuousModification::CopyValues {
                values: Box::new(values),
                display_source: DisplaySource::Card,
                printed_ref: None,
                token_image_ref: None,
            }],
            None,
        );

        let views = derive_views(&state, None);

        assert_eq!(
            views.copied_permanents,
            vec![clone],
            "only the permanent the copy effect applies to is a copy; the \
             original it copied is not"
        );
    }

    #[test]
    fn derive_views_drops_the_copy_flag_once_a_temporary_copy_effect_lapses() {
        // CR 611.2b: a `ForAsLongAs` copy ends the moment its condition goes
        // false, but the effect stays STORED until it is swept. Zygon
        // Infiltrator copies "for as long as that creature remains tapped", so
        // untapping the target ends the copy while the TCE is still in the list.
        // Reading membership alone would keep badging a permanent that is no
        // longer a copy, so the projection asks the layer engine's own
        // liveness predicate instead.
        use crate::types::ability::ObjectScope;

        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let target = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tapped Bear".into(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&target).unwrap().tapped = true;
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Zygon".into(),
            Zone::Battlefield,
        );

        let values = crate::game::printed_cards::intrinsic_copiable_values(
            state.objects.get(&target).unwrap(),
        );
        let tce_id = state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::ForAsLongAs {
                condition: StaticCondition::IsTapped {
                    scope: ObjectScope::Target,
                },
            },
            TargetFilter::SpecificObject { id: source },
            vec![ContinuousModification::CopyValues {
                values: Box::new(values),
                display_source: DisplaySource::Card,
                printed_ref: None,
                token_image_ref: None,
            }],
            None,
        );
        // CR 611.2b: the duration tracks the copy TARGET's tap state, not the
        // source's — the same binding the layer engine uses.
        state.set_transient_duration_subject(tce_id, target);

        assert_eq!(
            derive_views(&state, None).copied_permanents,
            vec![source],
            "while the target is tapped the copy applies, so the badge shows"
        );

        // Untap the target: the duration ends and the copy lapses, but the
        // effect is still stored.
        state.objects.get_mut(&target).unwrap().tapped = false;
        assert!(
            state
                .transient_continuous_effects
                .iter()
                .any(|t| t.id == tce_id),
            "precondition: the lapsed effect is still stored, so this test is \
             exercising liveness rather than removal"
        );
        assert!(
            derive_views(&state, None).copied_permanents.is_empty(),
            "a lapsed copy must not keep the badge (CR 611.2b)"
        );
    }

    #[test]
    fn derive_views_does_not_flag_a_merged_permanent_as_a_copy() {
        // CR 730.2a: merge represents the top component with a private
        // `CopyValues` Layer-1 effect, but the resulting permanent is merged,
        // not a copy. The projection must exclude that exact effect while
        // continuing to admit independent copy effects on the same object.
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let host = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Host".into(),
            Zone::Battlefield,
        );
        let rider = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Rider".into(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        crate::game::merge::merge_object_onto(
            &mut state,
            rider,
            host,
            crate::game::merge::MergeSide::Top,
            &mut events,
        );

        let merge_effect_id = state
            .objects
            .get(&host)
            .and_then(|object| object.merge_layer_effect_id);
        assert!(
            merge_effect_id.is_some(),
            "precondition: merge is represented by a CopyValues layer effect"
        );
        assert!(
            derive_views(&state, None).copied_permanents.is_empty(),
            "a merge representation must not produce a Copy badge"
        );

        // The exclusion is effect-scoped, not object-scoped: a merged
        // permanent can still acquire an independent copy effect.
        let values = crate::game::printed_cards::intrinsic_copiable_values(
            state.objects.get(&host).unwrap(),
        );
        let independent_copy_effect_id = state.add_transient_continuous_effect(
            host,
            PlayerId(0),
            Duration::Permanent,
            TargetFilter::SpecificObject { id: host },
            vec![ContinuousModification::CopyValues {
                values: Box::new(values),
                display_source: DisplaySource::Card,
                printed_ref: None,
                token_image_ref: None,
            }],
            None,
        );
        assert_ne!(
            Some(independent_copy_effect_id),
            merge_effect_id,
            "precondition: the independent copy effect is distinct from merge's representation"
        );
        assert_eq!(
            derive_views(&state, None).copied_permanents,
            vec![host],
            "a later independent copy effect on a merged permanent must still produce a badge"
        );
    }

    #[test]
    fn derive_views_omits_copy_flag_for_a_face_down_permanent() {
        // CR 708.2: a face-down permanent's characteristics are only those the
        // face-down rules grant. Surfacing "copy" on one would leak hidden
        // information about what it really is.
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let hidden = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Face-Down Copy".into(),
            Zone::Battlefield,
        );
        let values = crate::game::printed_cards::intrinsic_copiable_values(
            state.objects.get(&hidden).unwrap(),
        );
        state.objects.get_mut(&hidden).unwrap().face_down = true;
        state.add_transient_continuous_effect(
            hidden,
            PlayerId(0),
            Duration::Permanent,
            TargetFilter::SpecificObject { id: hidden },
            vec![ContinuousModification::CopyValues {
                values: Box::new(values),
                display_source: DisplaySource::Card,
                printed_ref: None,
                token_image_ref: None,
            }],
            None,
        );

        assert!(
            derive_views(&state, None).copied_permanents.is_empty(),
            "a face-down permanent must never be reported as a copy (CR 708.2)"
        );
    }

    #[test]
    fn derive_views_omits_copy_flag_when_no_copy_effect_is_live() {
        // Discriminating guard: an ordinary board must report nothing, so the
        // projection cannot degenerate into "every permanent is a copy".
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".into(),
            Zone::Battlefield,
        );
        assert!(derive_views(&state, None).copied_permanents.is_empty());
    }

    #[test]
    fn derive_views_projects_only_battlefield_relevant_keyword_badges() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let permanent = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Keyword Test Creature".into(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&permanent).unwrap().keywords = vec![
            Keyword::Flying,
            Keyword::Ravenous,
            Keyword::Evoke(crate::types::keywords::EvokeCost::Mana(ManaCost::NoCost)),
            Keyword::Fading(3),
        ];

        let hand_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Off-Battlefield Flyer".into(),
            Zone::Hand,
        );
        state.objects.get_mut(&hand_card).unwrap().keywords = vec![Keyword::Flying];

        let views = derive_views(&state, None);

        assert_eq!(
            views.battlefield_keyword_badges.get(&permanent),
            Some(&vec![Keyword::Flying, Keyword::Fading(3)]),
            "the strip keeps live battlefield abilities but hides Ravenous and Evoke"
        );
        assert!(
            !views.battlefield_keyword_badges.contains_key(&hand_card),
            "only battlefield permanents receive keyword badge entries"
        );
    }

    #[test]
    fn turn_order_hidden_for_two_or_fewer_live_representatives() {
        let state = GameState::new(FormatConfig::standard(), 2, 42);

        let views = derive_views(&state, None);

        assert!(
            views.turn_order.is_empty(),
            "one-on-one games should not emit redundant turn-order chips"
        );

        let json =
            serde_json::to_string(&ClientGameStateRef::wrap(&state, None)).expect("serialize");
        assert!(
            !json.contains("turn_order"),
            "empty turn order must be omitted from the wire payload"
        );
    }

    #[test]
    fn turn_order_duplicates_survive_wire_round_trip() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 4, 42);
        state.active_player = PlayerId(0);
        state.extra_turns.push(crate::types::game_state::ExtraTurn {
            player: PlayerId(2),
            anchor: PlayerId(0),
        });
        state.extra_turns.push(crate::types::game_state::ExtraTurn {
            player: PlayerId(0),
            anchor: PlayerId(0),
        });

        let views = derive_views(&state, None);

        assert_eq!(
            views.turn_order[..2],
            [
                TurnOrderSlotView {
                    player: PlayerId(0),
                    slot_index: 0,
                    turns_from_now: 0,
                    turn_number: 1,
                    is_viewer: false,
                    is_starting_player: true,
                },
                TurnOrderSlotView {
                    player: PlayerId(0),
                    slot_index: 1,
                    turns_from_now: 1,
                    turn_number: 2,
                    is_viewer: false,
                    is_starting_player: false,
                },
            ],
            "same player can be both NOW and NEXT when an extra turn is queued"
        );

        let viewer_views = derive_views(&state, Some(PlayerId(2)));
        assert_eq!(viewer_views.viewer_turn_number, Some(3));
        assert!(viewer_views.turn_order[2].is_viewer);
        assert_eq!(viewer_views.turn_order[2].turn_number, 3);

        let json =
            serde_json::to_string(&ClientGameStateRef::wrap(&state, None)).expect("serialize");
        assert!(
            json.contains("turn_order"),
            "multiplayer turn-order rows must serialize"
        );

        let round: ClientGameState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            round.derived.turn_order[..2],
            views.turn_order[..2],
            "duplicate same-player rows must survive the JSON round-trip"
        );
    }

    /// Four-player pod: P0 receives damage from two different opponents'
    /// commanders. The view must key entries by the attacking commander's
    /// controller, preserving per-commander granularity for the HUD.
    #[test]
    fn derive_views_groups_by_attacker_in_four_player_pod() {
        let mut state = setup_commander_game(4);
        let cmd_p1 = create_object(
            &mut state,
            CardId(1001),
            PlayerId(1),
            "P1 Commander".into(),
            Zone::Command,
        );
        let cmd_p2 = create_object(
            &mut state,
            CardId(1002),
            PlayerId(2),
            "P2 Commander".into(),
            Zone::Command,
        );
        state.objects.get_mut(&cmd_p1).unwrap().is_commander = true;
        state.objects.get_mut(&cmd_p2).unwrap().is_commander = true;
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(0),
            commander: cmd_p1,
            damage: 7,
        });
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(0),
            commander: cmd_p2,
            damage: 11,
        });

        let views = derive_views(&state, None);
        let from_p1 = views
            .commander_damage_by_attacker
            .get(&PlayerId(1))
            .expect("P1 should have an entry");
        let from_p2 = views
            .commander_damage_by_attacker
            .get(&PlayerId(2))
            .expect("P2 should have an entry");
        assert_eq!(from_p1.len(), 1);
        assert_eq!(from_p1[0].damage, 7);
        assert_eq!(from_p1[0].victim, PlayerId(0));
        assert_eq!(from_p1[0].commander, cmd_p1);
        assert_eq!(from_p2.len(), 1);
        assert_eq!(from_p2[0].damage, 11);
    }

    #[test]
    fn planechase_can_roll_view_uses_controlled_priority_seat() {
        let controller = PlayerId(0);
        let controlled = PlayerId(1);
        let mut state = GameState::new(FormatConfig::planechase(), 2, 7);
        state.active_player = controlled;
        state.priority_player = controller;
        state.turn_decision_controller = Some(controller);
        state.waiting_for = WaitingFor::Priority { player: controlled };
        state.phase = Phase::PreCombatMain;
        state.planar_controller = Some(controlled);
        state.planar_die_actions_this_turn.insert(controller, 2);

        let plane = create_object(
            &mut state,
            CardId(9000),
            controlled,
            "Controlled Turn Plane".to_string(),
            Zone::Command,
        );
        state
            .objects
            .get_mut(&plane)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Plane);
        state.command_zone.push_back(plane);

        let controller_view = derive_views(&state, Some(controller))
            .planechase
            .expect("Planechase view should be present");
        assert_eq!(
            controller_view.current_roll_cost,
            ManaCost::generic(0),
            "roll cost must be derived from the controlled active seat, not the submitter"
        );
        assert!(
            controller_view.can_roll,
            "authorized turn controller should see the planar-die action"
        );

        let controlled_view = derive_views(&state, Some(controlled))
            .planechase
            .expect("Planechase view should be present");
        assert!(
            !controlled_view.can_roll,
            "controlled seat is not the authorized human submitter during turn control"
        );
    }

    /// Partner commanders (two commanders under the same controller) must
    /// remain separate entries — CR 903.10a tracks commander damage per
    /// commander identity, so summing them would misreport the SBA-lethal
    /// progress when one partner is at 20 damage and the other at 5.
    #[test]
    fn derive_views_respects_partner_commanders() {
        let mut state = setup_commander_game(2);
        let partner_a = create_object(
            &mut state,
            CardId(2001),
            PlayerId(1),
            "Partner A".into(),
            Zone::Command,
        );
        let partner_b = create_object(
            &mut state,
            CardId(2002),
            PlayerId(1),
            "Partner B".into(),
            Zone::Command,
        );
        state.objects.get_mut(&partner_a).unwrap().is_commander = true;
        state.objects.get_mut(&partner_b).unwrap().is_commander = true;
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(0),
            commander: partner_a,
            damage: 20,
        });
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(0),
            commander: partner_b,
            damage: 5,
        });

        let views = derive_views(&state, None);
        let from_p1 = views
            .commander_damage_by_attacker
            .get(&PlayerId(1))
            .expect("P1 should have an entry");
        assert_eq!(
            from_p1.len(),
            2,
            "partner commanders must stay as separate entries, not be summed"
        );
        let damages: Vec<u32> = from_p1.iter().map(|e| e.damage).collect();
        assert!(damages.contains(&20));
        assert!(damages.contains(&5));
    }

    /// Stack grouping rides alongside commander damage in the same derived
    /// view: one `derive_views` pass populates both. The detailed grouping
    /// behavior (coalescing rules, target-aware keys, keyword-action opt-
    /// outs) is covered by the dedicated tests in `game::stack`; this test
    /// only verifies wiring — that `derive_views` invokes the grouper when
    /// the stack is non-empty and short-circuits when it is.
    #[test]
    fn derive_views_wires_stack_display_groups() {
        use crate::types::ability::{Effect, ResolvedAbility};
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(4001),
            PlayerId(0),
            "Scute Swarm".into(),
            Zone::Battlefield,
        );
        let mk_effect = || Effect::Unimplemented {
            name: "test".into(),
            description: None,
        };
        for i in 0..2u64 {
            state.stack.push_back(StackEntry {
                id: ObjectId(9000 + i),
                source_id: source,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: source,
                    ability: Box::new(ResolvedAbility::new(
                        mk_effect(),
                        vec![],
                        source,
                        PlayerId(0),
                    )),
                    condition: None,
                    trigger_event: None,
                    description: Some("landfall".into()),
                    source_name: String::new(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            });
        }

        let views = derive_views(&state, None);
        assert_eq!(
            views.stack_display_groups.len(),
            1,
            "identical adjacent triggers must coalesce into one group"
        );
        assert_eq!(views.stack_display_groups[0].count, 2);

        state.stack.clear();
        let empty = derive_views(&state, None);
        assert!(
            empty.stack_display_groups.is_empty(),
            "empty-stack short-circuit must leave the group vec empty"
        );
    }

    #[test]
    fn prospective_storm_counts_are_viewer_scoped() {
        use crate::types::identifiers::CardId;

        let mut state = GameState::new_two_player(42);
        let p0_storm = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grapeshot".to_string(),
            Zone::Hand,
        );
        let p1_storm = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Empty the Warrens".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&p0_storm)
            .unwrap()
            .keywords
            .push(Keyword::Storm);
        state
            .objects
            .get_mut(&p1_storm)
            .unwrap()
            .keywords
            .push(Keyword::Storm);
        state.players[0].hand.push_back(p0_storm);
        state.players[1].hand.push_back(p1_storm);
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            im::Vector::from(vec![
                crate::types::game_state::SpellCastRecord::default();
                2
            ]),
        );

        let views = derive_views(&state, Some(PlayerId(0)));
        assert_eq!(views.prospective_storm_counts.get(&p0_storm), Some(&2));
        assert!(!views.prospective_storm_counts.contains_key(&p1_storm));
    }

    #[test]
    fn prospective_storm_counts_include_effectively_granted_storm() {
        use crate::types::ability::{ControllerRef, StaticDefinition, TypeFilter, TypedFilter};

        let mut state = GameState::new_two_player(42);
        let grantor = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Storm Grantor".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&grantor).unwrap().static_definitions =
            vec![StaticDefinition::new(StaticMode::CastWithKeyword {
                keyword: Keyword::Storm,
            })
            .affected(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Instant).controller(ControllerRef::You),
            ))]
            .into();
        let spell = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Granted Storm Spell".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        state.players[0].hand.push_back(spell);
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            im::Vector::from(vec![crate::types::game_state::SpellCastRecord::default()]),
        );

        let views = derive_views(&state, Some(PlayerId(0)));

        assert_eq!(views.prospective_storm_counts.get(&spell), Some(&1));
    }

    #[test]
    fn prospective_storm_counts_include_next_spell_granted_storm() {
        let mut state = GameState::new_two_player(42);
        let spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Next Storm Spell".to_string(),
            Zone::Hand,
        );
        state.players[0].hand.push_back(spell);
        state.pending_next_spell_modifiers.push(
            crate::types::game_state::PendingNextSpellModifier {
                player: PlayerId(0),
                modifier: crate::types::game_state::NextSpellModifier::HasKeyword {
                    keyword: Keyword::Storm,
                },
                spell_filter: None,
                source_id: None,
            },
        );

        let views = derive_views(&state, Some(PlayerId(0)));

        assert_eq!(views.prospective_storm_counts.get(&spell), Some(&0));
    }

    /// SHAPE test (constructs `pending_cast`/pool directly, not via the cast
    /// pipeline): `pending_payment_remaining` is the locked cost reduced by ONLY
    /// the units the caster has pinned, so the payment UI's cost visibly shrinks
    /// as mana is selected and reads covered (`NoCost`) once the selection alone
    /// pays the whole cost. Also pins the viewer-scoping: an opponent never sees
    /// the caster's in-progress private selection.
    #[test]
    fn pending_payment_remaining_reflects_pinned_selection() {
        use crate::types::ability::{Effect, ResolvedAbility};
        use crate::types::game_state::{PendingCast, WaitingFor};
        use crate::types::mana::{ManaCost, ManaType, ManaUnit};

        let mut state = GameState::new_two_player(42);
        let p0 = PlayerId(0);
        let spell = create_object(&mut state, CardId(1), p0, "Test Spell".into(), Zone::Stack);

        // Three colorless pool units, each stamped with a distinct pip id.
        for _ in 0..3 {
            let _ = state.add_mana_to_pool(
                p0,
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            );
        }
        let pip_ids: Vec<_> = state.players[0]
            .mana_pool
            .mana
            .iter()
            .map(|u| u.pip_id)
            .collect();

        let ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "test".into(),
                description: None,
            },
            vec![],
            spell,
            p0,
        );
        state.pending_cast = Some(Box::new(PendingCast::new(
            spell,
            CardId(1),
            ability,
            ManaCost::generic(2),
        )));
        state.waiting_for = WaitingFor::ManaPayment {
            player: p0,
            convoke_mode: None,
        };

        // No selection → the whole {2} still has to be paid.
        assert_eq!(
            derive_views(&state, Some(p0)).pending_payment_remaining,
            Some(ManaCost::generic(2)),
        );

        // Pin one unit → {1} remains.
        state
            .pending_cast
            .as_mut()
            .unwrap()
            .pinned_pool_units
            .push(pip_ids[0]);
        assert_eq!(
            derive_views(&state, Some(p0)).pending_payment_remaining,
            Some(ManaCost::generic(1)),
        );

        // Pin a second → the selection alone covers the cost (NoCost).
        state
            .pending_cast
            .as_mut()
            .unwrap()
            .pinned_pool_units
            .push(pip_ids[1]);
        assert_eq!(
            derive_views(&state, Some(p0)).pending_payment_remaining,
            Some(ManaCost::NoCost),
        );

        // Viewer scoping: the opponent never sees the caster's private selection.
        assert_eq!(
            derive_views(&state, Some(PlayerId(1))).pending_payment_remaining,
            None,
        );
    }

    #[test]
    fn derive_views_wires_stack_entry_details() {
        let mut state = GameState::new_two_player(42);
        let spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Prismatic Ending".to_string(),
            Zone::Stack,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Sol Ring".to_string(),
            Zone::Battlefield,
        );
        let mut ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "exile".to_string(),
                description: None,
            },
            vec![TargetRef::Object(target)],
            spell,
            PlayerId(0),
        );
        ability.chosen_x = Some(1);
        state.stack.push_back(StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: Some(Box::new(ability)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 2,
            },
        });
        state.stack_paid_facts.insert(
            spell,
            StackPaidSnapshot {
                actual_mana_spent: 2,
                x_value: Some(1),
                distinct_colors_spent: 2,
                ..Default::default()
            },
        );

        let views = derive_views(&state, None);
        let details = views
            .stack_entry_details
            .get(&spell)
            .expect("stack details include the spell");
        assert_eq!(details.source_name, "Prismatic Ending");
        assert_eq!(details.targets[0].label, "Sol Ring");
        assert!(details
            .paid
            .iter()
            .any(|fact| matches!(fact, StackPaidFactView::XValue { value: 1 })));
        assert!(details
            .paid
            .iter()
            .any(|fact| matches!(fact, StackPaidFactView::ColorsSpent { distinct: 2 })));
    }

    #[test]
    fn stack_entry_details_projects_storm_provenance_to_the_client_wire() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grapeshot".to_string(),
            Zone::Stack,
        );
        let trigger = ObjectId(2);
        state.stack.push_back(StackEntry {
            id: trigger,
            source_id: source,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: source,
                ability: Box::new(ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "storm".to_string(),
                        description: None,
                    },
                    Vec::new(),
                    source,
                    PlayerId(0),
                )),
                condition: None,
                trigger_event: None,
                description: Some("Storm".to_string()),
                source_name: "Grapeshot".to_string(),
                subject_match_count: None,
                die_result: None,
                provenance: Some(SyntheticTriggerProvenance::Storm { copy_count: 2 }),
            },
        });

        let views = derive_views(&state, Some(PlayerId(0)));
        assert_eq!(
            views.stack_entry_details[&trigger].provenance,
            Some(SyntheticTriggerProvenance::Storm { copy_count: 2 }),
            "the stack detail projection carries typed Storm provenance"
        );

        let wire = serde_json::to_value(ClientGameStateRef::wrap(&state, Some(PlayerId(0))))
            .expect("serialize client game state");
        assert_eq!(
            wire["derived"]["stack_entry_details"][trigger.0.to_string()]["provenance"],
            serde_json::json!({"type": "Storm", "data": {"copy_count": 2}}),
            "the frontend's derived stack-detail wire retains Storm provenance",
        );
    }

    #[test]
    fn pending_modal_spell_details_survive_filtering_and_client_wire_round_trip() {
        let mut state = GameState::new_two_player(42);
        let spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Brotherhood's End".to_string(),
            Zone::Stack,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Sol Ring".to_string(),
            Zone::Battlefield,
        );
        state.stack.push_back(StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        let mut pending_ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "destroy".to_string(),
                description: None,
            },
            vec![TargetRef::Object(target)],
            spell,
            PlayerId(0),
        );
        pending_ability.selected_mode_labels = vec![
            "Brotherhood's End deals 3 damage to each creature and each planeswalker.".to_string(),
        ];
        state.waiting_for = WaitingFor::ModeChoice {
            player: PlayerId(0),
            modal: ModalChoice::default(),
            pending_cast: Box::new(PendingCast::new(
                spell,
                CardId(1),
                pending_ability,
                ManaCost::NoCost,
            )),
            unavailable_modes: Vec::new(),
        };

        let filtered = crate::game::visibility::filter_state_for_viewer(&state, PlayerId(1));
        let json = serde_json::to_string(&ClientGameStateRef::wrap_filtered(
            &state,
            &filtered,
            Some(PlayerId(1)),
        ))
        .expect("serialize filtered opponent view");
        let client: ClientGameState = serde_json::from_str(&json).expect("deserialize client view");
        let details = client
            .derived
            .stack_entry_details
            .get(&spell)
            .expect("public pending spell has stack details");

        assert!(
            details.is_pending,
            "the engine marks the matching entry pending"
        );
        assert_eq!(
            details.selected_mode_labels,
            ["Brotherhood's End deals 3 damage to each creature and each planeswalker."],
            "the public selected mode reaches an opponent through filtering and the client wrapper",
        );
        assert_eq!(details.targets[0].label, "Sol Ring");
    }

    #[test]
    fn stack_entry_display_selected_modes_are_wire_compatible_and_cloneable() {
        let empty = StackEntryDisplay {
            source_name: "Spell".to_string(),
            token_image_ref: None,
            kind_label: "Spell".to_string(),
            ability_description: None,
            selected_mode_labels: Vec::new(),
            is_pending: false,
            targets: Vec::new(),
            paid: Vec::new(),
            trigger_context: Vec::new(),
            provenance: None,
        };
        let empty_json = serde_json::to_string(&empty).expect("serialize empty display");
        assert!(
            !empty_json.contains("selected_mode_labels") && !empty_json.contains("is_pending"),
            "empty additions must preserve the legacy wire shape",
        );
        let legacy: StackEntryDisplay =
            serde_json::from_str(r#"{"source_name":"Spell","kind_label":"Spell"}"#)
                .expect("legacy display payload deserializes");
        assert!(legacy.selected_mode_labels.is_empty());
        assert!(!legacy.is_pending);

        let mut selected = empty;
        selected.selected_mode_labels = vec!["Choose this mode.".to_string()];
        selected.is_pending = true;
        let copied = selected.clone();
        let selected_json = serde_json::to_string(&selected).expect("serialize selected modes");
        assert!(selected_json.contains("selected_mode_labels"));
        assert_eq!(
            copied, selected,
            "derived display copies retain selected modes"
        );
    }

    #[test]
    fn derive_views_uses_filtered_names_for_trigger_context() {
        let mut state = GameState::new_two_player(42);
        let trigger_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Watcher".to_string(),
            Zone::Battlefield,
        );
        let hidden_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Secret Card".to_string(),
            Zone::Library,
        );
        let trigger_event = GameEvent::ZoneChanged {
            object_id: hidden_card,
            from: Some(Zone::Library),
            to: Zone::Hand,
            record: Box::new(ZoneChangeRecord {
                object_id: hidden_card,
                name: "Secret Card".to_string(),
                core_types: Vec::new(),
                subtypes: Vec::new(),
                supertypes: Vec::new(),
                keywords: Vec::new(),
                trigger_definitions: Vec::new(),
                trigger_source_context: None,
                power: None,
                toughness: None,
                base_power: None,
                base_toughness: None,
                colors: Vec::new(),
                mana_value: 0,
                controller: PlayerId(1),
                owner: PlayerId(1),
                from_zone: Some(Zone::Library),
                cast_from_zone: None,
                played_from_zone: None,
                to_zone: Zone::Hand,
                attachments: Vec::new(),
                linked_exile_snapshot: Vec::new(),
                is_token: false,
                combat_status: Default::default(),
                co_departed: Vec::new(),
                attached_to: None,
                entered_incarnation: None,
                turn_zone_change_index: 0,
                is_suspected: false,
            }),
        };
        let ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "trigger".to_string(),
                description: None,
            },
            Vec::new(),
            trigger_source,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: ObjectId(900),
            source_id: trigger_source,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: trigger_source,
                ability: Box::new(ability),
                condition: None,
                trigger_event: Some(trigger_event),
                description: Some("hidden-zone trigger".to_string()),
                source_name: "Watcher".to_string(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        });

        let filtered = crate::game::visibility::filter_state_for_viewer(&state, PlayerId(0));
        let mut views = derive_views(&filtered, None);
        let details = views
            .stack_entry_details
            .remove(&ObjectId(900))
            .expect("trigger details are present");
        let label = details
            .trigger_context
            .first()
            .expect("trigger context is present")
            .label
            .clone();
        assert!(
            !label.contains("Secret Card"),
            "trigger context must not bypass multiplayer hidden-card filtering"
        );
        assert!(label.contains("Hidden Card"));
    }

    /// Wire-format round-trip: the JSON produced from `ClientGameStateRef`
    /// must deserialize cleanly into `ClientGameState`. This guarantees the
    /// frontend's hand-maintained TypeScript type can consume what the
    /// WASM boundary produces.
    #[test]
    fn client_game_state_roundtrips_through_json() {
        let mut state = setup_commander_game(2);
        let cmd = create_object(
            &mut state,
            CardId(3001),
            PlayerId(1),
            "Roundtrip Cmdr".into(),
            Zone::Command,
        );
        state.objects.get_mut(&cmd).unwrap().is_commander = true;
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(0),
            commander: cmd,
            damage: 14,
        });

        let wrapped = ClientGameStateRef::wrap(&state, None);
        let json = serde_json::to_string(&wrapped).expect("serialize");
        let round: ClientGameState = serde_json::from_str(&json).expect("deserialize");
        let from_p1 = round
            .derived
            .commander_damage_by_attacker
            .get(&PlayerId(1))
            .expect("P1 entry survives round-trip");
        assert_eq!(from_p1[0].damage, 14);
    }

    #[test]
    fn client_wire_omits_private_delayed_trigger_authority() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(3_002),
            PlayerId(0),
            "Delayed source".into(),
            Zone::Battlefield,
        );
        let provenance = DelayedTriggerOrigin {
            token: DelayedTriggerToken(17),
            instance: DelayedTriggerInstanceId(23),
            source_id: source,
        };
        state.next_delayed_trigger_token = 18;
        state.next_delayed_trigger_instance = 24;
        state.delayed_triggers.push(DelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase {
                phase: Phase::Upkeep,
            },
            ability: Box::new(ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "client-wire fixture".into(),
                    description: None,
                },
                Vec::new(),
                source,
                PlayerId(0),
            )),
            controller: PlayerId(0),
            source_id: source,
            one_shot: true,
            provenance: crate::types::identifiers::DelayedInstallIdentity::ReceiptEligible(
                provenance,
            ),
        });
        state
            .stack_trigger_firings
            .insert(ObjectId(999), TriggerFiring::ReceiptEligible(provenance));
        state.resolving_trigger_firing = Some(TriggerFiring::ReceiptEligible(provenance));
        let delayed_pending_context = || {
            PendingTriggerContext::delayed_for_test(
                PendingTrigger {
                    source_id: source,
                    controller: PlayerId(0),
                    condition: None,
                    ability: Box::new(ResolvedAbility::new(
                        Effect::Unimplemented {
                            name: "delayed queue fixture".into(),
                            description: None,
                        },
                        Vec::new(),
                        source,
                        PlayerId(0),
                    )),
                    timestamp: 0,
                    target_constraints: Vec::new(),
                    distribute: None,
                    trigger_event: None,
                    modal: None,
                    mode_abilities: Vec::new(),
                    description: None,
                    may_trigger_origin: None,
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
                provenance,
            )
        };
        state.pending_trigger = Some(Box::new(delayed_pending_context().pending));
        state.pending_trigger_firing = Some(TriggerFiring::ReceiptEligible(provenance));
        state.deferred_triggers.push(delayed_pending_context());
        state.pending_trigger_order = Some(crate::types::game_state::PendingTriggerOrder {
            groups: vec![TriggerOrderGroup {
                controller: PlayerId(0),
                triggers: vec![delayed_pending_context()],
                ordered: false,
            }],
            resume_after_ordering: None,
        });

        let persisted = serde_json::to_value(&state).expect("serialize trusted state");
        assert_eq!(persisted["next_delayed_trigger_token"], 18);
        assert_eq!(persisted["next_delayed_trigger_instance"], 24);
        assert_eq!(
            persisted["delayed_triggers"][0]["provenance"]["ReceiptEligible"]["token"],
            17
        );
        assert!(persisted["stack_trigger_firings"].is_object());
        assert_eq!(
            persisted["resolving_trigger_firing"]["ReceiptEligible"]["instance"],
            23,
        );
        assert_eq!(
            persisted["pending_trigger_firing"]["ReceiptEligible"]["token"], 17,
            "test precondition: trusted persistence retains active delayed authority"
        );
        assert_eq!(
            persisted["deferred_triggers"][0]["firing"]["ReceiptEligible"]["token"], 17,
            "test precondition: trusted persistence retains delayed queue authority"
        );
        assert_eq!(
            persisted["pending_trigger_order"]["groups"][0]["triggers"][0]["firing"]
                ["ReceiptEligible"]["instance"],
            23,
            "test precondition: trusted ordering queue retains delayed authority"
        );

        let client = serde_json::to_value(ClientGameStateRef::wrap(&state, Some(PlayerId(0))))
            .expect("serialize client state");
        let filtered = crate::game::visibility::filter_state_for_viewer(&state, PlayerId(0));
        let filtered_client = serde_json::to_value(ClientGameStateRef::wrap_filtered(
            &state,
            &filtered,
            Some(PlayerId(0)),
        ))
        .expect("serialize filtered client state");

        for client_state in [&client["state"], &filtered_client["state"]] {
            let error = serde_json::from_value::<GameState>(client_state.clone())
                .expect_err("redacted client state must not restore as trusted authority");
            assert!(
                error
                    .to_string()
                    .contains("pending trigger has no firing carrier"),
                "client redaction must fail only because it removes private trigger authority: {error}"
            );
            for private_field in [
                "next_delayed_trigger_token",
                "next_delayed_trigger_instance",
                "pending_trigger_firing",
                "stack_trigger_firings",
                "resolving_trigger_firing",
                "resolved_rules_journal",
            ] {
                assert!(
                    client_state.get(private_field).is_none(),
                    "client wire must omit {private_field}"
                );
            }
            assert!(
                client_state["delayed_triggers"][0]
                    .get("provenance")
                    .is_none(),
                "client wire must omit delayed-trigger provenance"
            );
            for contexts in [
                &client_state["deferred_triggers"],
                &client_state["pending_trigger_order"]["groups"][0]["triggers"],
            ] {
                let contexts = contexts.as_array().expect("nonempty trigger queue");
                assert!(
                    !contexts.is_empty(),
                    "test precondition: trigger queue must remain present"
                );
                for context in contexts {
                    assert!(
                        context.get("firing").is_none(),
                        "client wire must omit delayed firing classification"
                    );
                    let context = context.to_string();
                    assert!(
                        !context.contains("\"token\"") && !context.contains("\"instance\""),
                        "client queue must omit delayed authority: {context}"
                    );
                }
            }
        }
    }

    /// CR 303.4 + CR 702.5: A Player-attached Aura on the battlefield must
    /// surface in `auras_attached_to_player` keyed by the host player. The
    /// frontend has no other channel for this — the FE doesn't (and per
    /// CLAUDE.md, must not) scan the battlefield itself for player-host
    /// attachments. Object-host attachments must NOT appear here; those
    /// route through `GameObject::attachments` on the host.
    #[test]
    fn derive_views_surfaces_auras_attached_to_player() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let curse = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Curse of Opulence".into(),
            Zone::Battlefield,
        );
        // Only Auras may have a Player host (mirrors `attach_to_player`'s
        // CR 303.4 gate). Mark the subtype so a future tightening that
        // double-checks at the derive layer wouldn't yank this entry.
        state
            .objects
            .get_mut(&curse)
            .unwrap()
            .card_types
            .subtypes
            .push("Aura".to_string());
        state.objects.get_mut(&curse).unwrap().attached_to =
            Some(AttachTarget::Player(PlayerId(1)));
        // `create_object` already added `curse` to `state.battlefield`
        // through `add_to_zone(Zone::Battlefield)` — no manual push needed
        // (a duplicate push would surface as duplicate entries in the
        // derived view's per-player Vec, which the assertion catches).

        // Object-host control: a hypothetical Aura attached to a creature
        // must NOT leak into the player map.
        let creature = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "A Creature".into(),
            Zone::Battlefield,
        );
        let aura_on_creature = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Some Aura".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&aura_on_creature)
            .unwrap()
            .card_types
            .subtypes
            .push("Aura".to_string());
        state
            .objects
            .get_mut(&aura_on_creature)
            .unwrap()
            .attached_to = Some(AttachTarget::Object(creature));
        // No manual battlefield pushes — `create_object` did it for both.

        let views = derive_views(&state, None);
        let p1_auras = views
            .auras_attached_to_player
            .get(&PlayerId(1))
            .expect("P1 should appear as an Aura host");
        assert_eq!(p1_auras, &vec![curse], "Curse must be the only entry");
        assert!(
            !views.auras_attached_to_player.contains_key(&PlayerId(0)),
            "P0 has no Aura host — must not get an empty entry",
        );
    }

    /// CR 101.2 / CR 614.16 / CR 702.50b: stored restrictions and epic locks
    /// project into per-player status rows; the scope is resolved to concrete
    /// players, kinds map correctly, and `DamagePreventionDisabled` (no
    /// per-player axis) contributes nothing.
    #[test]
    fn derive_views_projects_stored_player_conditions() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        // A source permanent controlled by P0 (imposes the restrictions).
        let source = create_object(
            &mut state,
            CardId(7),
            PlayerId(0),
            "Restrictor".into(),
            Zone::Battlefield,
        );

        // CR 101.2: P1 specifically can't cast spells.
        state.restrictions.push(GameRestriction::ProhibitActivity {
            source,
            affected_players: RestrictionPlayerScope::SpecificPlayer(PlayerId(1)),
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::CastSpells { spell_filter: None },
        });
        // CR 602.5: all players can't activate non-mana abilities.
        state.restrictions.push(GameRestriction::ProhibitActivity {
            source,
            affected_players: RestrictionPlayerScope::AllPlayers,
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::ActivateAbilities {
                exemption: ActivationExemption::ManaAbilities,
                only_tag: None,
            },
        });
        // CR 614.16: no per-player axis — must NOT produce a status row.
        state
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source,
                expiry: RestrictionExpiry::EndOfTurn,
                scope: None,
            });

        let status = derive_views(&state, None).player_status;

        // P1 can't cast (SpecificPlayer), attributed to the source.
        assert!(
            status.contains(&PlayerStatusView {
                player: PlayerId(1),
                kind: PlayerConditionKind::CantCastSpells,
                source: Some(source),
            }),
            "P1's cast prohibition should project with its source",
        );
        // Both players can't activate abilities (AllPlayers).
        for pid in [PlayerId(0), PlayerId(1)] {
            assert!(
                status.contains(&PlayerStatusView {
                    player: pid,
                    kind: PlayerConditionKind::CantActivateAbilities,
                    source: Some(source),
                }),
                "AllPlayers scope should project to {pid:?}",
            );
        }
        // P0 is NOT cast-locked (the cast prohibition was P1-specific).
        assert!(
            !status
                .iter()
                .any(|v| v.player == PlayerId(0) && v.kind == PlayerConditionKind::CantCastSpells),
            "P0 must not inherit P1's specific cast prohibition",
        );
        // DamagePreventionDisabled contributes no rows.
        assert_eq!(
            status.len(),
            3,
            "exactly 3 rows: P1 can't-cast + both players can't-activate; \
             DamagePreventionDisabled excluded",
        );
    }

    /// CR 101.2: `OpponentsOfSourceController` resolves to every player except
    /// the source's controller.
    #[test]
    fn derive_views_resolves_opponents_of_source_controller() {
        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        let source = create_object(
            &mut state,
            CardId(8),
            PlayerId(1),
            "Silence Engine".into(),
            Zone::Battlefield,
        );
        state.restrictions.push(GameRestriction::ProhibitActivity {
            source,
            affected_players: RestrictionPlayerScope::OpponentsOfSourceController,
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::CastSpells { spell_filter: None },
        });

        let afflicted: Vec<PlayerId> = derive_views(&state, None)
            .player_status
            .into_iter()
            .filter(|v| v.kind == PlayerConditionKind::CantCastSpells)
            .map(|v| v.player)
            .collect();

        assert!(
            !afflicted.contains(&PlayerId(1)),
            "the source's controller (P1) is not their own opponent",
        );
        assert!(
            afflicted.contains(&PlayerId(0)) && afflicted.contains(&PlayerId(2)),
            "both opponents (P0, P2) should be cast-locked",
        );
    }

    /// CR 702.188a + CR 604.1: web-slinging costs are VIEWER-scoped. P0 controls
    /// the grantor; both P0 and P1 hold a qualifying spell. `derive_views` for P0
    /// must surface ONLY P0's card (never P1's, even though the grant is symmetric
    /// in the abstract) so the unfiltered path can't leak opponent hand contents.
    /// `derive_views(_, None)` must surface nothing.
    #[test]
    fn web_slinging_costs_are_viewer_scoped_and_leak_proof() {
        use crate::types::ability::{
            Comparator, ControllerRef, FilterProp, StaticDefinition, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::{CoreType, Supertype};
        use crate::types::keywords::Keyword;
        use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
        use crate::types::statics::StaticMode;

        let mut state = GameState::new(FormatConfig::standard(), 2, 7);

        // P0 controls the Amazing Spider-Man grantor static.
        let grantor = create_object(
            &mut state,
            CardId(8000),
            PlayerId(0),
            "Amazing Spider-Man".to_string(),
            Zone::Battlefield,
        );
        {
            let affected = TargetFilter::Typed(TypedFilter {
                type_filters: vec![],
                controller: Some(ControllerRef::You),
                properties: vec![
                    FilterProp::HasSupertype {
                        value: Supertype::Legendary,
                    },
                    FilterProp::ColorCount {
                        comparator: Comparator::GE,
                        count: 1,
                    },
                ],
            });
            let cost = ManaCost::Cost {
                shards: vec![
                    ManaCostShard::Green,
                    ManaCostShard::White,
                    ManaCostShard::Blue,
                ],
                generic: 0,
            };
            let def = StaticDefinition::new(StaticMode::CastWithKeyword {
                keyword: Keyword::WebSlinging(cost),
            })
            .affected(affected);
            state.objects.get_mut(&grantor).unwrap().static_definitions = vec![def].into();
        }

        // A qualifying legendary multicolored card in each player's hand.
        let add_qualifying = |state: &mut GameState, card: CardId, owner: PlayerId| -> ObjectId {
            let id = create_object(state, card, owner, "Legend".to_string(), Zone::Hand);
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.supertypes.push(Supertype::Legendary);
            obj.color = vec![ManaColor::Green, ManaColor::Blue];
            id
        };
        let p0_card = add_qualifying(&mut state, CardId(8001), PlayerId(0));
        let p1_card = add_qualifying(&mut state, CardId(8002), PlayerId(1));

        // Viewer = P0: only P0's card is surfaced.
        let p0_views = derive_views(&state, Some(PlayerId(0)));
        assert!(
            p0_views.web_slinging_costs.contains_key(&p0_card),
            "P0's own qualifying card must be surfaced for viewer P0"
        );
        assert!(
            !p0_views.web_slinging_costs.contains_key(&p1_card),
            "P1's card must NOT leak into P0's viewer-scoped web-slinging costs"
        );

        // No viewer: nothing surfaced.
        let none_views = derive_views(&state, None);
        assert!(
            none_views.web_slinging_costs.is_empty(),
            "derive_views(_, None) must not populate web-slinging costs"
        );
    }

    /// PR-6 test 7: `attribution_player` is exhaustive and routes payload-keyed
    /// axes both directions — a controller-self payload stays on the controller, a
    /// victim payload routes to the named victim — while every aggregate axis stays
    /// on the controller.
    ///
    /// REVERT-PROBE: change the `Life | DamageDealt | LibraryDelta | Poison => p` arm to
    /// `=> controller` → the four victim assertions (`p1` expected) fail.
    #[test]
    fn attribution_player_routes_payload_axes_both_directions() {
        use crate::analysis::resource::{CounterClass, ObjectClass, TriggerKind};
        use crate::types::mana::ManaType;

        let p0 = PlayerId(0);
        let p1 = PlayerId(1);

        // Controller-self payload axes attribute to the controller.
        assert_eq!(attribution_player(ResourceAxis::Life(p0), p0), p0);
        assert_eq!(attribution_player(ResourceAxis::LibraryDelta(p0), p0), p0);
        assert_eq!(attribution_player(ResourceAxis::DamageDealt(p0), p0), p0);

        // Victim payload axes attribute to the NAMED victim, not the controller.
        assert_eq!(attribution_player(ResourceAxis::Life(p1), p0), p1);
        assert_eq!(attribution_player(ResourceAxis::DamageDealt(p1), p0), p1);
        assert_eq!(attribution_player(ResourceAxis::LibraryDelta(p1), p0), p1);
        // CR 704.5c: a poison ∞ belongs on the afflicted player's HUD, not the controller's.
        assert_eq!(attribution_player(ResourceAxis::Poison(p1), p0), p1);

        // Aggregate axes (no victim PlayerId) attribute to the controller.
        assert_eq!(
            attribution_player(ResourceAxis::Mana(ManaType::Red), p0),
            p0
        );
        assert_eq!(
            attribution_player(
                ResourceAxis::Counter(CounterClass::Plus1Plus1, ObjectClass::Creature),
                p0
            ),
            p0
        );
        assert_eq!(
            attribution_player(ResourceAxis::Trigger(TriggerKind::Proliferate), p0),
            p0
        );
        assert_eq!(attribution_player(ResourceAxis::TokensCreated, p0), p0);
    }

    /// Two controllers draining the SAME victim, only one of them accepted.
    ///
    /// THIS DOC ANSWERS THE OBJECTION IT USED TO MAKE. It previously argued that a separate
    /// schedule channel cannot represent this shape and that the flag therefore had to ride on the
    /// row. That was RIGHT about a `(player, axis)`-keyed ROW-MARKING channel, and it remains a
    /// correct reason never to build one: both rows are attributed to the victim (CR 119.3 +
    /// CR 704.5a, the same pair `attribution_player` cites), so they share an identical
    /// `(player, axis)` wire key, and any join on that key marks P0's row scheduled off P1's
    /// accepted stash.
    ///
    /// This projection does not build one. It deletes the row flag and publishes a
    /// `(player, FAMILY)`-keyed [`FamilyCollapseState`] whose join lattice DOES represent the
    /// disagreement. Both drains fold into the victim's `life` family, and
    /// `Scheduled(_) ⊔ Unscheduled = Mixed`, which renders a bare `∞` — the honest answer, where
    /// the old frontend OR-fold rendered a wrong `∞→N`. The per-controller distinction is joined
    /// at family granularity deliberately, BECAUSE THE BADGE IS PER FAMILY: a single glyph cannot
    /// say two things, so it says the true weaker one. The controller key still does the work — it
    /// is read before `attribution_player` erases it — it just no longer has to survive onto the
    /// wire.
    ///
    /// MUTATIONS THAT RED THIS (two-sided): (a) compute the state by testing each axis against the
    /// union of every controller's scheduled set — the `(player, axis)` join — instead of
    /// `controller`'s own ⇒ the family comes back `Scheduled(Conditional)`; (b) never consult the
    /// stash ⇒ it comes back `Unscheduled`.
    #[test]
    fn two_controllers_draining_one_victim_do_not_cross_schedule() {
        use crate::types::game_state::PersistentAxisMaterialization;

        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        let (p0, p1, victim) = (PlayerId(0), PlayerId(1), PlayerId(2));
        let axis = ResourceAxis::Life(victim);

        // Both controllers run their own drain loop on the same victim.
        state.mark_unbounded_loop(p0, &[axis]);
        state.mark_unbounded_loop(p1, &[axis]);
        // Only P1's collapse has been accepted.
        state.register_pending_materialization(
            p1,
            PersistentAxisMaterialization::Life {
                player: victim,
                per_cycle_delta: 1,
            },
        );

        let views = derive_views(&state, Some(victim));
        let rows: Vec<&UnboundedResourceView> = views
            .unbounded_resources
            .iter()
            .filter(|r| r.axis == axis)
            .collect();

        // REACH GUARD: both loops really reached the wire, and both landed on the victim's HUD.
        // Without this, the `{false, true}` assertion below could be satisfied by a single row
        // plus a phantom, or by rows that never got attributed to the victim at all.
        assert_eq!(
            rows.len(),
            2,
            "reach: both controllers' drain loops must project a row, got {rows:?}"
        );
        assert!(
            rows.iter().all(|r| r.player == victim),
            "reach: a life axis attributes to the victim (CR 119.3 + CR 704.5a), got {rows:?}"
        );

        // THE assertion, and WHY IT FLIPPED FROM THE OLD ONE. This test used to assert
        // `{false, true}` over the two rows' `scheduled` flags — a claim only expressible on a
        // per-row channel. The row flag is gone; the badge is per family, so the engine now answers
        // per family and the two disagreeing controllers collapse to ONE `life` row on the victim
        // whose state is `Mixed`. That is a strictly more honest answer than either old flag: the
        // frontend used to OR them and render `∞→N` for a collapse only one controller accepted.
        let life_rows: Vec<&UnboundedFamilyView> = views
            .unbounded_families
            .iter()
            .filter(|f| f.player == victim && f.family == UnboundedFamily::Life)
            .collect();
        assert_eq!(
            life_rows.len(),
            1,
            "one badge per (seat, family), even with two controllers on the axis, got {life_rows:?}"
        );
        assert_eq!(
            life_rows[0].state,
            FamilyCollapseState::Mixed,
            "P1 accepted a collapse and P0 did not, so the victim's life family is Mixed and \
             renders a bare ∞ — a (player, axis) union join would say Scheduled(Conditional) and \
             never consulting the stash would say Unscheduled; got {:?}",
            life_rows[0]
        );

        // CROSS-CHECK AGAINST THE CONTRACT'S OWN AUTHORITY, not against a second wire channel.
        // `pending_unbounded_materialization` is what the boundary reads to cash the collapse out,
        // so it — not the projection — decides how many accepted collapses exist. Asserting the
        // flag against it is what keeps the flag a display shadow rather than a second source of
        // truth: exactly one controller accepted, and it is the one whose row came back `true`.
        let accepted: Vec<PlayerId> = state
            .pending_unbounded_materialization
            .iter()
            .filter(|(_, items)| state.scheduled_collapse_axes(items).contains(&axis))
            .map(|(pid, _)| *pid)
            .collect();
        assert_eq!(
            accepted,
            vec![p1],
            "the accepted-collapse contract names exactly P1 for this axis, got {accepted:?}"
        );
    }

    /// PR-6 test 1: a REAL opponent-burn certificate's axes project into victim-HUD
    /// rows. The axis set is derived via the SAME authority `detect_loop` uses
    /// (`ResourceVector::unbounded_axes_for`) from the delta a damage pinger loop
    /// produces (positive damage to P1, P1's life driven negative), so it is the
    /// genuine `{DamageDealt(P1), Life(P1)}` cert — both on the victim P1, never a
    /// controller `Life(P0)`.
    ///
    /// REVERT-PROBE: delete the `derive_views` projection loop → `unbounded_resources`
    /// is empty → both `contains` assertions fail. Without the `mark_unbounded_loop`
    /// call the projection is also empty.
    #[test]
    fn real_certificate_axes_project_to_victim_hud() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);

        // The delta an opponent-burn pinger loop pumps each cycle.
        let mut delta = crate::analysis::ResourceVector::default();
        delta.damage_dealt.insert(PlayerId(1), 1);
        delta.life.insert(PlayerId(1), -1);
        // Same single authority that fills LoopCertificate.unbounded (loop_check.rs).
        let cert_axes = delta.unbounded_axes_for(PlayerId(0));
        assert!(cert_axes.contains(&ResourceAxis::DamageDealt(PlayerId(1))));
        assert!(cert_axes.contains(&ResourceAxis::Life(PlayerId(1))));
        assert!(
            !cert_axes.contains(&ResourceAxis::Life(PlayerId(0))),
            "the controller has no Life axis — the drain is on the victim P1"
        );

        state.mark_unbounded_loop(PlayerId(0), &cert_axes);
        let views = derive_views(&state, None);
        assert!(
            views.unbounded_resources.contains(&UnboundedResourceView {
                player: PlayerId(1),
                axis: ResourceAxis::DamageDealt(PlayerId(1)),
            }),
            "opponent-burn ∞ damage must land on the victim P1's HUD"
        );
        assert!(
            views.unbounded_resources.contains(&UnboundedResourceView {
                player: PlayerId(1),
                axis: ResourceAxis::Life(PlayerId(1)),
            }),
            "opponent-drain ∞ life must land on the victim P1's HUD"
        );
    }

    /// PR-6 test 9 (hostile e2e): a hand-built `{DamageDealt(P1), Life(P1)}` cert
    /// where the VICTIM P1 ALSO controls a permanent. Attribution must follow the
    /// axis payload PlayerId, NOT permanent control — both rows land on P1's HUD,
    /// none on the loop controller P0.
    ///
    /// REVERT-PROBE: make `attribution_player` return `controller` for
    /// `DamageDealt`/`Life` → both rows move to P0 → the P1 assertions fail and the
    /// "no controller rows" assertion fails.
    #[test]
    fn attribution_hostile_victim_controls_permanent() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        // Hostile element: the victim P1 controls a battlefield permanent. If
        // attribution keyed off permanent control rather than the axis payload, the
        // routing could be fooled — it must not be.
        create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Victim's Permanent".into(),
            Zone::Battlefield,
        );

        // P0 is the loop controller; the cert names P1 (victim) on both axes.
        state.mark_unbounded_loop(
            PlayerId(0),
            &[
                ResourceAxis::DamageDealt(PlayerId(1)),
                ResourceAxis::Life(PlayerId(1)),
            ],
        );

        let views = derive_views(&state, None);
        assert!(
            views.unbounded_resources.contains(&UnboundedResourceView {
                player: PlayerId(1),
                axis: ResourceAxis::DamageDealt(PlayerId(1)),
            }),
            "damage ∞ belongs to the victim P1, not the controller"
        );
        assert!(
            views.unbounded_resources.contains(&UnboundedResourceView {
                player: PlayerId(1),
                axis: ResourceAxis::Life(PlayerId(1)),
            }),
            "drain ∞ belongs to the victim P1, not the controller"
        );
        assert!(
            !views
                .unbounded_resources
                .iter()
                .any(|v| v.player == PlayerId(0)),
            "no ∞ row may attribute to the controller P0 for victim-keyed axes"
        );
    }

    /// Minimal 1/1 token profile for the `Tokens` stash kind. Only the VARIANT matters to these
    /// tests — `family_of` and `materialization_certainty` are both payload-independent.
    fn family_test_token_profile() -> Box<crate::types::ability::CopiableValues> {
        Box::new(crate::types::ability::CopiableValues {
            name: "Saproling".to_string(),
            mana_cost: ManaCost::default(),
            color: vec![],
            card_types: crate::types::card_type::CardType::default(),
            power: Some(1),
            toughness: Some(1),
            loyalty: None,
            printed_loyalty: None,
            keywords: vec![],
            abilities: std::sync::Arc::default(),
            trigger_definitions: std::sync::Arc::default(),
            replacement_definitions: std::sync::Arc::default(),
            static_definitions: std::sync::Arc::default(),
        })
    }

    /// PR-6 test 3 (projection half): a NON-mana unbounded axis still projects an
    /// `∞` row attributed to its controller; the empty map yields no rows (field
    /// omitted). The mana-vs-non-mana refill gating half lives in
    /// `mana_payment::refill_infinite_mana_gated_on_mana_axis_only`.
    ///
    /// M-F3 EXTENSION — the FORMAT branch for `unbounded_families`. This is a
    /// `FormatConfig::standard()` game, so it runs past the Commander short-circuit that would
    /// swallow a channel emitted too late.
    ///
    /// REVERT-PROBE: delete the `derive_views` projection loop → the `TokensCreated`
    /// row is absent → the `contains` assertion fails. MOVE the `unbounded_families` emit below
    /// the `commander_damage_threshold.is_none()` return → assertions (a) and (b) fail HERE while
    /// every Commander test stays green.
    #[test]
    fn non_mana_axis_projects_to_controller_hud() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        state.mark_unbounded_loop(PlayerId(0), &[ResourceAxis::TokensCreated]);

        let views = derive_views(&state, None);
        assert!(
            views.unbounded_resources.contains(&UnboundedResourceView {
                player: PlayerId(0),
                axis: ResourceAxis::TokensCreated,
            }),
            "a non-mana unbounded axis must still project an ∞ row on its controller"
        );
        // (a) nothing accepted yet ⇒ the badge promises nothing.
        assert!(
            views.unbounded_families.contains(&UnboundedFamilyView {
                player: PlayerId(0),
                family: UnboundedFamily::Tokens,
                state: FamilyCollapseState::Unscheduled,
            }),
            "an un-accepted ∞ tokens axis is Unscheduled in a non-Commander game, got {:?}",
            views.unbounded_families
        );

        // (b) once the accept registers a stash, the SAME frame reports the schedule — and
        // `Tokens` is Conditional, because its boundary mint can park on a replacement choice.
        state.register_pending_materialization(
            PlayerId(0),
            crate::types::game_state::PersistentAxisMaterialization::Tokens(
                family_test_token_profile(),
            ),
        );
        let scheduled_views = derive_views(&state, None);
        assert!(
            scheduled_views
                .unbounded_families
                .contains(&UnboundedFamilyView {
                    player: PlayerId(0),
                    family: UnboundedFamily::Tokens,
                    state: FamilyCollapseState::Scheduled(CollapseCertainty::Conditional),
                }),
            "an accepted Tokens collapse is Scheduled(Conditional) — never Committed; got {:?}",
            scheduled_views.unbounded_families
        );

        // (c) the empty state: no loop ⇒ neither channel exists.
        let empty = GameState::new(FormatConfig::standard(), 2, 42);
        let empty_views = derive_views(&empty, None);
        assert!(
            empty_views.unbounded_resources.is_empty(),
            "no unbounded loop → no ∞ rows (field omitted)"
        );
        assert!(
            empty_views.unbounded_families.is_empty(),
            "no unbounded loop → no family rows either (field omitted)"
        );
    }

    /// M1-a: a family holding one SCHEDULED and one UNSCHEDULED axis is `Mixed`, never
    /// `Scheduled`. `Poison(_)` and `Counter(_, _)` both fold into the `counters` family
    /// (an ADJACENT-VARIANT hostile fixture: the two axes are different `ResourceAxis`
    /// variants that must nonetheless share one badge).
    ///
    /// MUTATIONS: (a) fold with OR ("scheduled if ANY member is") ⇒ `Scheduled(Conditional)`;
    /// (b) fold with AND ⇒ `Unscheduled`. (c) MATCHED POSITIVE in this same test: the same
    /// state without the `Poison` seed yields `Scheduled(Conditional)`, so a badge that never
    /// reports a schedule at all cannot pass either.
    #[test]
    fn mixed_family_is_not_scheduled() {
        use crate::types::counter::CounterType;
        use crate::types::game_state::PersistentAxisMaterialization;

        let p0 = PlayerId(0);
        let charge = CounterType::Generic("charge".into());
        let seed = |also_poison: bool| {
            let mut state = GameState::new(FormatConfig::standard(), 2, 42);
            let prism = create_object(
                &mut state,
                CardId(1),
                p0,
                "Pentad Prism".into(),
                Zone::Battlefield,
            );
            let counter_axis =
                crate::types::game_state::collapsed_counter_axis(&state, prism, &charge);
            let mut axes = vec![counter_axis];
            if also_poison {
                axes.push(ResourceAxis::Poison(p0));
            }
            state.mark_unbounded_loop(p0, &axes);
            // Only the counter axis is accepted; `Poison` is marked ∞ but unscheduled.
            state.register_pending_materialization(
                p0,
                PersistentAxisMaterialization::Counters(vec![
                    crate::types::game_state::CounterGrowth {
                        object: prism,
                        counter: charge.clone(),
                        per_cycle_delta: 1,
                    },
                ]),
            );
            (state, counter_axis)
        };

        let (mixed_state, counter_axis) = seed(true);
        // IN-TEST PREMISE GUARD: the stash really does schedule the counter axis and only it.
        let stash = mixed_state
            .pending_unbounded_materialization
            .get(&p0)
            .expect("the fixture registered a stash")
            .clone();
        assert_eq!(
            mixed_state
                .scheduled_collapse_axes(&stash)
                .into_iter()
                .collect::<Vec<_>>(),
            vec![counter_axis],
            "premise: the stash schedules exactly the counter axis; if this drifts the Mixed \
             assertion below measures something else"
        );

        let counters_rows: Vec<UnboundedFamilyView> = derive_views(&mixed_state, None)
            .unbounded_families
            .into_iter()
            .filter(|f| f.player == p0 && f.family == UnboundedFamily::Counters)
            .collect();
        assert_eq!(
            counters_rows.len(),
            1,
            "Counter(..) and Poison(..) share ONE counters badge, got {counters_rows:?}"
        );
        assert_eq!(
            counters_rows[0].state,
            FamilyCollapseState::Mixed,
            "one scheduled axis and one unscheduled axis in the same family is Mixed — an OR fold \
             would say Scheduled(Conditional), an AND fold Unscheduled; got {:?}",
            counters_rows[0]
        );

        // MATCHED POSITIVE: drop the unscheduled sibling and the same family does report a
        // schedule, so the assertion above is not satisfied by a badge that never schedules.
        let (positive_state, _) = seed(false);
        let scheduled_rows: Vec<UnboundedFamilyView> = derive_views(&positive_state, None)
            .unbounded_families
            .into_iter()
            .filter(|f| f.player == p0 && f.family == UnboundedFamily::Counters)
            .collect();
        assert_eq!(
            scheduled_rows.len(),
            1,
            "matched positive: one counters badge, got {scheduled_rows:?}"
        );
        assert_eq!(
            scheduled_rows[0].state,
            FamilyCollapseState::Scheduled(CollapseCertainty::Conditional),
            "matched positive: with no unscheduled sibling the counters family IS scheduled, and \
             a batched Counters collapse is Conditional; got {:?}",
            scheduled_rows[0]
        );
    }

    /// M1-c (migrated from `PlayerHud.designations.test.tsx`, where the frontend used to do this
    /// fold): two distinct `Mana(_)` axes collapse to ONE `mana` badge, and it is `Unscheduled` —
    /// `scheduled_display_axes` excludes `Mana(_)` because the pool is spendable throughout the
    /// window, so no `N` bounds it. Migrating the test IS the evidence the fold moved into the
    /// engine.
    ///
    /// MUTATION: key the accumulator by `axis` instead of `family` ⇒ two mana rows ⇒ RED.
    /// MUTATION: drop the `Mana(_)` exclusion in `scheduled_display_axes` ⇒ `Mixed` ⇒ RED.
    #[test]
    fn two_mana_axes_fold_to_one_family_row() {
        use crate::types::game_state::PersistentAxisMaterialization;
        use crate::types::mana::ManaType;

        let p0 = PlayerId(0);
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        state.mark_unbounded_loop(
            p0,
            &[
                ResourceAxis::Mana(ManaType::Green),
                ResourceAxis::Mana(ManaType::White),
            ],
        );
        // A DriveSequence naming the whole unbounded set — the one stash kind that would schedule
        // a mana axis if the exclusion were dropped.
        state.register_pending_materialization(
            p0,
            PersistentAxisMaterialization::DriveSequence {
                sequence: vec![],
                collapsed_axes: vec![
                    ResourceAxis::Mana(ManaType::Green),
                    ResourceAxis::Mana(ManaType::White),
                ],
            },
        );

        let views = derive_views(&state, None);
        assert_eq!(
            views.unbounded_resources.len(),
            2,
            "reach: both mana axes must project rows, got {:?}",
            views.unbounded_resources
        );
        assert_eq!(
            views.unbounded_families,
            vec![UnboundedFamilyView {
                player: p0,
                family: UnboundedFamily::Mana,
                state: FamilyCollapseState::Unscheduled,
            }],
            "two mana axes are ONE mana badge, and mana is never scheduled — the pool stays \
             spendable for the whole window"
        );
    }

    /// F4: the cross-language family-grouping guard. Emits every `ResourceAxis` TAG paired with
    /// the family `family_of` puts it in, as the golden the client's
    /// `Record<ResourceAxisTag, UnboundedFamily>` is checked against. Without it the offer modal
    /// and the HUD badge could group the same axis differently in the two languages.
    ///
    /// WRITE-FIRST, per the golden-emitter ordering rule: both assertions sit BELOW the write, so
    /// a mutation that reds one can still regenerate the golden and let the client-side half of
    /// the same probe run. An assert panic aborts the test.
    ///
    /// COMPLETENESS is enforced by three independent breaks, with no scaffolding: an 18th
    /// `ResourceAxis` build-breaks `family_of` (no wildcard); it build-breaks the client's
    /// `Record<ResourceAxisTag, …>`; and until `AXIS_REPRESENTATIVES` is extended the golden
    /// carries 17 keys while TS carries 18, so the seam test's key-set equality reds.
    #[test]
    fn family_tag_table_matches_the_client_golden() {
        use crate::analysis::resource::{CounterClass, ObjectClass, TriggerKind};
        use crate::types::mana::ManaType;

        // One representative per tag. Payloads are arbitrary because `family_of` is
        // payload-independent by construction, exactly as the client's Record keys on the tag.
        const AXIS_REPRESENTATIVES: [ResourceAxis; 17] = [
            ResourceAxis::Mana(ManaType::Colorless),
            ResourceAxis::Life(PlayerId(0)),
            ResourceAxis::DamageDealt(PlayerId(0)),
            ResourceAxis::LibraryDelta(PlayerId(0)),
            ResourceAxis::Counter(CounterClass::Plus1Plus1, ObjectClass::Creature),
            ResourceAxis::Trigger(TriggerKind::Proliferate),
            ResourceAxis::TokensCreated,
            ResourceAxis::CardsDrawn,
            ResourceAxis::Casts,
            ResourceAxis::LandfallTriggers,
            ResourceAxis::CombatPhases,
            ResourceAxis::ExtraTurns,
            ResourceAxis::DeathTriggers,
            ResourceAxis::EtbTriggers,
            ResourceAxis::LtbTriggers,
            ResourceAxis::SacTriggers,
            ResourceAxis::Poison(PlayerId(0)),
        ];

        // Tag extraction uses the SAME rule as the client's `axisTag`: externally-tagged serde
        // gives a bare string for unit variants and a single-key object for data variants.
        let serde_tag = |axis: &ResourceAxis| -> String {
            match serde_json::to_value(axis).expect("ResourceAxis serializes") {
                serde_json::Value::String(s) => s,
                serde_json::Value::Object(map) => map
                    .keys()
                    .next()
                    .cloned()
                    .expect("a data variant serializes to exactly one key"),
                other => panic!("unexpected ResourceAxis encoding: {other}"),
            }
        };

        let table: BTreeMap<String, UnboundedFamily> = AXIS_REPRESENTATIVES
            .iter()
            .map(|a| (serde_tag(a), family_of(*a)))
            .collect();

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../client/src/test/fixtures/unbounded-family-tags.json"
        );
        if std::env::var_os("UPDATE_WIRE_GOLDEN").is_some() {
            std::fs::create_dir_all(
                std::path::Path::new(path)
                    .parent()
                    .expect("golden has a parent"),
            )
            .expect("create the client wire-golden directory");
            std::fs::write(
                path,
                format!("{}\n", serde_json::to_string_pretty(&table).unwrap()),
            )
            .expect("write the family-tag golden");
        }

        assert_eq!(
            table.len(),
            17,
            "one entry per ResourceAxis tag — a duplicate representative silently shrinks this"
        );
        let committed: BTreeMap<String, UnboundedFamily> =
            serde_json::from_str(&std::fs::read_to_string(path).expect("committed family golden"))
                .expect("the family golden parses as tag -> UnboundedFamily");
        assert_eq!(
            table, committed,
            "the client's family-tag golden drifted — re-run with UPDATE_WIRE_GOLDEN=1"
        );
    }

    /// M1-d (migrated from the frontend's U6): `FamilyCollapseState::merge` is a genuine join —
    /// commutative, associative, idempotent. That is what makes the accumulation order of the row
    /// loop irrelevant; the frontend fold it replaces documented a last-wins order hazard.
    ///
    /// MUTATION: make `merge` last-wins (`|_, other| other`) ⇒ commutativity reds in exactly one
    /// order, e.g. `Unscheduled ⊔ Mixed` vs `Mixed ⊔ Unscheduled`.
    #[test]
    fn family_collapse_state_merge_is_a_join() {
        let all = [
            FamilyCollapseState::Unscheduled,
            FamilyCollapseState::Mixed,
            FamilyCollapseState::Scheduled(CollapseCertainty::Committed),
            FamilyCollapseState::Scheduled(CollapseCertainty::Conditional),
        ];
        for x in all {
            assert_eq!(x.merge(x), x, "idempotent: {x:?}");
            for y in all {
                assert_eq!(
                    x.merge(y),
                    y.merge(x),
                    "commutative: {x:?} ⊔ {y:?} must equal {y:?} ⊔ {x:?}"
                );
                for z in all {
                    assert_eq!(
                        x.merge(y).merge(z),
                        x.merge(y.merge(z)),
                        "associative: ({x:?} ⊔ {y:?}) ⊔ {z:?} must equal {x:?} ⊔ ({y:?} ⊔ {z:?})"
                    );
                }
            }
        }
        // The lattice's load-bearing shape, stated so a reader need not re-derive it.
        assert_eq!(
            FamilyCollapseState::Scheduled(CollapseCertainty::Committed).merge(
                FamilyCollapseState::Scheduled(CollapseCertainty::Conditional)
            ),
            FamilyCollapseState::Scheduled(CollapseCertainty::Conditional),
            "two schedules keep the WEAKER certainty"
        );
        assert_eq!(
            FamilyCollapseState::Scheduled(CollapseCertainty::Committed)
                .merge(FamilyCollapseState::Unscheduled),
            FamilyCollapseState::Mixed,
            "a schedule beside an unscheduled sibling is Mixed, never Scheduled"
        );
    }

    /// DESIGN STEP 4 (∞-pile projection): `GameState::unbounded_loop_pile` projects into
    /// `DerivedViews::unbounded_pile`, filtered to objects still on the battlefield — a
    /// registered member that has since left is dropped (stale).
    ///
    /// REVERT-PROBE: delete the `derive_views` projection loop → `unbounded_pile` is empty
    /// → the two positive `contains` assertions fail.
    #[test]
    fn derive_views_projects_unbounded_pile() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Saproling".into(),
            Zone::Battlefield,
        );
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Saproling".into(),
            Zone::Battlefield,
        );
        // A registered id that is NOT on the battlefield (already left) — must be dropped.
        let gone = ObjectId(9999);
        state.register_unbounded_loop_pile(PlayerId(0), BTreeSet::from([a, b, gone]));

        let views = derive_views(&state, None);
        assert!(
            views.unbounded_pile.contains(&a),
            "on-battlefield pile member projects"
        );
        assert!(
            views.unbounded_pile.contains(&b),
            "on-battlefield pile member projects"
        );
        assert!(
            !views.unbounded_pile.contains(&gone),
            "a member no longer on the battlefield is dropped (stale)"
        );

        let empty = GameState::new(FormatConfig::standard(), 2, 42);
        assert!(
            derive_views(&empty, None).unbounded_pile.is_empty(),
            "no object-growth loop → no pile (field omitted)"
        );
    }

    /// DESIGN STEP 4 (serde wire shape + omission): `unbounded_pile` serializes through
    /// `ClientGameStateRef` → JSON → `ClientGameState` as an ObjectId array, and the empty
    /// case omits the key entirely (`skip_serializing_if = "Vec::is_empty"`).
    #[test]
    fn unbounded_pile_round_trip_through_wire() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Saproling".into(),
            Zone::Battlefield,
        );
        state.register_unbounded_loop_pile(PlayerId(0), BTreeSet::from([a]));

        let json =
            serde_json::to_string(&ClientGameStateRef::wrap(&state, None)).expect("serialize");
        assert!(
            json.contains("unbounded_pile"),
            "pile key present when non-empty"
        );

        let round: ClientGameState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            round.derived.unbounded_pile,
            vec![a],
            "the pile ObjectId survives the wire round-trip"
        );

        // Empty case: skip_serializing_if omits the key entirely.
        let empty = GameState::new(FormatConfig::standard(), 2, 42);
        let empty_json = serde_json::to_string(&ClientGameStateRef::wrap(&empty, None))
            .expect("serialize empty");
        assert!(
            !empty_json.contains("unbounded_pile"),
            "empty pile → key omitted"
        );
    }

    /// PR-6 tests 4+5 (serde wire shape + round-trip): the `unbounded_resources`
    /// projection serializes through `ClientGameStateRef` → JSON → `ClientGameState`
    /// with the externally-tagged `ResourceAxis` shapes the TS mirror depends on
    /// (unit → bare string, single-data → `{"Mana":"Red"}`, PlayerId transparent
    /// `{"Life":1}` / `{"Poison":1}`, tuple → `{"Counter":["Energy","Player"]}`), and the empty case
    /// omits the key. Exercises the `Serialize`/`Deserialize` derives added to
    /// `ResourceAxis`/`CounterClass`/`ObjectClass`/`TriggerKind`.
    ///
    /// REVERT-PROBE: remove `Deserialize` from `ResourceAxis` → this test fails to
    /// compile (the wire round-trip can no longer deserialize the axis rows).
    #[test]
    fn unbounded_resources_round_trip_through_wire() {
        use crate::analysis::resource::{CounterClass, ObjectClass, TriggerKind};
        use crate::types::mana::ManaType;

        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        state.mark_unbounded_loop(
            PlayerId(0),
            &[
                ResourceAxis::Mana(ManaType::Red),
                ResourceAxis::Life(PlayerId(1)),
                // Victim-keyed poison axis (PlayerId-transparent wire shape).
                ResourceAxis::Poison(PlayerId(1)),
                // A non-poison Counter keeps the tuple externally-tagged wire shape covered.
                ResourceAxis::Counter(CounterClass::Energy, ObjectClass::Player),
                ResourceAxis::Trigger(TriggerKind::Proliferate),
                ResourceAxis::TokensCreated,
            ],
        );

        let json =
            serde_json::to_string(&ClientGameStateRef::wrap(&state, None)).expect("serialize");
        // The externally-tagged wire shapes the hand-maintained TS union mirrors.
        assert!(json.contains(r#"{"Mana":"Red"}"#), "single-data axis shape");
        assert!(json.contains(r#"{"Life":1}"#), "PlayerId transparent shape");
        assert!(
            json.contains(r#"{"Poison":1}"#),
            "poison PlayerId-transparent wire shape"
        );
        assert!(
            json.contains(r#"{"Counter":["Energy","Player"]}"#),
            "tuple axis shape"
        );
        assert!(
            json.contains(r#""TokensCreated""#),
            "unit axis bare-string shape"
        );

        let round: ClientGameState = serde_json::from_str(&json).expect("deserialize");
        let rows = &round.derived.unbounded_resources;
        assert_eq!(rows.len(), 6, "all six axis rows survive the round-trip");
        // CR 704.5c: the victim-keyed poison axis attributes to the afflicted P1, NOT the
        // controller P0 (the re-key discharge — see attribution_player).
        assert!(rows
            .iter()
            .any(|r| r.player == PlayerId(1) && r.axis == ResourceAxis::Poison(PlayerId(1))));
        // Victim-keyed life axis attributes to P1.
        assert!(rows
            .iter()
            .any(|r| r.player == PlayerId(1) && r.axis == ResourceAxis::Life(PlayerId(1))));

        // Empty case: skip_serializing_if omits the key entirely.
        let empty = GameState::new(FormatConfig::standard(), 2, 42);
        let empty_json = serde_json::to_string(&ClientGameStateRef::wrap(&empty, None))
            .expect("serialize empty");
        assert!(
            !empty_json.contains("unbounded_resources"),
            "empty unbounded resources must omit the wire key"
        );
    }

    #[test]
    fn unique_submitter_projection_uses_search_latch_and_omits_no_actor() {
        use crate::types::ability::SearchSelectionConstraint;
        use crate::types::game_state::{
            ActiveSearchDecisionAuthority, ActiveSearchDecisionControl, WaitingFor,
        };

        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            library_owner: Some(PlayerId(0)),
            cards: Vec::new(),
            count: 0,
            reveal: false,
            up_to: true,
            allows_partial_find: true,
            constraint: SearchSelectionConstraint::None,
            split: None,
        };
        state
            .active_search_decision_controls
            .insert(ActiveSearchDecisionControl {
                searcher: PlayerId(0),
                searched_zone_owner: PlayerId(0),
                authority: ActiveSearchDecisionAuthority::LatchedController {
                    controller: PlayerId(1),
                },
            });
        state.turn_decision_controller = Some(PlayerId(2));

        assert_eq!(
            derive_views(&state, None).unique_authorized_submitter,
            Some(PlayerId(1))
        );

        state.waiting_for = WaitingFor::GameOver { winner: None };
        assert_eq!(derive_views(&state, None).unique_authorized_submitter, None);
    }

    #[test]
    fn filtered_search_views_preserve_latched_submitter_for_every_audience_role() {
        use crate::types::ability::SearchSelectionConstraint;
        use crate::types::game_state::{
            ActiveLibrarySearch, ActiveSearchDecisionAuthority, ActiveSearchDecisionControl,
            WaitingFor,
        };

        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            library_owner: Some(PlayerId(0)),
            cards: Vec::new(),
            count: 0,
            reveal: false,
            up_to: true,
            allows_partial_find: true,
            constraint: SearchSelectionConstraint::None,
            split: None,
        };
        state.active_library_searches.insert(
            ActiveLibrarySearch::try_new(
                PlayerId(0),
                PlayerId(0),
                Some(PlayerId(0)),
                Vec::new(),
                Vec::new(),
            )
            .expect("valid search"),
        );
        state
            .active_search_decision_controls
            .insert(ActiveSearchDecisionControl {
                searcher: PlayerId(0),
                searched_zone_owner: PlayerId(0),
                authority: ActiveSearchDecisionAuthority::LatchedController {
                    controller: PlayerId(1),
                },
            });
        // Live turn control has changed since the search decision was latched.
        state.turn_decision_controller = Some(PlayerId(2));

        for viewer in [PlayerId(0), PlayerId(1), PlayerId(2)] {
            let filtered = crate::game::visibility::filter_state_for_viewer(&state, viewer);
            let filtered_twice =
                crate::game::visibility::filter_state_for_viewer(&filtered, viewer);
            assert_eq!(filtered_twice, filtered, "filtering must be idempotent");
            assert_eq!(
                derive_filtered_views(&state, &filtered, Some(viewer)).unique_authorized_submitter,
                Some(PlayerId(1)),
                "searcher, latched controller, and observer must share one authority projection"
            );
            let mut mutated_filtered = filtered.clone();
            mutated_filtered
                .active_search_decision_controls
                .remove(&PlayerId(0));
            mutated_filtered.turn_decision_controller = Some(PlayerId(2));
            assert_eq!(
                derive_filtered_views(&state, &mutated_filtered, Some(viewer))
                    .unique_authorized_submitter,
                Some(PlayerId(1)),
                "filtered-state mutation must not alter authoritative submission rights"
            );
            let wire = serde_json::to_value(ClientGameStateRef::wrap_filtered(
                &state,
                &filtered,
                Some(viewer),
            ))
            .expect("serialize filtered search view");
            assert_eq!(wire["derived"]["unique_authorized_submitter"], 1);
        }
    }
}
