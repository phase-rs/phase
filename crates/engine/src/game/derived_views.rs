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
use crate::game::game_object::AttachTarget;
use crate::game::stack::{effective_stack_ability, stack_display_groups, StackDisplayGroup};
use crate::types::ability::{
    GameRestriction, KeywordAction, ProhibitedActivity, RestrictionExpiry, RestrictionPlayerScope,
    TargetRef,
};
use crate::types::card::TokenImageRef;
use crate::types::events::GameEvent;
use crate::types::format::GameFormat;
use crate::types::game_state::{
    CastingVariant, GameState, StackEntry, StackEntryKind, StackPaidSnapshot,
};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
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
/// the engine attributes the badge to `player` (the HUD it attaches to). `axis`
/// is the engine-provided identity the frontend formats to a family label — the
/// display layer never decides attribution or which axes are unbounded.
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

    /// CR 732.2a: the `∞` HUD rows — one per (attributed player, pumped axis) of
    /// every unbounded-resource loop in `GameState::unbounded_resources`. The
    /// engine decides both the axis identity and the player attribution
    /// ([`attribution_player`]); the frontend only formats each axis to a display
    /// family. Empty (and omitted) in the dominant case where no loop is active.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unbounded_resources: Vec<UnboundedResourceView>,
}

/// Serialize-only wrapper: the WASM getter passes `&GameState` by reference
/// to avoid an O(n) clone of `state.objects` and other owned collections
/// (GameState is not rpds-backed at the top level). The wire shape is
/// `{ state: <GameState>, derived: <DerivedViews> }`.
#[derive(Debug, Serialize)]
pub struct ClientGameStateRef<'a> {
    pub state: &'a GameState,
    pub derived: DerivedViews,
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

pub fn derive_views(state: &GameState, viewer: Option<PlayerId>) -> DerivedViews {
    let mut views = DerivedViews {
        unique_authorized_submitter: unique_authorized_submitter(state),
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
        if let Some(AttachTarget::Player(host)) = obj.attached_to {
            views
                .auras_attached_to_player
                .entry(host)
                .or_default()
                .push(obj_id);
        }
    }

    // CR 702.188a + 604.1: viewer-scoped web-slinging costs (own hand only → leak-proof).
    if let Some(viewer) = viewer {
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

    views.turn_order = turn_order_views(state);

    // CR 732.2a: project every unbounded-resource loop into per-(player, axis)
    // `∞` HUD rows. Runs in every format (placed BEFORE the Commander
    // short-circuit below) and stays empty (field omitted) when no loop is
    // active — the dominant case. The engine owns attribution
    // (`attribution_player`); the frontend only formats each axis to a family.
    for (&controller, axes) in &state.unbounded_resources {
        for &axis in axes {
            views.unbounded_resources.push(UnboundedResourceView {
                player: attribution_player(axis, controller),
                axis,
            });
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
    views
}

fn unique_authorized_submitter(state: &GameState) -> Option<PlayerId> {
    let mut submitters = crate::game::turn_control::authorized_submitters(state);
    submitters.sort_unstable_by_key(|player| player.0);
    submitters.dedup();
    (submitters.len() == 1).then(|| submitters[0])
}

fn turn_order_views(state: &GameState) -> Vec<TurnOrderSlotView> {
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
        return Vec::new();
    }

    crate::game::turns::projected_turn_order(state, representatives.len())
        .into_iter()
        .enumerate()
        .map(|(index, player)| {
            let slot_index = index as u8;
            TurnOrderSlotView {
                player,
                slot_index,
                turns_from_now: slot_index,
            }
        })
        .collect()
}

/// CR 732.2a: which player's HUD a pumped `axis` belongs to, given the loop's
/// `controller`. Exhaustive by design (no wildcard) — a new `ResourceAxis`
/// variant must make a deliberate attribution choice here, never silently inherit
/// a default.
///
/// A payload-keyed axis names the player it acts on, so the badge follows the
/// payload, NOT permanent control:
/// - CR 704.5a: `Life(p)` — a drain drives an opponent's life down (the win
///   condition is the afflicted player reaching 0 life) and lifegain raises the
///   controller's own; either way the badge belongs on `p`'s HUD.
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
    use crate::game::zones::create_object;
    use crate::types::ability::{
        Effect, ModalChoice, ResolvedAbility, RestrictionExpiry, TargetRef,
    };
    use crate::types::card_type::CoreType;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{
        CommanderDamageEntry, PendingCast, StackEntry, StackEntryKind, StackPaidSnapshot,
        WaitingFor, ZoneChangeRecord,
    };
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaCost;
    use crate::types::phase::Phase;
    use crate::types::statics::ActivationExemption;
    use crate::types::zones::Zone;

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
        state.extra_turns.push(PlayerId(2));
        state.extra_turns.push(PlayerId(0));

        let views = derive_views(&state, None);

        assert_eq!(
            views.turn_order[..2],
            [
                TurnOrderSlotView {
                    player: PlayerId(0),
                    slot_index: 0,
                    turns_from_now: 0,
                },
                TurnOrderSlotView {
                    player: PlayerId(0),
                    slot_index: 1,
                    turns_from_now: 1,
                },
            ],
            "same player can be both NOW and NEXT when an extra turn is queued"
        );

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
            state.add_mana_to_pool(
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
                ability: Some(ability),
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

    /// PR-6 test 3 (projection half): a NON-mana unbounded axis still projects an
    /// `∞` row attributed to its controller; the empty map yields no rows (field
    /// omitted). The mana-vs-non-mana refill gating half lives in
    /// `mana_payment::refill_infinite_mana_gated_on_mana_axis_only`.
    ///
    /// REVERT-PROBE: delete the `derive_views` projection loop → the `TokensCreated`
    /// row is absent → the `contains` assertion fails.
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

        let empty = GameState::new(FormatConfig::standard(), 2, 42);
        assert!(
            derive_views(&empty, None).unbounded_resources.is_empty(),
            "no unbounded loop → no ∞ rows (field omitted)"
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
