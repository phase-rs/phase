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
use std::collections::{BTreeMap, HashMap};

use crate::game::ability_utils::flatten_targets_in_chain;
use crate::game::game_object::AttachTarget;
use crate::game::stack::{stack_display_groups, StackDisplayGroup};
use crate::types::ability::{
    GameRestriction, KeywordAction, ProhibitedActivity, RestrictionExpiry, RestrictionPlayerScope,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CastingVariant, GameState, StackEntry, StackEntryKind, StackPaidSnapshot,
};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

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
    pub kind_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ability_description: Option<String>,
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

/// Engine-authored projections used by the display layer. Keep this struct
/// small — every field becomes mandatory payload on every state snapshot
/// the client receives. Add a new field only when the frontend would
/// otherwise have to compute game logic (a CLAUDE.md violation).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedViews {
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
    /// Wrap a borrowed `GameState` with its derived projections.
    /// Invoke AFTER any viewer-side filtering (e.g. `filter_state_for_player`)
    /// so the derived shape reflects what the viewer will actually see.
    pub fn wrap(state: &'a GameState, viewer: Option<PlayerId>) -> Self {
        Self {
            state,
            derived: derive_views(state, viewer),
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
pub fn derive_views(state: &GameState, viewer: Option<PlayerId>) -> DerivedViews {
    let mut views = DerivedViews::default();

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

    // CR 104.2b / 119.7 / 119.8 / 118.3 / 101.2 / 702.50b: aggregate
    // player-affecting conditions so the HUD can render status icons without
    // re-scanning static abilities. Runs in every format (not gated by the
    // Commander short-circuit below).
    views.player_status = player_status_views(state);

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
        RestrictionPlayerScope::TargetedPlayer | RestrictionPlayerScope::ParentTargetedPlayer => {
            Vec::new()
        }
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
        kind_label,
        ability_description,
        targets: stack_entry_targets(state, entry),
        paid: stack_paid_facts(state.stack_paid_facts.get(&entry.id)),
        trigger_context: stack_trigger_context(state, entry),
    }
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
        _ => entry
            .ability()
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
    use crate::types::ability::{Effect, ResolvedAbility, RestrictionExpiry, TargetRef};
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{
        CommanderDamageEntry, StackEntry, StackEntryKind, StackPaidSnapshot, ZoneChangeRecord,
    };
    use crate::types::identifiers::CardId;
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
}
