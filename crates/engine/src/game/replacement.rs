use indexmap::IndexMap;
use std::collections::HashMap;

use crate::types::ability::{
    AbilityCost, AbilityDefinition, CombatDamageScope, ControllerRef, DamageModification,
    DamageTargetFilter, DamageTargetPlayerScope, Effect, EffectScope, PostReplacementContinuation,
    PreventionAmount, QuantityExpr, QuantityModification, ReplacementCondition,
    ReplacementDefinition, ReplacementMode, ResolvedAbility, ShieldKind, TapStateChange,
    TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;

use super::filter::{
    matches_target_filter, matches_target_filter_on_battlefield_entry,
    matches_target_filter_on_damage_record_source, FilterContext,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingReplacement, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::{StepEndManaAction, UnitDisposition};
use crate::types::player::PlayerId;
use crate::types::proposed_event::{
    CounterMoveStage, CounterPlacement, EtbTapState, ProposedEvent, ReplacementId,
};
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

use super::ability_utils::build_resolved_from_def;
use super::game_object::GameObject;

// CR 122.1c shield-counter effects are intrinsic to counters, not stored
// `ReplacementDefinition`s: ordinary `ShieldKind` definitions expire at cleanup,
// while shield counters persist. Use reserved per-object candidate IDs so the
// existing CR 616 replacement-ordering pipeline can still own choice/application.
const SHIELD_COUNTER_DESTROY_INDEX: usize = usize::MAX;
const SHIELD_COUNTER_DAMAGE_INDEX: usize = usize::MAX - 1;
/// CR 702.89a: Umbra armor — virtual destroy-replacement keyed on the enchanted
/// permanent (the `source` is the would-be-destroyed host, not the Aura). Reserved
/// candidate id so the CR 616 replacement-ordering pipeline owns its application.
const UMBRA_ARMOR_DESTROY_INDEX: usize = usize::MAX - 2;
/// CR 702.150a: Compleated — virtual loyalty-counter replacement keyed on the
/// resolving planeswalker. Compleated is an intrinsic cast-payment replacement,
/// not a battlefield `ReplacementDefinition`, but it must still participate in
/// CR 616 ordering against AddCounter replacements such as Doubling Season.
const COMPLEATED_LOYALTY_INDEX: usize = usize::MAX - 3;
/// CR 614.10 + CR 614.10a: Turn-scoped combat-phase skip (False Peace / Empty
/// City Ruse — "skips all combat phases of their next turn"). The skip effect
/// leaves no battlefield object, so it is a virtual BeginPhase replacement keyed
/// on the affected player (whose `PlayerId` is encoded into the sentinel
/// `source` `ObjectId`). It is armed by `GameState::combat_phase_skip_next_turn`
/// being `Active` for the active player on a combat phase.
const TURN_SCOPED_COMBAT_SKIP_INDEX: usize = usize::MAX - 4;

/// CR 109.4 + CR 108.4a: Cards outside the battlefield/stack have no
/// controller; if an effect asks for a card's controller, use its owner
/// instead. Command-zone emblems keep their controller under CR 109.4c.
pub(crate) fn replacement_source_player(obj: &GameObject) -> PlayerId {
    match obj.zone {
        Zone::Battlefield | Zone::Stack | Zone::Command => obj.controller,
        Zone::Library | Zone::Hand | Zone::Graveyard | Zone::Exile => obj.owner,
    }
}

fn compleated_replacement_id(object_id: ObjectId) -> ReplacementId {
    ReplacementId {
        source: object_id,
        index: COMPLEATED_LOYALTY_INDEX,
    }
}

fn is_compleated_replacement(rid: ReplacementId) -> bool {
    rid.index == COMPLEATED_LOYALTY_INDEX
}

fn umbra_armor_replacement_id(aura_id: ObjectId) -> ReplacementId {
    ReplacementId {
        source: aura_id,
        index: UMBRA_ARMOR_DESTROY_INDEX,
    }
}

fn is_umbra_armor_replacement(rid: ReplacementId) -> bool {
    rid.index == UMBRA_ARMOR_DESTROY_INDEX
}

/// CR 614.10 + CR 614.10a: virtual replacement id for the turn-scoped combat
/// skip. The affected `PlayerId` is encoded into the sentinel `ObjectId` the
/// same way `compleated_replacement_id` carries its host object id.
fn turn_scoped_combat_skip_replacement_id(player: PlayerId) -> ReplacementId {
    ReplacementId {
        source: ObjectId(player.0 as u64),
        index: TURN_SCOPED_COMBAT_SKIP_INDEX,
    }
}

fn is_turn_scoped_combat_skip_replacement(rid: ReplacementId) -> bool {
    rid.index == TURN_SCOPED_COMBAT_SKIP_INDEX
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShieldCounterReplacementKind {
    Destroy,
    Damage,
}

fn shield_counter_replacement_id(
    object_id: ObjectId,
    kind: ShieldCounterReplacementKind,
) -> ReplacementId {
    ReplacementId {
        source: object_id,
        index: match kind {
            ShieldCounterReplacementKind::Destroy => SHIELD_COUNTER_DESTROY_INDEX,
            ShieldCounterReplacementKind::Damage => SHIELD_COUNTER_DAMAGE_INDEX,
        },
    }
}

fn shield_counter_replacement_kind(rid: ReplacementId) -> Option<ShieldCounterReplacementKind> {
    match rid.index {
        SHIELD_COUNTER_DESTROY_INDEX => Some(ShieldCounterReplacementKind::Destroy),
        SHIELD_COUNTER_DAMAGE_INDEX => Some(ShieldCounterReplacementKind::Damage),
        _ => None,
    }
}

pub(crate) fn is_shield_counter_damage_replacement(rid: ReplacementId) -> bool {
    matches!(
        shield_counter_replacement_kind(rid),
        Some(ShieldCounterReplacementKind::Damage)
    )
}

fn object_has_shield_counter(state: &GameState, object_id: ObjectId) -> bool {
    state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.counters.get(&CounterType::Shield))
        .is_some_and(|count| *count > 0)
}

fn compleated_life_paid(state: &GameState, object_id: ObjectId) -> Option<u32> {
    state.objects.get(&object_id).and_then(|obj| {
        (obj.phyrexian_life_paid > 0
            && obj.has_keyword(&crate::types::keywords::Keyword::Compleated))
        .then_some(obj.phyrexian_life_paid)
    })
}

fn is_functioning_umbra_armor_aura(state: &GameState, aura_id: ObjectId) -> bool {
    state.objects.get(&aura_id).is_some_and(|aura| {
        aura.zone == Zone::Battlefield
            && aura.is_phased_in()
            && aura.card_types.subtypes.iter().any(|s| s == "Aura")
            && aura.has_keyword(&crate::types::keywords::Keyword::TotemArmor)
    })
}

/// CR 702.89a: Iterate functioning Umbras (Auras with umbra/totem armor)
/// attached to `object_id`. Each Aura's umbra-armor static replaces destruction
/// of the permanent it enchants, so every attached Umbra is a separate CR 616
/// candidate and the affected permanent's controller chooses which one applies.
fn umbra_armor_attachments(
    state: &GameState,
    object_id: ObjectId,
) -> impl Iterator<Item = ObjectId> + '_ {
    state
        .objects
        .get(&object_id)
        .into_iter()
        .flat_map(|host| host.attachments.iter().copied())
        .filter(|aura_id| is_functioning_umbra_armor_aura(state, *aura_id))
}

/// CR 122.1c: Remove one shield counter from the permanent, emitting
/// `CounterRemoved`. Returns `true` if a shield counter was present and removed
/// (so the caller should treat the destruction/damage as replaced/prevented),
/// `false` otherwise. Mirrors the CR 122.1d stun-counter removal model in
/// `turns.rs`: decrement, drop the map entry at zero, and emit one
/// `CounterRemoved { count: 1 }` event so counter-removal triggers observe it.
pub(crate) fn consume_shield_counter(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> bool {
    let Some(obj) = state.objects.get_mut(&object_id) else {
        return false;
    };
    let Some(entry) = obj.counters.get_mut(&CounterType::Shield) else {
        return false;
    };
    if *entry == 0 {
        return false;
    }
    *entry -= 1;
    if *entry == 0 {
        obj.counters.remove(&CounterType::Shield);
    }
    events.push(GameEvent::CounterRemoved {
        object_id,
        counter_type: CounterType::Shield,
        count: 1,
    });
    true
}

fn apply_compleated_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    rid: ReplacementId,
    events: &mut Vec<GameEvent>,
) -> ProposedEvent {
    let Some(life_paid) = compleated_life_paid(state, rid.source) else {
        return event;
    };
    match event {
        ProposedEvent::AddCounter {
            placement:
                CounterPlacement::Object {
                    actor,
                    object_id,
                    counter_type: CounterType::Loyalty,
                },
            count,
            mut applied,
        } if object_id == rid.source => {
            applied.insert(rid);
            if let Some(obj) = state.objects.get_mut(&rid.source) {
                obj.phyrexian_life_paid = 0;
            }
            events.push(GameEvent::ReplacementApplied {
                source_id: rid.source,
                event_type: ReplacementEvent::AddCounter.to_string(),
            });
            ProposedEvent::AddCounter {
                placement: CounterPlacement::Object {
                    actor,
                    object_id,
                    counter_type: CounterType::Loyalty,
                },
                count: count.saturating_sub(life_paid.saturating_mul(2)),
                applied,
            }
        }
        other => other,
    }
}

/// CR 614.1: Replacement effects modify events as they would occur.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplacementResult {
    Execute(ProposedEvent),
    Prevented,
    NeedsChoice(PlayerId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApplyResult {
    Modified(ProposedEvent),
    Prevented,
}

fn stash_post_replacement_continuation(
    state: &mut GameState,
    continuation: PostReplacementContinuation,
    source: ObjectId,
    event_source: Option<ObjectId>,
    event_target: Option<TargetRef>,
) {
    if state.post_replacement_continuation.is_some() {
        return;
    }
    state.post_replacement_continuation = Some(continuation);
    state.post_replacement_source = Some(source);
    state.post_replacement_event_source = event_source;
    state.post_replacement_event_target = event_target;
}

pub type ReplacementMatcher = fn(&ProposedEvent, ObjectId, &GameState) -> bool;
pub type ReplacementApplier =
    fn(ProposedEvent, ReplacementId, &mut GameState, &mut Vec<GameEvent>) -> ApplyResult;

pub struct ReplacementHandlerEntry {
    pub matcher: ReplacementMatcher,
    pub applier: ReplacementApplier,
}

/// Build a `WaitingFor::ReplacementChoice` from the current `pending_replacement` state.
/// Centralizes candidate count and description extraction so callers don't repeat this logic.
///
/// CR 616.1 + CR 703.4q: For `ProposedEvent::EmptyManaPool` events, descriptions
/// come from `state.pending_step_end_mana_handlers` (sentinel-source path)
/// rather than from each rid's source object's `replacement_definitions`,
/// because step-end mana handlers are not attached to a single object — they
/// are scanned per-player per-phase-transition.
pub fn replacement_choice_waiting_for(player: PlayerId, state: &GameState) -> WaitingFor {
    let (candidate_count, candidate_descriptions) = state
        .pending_replacement
        .as_ref()
        .map(|p| match &p.proposed {
            // CR 703.4q + CR 616.1: Sentinel-source dispatch. Descriptions are
            // read from the per-phase handler list rather than per-object
            // replacement_definitions.
            ProposedEvent::EmptyManaPool { .. } => {
                let descs: Vec<String> = p
                    .candidates
                    .iter()
                    .filter_map(|rid| {
                        state
                            .pending_step_end_mana_handlers
                            .get(rid.index)
                            .map(|entry| entry.description.clone())
                    })
                    .collect();
                (descs.len(), descs)
            }
            _ => {
                let count = if p.is_optional { 2 } else { p.candidates.len() };
                let descs: Vec<String> = if p.is_optional {
                    let (accept_desc, decline_desc) = p
                        .candidates
                        .first()
                        .and_then(|rid| {
                            state
                                .objects
                                .get(&rid.source)
                                .and_then(|obj| obj.replacement_definitions.get(rid.index))
                        })
                        .map(|repl| match &repl.mode {
                            ReplacementMode::MayCost { cost, .. } => {
                                (replacement_cost_description(cost), "Decline".to_string())
                            }
                            // CR 702.136a (Riot) / CR 702.98a (Unleash): label an
                            // Optional replacement's accept branch by the
                            // replacement's own `description` (which names its source
                            // keyword, e.g. "Riot — ..." / "Unleash — ..."), falling
                            // back to the `execute` effect text when there is none.
                            // The decline branch, when it is a distinct outcome
                            // (e.g. Riot's "It gains haste"), is labeled by that
                            // outcome rather than a bare "Decline" — the reported
                            // bug was that declining silently granted haste with no
                            // indication; a decline-less Optional (Unleash) keeps a
                            // plain "Decline".
                            ReplacementMode::Optional { decline } => {
                                let accept = if repl.event
                                    == crate::types::replacements::ReplacementEvent::Draw
                                {
                                    "Accept".to_string()
                                } else {
                                    repl.description
                                        .clone()
                                        .or_else(|| {
                                            repl.execute
                                                .as_ref()
                                                .and_then(|e| e.description.clone())
                                        })
                                        .unwrap_or_else(|| "Accept".to_string())
                                };
                                let decline_label = decline
                                    .as_ref()
                                    .and_then(|d| d.description.clone())
                                    .unwrap_or_else(|| "Decline".to_string());
                                (accept, decline_label)
                            }
                            ReplacementMode::Mandatory => (
                                repl.description
                                    .clone()
                                    .unwrap_or_else(|| "Accept".to_string()),
                                "Decline".to_string(),
                            ),
                        })
                        .unwrap_or_else(|| ("Accept".to_string(), "Decline".to_string()));
                    vec![accept_desc, decline_desc]
                } else {
                    // CR 616.1 / CR 614.1c / CR 614.1d: each candidate gets an
                    // outcome-descriptive label derived from its `execute`
                    // effect, or from its synthetic shield-counter kind.
                    // `map` (not `filter_map`) guarantees the vec is never
                    // shorter than `candidate_count`, so the frontend index
                    // lookup stays aligned.
                    p.candidates
                        .iter()
                        .map(|rid| replacement_choice_label_for_rid(state, *rid))
                        .collect()
                };
                (count, descs)
            }
        })
        .unwrap_or((0, vec![]));

    // Issue #4277 softlock guard: a zero-candidate `ReplacementChoice` is
    // unactionable. `candidate_actions_exact` enumerates `(0..candidate_count)`,
    // so count 0 yields an empty legal-action set, and the frontend
    // `ReplacementModal` returns null on `candidate_count == 0` — the game wedges
    // and `stuck_decision_diagnostic` reports "Waiting for: ReplacementChoice".
    // Every legitimate park flows from `pipeline_loop`, which only returns
    // `NeedsChoice` for an Optional candidate (count 2) or 2+ materially-ordered
    // candidates; reaching count 0 here means an upstream caller re-parked after
    // `continue_replacement` already `.take()`-consumed the record (or an
    // `EmptyManaPool` handler list emptied) — i.e. there is nothing left to
    // choose. Return to a clean priority state so the drain machinery resumes any
    // paused iteration (e.g. a mass simultaneous battlefield entry) instead of
    // softlocking.
    if candidate_count == 0 {
        return WaitingFor::Priority {
            player: state.active_player,
        };
    }

    WaitingFor::ReplacementChoice {
        player,
        candidate_count,
        candidate_descriptions,
    }
}

/// CR 614.12a: Park on the replacement choice for `player`, unless a downstream
/// effect (a Devour as-enters Sacrifice `EffectZoneChoice`) already surfaced its
/// own interactive prompt — then leave it so the pending choice isn't clobbered.
pub fn park_waiting_for(state: &mut GameState, player: PlayerId) {
    if matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }) {
        return;
    }
    state.waiting_for = replacement_choice_waiting_for(player, state);
}

/// CR 614.12a: Human-readable accept-label for a `MayCost` replacement prompt.
/// Returns a complete imperative phrase (the caller no longer prepends "Pay ")
/// so non-mana costs read naturally. Exhaustive — a new `AbilityCost` variant
/// forces a deliberate label decision here.
fn replacement_cost_description(cost: &AbilityCost) -> String {
    match cost {
        AbilityCost::Mana { cost } => format!("Pay {cost:?}"),
        AbilityCost::PayLife { amount } => format!("Pay {amount:?} life"),
        // CR 614.12a: Karoo self-ETB cost lands.
        AbilityCost::Sacrifice(cost) => match &cost.requirement {
            crate::types::ability::SacrificeRequirement::Count { count } => {
                if *count == 1 {
                    "Sacrifice a permanent".to_string()
                } else {
                    format!("Sacrifice {count} permanents")
                }
            }
            crate::types::ability::SacrificeRequirement::Aggregate {
                stat: crate::types::ability::SacrificeAggregateStat::TotalPower,
                comparator,
                value,
            } => {
                format!("Sacrifice creatures with total power {value} ({comparator:?} constraint)")
            }
        },
        AbilityCost::Discard { .. } => "Discard a card".to_string(),
        AbilityCost::Exile {
            count,
            zone,
            filter,
            ..
        } => {
            let zone_str = match zone {
                Some(Zone::Graveyard) => {
                    // CR 406.6: Check if the filter is controller-scoped. When the filter
                    // has controller: None (unrestricted "graveyards"), use "from graveyards".
                    // When controller: Some(ControllerRef::You) ("your graveyard"), use
                    // "from your graveyard".
                    let is_unrestricted = filter.as_ref().is_none_or(|f| {
                        matches!(
                            f,
                            crate::types::ability::TargetFilter::Typed(
                                crate::types::ability::TypedFilter {
                                    controller: None,
                                    ..
                                }
                            )
                        )
                    });
                    if is_unrestricted {
                        "from graveyards"
                    } else {
                        "from your graveyard"
                    }
                }
                Some(Zone::Hand) => "from your hand",
                Some(Zone::Battlefield) => "from the battlefield",
                _ => "",
            };
            if *count == 1 {
                format!("Exile a card {zone_str}")
            } else {
                format!("Exile {count} cards {zone_str}")
            }
        }
        // CR 702.24a: Delegate the label to the base cost so a "for each
        // counter" wrapper inherits its base's prompt phrasing (e.g.,
        // "Pay 1 life" → "Pay 1 life" for the per-counter scaling). The
        // multiplier itself doesn't change the *kind* of cost the prompt
        // describes; the resolved scaled amount is decided in Task 6.
        AbilityCost::PerCounter { base, .. } => replacement_cost_description(base),
        AbilityCost::ManaDynamic { .. }
        | AbilityCost::Tap
        | AbilityCost::Untap
        | AbilityCost::Loyalty { .. }
        | AbilityCost::ExileMaterials { .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::RemoveCounter { .. }
        | AbilityCost::PayEnergy { .. }
        | AbilityCost::PaySpeed { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Unattach
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::Composite { .. }
        | AbilityCost::OneOf { .. }
        | AbilityCost::Waterbend { .. }
        | AbilityCost::NinjutsuFamily { .. }
        | AbilityCost::EffectCost { .. }
        | AbilityCost::Unimplemented { .. } => "Pay cost".to_string(),
    }
}

/// CR 616.1 / CR 614.1c / CR 614.1d: Outcome-descriptive label for one
/// candidate in a competing-replacement (distinct, non-optional) choice.
/// Derived from the replacement's own `execute` effect so the label states
/// the *result* of selecting it, not the source card's Oracle text.
///
/// NOTE: unlike the sibling `replacement_cost_description` (which is a
/// fully-exhaustive `match` on `AbilityCost` with no wildcard, so a new
/// cost variant forces a deliberate decision), this helper is
/// INTENTIONALLY non-exhaustive: only the `EnterTapped`-writing effect
/// class produces a multi-candidate distinct-replacement CR 616.1 choice
/// that benefits from an outcome label. Every other `Effect` falls through
/// the `_ =>` arm to the raw-text fallback by design — do not "fix" this
/// into an exhaustive match.
fn replacement_choice_label(repl: &ReplacementDefinition) -> String {
    let fallback = || {
        repl.description
            .clone()
            .unwrap_or_else(|| "Replacement effect".to_string())
    };
    match &repl.execute {
        // The effect is `Box`-wrapped; deref to match, mirroring
        // `event_modifiers_for_ability` (`&*def.effect`).
        Some(ability) => match &*ability.effect {
            // CR 614.1c / CR 614.1d: a SelfRef tap/untap is exactly the
            // enters-tapped modifier class. The `target: TargetFilter::SelfRef`
            // constraint is load-bearing — a non-SelfRef tap is not an
            // enters-tapped modifier and must fall through to raw text.
            // CR 701.26a: SelfRef single tap → enters tapped.
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            } => "Enters tapped".to_string(),
            // CR 701.26b: SelfRef single untap → enters untapped.
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            } => "Enters untapped".to_string(),
            _ => fallback(),
        },
        None => fallback(),
    }
}

fn replacement_choice_label_for_rid(state: &GameState, rid: ReplacementId) -> String {
    if is_compleated_replacement(rid) {
        return "Compleated: enter with fewer loyalty counters".to_string();
    }
    if is_turn_scoped_combat_skip_replacement(rid) {
        // CR 614.10: mandatory skip — static label, never offered as a choice.
        return "Skip combat phase".to_string();
    }
    if is_umbra_armor_replacement(rid) {
        return state
            .objects
            .get(&rid.source)
            .map(|aura| format!("Umbra armor: destroy {} instead", aura.name))
            .unwrap_or_else(|| "Umbra armor: destroy the Aura instead".to_string());
    }
    match shield_counter_replacement_kind(rid) {
        Some(ShieldCounterReplacementKind::Destroy) => "Remove a shield counter".to_string(),
        Some(ShieldCounterReplacementKind::Damage) => {
            "Prevent damage with shield counter".to_string()
        }
        None => state
            .objects
            .get(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get(rid.index))
            .map(replacement_choice_label)
            .unwrap_or_else(|| "Replacement effect".to_string()),
    }
}

pub(crate) fn replacement_mode_is_optional(mode: &ReplacementMode) -> bool {
    matches!(
        mode,
        ReplacementMode::Optional { .. } | ReplacementMode::MayCost { .. }
    )
}

fn replacement_mode_decline(mode: &ReplacementMode) -> Option<&AbilityDefinition> {
    match mode {
        ReplacementMode::Optional { decline } | ReplacementMode::MayCost { decline, .. } => {
            decline.as_deref()
        }
        ReplacementMode::Mandatory => None,
    }
}

fn replacement_mode_decline_cloned(mode: &ReplacementMode) -> Option<Box<AbilityDefinition>> {
    match mode {
        ReplacementMode::Optional { decline } | ReplacementMode::MayCost { decline, .. } => {
            decline.clone()
        }
        ReplacementMode::Mandatory => None,
    }
}

/// CR 614.12a: outcome of attempting to pay an optional `MayCost` replacement's
/// accept-cost. The accept path applies the replacement only on [`Paid`]; on
/// [`Unpaid`] it falls through to the decline branch (CR 614.12); on
/// [`PausedForChoice`] the payment has set an interactive `WaitingFor` (e.g. a
/// `DiscardChoice`) and the replacement must re-park itself so the post-choice
/// resume can finish any remaining cost before entering the permanent — never
/// let it enter early.
///
/// [`Paid`]: MayCostOutcome::Paid
/// [`Unpaid`]: MayCostOutcome::Unpaid
/// [`PausedForChoice`]: MayCostOutcome::PausedForChoice
#[derive(Debug, Clone, PartialEq, Eq)]
enum MayCostOutcome {
    Paid,
    Unpaid,
    PausedForChoice { remaining_cost: Option<AbilityCost> },
}

fn combine_paused_may_cost(
    paused_remaining: Option<AbilityCost>,
    following_costs: &[AbilityCost],
) -> Option<AbilityCost> {
    let mut costs = Vec::new();
    if let Some(cost) = paused_remaining {
        costs.push(cost);
    }
    costs.extend(following_costs.iter().cloned());
    match costs.len() {
        0 => None,
        1 => costs.into_iter().next(),
        _ => Some(AbilityCost::Composite { costs }),
    }
}

fn pay_replacement_may_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    events: &mut Vec<GameEvent>,
) -> MayCostOutcome {
    if !cost.is_payable(state, player, source_id) {
        return MayCostOutcome::Unpaid;
    }
    let paid = match cost {
        AbilityCost::Mana { cost } => {
            crate::game::casting::pay_unless_cost(state, player, cost, events).is_ok()
        }
        AbilityCost::PayLife { amount } => {
            let amount =
                crate::game::quantity::resolve_quantity(state, amount, player, source_id).max(0);
            let amount = u32::try_from(amount).unwrap_or(0);
            matches!(
                crate::game::life_costs::pay_life_as_cost(state, player, amount, events),
                crate::game::life_costs::PayLifeCostResult::Paid { .. }
            )
        }
        AbilityCost::Composite { costs } => {
            // CR 614.12a: a composite accept-cost pays each sub-cost in order; a
            // mid-composite pause carries the unpaid suffix so the resume
            // completes the rest before the replacement applies.
            for (index, sub_cost) in costs.iter().enumerate() {
                match pay_replacement_may_cost(state, player, source_id, sub_cost, events) {
                    MayCostOutcome::Paid => {}
                    MayCostOutcome::PausedForChoice { remaining_cost } => {
                        return MayCostOutcome::PausedForChoice {
                            remaining_cost: combine_paused_may_cost(
                                remaining_cost,
                                &costs[index + 1..],
                            ),
                        };
                    }
                    MayCostOutcome::Unpaid => return MayCostOutcome::Unpaid,
                }
            }
            true
        }
        // CR 614.12a + CR 118.12 + CR 701.9a: a "discard a [type] card" cost
        // paid as the replacement is applied (Mox Diamond, Chrome Mox-style
        // as-enters discards). This is the chosen-from-hand discard shape, which
        // only has a real payment arm in *resolution* scope — the activation-
        // scope `pay_ability_cost` no-ops it (it expects the interactive
        // `WaitingFor::PayCost`/`DiscardChoice` detour to have run first, which
        // never happens on the replacement accept path). Routing through the
        // resolution authority discards the card(s) for real: when the eligible
        // set exactly fills the requirement the discard auto-pays synchronously
        // (`PaymentOutcome::Paid`); otherwise the authority sets
        // `WaitingFor::DiscardChoice` and returns `Paused`, which surfaces as
        // `PausedForChoice` so the accept path re-parks the replacement and the
        // permanent enters only after the card actually leaves the hand.
        AbilityCost::Discard {
            selection: crate::types::ability::CardSelectionMode::Chosen,
            self_scope: crate::types::ability::DiscardSelfScope::FromHand,
            ..
        } => {
            // The synthesized ability is the payment context for the resolution
            // authority: `pay_ability_cost_for_resolution` reads only its
            // `source_id` and resolves the (here fixed) discard `count` against
            // it. Modeling it as `Effect::PayCost { cost }` keeps the context
            // self-describing without inventing a fake target chain.
            let ability = ResolvedAbility::new(
                crate::types::ability::Effect::PayCost {
                    cost: cost.clone(),
                    scale: None,
                    payer: TargetFilter::Controller,
                },
                Vec::new(),
                source_id,
                player,
            );
            // CR 118.12 + CR 701.9b: when the eligible set exceeds the requirement
            // the resolution authority sets `WaitingFor::DiscardChoice` for the
            // player to pick *which* card(s) to discard. The non-composite discard
            // arm reports `Paid` in that case (the pending choice IS the payment),
            // so the set `waiting_for` — not just the `PaymentOutcome` — signals
            // the interactive pause. Snapshot it to distinguish a synchronous
            // forced/auto discard (`Paid`, no choice) from a paused one.
            let prior_waiting_for = state.waiting_for.clone();
            match crate::game::costs::pay_ability_cost_for_resolution(
                state, player, cost, &ability, events,
            ) {
                Ok(crate::game::costs::PaymentOutcome::Paid) => {
                    if state.waiting_for != prior_waiting_for
                        && matches!(state.waiting_for, WaitingFor::DiscardChoice { .. })
                    {
                        return MayCostOutcome::PausedForChoice {
                            remaining_cost: None,
                        };
                    }
                    true
                }
                Ok(crate::game::costs::PaymentOutcome::Paused { remaining_cost }) => {
                    return MayCostOutcome::PausedForChoice { remaining_cost };
                }
                Ok(crate::game::costs::PaymentOutcome::Failed { .. }) | Err(_) => false,
            }
        }
        // CR 406.6: Non-self exile cost paid as the replacement is applied
        // (The Mimeoplasm's "exile two creature cards from graveyards"). This
        // follows the same pattern as Discard: the resolution authority handles
        // the interactive choice via `WaitingFor::EffectZoneChoice` with is_cost_payment: true.
        AbilityCost::Exile { filter, .. } if !matches!(filter, Some(TargetFilter::SelfRef)) => {
            let ability = ResolvedAbility::new(
                crate::types::ability::Effect::PayCost {
                    cost: cost.clone(),
                    scale: None,
                    payer: TargetFilter::Controller,
                },
                Vec::new(),
                source_id,
                player,
            );
            let prior_waiting_for = state.waiting_for.clone();
            match crate::game::costs::pay_ability_cost_for_resolution(
                state, player, cost, &ability, events,
            ) {
                Ok(crate::game::costs::PaymentOutcome::Paid) => {
                    if state.waiting_for != prior_waiting_for
                        && matches!(
                            state.waiting_for,
                            WaitingFor::EffectZoneChoice {
                                library_position: None,
                                is_cost_payment: true,
                                ..
                            }
                        )
                    {
                        return MayCostOutcome::PausedForChoice {
                            remaining_cost: None,
                        };
                    }
                    true
                }
                Ok(crate::game::costs::PaymentOutcome::Paused { remaining_cost }) => {
                    return MayCostOutcome::PausedForChoice { remaining_cost };
                }
                Ok(crate::game::costs::PaymentOutcome::Failed { .. }) | Err(_) => false,
            }
        }
        _ => crate::game::casting::pay_ability_cost(state, player, source_id, cost, events).is_ok(),
    };
    if paid {
        MayCostOutcome::Paid
    } else {
        MayCostOutcome::Unpaid
    }
}

// --- Stub handler for recognized-but-unimplemented replacement types ---

fn stub_matcher(_event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    false
}

fn stub_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 1. Moved (ZoneChange) ---

fn change_zone_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(
        event,
        ProposedEvent::ZoneChange {
            to: Zone::Battlefield,
            ..
        } | ProposedEvent::CreateToken { .. }
    )
}

fn change_zone_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

fn moved_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::ZoneChange { .. })
}

fn moved_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

fn discard_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Discard { .. })
}

fn discard_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    match event {
        ProposedEvent::Discard {
            object_id, applied, ..
        } => ApplyResult::Modified(ProposedEvent::ZoneChange {
            object_id,
            from: Zone::Hand,
            to: Zone::Graveyard,
            cause: None,
            attach_to: None,
            enter_tapped: EtbTapState::Unspecified,
            enter_with_counters: Vec::new(),
            controller_override: None,
            enter_transformed: false,
            face_down_profile: None,
            applied,
        }),
        other => ApplyResult::Modified(other),
    }
}

// --- 2. DamageDone ---

fn damage_done_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Damage { .. })
}

/// CR 614.1a: Extract the damage modification formula from a replacement definition.
fn damage_modification_for_rid(
    state: &GameState,
    rid: ReplacementId,
) -> Option<DamageModification> {
    // CR 615.3: Pending prevention shields use sentinel ObjectId(0).
    if rid.source == ObjectId(0) {
        return state
            .pending_damage_replacements
            .get(rid.index)?
            .damage_modification
            .clone();
    }
    state
        .objects
        .get(&rid.source)?
        .replacement_definitions
        .get(rid.index)?
        .damage_modification
        .clone()
}

/// Look up the `ShieldKind` of the matched replacement (object-hosted or pending
/// registry), using the same `rid.source == ObjectId(0)` sentinel discriminator
/// as `damage_modification_for_rid`.
fn shield_kind_for_rid(state: &GameState, rid: ReplacementId) -> Option<ShieldKind> {
    if rid.source == ObjectId(0) {
        return state
            .pending_damage_replacements
            .get(rid.index)
            .map(|repl| repl.shield_kind);
    }
    state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index))
        .map(|repl| repl.shield_kind)
}

/// CR 614.9: Read back the captured chosen-object recipient stashed in the
/// matched replacement's `redirect_target` field (set at resolution time for
/// `DamageRedirectTarget::ChosenObjectTarget` — "to target creature").
fn redirect_chosen_object_for_rid(state: &GameState, rid: ReplacementId) -> Option<ObjectId> {
    let repl = if rid.source == ObjectId(0) {
        state.pending_damage_replacements.get(rid.index)
    } else {
        state
            .objects
            .get(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get(rid.index))
    };
    match repl.and_then(|r| r.redirect_target.as_ref()) {
        Some(TargetFilter::SpecificObject { id }) => Some(*id),
        _ => None,
    }
}

/// CR 614.1a: Apply damage modification or prevention from the replacement definition.
fn damage_done_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> ApplyResult {
    // Branch 1: Damage modification (Double, Triple, Plus, Minus)
    if let Some(modification) = damage_modification_for_rid(state, rid) {
        if let ProposedEvent::Damage {
            source_id,
            target,
            amount,
            is_combat,
            applied,
        } = event
        {
            let new_amount = match modification {
                DamageModification::Double => amount.saturating_mul(2),
                DamageModification::Triple => amount.saturating_mul(3),
                // CR 614.1a + CR 120 + CR 107.1b: additive damage modification.
                // The added magnitude is a game quantity resolved each time the
                // replacement applies, clamped >= 0 (CR 107.1b). A `Fixed` value
                // (Torbran, Artist's Talent, Rankle and Torbran, I Call for
                // Slaughter) needs no source object; a `Ref` (Hawkeye's "~'s
                // power") reads the object-hosted source's live characteristics
                // via `rid.source`. The controller authority mirrors
                // `damage_modification_for_rid`'s discriminator (CR 109.4):
                // object-hosted replacements derive it from the host's zone,
                // pending-registry replacements (ObjectId(0) sentinel) carry it
                // on the definition. `damage_modification_for_rid` returns an
                // owned clone, so this immutable `resolve_quantity` read does
                // not conflict with the `&mut state` applier (mirrors the
                // `SetToSourcePower` arm).
                DamageModification::Plus { value } => {
                    let controller = if rid.source == ObjectId(0) {
                        state
                            .pending_damage_replacements
                            .get(rid.index)
                            .and_then(|r| r.source_controller)
                            .unwrap_or(PlayerId(0))
                    } else {
                        state
                            .objects
                            .get(&rid.source)
                            .map(replacement_source_player)
                            .unwrap_or(PlayerId(0))
                    };
                    let added = crate::game::quantity::resolve_quantity(
                        state, &value, controller, rid.source,
                    )
                    .max(0) as u32;
                    amount.saturating_add(added)
                }
                // CR 615.1 + CR 614.1a: Saturating subtract. `Minus { value: u32::MAX }`
                // is the continuous prevent-all sentinel — yields 0 for any amount and
                // is not consumed (continuous, not shield-style).
                DamageModification::Minus { value } => amount.saturating_sub(value),
                // CR 614.1a: Conditional — if amount < source's power, set to power.
                // References the replacement source's (rid.source) post-layer power.
                DamageModification::SetToSourcePower => {
                    let source_power = state
                        .objects
                        .get(&rid.source)
                        .and_then(|obj| obj.power)
                        .unwrap_or(0)
                        .max(0) as u32;
                    if amount < source_power {
                        source_power
                    } else {
                        amount
                    }
                }
                // CR 614.1a: Flat override — replace event amount with `value`.
                DamageModification::SetTo { value } => value,
                // CR 614.1a: Life floor — cap damage so target player's life
                // stays at or above `minimum`. For a player target, computes
                // `max(0, life_total - minimum)`. For creature targets, no-ops
                // (non-player targets have no life total to floor).
                DamageModification::LifeFloor { minimum } => {
                    if let TargetRef::Player(pid) = target {
                        let life = state
                            .players
                            .iter()
                            .find(|p| p.id == pid)
                            .map(|p| p.life)
                            .unwrap_or(0);
                        if life < minimum {
                            amount
                        } else {
                            let max_damage = life.saturating_sub(minimum).max(0) as u32;
                            amount.min(max_damage)
                        }
                    } else {
                        amount
                    }
                }
            };
            // CR 614.5: A one-shot effect-created amount replacement (Desperate
            // Gambit) gets a single opportunity, then is consumed. Continuous
            // statics (Furnace of Rath) keep `ShieldKind::None` and are never
            // consumed here — they re-apply to every damage event.
            if let Some(ShieldKind::DamageReplacementOneShot) = shield_kind_for_rid(state, rid) {
                consume_prevention_shield(state, rid, None);
            }
            return ApplyResult::Modified(ProposedEvent::Damage {
                source_id,
                target,
                amount: new_amount,
                is_combat,
                applied,
            });
        }
        return ApplyResult::Modified(event);
    }

    // Branch 1b: CR 614.9 — one-shot redirection shield. Whole-event
    // redirections replace the damage event's recipient; amount-capped
    // redirections split the event, route the redirected portion through the
    // same replacement/damage application path, and leave any remainder on the
    // original recipient.
    if let Some(ShieldKind::Redirection {
        recipient,
        amount: redirect_amount,
    }) = shield_kind_for_rid(state, rid)
    {
        if let ProposedEvent::Damage {
            source_id,
            target,
            amount: damage_amount,
            is_combat,
            applied,
        } = event
        {
            // CR 614.7a: A source that would deal 0 damage deals no damage at
            // all — there is no damage event to redirect. Pass through and do
            // not consume the shield (no opportunity was spent).
            if damage_amount == 0 {
                return ApplyResult::Modified(ProposedEvent::Damage {
                    source_id,
                    target,
                    amount: damage_amount,
                    is_combat,
                    applied,
                });
            }

            let chosen = redirect_chosen_object_for_rid(state, rid);
            let new_recipient =
                super::effects::create_damage_replacement::resolve_redirect_recipient(
                    state, recipient, rid.source, chosen,
                )
                .filter(|new_target| {
                    super::effects::create_damage_replacement::redirect_recipient_is_legal(
                        state, new_target,
                    )
                });

            match redirect_amount {
                PreventionAmount::All => {
                    // CR 614.5: The one-shot opportunity is spent on this event
                    // whether or not the redirection succeeds — consume the
                    // shield in both the success and the "does nothing" (illegal
                    // recipient per CR 614.9) outcomes.
                    consume_prevention_shield(state, rid, None);

                    // CR 614.9: A legal recipient takes the damage instead; an
                    // illegal one (left the battlefield, no longer a
                    // battle/creature/planeswalker, or a player who left the
                    // game) makes the redirection do nothing, so the damage
                    // stays on the original recipient.
                    return ApplyResult::Modified(ProposedEvent::Damage {
                        source_id,
                        target: new_recipient.unwrap_or(target),
                        amount: damage_amount,
                        is_combat,
                        applied,
                    });
                }
                PreventionAmount::AllBut(_) => {
                    // CR 615.1a vs CR 614.9: `AllBut` is exclusively a *prevention*
                    // amount ("prevent all but N damage", Temple Altisaur) and is
                    // never produced for a redirection shield — `redirection_shield`
                    // defaults a missing amount to `PreventionAmount::All` and every
                    // other redirect constructor uses `PreventionAmount::Next`.
                    // Inventing a partial-redirect rule here would violate CR 614.9
                    // (an illegal recipient must make the redirection do nothing
                    // rather than silently drop the excess), so this state is
                    // treated as impossible rather than guessed at.
                    unreachable!(
                        "PreventionAmount::AllBut is never assigned to a ShieldKind::Redirection"
                    )
                }
                PreventionAmount::Next(n) => {
                    let redirected_amount = damage_amount.min(n);
                    let remaining_amount = damage_amount.saturating_sub(redirected_amount);
                    if redirected_amount == n {
                        consume_prevention_shield(state, rid, None);
                    } else {
                        update_redirection_shield(
                            state,
                            rid,
                            recipient,
                            PreventionAmount::Next(n - redirected_amount),
                        );
                    }

                    if let Some(new_target) = new_recipient.filter(|_| redirected_amount > 0) {
                        let redirected_event = ProposedEvent::Damage {
                            source_id,
                            target: new_target,
                            amount: redirected_amount,
                            is_combat,
                            applied: applied.clone(),
                        };
                        match replace_event(state, redirected_event, events) {
                            ReplacementResult::Execute(event) => {
                                let ctx = super::effects::deal_damage::DamageContext::from_source(
                                    state, source_id,
                                )
                                .unwrap_or_else(|| {
                                    let controller = state
                                        .objects
                                        .get(&source_id)
                                        .map(|obj| obj.controller)
                                        .unwrap_or(PlayerId(0));
                                    super::effects::deal_damage::DamageContext::fallback(
                                        source_id, controller,
                                    )
                                });
                                let _ = super::effects::deal_damage::apply_damage_after_replacement(
                                    state, &ctx, event, is_combat, events,
                                );
                            }
                            ReplacementResult::Prevented => {}
                            ReplacementResult::NeedsChoice(_) => {
                                state.pending_replacement = None;
                            }
                        }
                    } else {
                        return ApplyResult::Modified(ProposedEvent::Damage {
                            source_id,
                            target,
                            amount: damage_amount,
                            is_combat,
                            applied,
                        });
                    }

                    if remaining_amount == 0 {
                        return ApplyResult::Prevented;
                    }
                    return ApplyResult::Modified(ProposedEvent::Damage {
                        source_id,
                        target,
                        amount: remaining_amount,
                        is_combat,
                        applied,
                    });
                }
            }
        }
        return ApplyResult::Modified(event);
    }

    // Branch 2: CR 615 — Prevention shield
    // Look up shield from either object replacement_definitions or pending_damage_replacements.
    let shield_kind = if rid.source == ObjectId(0) {
        state
            .pending_damage_replacements
            .get(rid.index)
            .map(|repl| repl.shield_kind)
    } else {
        state
            .objects
            .get(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get(rid.index))
            .map(|repl| repl.shield_kind)
    };

    if let Some(ShieldKind::Prevention { amount }) = shield_kind {
        if let ProposedEvent::Damage {
            source_id,
            target,
            amount: dmg,
            is_combat,
            applied,
        } = event
        {
            let prevented_amount;
            let result;
            // CR 510.2 + CR 615.7: A `Prevention::All` shield encountered during a
            // simultaneous combat-damage batch defers its prevented-amount
            // bookkeeping to the post-batch aggregate. While the batch tally is
            // active, this branch accumulates per-shield and the combat resolver
            // emits a single `DamagePrevented` + fires the rider once for the
            // whole batch. `Prevention::Next(N)` keeps the per-event path.
            let mut accumulated_in_batch = false;

            match amount {
                PreventionAmount::All => {
                    // CR 615.1a: "Prevent all damage" is a duration-bound
                    // unbounded shield, not a depletion shield — only
                    // `PreventionAmount::Next(N)` is exhausted by use (CR 615.7).
                    // The shield's lifetime is governed entirely by its `expiry`
                    // (for resolution-time / "this turn" shields, cleanup at EOT
                    // per CR 514.2; for static-attached shields like Phyrexian
                    // Hydra / Pariah, the host permanent leaving the battlefield).
                    // Marking the shield consumed here would limit a Gatta and
                    // Luzzu / Pariah / Phyrexian Hydra shield to a single damage
                    // event in the turn — wrong for the whole "all damage"
                    // family. Leave the shield active so subsequent damage
                    // events in the same turn re-fire the prevention.
                    prevented_amount = dmg;
                    result = ApplyResult::Prevented;
                    // CR 510.2 + CR 615.7: In a combat-damage batch, route the
                    // prevented amount into the per-shield aggregate keyed by
                    // `rid`. The single rider firing happens post-batch in
                    // `combat_damage.rs` against the summed total.
                    if let Some(tally) = state.combat_prevention_tally.as_mut() {
                        *tally.entry(rid).or_insert(0) += prevented_amount as i32;
                        accumulated_in_batch = true;
                    }
                }
                PreventionAmount::AllBut(keep) => {
                    // CR 615.1a + CR 615.6: "Prevent all but N damage" is a
                    // continuous prevention shield like `All`, but only the
                    // excess above `keep` is prevented; the first `keep` points
                    // of each damage event are still dealt. Like `All`, it is
                    // duration-bound (not depletion-based per CR 615.7), so the
                    // shield is never consumed here and re-fires for every damage
                    // event within its lifetime.
                    let remaining_damage = dmg.min(keep);
                    prevented_amount = dmg.saturating_sub(remaining_damage);
                    if prevented_amount == 0 {
                        result = ApplyResult::Modified(ProposedEvent::Damage {
                            source_id,
                            target: target.clone(),
                            amount: dmg,
                            is_combat,
                            applied,
                        });
                    } else {
                        result = ApplyResult::Modified(ProposedEvent::Damage {
                            source_id,
                            target: target.clone(),
                            amount: remaining_damage,
                            is_combat,
                            applied,
                        });
                    }
                }
                PreventionAmount::Next(n) => {
                    // CR 615.7: Each 1 damage prevented reduces the remaining shield by 1.
                    if dmg <= n {
                        // All damage absorbed — shield may have remaining capacity
                        prevented_amount = dmg;
                        let remaining = n - dmg;
                        if remaining == 0 {
                            consume_prevention_shield(state, rid, None);
                        } else {
                            consume_prevention_shield(
                                state,
                                rid,
                                Some(PreventionAmount::Next(remaining)),
                            );
                        }
                        result = ApplyResult::Prevented;
                    } else {
                        // Damage exceeds shield — reduce damage, consume shield
                        prevented_amount = n;
                        let remaining_damage = dmg - n;
                        consume_prevention_shield(state, rid, None);
                        result = ApplyResult::Modified(ProposedEvent::Damage {
                            source_id,
                            target: target.clone(),
                            amount: remaining_damage,
                            is_combat,
                            applied,
                        });
                    }
                }
            }

            // Emit DamagePrevented event for "when damage is prevented" triggers.
            // CR 510.2 + CR 615.13: When this prevention was accumulated into the
            // combat-damage batch tally, the single `DamagePrevented` event and
            // `last_effect_count` stamp are deferred to the post-batch step in
            // `combat_damage.rs` — emitting them per-source here would fragment
            // the rider's `EventContextAmount` across attackers.
            if prevented_amount > 0 && !accumulated_in_batch {
                events.push(GameEvent::DamagePrevented {
                    source_id,
                    target,
                    amount: prevented_amount,
                });
                // CR 615.5: Stash the prevented amount as the chain's last effect
                // count so a post-replacement follow-up effect (e.g. Phyrexian
                // Hydra's "Put a -1/-1 counter on ~ for each 1 damage prevented
                // this way") can resolve `QuantityRef::EventContextAmount`
                // against the prevented amount. The follow-up runs outside the
                // trigger-resolution window, so `current_trigger_event` is None
                // and `last_effect_count` is the documented fallback slot
                // (see `quantity.rs` resolver).
                state.last_effect_count = Some(prevented_amount as i32);
            }

            return result;
        }
    }

    // No modification and no prevention shield — pass through
    ApplyResult::Modified(event)
}

/// CR 614.5: Mark a one-shot replacement as consumed after it successfully applies.
fn mark_replacement_consumed(state: &mut GameState, rid: ReplacementId) {
    let repl = if rid.source == ObjectId(0) {
        state.pending_damage_replacements.get_mut(rid.index)
    } else {
        state
            .objects
            .get_mut(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get_mut(rid.index))
    };
    if let Some(repl) = repl {
        repl.is_consumed = true;
    }
}

/// Consume or update a prevention shield on either an object or the game-state registry.
/// If `new_amount` is `None`, marks the shield as consumed.
/// If `new_amount` is `Some(amount)`, updates the remaining shield capacity.
fn consume_prevention_shield(
    state: &mut GameState,
    rid: ReplacementId,
    new_amount: Option<PreventionAmount>,
) {
    let repl = if rid.source == ObjectId(0) {
        state.pending_damage_replacements.get_mut(rid.index)
    } else {
        state
            .objects
            .get_mut(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get_mut(rid.index))
    };

    if let Some(repl) = repl {
        match new_amount {
            None => repl.is_consumed = true,
            Some(amt) => repl.shield_kind = ShieldKind::Prevention { amount: amt },
        }
    }
}

fn update_redirection_shield(
    state: &mut GameState,
    rid: ReplacementId,
    recipient: crate::types::ability::DamageRedirectTarget,
    amount: PreventionAmount,
) {
    let repl = if rid.source == ObjectId(0) {
        state.pending_damage_replacements.get_mut(rid.index)
    } else {
        state
            .objects
            .get_mut(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get_mut(rid.index))
    };

    if let Some(repl) = repl {
        repl.shield_kind = ShieldKind::Redirection { recipient, amount };
    }
}

// --- 3. Destroy ---

fn destroy_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Destroy { .. })
}

/// CR 701.19c: Returns true if `object_id` is currently marked with an active
/// `StaticMode::CantBeRegenerated` static. The standalone "[creature] can't be
/// regenerated this turn" effect (Hurr Jackal, Furnace Brood, Lim-Dûl's Cohort)
/// grants this mode onto the affected creature's `static_definitions` via a
/// transient until-end-of-turn continuous effect (CR 514.2 auto-expiry at
/// cleanup), so the mark is observed directly on the object through the
/// CR-gated `active_static_definitions` iterator.
fn object_has_active_cant_be_regenerated(state: &GameState, object_id: ObjectId) -> bool {
    state.objects.get(&object_id).is_some_and(|obj| {
        super::functioning_abilities::active_static_definitions(state, obj).any(|def| {
            matches!(
                def.mode,
                crate::types::statics::StaticMode::CantBeRegenerated
            )
        })
    })
}

/// CR 701.19: Regeneration shield applier for Destroy events.
/// If the replacement definition is a regeneration shield and the destruction allows
/// regeneration, removes damage, taps the permanent, removes it from combat,
/// consumes the shield, and prevents the destruction.
fn destroy_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> ApplyResult {
    // Check if this replacement is a regeneration shield
    let is_regen = state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index))
        .is_some_and(|repl| {
            matches!(
                repl.shield_kind,
                crate::types::ability::ShieldKind::Regeneration
            )
        });

    if !is_regen {
        return ApplyResult::Modified(event);
    }

    // CR 701.19c: Regeneration shields are not applied when the destruction
    // forbids regeneration. Two sources of this prohibition:
    //   1. The inline "Destroy X. It can't be regenerated." one-shot rides on the
    //      event's `cant_regenerate: true` flag (Effect::Destroy { cant_regenerate }).
    //   2. The standalone "[creature] can't be regenerated this turn" effect marks
    //      the destroy target with an active `StaticMode::CantBeRegenerated` static
    //      (Hurr Jackal, Furnace Brood, Lim-Dûl's Cohort).
    // In both cases the shield is left unconsumed (CR 701.19c: shields are not
    // applied, not destroyed) and destruction proceeds.
    let target_cant_regenerate = match &event {
        ProposedEvent::Destroy {
            object_id,
            cant_regenerate,
            ..
        } => *cant_regenerate || object_has_active_cant_be_regenerated(state, *object_id),
        _ => false,
    };
    if target_cant_regenerate {
        return ApplyResult::Modified(event);
    }

    let ProposedEvent::Destroy { object_id, .. } = &event else {
        return ApplyResult::Modified(event);
    };
    let oid = *object_id;

    // CR 701.19a: Remove all damage marked on it.
    if let Some(obj) = state.objects.get_mut(&oid) {
        obj.damage_marked = 0;
        obj.dealt_deathtouch_damage = false;
        // CR 701.19b: Tap it.
        obj.tapped = true;
    }

    // CR 701.19c: Remove it from combat if it's attacking or blocking.
    super::effects::remove_from_combat::remove_object_from_combat(state, oid);

    // Mark the shield as consumed (one-shot).
    if let Some(obj) = state.objects.get_mut(&rid.source) {
        if let Some(repl) = obj.replacement_definitions.get_mut(rid.index) {
            repl.is_consumed = true;
        }
    }

    events.push(GameEvent::Regenerated { object_id: oid });
    ApplyResult::Prevented
}

// clippy::result_large_err: both arms of this Result carry a `ProposedEvent`
// (the replacement pipeline returns the modified event on success and the
// unmodified event in `ApplyResult::Modified` on the no-op path), so the Err
// size is inherent to the design — boxing one arm of every applier would
// ripple across the whole pipeline. The `ZoneChange` variant is the largest
// `ProposedEvent` shape; see the note on `ProposedEvent::ZoneChange.face_down_profile`.
#[allow(clippy::result_large_err)]
fn apply_shield_counter_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    rid: ReplacementId,
    kind: ShieldCounterReplacementKind,
    events: &mut Vec<GameEvent>,
) -> Result<ProposedEvent, ApplyResult> {
    match (kind, event) {
        (
            ShieldCounterReplacementKind::Destroy,
            ProposedEvent::Destroy {
                object_id,
                source,
                cant_regenerate,
                applied,
            },
        ) if object_id == rid.source => {
            if consume_shield_counter(state, rid.source, events) {
                Err(ApplyResult::Prevented)
            } else {
                Ok(ProposedEvent::Destroy {
                    object_id,
                    source,
                    cant_regenerate,
                    applied,
                })
            }
        }
        (
            ShieldCounterReplacementKind::Damage,
            ProposedEvent::Damage {
                source_id,
                target,
                amount,
                is_combat,
                applied,
            },
        ) if matches!(target, TargetRef::Object(object_id) if object_id == rid.source) => {
            let event = ProposedEvent::Damage {
                source_id,
                target: target.clone(),
                amount,
                is_combat,
                applied,
            };

            // CR 615.12: Damage that can't be prevented is still subject to the
            // prevention effect, but no damage is prevented. The shield counter's
            // additional "remove a counter" effect still happens.
            if is_prevention_disabled(state, &event) {
                consume_shield_counter(state, rid.source, events);
                return Ok(event);
            }

            // CR 510.2 + CR 122.1c: one shield counter prevents all simultaneous
            // combat damage dealt to the permanent in the batch. Defer counter
            // removal until the post-batch aggregation fires exactly once.
            if let Some(tally) = state.combat_prevention_tally.as_mut() {
                *tally.entry(rid).or_insert(0) += amount as i32;
                return Err(ApplyResult::Prevented);
            }

            if consume_shield_counter(state, rid.source, events) {
                events.push(GameEvent::DamagePrevented {
                    source_id,
                    target,
                    amount,
                });
                Err(ApplyResult::Prevented)
            } else {
                Ok(event)
            }
        }
        (_, other) => Ok(other),
    }
}

/// CR 702.89a: Umbra armor — "If enchanted permanent would be destroyed, instead
/// remove all damage marked on it and destroy this Aura." Applied as a virtual
/// destroy-replacement keyed on the host (`rid.source`). Unlike regeneration
/// (CR 701.19), it does NOT tap the permanent or remove it from combat, and the
/// "shield" consumed is the Aura itself, which is destroyed.
#[allow(clippy::result_large_err)]
fn apply_umbra_armor_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    rid: ReplacementId,
    events: &mut Vec<GameEvent>,
) -> Result<ProposedEvent, ApplyResult> {
    let ProposedEvent::Destroy {
        object_id, source, ..
    } = event
    else {
        return Ok(event);
    };
    // The virtual replacement is keyed on the Aura so multiple Umbras on the
    // same host remain distinct CR 616 choices. Re-confirm the chosen Aura is
    // still attached to this host at apply time.
    let umbra_id = rid.source;
    if !umbra_armor_attachments(state, object_id).any(|id| id == umbra_id) {
        return Ok(event);
    }

    // CR 702.89a: remove all damage marked on the enchanted permanent. (No tap and
    // no combat removal — that is regeneration, CR 701.19b/c, not umbra armor.)
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.damage_marked = 0;
        obj.dealt_deathtouch_damage = false;
    }

    // CR 702.89a: destroy this Aura. Routed through the post-replacement destroy so
    // the Aura's leave-the-battlefield triggers fire; it is not a creature, so
    // `cant_regenerate` is irrelevant.
    let _ = crate::game::effects::destroy::apply_destroy_after_replacement(
        state,
        ProposedEvent::Destroy {
            object_id: umbra_id,
            source,
            cant_regenerate: false,
            applied: std::collections::HashSet::new(),
        },
        events,
    );

    // The enchanted permanent's destruction is replaced.
    Err(ApplyResult::Prevented)
}

// --- 4. Draw ---

fn draw_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Draw { count, .. } if *count > 0)
}

fn draw_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    use crate::types::ability::QuantityModification;
    // CR 614.6 + CR 121.6: A `Prevent` draw replacement ("skip that draw
    // instead", Living Conundrum) fully suppresses the draw — the replaced
    // event never happens. Carried as a structured `quantity_modification`
    // (no `execute`), mirroring the lifegain-negation / counter-prevention
    // surface. Checked before the count-substitution path because a prevented
    // draw has no surviving count to scale.
    let prevents = state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index))
        .and_then(|def| def.quantity_modification.as_ref())
        .is_some_and(|m| matches!(m, QuantityModification::Prevent));
    if prevents {
        return ApplyResult::Prevented;
    }
    // CR 614.6 + CR 614.11: Count-modifying replacements (Alhammarret's Archive:
    // `count -> 2 * count`) substitute the count via `draw_replacement_count`.
    // Full-substitution replacements (Jace WinTheGame, Abundance reveal-until)
    // are pre-zeroed in `apply_single_replacement` so the original draw is a
    // no-op (CR 614.6 — the replaced event never happens), and the substitute
    // runs via the `post_replacement_continuation` drain.
    if let Some(new_count) = draw_replacement_count(state, rid, &event) {
        if let ProposedEvent::Draw {
            player_id, applied, ..
        } = event
        {
            return ApplyResult::Modified(ProposedEvent::Draw {
                player_id,
                count: new_count,
                applied,
            });
        }
    }
    ApplyResult::Modified(event)
}

fn draw_replacement_count(
    state: &GameState,
    rid: ReplacementId,
    event: &ProposedEvent,
) -> Option<u32> {
    let ProposedEvent::Draw { count, .. } = event else {
        return None;
    };

    let execute = state
        .objects
        .get(&rid.source)?
        .replacement_definitions
        .get(rid.index)?
        .execute
        .as_deref()?;

    match &*execute.effect {
        Effect::Draw { count: qty, .. } => {
            // CR 121.2 + CR 614.11a: "draw N cards instead" replacements
            // (Teferi's Ageless Insight: Fixed(2)) apply to each card draw
            // in the draw sequence — Brainsurge drawing four becomes eight,
            // not two. Chained riders (Blood Scrivener's life loss, issue
            // #3305) are resolved via the post-replacement continuation after
            // the count-modified draw executes.
            let resolved = match qty {
                QuantityExpr::Fixed { value } => value.saturating_mul(*count as i32),
                _ => resolve_event_replacement_quantity(qty, *count)?,
            };
            Some(resolved.max(0) as u32)
        }
        _ => None,
    }
}

// --- 4b. Scry ---

// CR 614.6: A replacement effect applies only once to a given event. The
// `applied: HashSet<ReplacementId>` carried in the event prevents the
// pipeline from re-entering the same effect on the modified event.
fn scry_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Scry { count, .. } if *count > 0)
}

fn scry_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    let (player_id, count, applied) = match event {
        ProposedEvent::Scry {
            player_id,
            count,
            applied,
        } => (player_id, count, applied),
        other => return ApplyResult::Modified(other),
    };

    let execute = state
        .objects
        .get(&rid.source)
        .and_then(|source| source.replacement_definitions.get(rid.index))
        .and_then(|def| def.execute.as_deref());

    match execute {
        Some(ability) if ability.sub_ability.is_none() => match &*ability.effect {
            Effect::Draw { count: qty, .. } => {
                let new_count = resolve_event_replacement_quantity(qty, count)
                    .map(|resolved| resolved.max(0) as u32)
                    .unwrap_or(count);
                ApplyResult::Modified(ProposedEvent::Draw {
                    player_id,
                    count: new_count,
                    applied,
                })
            }
            Effect::Scry { count: qty, .. } => {
                let new_count = resolve_event_replacement_quantity(qty, count)
                    .map(|resolved| resolved.max(0) as u32)
                    .unwrap_or(count);
                ApplyResult::Modified(ProposedEvent::Scry {
                    player_id,
                    count: new_count,
                    applied,
                })
            }
            _ => ApplyResult::Modified(ProposedEvent::Scry {
                player_id,
                count,
                applied,
            }),
        },
        _ => ApplyResult::Modified(ProposedEvent::Scry {
            player_id,
            count,
            applied,
        }),
    }
}

// --- 4d. Explore (Twists and Turns / Topography Tracker) ---

// CR 701.37a + CR 614.1a: A creature is about to explore. Replacement
// effects can modify the explore action (e.g., add a scry prelude or double explore).
fn explore_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Explore { .. })
}

fn explore_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> ApplyResult {
    let ProposedEvent::Explore { object_id, applied } = event else {
        return ApplyResult::Modified(event);
    };

    let Some(source) = state.objects.get(&rid.source) else {
        return ApplyResult::Modified(ProposedEvent::Explore { object_id, applied });
    };
    let Some(execute) = source
        .replacement_definitions
        .get(rid.index)
        .and_then(|def| def.execute.clone())
    else {
        return ApplyResult::Modified(ProposedEvent::Explore { object_id, applied });
    };

    use crate::game::ability_utils::build_resolved_from_def;
    use crate::types::ability::TargetRef;

    let controller = source.controller;
    let mut current = Some(execute.as_ref());
    while let Some(def) = current {
        match &*def.effect {
            Effect::Scry { .. } => {
                let ability = build_resolved_from_def(def, rid.source, controller);
                let _ = crate::game::effects::scry::resolve(state, &ability, events);
            }
            Effect::Explore => {
                let ability = ResolvedAbility::new(
                    Effect::Explore,
                    vec![TargetRef::Object(object_id)],
                    rid.source,
                    controller,
                );
                let _ = crate::game::effects::explore::resolve_explore_effect(
                    state, &ability, object_id, events,
                );
            }
            _ => {
                let mut ability = build_resolved_from_def(def, rid.source, controller);
                ability.targets = vec![TargetRef::Object(object_id)];
                let _ = crate::game::effects::resolve_ability_chain(state, &ability, events, 1);
            }
        }
        current = def.sub_ability.as_deref();
    }

    ApplyResult::Prevented
}

// --- 4b2. Connive (Leader, Super-Genius) ---

fn connive_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Connive { .. })
}

/// CR 701.50a + CR 614.5 + CR 616.1f: Apply a connive replacement (Leader,
/// Super-Genius — "If a creature you control would connive, instead you draw a
/// card, then that creature connives"). CR 701.50a's replacement reads "you draw
/// a card, THEN that creature connives" — the "then" fixes the printed order, so
/// the connive link runs only after the leading draw completes. Runs the
/// replacement's `execute` chain (the sole production chain is exactly `Draw 1`
/// then `Connive`) and fully replaces the original connive event (`Prevented`).
/// The `Connive` link in the chain RE-ENTERS the replacement pipeline via
/// `propose_connive`, carrying the `applied` set (which already contains this rid
/// — the loop/resume marked it before the applier ran), so the process repeats
/// over the OTHER still-applicable connive replacements (CR 616.1f) while
/// `find_applicable_replacements` excludes this one (CR 614.5) — this replacement
/// cannot self-invoke.
///
/// When the leading draw link itself parks an interactive `ReplacementChoice`
/// (the controller's own draw is replaced), the applier must NOT run the
/// `Connive` link early — that would violate CR 701.50a's printed order and
/// clobber the live draw choice. Instead it defers the remaining `Connive` link
/// (always a single link for this chain) into the DEDICATED
/// `state.pending_connive_reentry` slot (NOT `post_replacement_continuation`, so
/// the shared zone-delivery tail cannot drain it mid-draw) and returns
/// `Prevented`; the post-replacement-choice epilogue
/// (`engine_replacement::handle_replacement_choice`) resumes the connive in order
/// once the parked draw choice resolves. (CR 614.11a — completing a replacement's
/// actions before resuming a draw sequence — is the analogous supporting
/// principle.) This parking path is specific to this one caller; it is not a
/// general mechanism.
fn connive_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> ApplyResult {
    let ProposedEvent::Connive {
        object_id,
        count,
        applied,
    } = event
    else {
        return ApplyResult::Modified(event);
    };

    let Some(source) = state.objects.get(&rid.source) else {
        // CR 614.5: carry the captured `applied` set (already marks this rid) so
        // the fallback survivor event cannot re-apply the same replacement.
        return ApplyResult::Modified(ProposedEvent::Connive {
            object_id,
            count,
            applied,
        });
    };
    let Some(execute) = source
        .replacement_definitions
        .get(rid.index)
        .and_then(|def| def.execute.clone())
    else {
        // CR 614.5: carry the captured `applied` set (already marks this rid) so
        // the fallback survivor event cannot re-apply the same replacement.
        return ApplyResult::Modified(ProposedEvent::Connive {
            object_id,
            count,
            applied,
        });
    };

    use crate::game::ability_utils::build_resolved_from_def;
    use crate::types::ability::TargetRef;

    let controller = source.controller;
    let mut current = Some(execute.as_ref());
    while let Some(def) = current {
        match &*def.effect {
            // CR 701.50a + CR 701.50d: "then that creature connives" runs the
            // chain's OWN connive at its parsed count (plain connive = Fixed(1);
            // connive N = Fixed(N) / dynamic), NOT the replaced event's count.
            // Resolve the def's QuantityExpr against the conniving permanent as
            // the target, mirroring the normal connive resolver
            // (effects/connive.rs).
            //
            // Build the ResolvedAbility from `def` directly (NOT a
            // sub_ability-stripped clone like the `_ =>` arm): resolve_quantity_
            // with_targets reads only ability.effect/targets/controller/source_id
            // and never walks `sub_ability`, so the extra clone is unnecessary
            // here.
            Effect::Connive {
                count: connive_count_expr,
                ..
            } => {
                let mut ability = build_resolved_from_def(def, rid.source, controller);
                ability.targets = vec![TargetRef::Object(object_id)];
                let connive_count = crate::game::quantity::resolve_quantity_with_targets(
                    state,
                    connive_count_expr,
                    &ability,
                )
                .max(0) as u32;
                // CR 616.1f + CR 614.5: re-propose the nested connive through the
                // pipeline so OTHER still-applicable connive replacements get
                // their CR 616.1f repeat. `applied` already contains this rid (the
                // loop/resume marked it before the applier ran), so
                // `find_applicable_replacements` excludes it (CR 614.5) — this
                // replacement cannot self-invoke. The link's OWN parsed count
                // (Fixed(1)/N) still seeds the re-proposed event, preserving the
                // count fix. The chain loop may drive multiple links, so clone the
                // (small) `applied` set per re-entry.
                let _ = crate::game::effects::connive::propose_connive(
                    state,
                    object_id,
                    connive_count,
                    applied.clone(),
                    events,
                );
            }
            // CR 701.50a: "you draw a card" and any other modeled effect in the
            // chain resolve against the replacement source / conniving permanent.
            // Resolve THIS link only — `connive_applier`'s loop drives the chain,
            // so the def's `sub_ability` is stripped before dispatch. Otherwise
            // `resolve_ability_chain` would also walk the `then ... connives`
            // sub-link through the propose path and re-trigger this replacement
            // (infinite recursion; CR 614.5 bars self-invocation).
            _ => {
                let mut single = def.clone();
                single.sub_ability = None;
                let mut ability = build_resolved_from_def(&single, rid.source, controller);
                ability.targets = vec![TargetRef::Object(object_id)];
                let _ = crate::game::effects::resolve_ability_chain(state, &ability, events, 1);

                // CR 701.50a + CR 614.5 + CR 616.1f: if this draw link parked an
                // interactive ReplacementChoice (the controller's own draw is
                // itself replaced) and its successor is the `then ... connives`
                // link, the connive must NOT run now — CR 701.50a's "then" fixes
                // the printed order. Defer the connive into the dedicated
                // `state.pending_connive_reentry` slot (resumed by the
                // post-replacement-choice epilogue once the parked draw choice
                // resolves) and return `Prevented`.
                //
                // The park signal is precise: `draw_through_replacement` parks via
                // `replace_event`'s `NeedsChoice`, which BOTH sets `waiting_for` to
                // a `ReplacementChoice` AND leaves a live `pending_replacement`
                // record. A normally-completed draw (the multi-Leader connive
                // re-entry path) consumes its pending record and leaves
                // `pending_replacement == None`, so this guard does not misfire on
                // a stale non-Priority `waiting_for` left by the surrounding
                // connive-ordering resume. Reached ONLY on the parked-draw +
                // Connive-successor path; every other case advances the loop
                // unchanged.
                if matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. })
                    && state.pending_replacement.is_some()
                {
                    if let Some(next) = def.sub_ability.as_deref() {
                        if let Effect::Connive {
                            count: connive_count_expr,
                            ..
                        } = &*next.effect
                        {
                            let mut next_ability =
                                build_resolved_from_def(next, rid.source, controller);
                            next_ability.targets = vec![TargetRef::Object(object_id)];
                            let connive_count =
                                crate::game::quantity::resolve_quantity_with_targets(
                                    state,
                                    connive_count_expr,
                                    &next_ability,
                                )
                                .max(0) as u32;
                            // CR 614.5: `applied` already excludes this rid, so the
                            // resumed `propose_connive` cannot self-invoke and the
                            // CR 616.1f repeat covers the remaining connives.
                            // Dedicated slot (NOT post_replacement_continuation) so
                            // the leading draw's DeliveryTail drain cannot consume it
                            // mid-draw; the post-replacement-choice epilogue drains
                            // it after the draw fully delivers (CR 701.50a order).
                            if state.pending_connive_reentry.is_none() {
                                state.pending_connive_reentry =
                                    Some(crate::types::game_state::PendingConniveReentry {
                                        conniver: object_id,
                                        count: connive_count,
                                        applied: applied.clone(),
                                    });
                            }
                            return ApplyResult::Prevented;
                        }
                    }
                }
            }
        }
        current = def.sub_ability.as_deref();
    }

    ApplyResult::Prevented
}

// --- 4c. CoinFlip (Krark's Thumb) ---

// CR 705.1 + CR 614.1a: A coin flip is about to happen. Krark's Thumb replaces
// each individual flip ("instead flip two coins and ignore one"), so the
// matcher fires per flip while `count > 0`.
fn coin_flip_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::CoinFlip { count, .. } if *count > 0)
}

fn coin_flip_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    let ProposedEvent::CoinFlip {
        player_id,
        count,
        applied,
    } = event
    else {
        return ApplyResult::Modified(event);
    };

    // CR 614.1a: "instead flip two coins" — double the flip count via the
    // replacement definition's `FlipCoins { count: Multiply { factor: 2, .. } }`.
    let execute = state
        .objects
        .get(&rid.source)
        .and_then(|source| source.replacement_definitions.get(rid.index))
        .and_then(|def| def.execute.as_deref());

    let new_count = match execute {
        Some(ability) if ability.sub_ability.is_none() => match &*ability.effect {
            Effect::FlipCoins { count: qty, .. } => resolve_event_replacement_quantity(qty, count)
                .map(|resolved| resolved.max(0) as u32)
                .unwrap_or(count),
            _ => count,
        },
        _ => count,
    };

    ApplyResult::Modified(ProposedEvent::CoinFlip {
        player_id,
        count: new_count,
        applied,
    })
}

// --- 4c2. Proliferate (Tekuthal, Inquiry Dominus) ---

// CR 701.34a + CR 614.1a: A proliferate action is about to happen. Count-
// modifying replacements ("proliferate twice instead") substitute the action
// count before the chooser opens.
fn proliferate_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Proliferate { count, .. } if *count > 0)
}

fn proliferate_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    let ProposedEvent::Proliferate {
        player_id,
        count,
        applied,
    } = event
    else {
        return ApplyResult::Modified(event);
    };

    let new_count = state
        .objects
        .get(&rid.source)
        .and_then(|source| source.replacement_definitions.get(rid.index))
        .and_then(|def| def.execute.as_deref())
        .and_then(|execute| match &*execute.effect {
            Effect::Proliferate if execute.sub_ability.is_none() => execute
                .repeat_for
                .as_ref()
                .and_then(|qty| resolve_event_replacement_quantity(qty, count)),
            _ => None,
        })
        .map(|resolved| resolved.max(0) as u32)
        .unwrap_or(count);

    ApplyResult::Modified(ProposedEvent::Proliferate {
        player_id,
        count: new_count,
        applied,
    })
}

fn resolve_event_replacement_quantity(expr: &QuantityExpr, event_count: u32) -> Option<i32> {
    match expr {
        QuantityExpr::Ref {
            qty: crate::types::ability::QuantityRef::EventContextAmount,
        } => Some(event_count as i32),
        QuantityExpr::Fixed { value } => Some(*value),
        QuantityExpr::DivideRounded {
            inner,
            divisor,
            rounding,
        } => {
            let value = resolve_event_replacement_quantity(inner, event_count)?;
            let divisor = i32::try_from((*divisor).max(1)).ok()?;
            Some(match rounding {
                crate::types::ability::RoundingMode::Up => (value + divisor - 1) / divisor,
                crate::types::ability::RoundingMode::Down => value / divisor,
            })
        }
        QuantityExpr::Offset { inner, offset } => {
            Some(resolve_event_replacement_quantity(inner, event_count)? + offset)
        }
        QuantityExpr::ClampMin { inner, minimum } => {
            Some(resolve_event_replacement_quantity(inner, event_count)?.max(*minimum))
        }
        QuantityExpr::Multiply { factor, inner } => {
            Some(factor * resolve_event_replacement_quantity(inner, event_count)?)
        }
        QuantityExpr::Sum { exprs } => {
            let mut total = 0i32;
            for inner in exprs {
                total += resolve_event_replacement_quantity(inner, event_count)?;
            }
            Some(total)
        }
        // CR 107.1: the maximum of the computed operand values; empty → 0.
        QuantityExpr::Max { exprs } => {
            let mut best: Option<i32> = None;
            for inner in exprs {
                let value = resolve_event_replacement_quantity(inner, event_count)?;
                best = Some(best.map_or(value, |b| b.max(value)));
            }
            Some(best.unwrap_or(0))
        }
        // CR 107.1c + CR 608.2d: For replacement quantity resolution, treat
        // `UpTo` transparently as its upper bound — the replacement-effect
        // pipeline does not honor "may pick fewer" semantics (the choice
        // already happened at effect resolution before the replacement fires).
        QuantityExpr::UpTo { max } => resolve_event_replacement_quantity(max, event_count),
        // CR 107.3: `base ^ exponent`. Negative exponents clamp to 0 per
        // CR 107.1b; `saturating_pow` prevents overflow.
        QuantityExpr::Power { base, exponent } => {
            let exp = resolve_event_replacement_quantity(exponent, event_count)?.max(0) as u32;
            Some(base.saturating_pow(exp))
        }
        // "The difference between A and B" being unsigned is an Oracle
        // templating convention with no dedicated CR number — resolves to the
        // absolute value of the gap. (CR 107.1b is distinct: it clamps a
        // negative result to zero, not the operand-order-independent magnitude
        // taken here.)
        QuantityExpr::Difference { left, right } => {
            let l = resolve_event_replacement_quantity(left, event_count)?;
            let r = resolve_event_replacement_quantity(right, event_count)?;
            Some((l - r).abs())
        }
        QuantityExpr::Ref { .. } => None,
    }
}

// --- 5. GainLife ---

fn gain_life_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    // CR 614.1a: Basic event type match. Player scope is checked by `valid_player`
    // in `find_applicable_replacements`. Without `valid_player`, defaults to
    // the replacement source player.
    matches!(event, ProposedEvent::LifeGain { .. })
}

// CR 614.1a: Replacement effect modifies life gain amount.
fn gain_life_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    use crate::types::ability::QuantityModification;
    // Branch 1: structured `quantity_modification` (Double / Plus / Minus).
    // Used by Boon Reflection / Rhox Faithmender (Twice) and
    // Hardened Heart-style "+N" replacements.
    let qmod = state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index))
        .and_then(|def| def.quantity_modification.clone());
    if let Some(modification) = qmod {
        if let ProposedEvent::LifeGain {
            player_id,
            amount,
            applied,
        } = event
        {
            let new_amount = match modification {
                QuantityModification::Times { factor } => amount.saturating_mul(factor),
                QuantityModification::Half => amount / 2,
                QuantityModification::Plus { value } => amount.saturating_add(value),
                QuantityModification::Minus { value } => amount.saturating_sub(value),
                // CR 614.6 + CR 614.7: No life-gain replacement uses Prevent
                // today (Tainted Remedy converts gain → loss via execute chain),
                // but the variant composes here for symmetry — fully suppress
                // the gain event.
                QuantityModification::Prevent => return ApplyResult::Prevented,
            };
            return ApplyResult::Modified(ProposedEvent::LifeGain {
                player_id,
                amount: new_amount,
                applied,
            });
        }
        // qmod set but event isn't LifeGain — fall through (no-op).
    }

    // Branch 2: parser-emitted `Effect::GainLife { amount: <expr> }` where
    // `<expr>` describes the *replaced* amount (not a delta). E.g.,
    // Alhammarret's Archive / Boon Reflection / Rhox Faithmender emit
    // `Multiply { factor: 2, inner: EventContextAmount }` for "you gain twice
    // that much life instead". Heron of Hope / Angel of Vitality emit
    // `Offset { inner: EventContextAmount, offset: 1 }` for "you gain that
    // much life plus 1 instead". CR 614.1a: the replacement substitutes a
    // new event (the replaced amount), not an additive delta.
    if let Some(new_amount) = gain_life_replacement_amount(state, rid, &event) {
        if let ProposedEvent::LifeGain {
            player_id, applied, ..
        } = event
        {
            return ApplyResult::Modified(ProposedEvent::LifeGain {
                player_id,
                amount: new_amount,
                applied,
            });
        }
        return ApplyResult::Modified(event);
    }

    // Branch 3: Cross-event-type substitution — "If you would gain life,
    // [other-effect] instead." Lich ("draw that many cards instead"),
    // Lich's Mirror, etc. CR 614.1a: the replacement substitutes a new
    // event of a different type. The original LifeGain event is
    // suppressed; the substitute effect runs as a post-replacement
    // continuation (stashed by `apply_single_replacement`'s mandatory
    // branch). `EventContextAmount` in the substitute reads
    // `last_effect_count` (CR 615.5 fallback); stamp it to the original
    // amount so "draw that many" sees the prevented life-gain quantity.
    if gain_life_execute_substitutes_event_type(state, rid) {
        if let ProposedEvent::LifeGain { amount, .. } = event {
            state.last_effect_count = Some(amount as i32);
        }
        return ApplyResult::Prevented;
    }

    ApplyResult::Modified(event)
}

/// CR 614.1a: True iff the replacement's `execute` carries an effect whose
/// type does NOT match the LifeGain event — i.e., this is a cross-event-type
/// substitution ("If you would gain life, X instead" where X is not
/// `GainLife`). `Effect::Unimplemented` is treated as **not** substitution
/// (silent passthrough preserves coverage when the parser hasn't fully
/// decomposed the replacement yet — a future parser improvement promotes the
/// case to the proper branch).
///
/// Centralizes the "execute shape ≠ matched event type" check so siblings
/// (life-loss substitution, counter substitution, …) can extend through the
/// same primitive when their cards land.
fn gain_life_execute_substitutes_event_type(state: &GameState, rid: ReplacementId) -> bool {
    let Some(execute) = state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index))
        .and_then(|def| def.execute.as_deref())
    else {
        return false;
    };
    let effect = &*execute.effect;
    if matches!(effect, Effect::Unimplemented { .. }) {
        return false;
    }
    !matches!(effect, Effect::GainLife { .. })
}

fn gain_life_replacement_amount(
    state: &GameState,
    rid: ReplacementId,
    event: &ProposedEvent,
) -> Option<u32> {
    let ProposedEvent::LifeGain { amount, .. } = event else {
        return None;
    };

    let execute = state
        .objects
        .get(&rid.source)?
        .replacement_definitions
        .get(rid.index)?
        .execute
        .as_deref()?;

    if execute.sub_ability.is_some() {
        return None;
    }

    match &*execute.effect {
        Effect::GainLife { amount: qty, .. } => {
            let resolved = resolve_event_replacement_quantity(qty, *amount)?;
            Some(resolved.max(0) as u32)
        }
        _ => None,
    }
}

// --- 6. LifeReduced ---

fn life_reduced_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::LifeLoss { .. })
}

fn life_reduced_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 6b. LoseLife (oracle-parsed: e.g. Bloodletter of Aclazotz) ---

fn lose_life_matcher(event: &ProposedEvent, source: ObjectId, state: &GameState) -> bool {
    if let ProposedEvent::LifeLoss { player_id, .. } = event {
        // Match when opponent loses life during source controller's turn
        if let Some(obj) = state.objects.get(&source) {
            *player_id != obj.controller && state.active_player == obj.controller
        } else {
            false
        }
    } else {
        false
    }
}

fn lose_life_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::LifeLoss {
        player_id,
        amount,
        applied,
    } = event
    {
        ApplyResult::Modified(ProposedEvent::LifeLoss {
            player_id,
            amount: amount * 2,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 7. AddCounter ---

fn add_counter_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(
        event,
        ProposedEvent::AddCounter { count, .. } if *count > 0
    ) || matches!(
        event,
        ProposedEvent::MoveCounter {
            stage: CounterMoveStage::Add,
            add_count,
            ..
        } if *add_count > 0
    )
}

fn add_counter_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    use crate::types::ability::QuantityModification;
    let modification = state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index))
        .and_then(|def| def.quantity_modification.clone());
    let Some(modification) = modification else {
        return ApplyResult::Modified(event);
    };
    if matches!(modification, QuantityModification::Prevent) {
        // CR 614.6 + CR 614.7 + CR 122.1: "~ can't have counters put on it."
        // — the proposed counter-placement event never happens
        // (Melira's Keepers class). The replacement fires, but its outcome
        // is to fully suppress the event rather than scale the count.
        return ApplyResult::Prevented;
    }
    let new_count = |count: u32| match modification {
        QuantityModification::Times { factor } => count.saturating_mul(factor),
        QuantityModification::Half => count / 2,
        QuantityModification::Plus { value } => count.saturating_add(value),
        QuantityModification::Minus { value } => count.saturating_sub(value),
        QuantityModification::Prevent => unreachable!(),
    };

    match event {
        ProposedEvent::AddCounter {
            placement,
            count,
            applied,
        } => ApplyResult::Modified(ProposedEvent::AddCounter {
            placement,
            count: new_count(count),
            applied,
        }),
        ProposedEvent::MoveCounter {
            actor,
            source_id,
            destination_id,
            counter_type,
            remove_count,
            add_count,
            stage: CounterMoveStage::Add,
            applied,
        } => ApplyResult::Modified(ProposedEvent::MoveCounter {
            actor,
            source_id,
            destination_id,
            counter_type,
            remove_count,
            add_count: new_count(add_count),
            stage: CounterMoveStage::Add,
            applied,
        }),
        event => ApplyResult::Modified(event),
    }
}

// --- 8. RemoveCounter ---

fn remove_counter_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(
        event,
        ProposedEvent::RemoveCounter { count, .. } if *count > 0
    ) || matches!(
        event,
        ProposedEvent::MoveCounter {
            stage: CounterMoveStage::Remove,
            remove_count,
            ..
        } if *remove_count > 0
    )
}

fn remove_counter_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 9. CreateToken ---

fn create_token_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::CreateToken { .. })
}

fn create_token_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> ApplyResult {
    use crate::types::ability::QuantityModification;
    let (
        modification,
        additional_spec,
        ensure_specs,
        owner_redirect,
        substitute_effect,
        source_controller,
    ) = state
        .objects
        .get(&rid.source)
        .and_then(|obj| {
            obj.replacement_definitions
                .get(rid.index)
                .map(|def| (def, obj.controller))
        })
        .map(|(def, controller)| {
            (
                def.quantity_modification.clone(),
                def.additional_token_spec.clone(),
                def.ensure_token_specs.clone(),
                def.token_owner_redirect.clone(),
                // CR 614.1a + CR 111.1: Full token-substitution payload
                // (Divine Visitation) — carried as an Effect::Token in the
                // existing `execute` field (Approach A, no new field).
                def.execute
                    .as_deref()
                    .map(|ability| (*ability.effect).clone())
                    .filter(|effect| matches!(effect, Effect::Token { .. })),
                controller,
            )
        })
        .unwrap_or((None, None, None, None, None, PlayerId(0)));

    if let ProposedEvent::CreateToken {
        owner,
        mut spec,
        mut copy,
        enter_tapped,
        count,
        applied,
    } = event
    {
        // CR 111.2 + CR 614.1a: Apply controller redirect (Crafty Cutpurse).
        // CR 111.2: "The token enters the battlefield under that player's
        // control" — the default the replacement is overriding.
        // The redirect's `ControllerRef` is resolved relative to the source's
        // controller — `You` redirects to that controller; `Opponent` would
        // redirect away (not currently a Magic pattern but representable).
        let original_owner = owner;
        let owner = match owner_redirect {
            Some(crate::types::ability::ControllerRef::You) => source_controller,
            // No other ControllerRef scope is a Magic token-redirect pattern today,
            // and `try_parse_token_controller_redirect` enforces `You` as the only
            // legal target. Programmatic constructions that set a non-`You` scope
            // fall through to the original owner rather than to incorrect
            // multiplayer semantics (e.g., "first non-source player" for Opponent).
            Some(_) | None => owner,
        };
        // CR 111.2: When the redirect actually rewires ownership, the apply
        // path's `spec.controller`-keyed lookups (combat::enter_attacking
        // defending-player resolution, etc.) must see the new controller —
        // otherwise an "enters attacking" token (Goblin Rabblemaster class)
        // would resolve its defender against the original effect's controller
        // and end up attacking the player who now controls it.
        if owner != original_owner {
            spec.controller = owner;
            if let Some(copy) = copy.as_mut() {
                copy.controller = owner;
            }
        }
        // CR 614.1a: Modify token count per replacement effect.
        let new_count = match modification {
            Some(QuantityModification::Times { factor }) => count.saturating_mul(factor),
            Some(QuantityModification::Half) => count / 2,
            Some(QuantityModification::Plus { value }) => count.saturating_add(value),
            Some(QuantityModification::Minus { value }) => count.saturating_sub(value),
            // CR 614.6 + CR 614.7 + CR 111.1: No printed token-creation
            // replacement uses Prevent today, but the variant composes here for
            // symmetry — fully suppress the token-creation event so any future
            // "tokens can't be created" replacement slots in without re-touching
            // this applier.
            Some(QuantityModification::Prevent) => return ApplyResult::Prevented,
            None => count,
        };

        // CR 614.1a + CR 111.1: Full token substitution (Divine Visitation —
        // "that many 4/4 white Angel creature tokens … are created instead").
        // The `execute` Effect::Token describes the substitute token; resolve it
        // to a TokenSpec and swap it for the proposed spec, keeping the event's
        // `new_count` ("that many" — same count) and `owner`. The creature-type
        // gate (`TokenCoreTypeMatches`) already passed in
        // `find_applicable_replacements`, so non-creature tokens never reach here.
        if let Some(token_effect) = substitute_effect {
            let ability = crate::types::ability::ResolvedAbility::new(
                token_effect,
                Vec::new(),
                rid.source,
                source_controller,
            );
            if let Some((substitute_spec, _, _, _)) =
                crate::game::effects::token::resolve_token_spec(state, &ability)
            {
                spec = Box::new(substitute_spec);
            }
        }

        // CR 614.1a + CR 111.1: "those tokens plus ..." — emit an additional
        // CreateToken for the appended spec class (Chatterfang Squirrels,
        // Donatello Mutagen). The additional batch counts equal the
        // already-modified `new_count`, so replacement-ordering choices
        // (CR 616) applied before this replacement flow through to the
        // appended batch. The additional batch is proposed through
        // `replace_event` so further replacements (e.g., Doubling Season on
        // the creating player) apply to it as a separate event per CR 614.1a.
        if let Some(mut extra) = additional_spec {
            // Fill in the replacement source's runtime identity. The parser
            // stores placeholder ObjectId(0) / PlayerId(0) since these cannot
            // be known until the replacement fires.
            let source_controller = state
                .objects
                .get(&rid.source)
                .map(|o| o.controller)
                .unwrap_or(owner);
            extra.source_id = rid.source;
            extra.controller = source_controller;
            // CR 614.5: Inherit the primary event's applied set to prevent
            // replacements that already applied to the primary event from
            // re-applying to the recursive extra event. Insert this
            // Chatterfang-class replacement too so it cannot re-fire on its own
            // appended batch.
            let mut applied_on_extra = applied.clone();
            applied_on_extra.insert(rid);
            // CR 614.1c: The appended batch is a separate event — it does not
            // inherit an `enter_tapped` override applied to the primary batch.
            // The appended spec's own `tapped` field (from the parser) governs
            // its entry state; further replacements (shock-land-style ETB-tap
            // replacements on the appended batch itself) still compose via
            // the recursive `replace_event` call below.
            let extra_proposed = ProposedEvent::CreateToken {
                owner,
                spec: extra,
                copy: None,
                enter_tapped: EtbTapState::Unspecified,
                count: new_count,
                applied: applied_on_extra,
            };
            match replace_event(state, extra_proposed, events) {
                ReplacementResult::Execute(extra_event) => {
                    crate::game::effects::token::apply_create_token_after_replacement(
                        state,
                        extra_event,
                        events,
                    );
                }
                // Prevented / NeedsChoice branches on the appended batch do not
                // affect the primary event. A NeedsChoice here would require
                // infrastructure to queue replacement prompts inside an applier
                // (none exists yet); the appended batch is silently dropped in
                // that rare collision case, which is acceptable for the
                // current class (no cards combine Chatterfang-style appends
                // with optional ETB replacements on their targets).
                ReplacementResult::Prevented | ReplacementResult::NeedsChoice(_) => {}
            }
        }

        // CR 614.1a + CR 111.1: Manufactor's "ensure one of each" — emit a
        // recursive CreateToken event for every listed spec whose subtype is
        // *not* already in the primary event's spec. The primary event keeps
        // the original subtype's count (Doubling Season etc. composes via
        // `quantity_modification` above), and each additional batch is sized
        // at `new_count` so any post-Manufactor multiplier ordered earlier in
        // CR 616 reaches the appended subtypes.
        if let Some(specs) = ensure_specs {
            let source_controller = state
                .objects
                .get(&rid.source)
                .map(|o| o.controller)
                .unwrap_or(owner);
            for mut extra in specs {
                let already_present = extra.characteristics.subtypes.iter().any(|s| {
                    spec.characteristics
                        .subtypes
                        .iter()
                        .any(|already| already.eq_ignore_ascii_case(s))
                });
                if already_present {
                    continue;
                }
                extra.source_id = rid.source;
                extra.controller = source_controller;
                // CR 614.5: Inherit the primary event's applied set to prevent
                // replacements that already applied to the primary event from
                // re-applying to the recursive extra event.
                let mut applied_on_extra = applied.clone();
                applied_on_extra.insert(rid);
                let extra_proposed = ProposedEvent::CreateToken {
                    owner,
                    spec: Box::new(extra),
                    copy: None,
                    enter_tapped: EtbTapState::Unspecified,
                    count: new_count,
                    applied: applied_on_extra,
                };
                match replace_event(state, extra_proposed, events) {
                    ReplacementResult::Execute(extra_event) => {
                        crate::game::effects::token::apply_create_token_after_replacement(
                            state,
                            extra_event,
                            events,
                        );
                    }
                    ReplacementResult::Prevented | ReplacementResult::NeedsChoice(_) => {}
                }
            }
        }

        ApplyResult::Modified(ProposedEvent::CreateToken {
            owner,
            spec,
            copy,
            enter_tapped,
            count: new_count,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 10. ProduceMana ---

/// CR 106.3 + CR 614.1a: Matches any mana-production event. The replacement def's
/// optional `valid_card` filter (checked in the dispatcher against the mana source)
/// further gates whether this specific definition applies.
fn produce_mana_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::ProduceMana { .. })
}

/// CR 106.3 + CR 614.1a: Applies a `ManaModification` to a produced mana unit,
/// replacing its type before it enters the player's mana pool.
fn produce_mana_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    use crate::types::ability::ManaModification;
    let modification = state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index))
        .and_then(|def| def.mana_modification.clone());

    if let ProposedEvent::ProduceMana {
        source_id,
        player_id,
        mana_type,
        count,
        tapped_for_mana,
        applied,
    } = event
    {
        let (new_mana_type, new_count) = match modification {
            Some(ManaModification::ReplaceWith {
                mana_type: replacement,
            }) => (replacement, count),
            Some(ManaModification::Multiply { factor }) => {
                (mana_type, count.saturating_mul(factor))
            }
            None => (mana_type, count),
        };
        ApplyResult::Modified(ProposedEvent::ProduceMana {
            source_id,
            player_id,
            mana_type: new_mana_type,
            count: new_count,
            tapped_for_mana,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- LoseMana (CR 703.4q step-end empty-mana replacement) ---

/// CR 703.4q + CR 614.1a + CR 614.5: An `EmptyManaPool` event is applicable to
/// a `StepEndManaScanEntry` iff it carries at least one unit with `Drop`
/// disposition that the entry's filter accepts. CR 614.5 enforces "one
/// opportunity per event" via the `applied` set checked by
/// `event.already_applied(&rid)` upstream; the disposition gate here is a
/// secondary correctness property that prevents a handler from re-acting on
/// units it has already transformed in a prior pipeline pass.
fn empty_mana_pool_matcher(event: &ProposedEvent, _source: ObjectId, state: &GameState) -> bool {
    let ProposedEvent::EmptyManaPool { units, .. } = event else {
        return false;
    };
    // Sentinel scan path: `find_applicable_replacements` only calls this with
    // the sentinel source `ObjectId(0)`; per-source scans never produce
    // EmptyManaPool candidates. Look up the handler entry currently being
    // tested via the per-phase handler list.
    //
    // The handler index is not threaded into the matcher signature, so this
    // function approves any event with at least one Drop-disposition unit;
    // the per-handler filter is enforced in the sentinel block of
    // `find_applicable_replacements`. This keeps the matcher signature
    // homogeneous with other matchers in the registry.
    let _ = state;
    units
        .iter()
        .any(|u| matches!(u.disposition, UnitDisposition::Drop))
}

/// CR 703.4q + CR 614.1a: Dead applier for the `LoseMana` registry slot.
/// `apply_single_replacement` discriminates `ProposedEvent::EmptyManaPool`
/// to `apply_empty_mana_pool_replacement` (the Path A carve-out) before
/// registry dispatch, so this function is never invoked at runtime. The
/// matcher + applier pair exist only to occupy the `LoseMana` slot in the
/// `ReplacementEvent` enum — `build_replacement_registry`'s exhaustive
/// match would otherwise fail to compile, and a `None` entry would mask
/// the slot's "structurally registered, dispatched out-of-band" intent.
///
/// Reaching this code path is a discriminator regression: either the
/// carve-out branch was removed, or a new ProposedEvent variant was added
/// that routes through `LoseMana` instead of past it.
fn empty_mana_pool_applier(
    _event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    unreachable!(
        "empty_mana_pool_applier reached: apply_single_replacement \
         discriminator should have routed to apply_empty_mana_pool_replacement \
         (Path A carve-out for ProposedEvent::EmptyManaPool)"
    );
}

/// CR 703.4q + CR 614.1a + CR 614.5 + CR 614.6: Path A carve-out applier for
/// `ProposedEvent::EmptyManaPool`. Bypasses the registry's
/// `ReplacementDefinition`-driven dispatch (matchers, event modifiers,
/// post-replacement continuation) — step-end mana handlers have no sub-ability
/// work to stash, so the carve-out IS the applier.
///
/// For the handler addressed by `rid.index` in
/// `state.pending_step_end_mana_handlers`, walks `units` and flips each
/// `Drop`-disposition unit whose color matches the handler filter to either
/// `Keep` (CR 614.6, `StepEndManaAction::Retain`) or `Recolor(_)`
/// (CR 614.1a, `StepEndManaAction::Transform(_)`). Records the handler on
/// the event's `applied` set so CR 614.5 prevents re-application.
// clippy::result_large_err: see `apply_shield_counter_replacement` — the Err
// arm carries an inherent `ProposedEvent` from the shared replacement pipeline.
#[allow(clippy::result_large_err)]
fn apply_empty_mana_pool_replacement(
    state: &mut GameState,
    proposed: ProposedEvent,
    rid: ReplacementId,
    _events: &mut Vec<GameEvent>,
) -> Result<ProposedEvent, ApplyResult> {
    let ProposedEvent::EmptyManaPool {
        player_id,
        mut units,
        mut applied,
    } = proposed
    else {
        unreachable!("apply_empty_mana_pool_replacement discriminator guarantees variant");
    };

    let entry = match state.pending_step_end_mana_handlers.get(rid.index) {
        Some(e) => e.clone(),
        None => {
            // Handler vanished — return event unchanged so the pipeline can complete.
            return Ok(ProposedEvent::EmptyManaPool {
                player_id,
                units,
                applied,
            });
        }
    };

    // CR 614.5 + CR 614.6 + CR 614.1a: Mutate per-unit disposition. Filter
    // matches on the unit's *current* color (a previously-recolored unit reads
    // its `Recolor(_)` target only via the disposition, not via `color`; the
    // disposition gate ensures handlers don't re-act on units they already
    // transformed).
    for unit in units.iter_mut() {
        if !matches!(unit.disposition, UnitDisposition::Drop) {
            continue;
        }
        if let Some(filter_color) = entry.filter {
            if crate::types::mana::ManaType::from(filter_color) != unit.color {
                continue;
            }
        }
        match entry.action {
            StepEndManaAction::Retain => unit.disposition = UnitDisposition::Keep,
            StepEndManaAction::Transform(t) => unit.disposition = UnitDisposition::Recolor(t),
        }
    }

    applied.insert(rid);
    Ok(ProposedEvent::EmptyManaPool {
        player_id,
        units,
        applied,
    })
}

// --- 11. Tap ---

fn tap_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Tap { .. })
}

fn tap_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 12. Untap ---

// CR 614.1a: Replacement effect modifies untap event.
fn untap_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Untap { .. })
}

// CR 614.1a + CR 614.6: An untap-step replacement ("If [perm] would untap
// during [...] untap step, [effect] instead") replaces the untap with its
// alternative effect, bound to the permanent that would have untapped ("it").
// With no alternative effect it is a pure prevention ("doesn't untap"). Either
// way the original untap does not happen, so the applier returns `Prevented`.
fn untap_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> ApplyResult {
    let ProposedEvent::Untap { object_id, applied } = event else {
        return ApplyResult::Modified(event);
    };

    let Some(source) = state.objects.get(&rid.source) else {
        return ApplyResult::Modified(ProposedEvent::Untap { object_id, applied });
    };
    let controller = source.controller;
    let execute = source
        .replacement_definitions
        .get(rid.index)
        .and_then(|def| def.execute.clone());

    // Run the alternative effect chain (if any) against the would-be-untapped
    // permanent, then prevent the untap. A replacement with no execute is a
    // bare "doesn't untap" prevention.
    if let Some(execute) = execute {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::types::ability::TargetRef;

        // CR 614.6: the alternative effect ("put two +1/+1 counters on it",
        // "remove all wind counters from it") refers to the permanent that would
        // have untapped — NOT the replacement source. Resolve the chain with the
        // would-be-untapped object as the source so its `it`/SelfRef anaphor
        // binds to that permanent, and seed `targets` for the `None`-anaphor form.
        let mut current = Some(execute.as_ref());
        while let Some(def) = current {
            let mut ability = build_resolved_from_def(def, object_id, controller);
            ability.targets = vec![TargetRef::Object(object_id)];
            let _ = crate::game::effects::resolve_ability_chain(state, &ability, events, 1);
            current = def.sub_ability.as_deref();
        }
    }

    ApplyResult::Prevented
}

// --- 13. TurnFaceUp ---

fn turn_face_up_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::TurnFaceUp { .. })
}

// CR 614.1e + CR 708.11: "As ~ is turned face up, [effect]"
// applies its alternative action AS the permanent is turned face up. Unlike a
// prevention the turn-up still happens, so the applier performs the replacement's
// actions (bound to the permanent being turned up) and returns the event
// unchanged. The effect's `it`/SelfRef anaphor binds to that permanent.
fn turn_face_up_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> ApplyResult {
    let ProposedEvent::TurnFaceUp { object_id, applied } = event else {
        return ApplyResult::Modified(event);
    };

    let Some(source) = state.objects.get(&rid.source) else {
        return ApplyResult::Modified(ProposedEvent::TurnFaceUp { object_id, applied });
    };
    let controller = source.controller;
    let execute = source
        .replacement_definitions
        .get(rid.index)
        .and_then(|def| def.execute.clone());

    if let Some(execute) = execute {
        // Bind only the anaphoric self-reference: the execute is resolved with the
        // turned-up permanent as its `source_id`, so "it"/`SelfRef` references the
        // permanent ("put five +1/+1 counters on it"). The permanent is NOT stuffed
        // into ordinary target slots — effects with their own host/target (e.g.
        // Gift of Doom's `Effect::Attach` "attach it to a creature") must resolve
        // that target/host themselves rather than consuming the permanent as the
        // host. `resolve_ability_chain` walks the typed `sub_ability` chain itself,
        // so the root execute is resolved exactly once — iterating the chain here
        // too would run each sub-ability a second time.
        let ability = build_resolved_from_def(execute.as_ref(), object_id, controller);
        let _ = crate::game::effects::resolve_ability_chain(state, &ability, events, 1);
    }

    ApplyResult::Modified(ProposedEvent::TurnFaceUp { object_id, applied })
}

// --- 14. Counter (spell countering) ---

fn counter_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(
        event,
        ProposedEvent::ZoneChange {
            from: Zone::Stack,
            ..
        }
    )
}

fn counter_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 15. Attached (ZoneChange to Battlefield for attachments) ---

fn attached_matcher(event: &ProposedEvent, _source: ObjectId, state: &GameState) -> bool {
    if let ProposedEvent::ZoneChange { object_id, to, .. } = event {
        if *to != Zone::Battlefield {
            return false;
        }
        // Check if the entering object is an attachment (Aura or Equipment)
        state
            .objects
            .get(object_id)
            .map(|obj| {
                obj.card_types
                    .subtypes
                    .iter()
                    .any(|s| s == "Aura" || s == "Equipment")
            })
            .unwrap_or(false)
    } else {
        false
    }
}

fn attached_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 16. DealtDamage (from target's perspective) ---

fn dealt_damage_matcher(event: &ProposedEvent, source: ObjectId, state: &GameState) -> bool {
    if let ProposedEvent::Damage { target, .. } = event {
        // Match if the source object of this replacement is the target of the damage
        match target {
            crate::types::ability::TargetRef::Object(oid) => *oid == source,
            crate::types::ability::TargetRef::Player(pid) => state
                .objects
                .get(&source)
                .map(|o| o.controller == *pid)
                .unwrap_or(false),
        }
    } else {
        false
    }
}

fn dealt_damage_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    // CR 614.1a + CR 120.6 + CR 510.2: Wolverine, Fierce Fighter — "instead
    // that damage is dealt, but all other damage already dealt to him is
    // healed." The new damage instance is delivered UNCHANGED (we return
    // `Modified(event)` verbatim, NO prevention); we only clear the receiver's
    // PRIOR marked damage here, when the replacement carries an
    // `Effect::RemoveAllDamage` in `execute`.
    //
    // COMBAT-BATCH INVARIANT (load-bearing — do not break without updating the
    // gang-block regression test): this applier runs in **Phase B**
    // (`replace_combat_damage_batch`, combat_damage.rs:869-871) for EVERY damage
    // event in the combat step, BEFORE any Phase-C delivery. The SOLE
    // `damage_marked` increment lives in `apply_damage_after_replacement`
    // (deal_damage.rs:446), reached only in **Phase C**. Therefore at the
    // instant this heal runs, `damage_marked` holds exactly the PRE-BATCH value
    // and ZERO same-batch combat instances are marked yet. Clearing it here
    // heals only prior damage and preserves all simultaneous same-batch
    // instances (CR 510.2). A future refactor that interleaves Phase-C delivery
    // into the Phase-B loop would silently over-heal — the combat-batch test
    // guards against exactly that.
    let heals = matches!(
        &event,
        ProposedEvent::Damage {
            target: crate::types::ability::TargetRef::Object(oid),
            ..
        } if *oid == rid.source
    ) && state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index))
        .and_then(|def| def.execute.as_deref())
        .is_some_and(|execute| {
            execute.sub_ability.is_none()
                && matches!(*execute.effect, Effect::RemoveAllDamage { .. })
        });

    if heals {
        if let Some(obj) = state.objects.get_mut(&rid.source) {
            crate::game::effects::remove_all_damage::heal_marked_damage(obj);
        }
    }

    ApplyResult::Modified(event)
}

// --- 17. Mill ---

// CR 614.6: A replacement effect applies only once to a given event. The
// `applied: HashSet<ReplacementId>` carried in the event prevents the
// pipeline from re-entering the same effect on the modified event.
fn mill_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(
        event,
        ProposedEvent::Mill {
            count,
            destination: Zone::Graveyard,
            ..
        } if *count > 0
    )
}

fn mill_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    let (player_id, count, destination, applied) = match event {
        ProposedEvent::Mill {
            player_id,
            count,
            destination,
            applied,
        } => (player_id, count, destination, applied),
        other => {
            return ApplyResult::Modified(other);
        }
    };

    let new_count = state
        .objects
        .get(&rid.source)
        .and_then(|source| source.replacement_definitions.get(rid.index))
        .and_then(|def| def.execute.as_deref())
        .and_then(|execute| match &*execute.effect {
            Effect::Mill { count: qty, .. } if execute.sub_ability.is_none() => {
                resolve_event_replacement_quantity(qty, count)
            }
            _ => None,
        })
        .map(|resolved| resolved.max(0) as u32)
        .unwrap_or(count);

    ApplyResult::Modified(ProposedEvent::Mill {
        player_id,
        count: new_count,
        destination,
        applied,
    })
}

// --- 18. PayLife (matches LifeLoss) ---

fn pay_life_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::LifeLoss { .. })
}

fn pay_life_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- BeginTurn / BeginPhase (CR 614.1b, CR 614.10) ---

/// CR 614.1b + CR 614.10: Match a pending turn-start event shape. Per-def
/// condition gating (`OnlyExtraTurn`) is evaluated by
/// `evaluate_replacement_condition` with full event context.
fn begin_turn_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::BeginTurn { .. })
}

/// CR 614.1b + CR 614.10: Skip the turn. Permanent statics (`ShieldKind::None`,
/// the default) are never consumed — every matching turn-begin is skipped.
fn begin_turn_applier(
    _event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Prevented
}

/// CR 614.1b: Match a pending phase-start event shape. No phase-specific
/// conditions are currently wired; parser enrichment for "skip next combat"
/// etc. is a future batch and will layer via `evaluate_replacement_condition`.
fn begin_phase_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::BeginPhase { .. })
}

/// CR 614.1b + CR 614.10: Skip the phase. Like `begin_turn_applier`, permanent
/// statics fire every time their predicate matches and are never consumed.
fn begin_phase_applier(
    _event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Prevented
}

// --- Registry ---

/// CR 614.1: Build the registry of applicable replacement effects.
pub fn build_replacement_registry() -> IndexMap<ReplacementEvent, ReplacementHandlerEntry> {
    let mut registry = IndexMap::new();

    let stub = || ReplacementHandlerEntry {
        matcher: stub_matcher,
        applier: stub_applier,
    };

    // 14 core types with real logic
    registry.insert(
        ReplacementEvent::DamageDone,
        ReplacementHandlerEntry {
            matcher: damage_done_matcher,
            applier: damage_done_applier,
        },
    );
    registry.insert(
        ReplacementEvent::ChangeZone,
        ReplacementHandlerEntry {
            matcher: change_zone_matcher,
            applier: change_zone_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Moved,
        ReplacementHandlerEntry {
            matcher: moved_matcher,
            applier: moved_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Discard,
        ReplacementHandlerEntry {
            matcher: discard_matcher,
            applier: discard_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Destroy,
        ReplacementHandlerEntry {
            matcher: destroy_matcher,
            applier: destroy_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Draw,
        ReplacementHandlerEntry {
            matcher: draw_matcher,
            applier: draw_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Scry,
        ReplacementHandlerEntry {
            matcher: scry_matcher,
            applier: scry_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Explore,
        ReplacementHandlerEntry {
            matcher: explore_matcher,
            applier: explore_applier,
        },
    );
    // CR 701.50a + CR 614.1a: Connive replacements (Leader, Super-Genius)
    // intercept "a creature would connive" and substitute a modified action.
    registry.insert(
        ReplacementEvent::Connive,
        ReplacementHandlerEntry {
            matcher: connive_matcher,
            applier: connive_applier,
        },
    );
    registry.insert(
        ReplacementEvent::CoinFlip,
        ReplacementHandlerEntry {
            matcher: coin_flip_matcher,
            applier: coin_flip_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Proliferate,
        ReplacementHandlerEntry {
            matcher: proliferate_matcher,
            applier: proliferate_applier,
        },
    );
    registry.insert(ReplacementEvent::DrawCards, stub()); // stays stub (alias for Draw)
    registry.insert(
        ReplacementEvent::GainLife,
        ReplacementHandlerEntry {
            matcher: gain_life_matcher,
            applier: gain_life_applier,
        },
    );
    registry.insert(
        ReplacementEvent::LifeReduced,
        ReplacementHandlerEntry {
            matcher: life_reduced_matcher,
            applier: life_reduced_applier,
        },
    );
    registry.insert(
        ReplacementEvent::LoseLife,
        ReplacementHandlerEntry {
            matcher: lose_life_matcher,
            applier: lose_life_applier,
        },
    );
    registry.insert(
        ReplacementEvent::AddCounter,
        ReplacementHandlerEntry {
            matcher: add_counter_matcher,
            applier: add_counter_applier,
        },
    );
    registry.insert(
        ReplacementEvent::RemoveCounter,
        ReplacementHandlerEntry {
            matcher: remove_counter_matcher,
            applier: remove_counter_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Tap,
        ReplacementHandlerEntry {
            matcher: tap_matcher,
            applier: tap_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Untap,
        ReplacementHandlerEntry {
            matcher: untap_matcher,
            applier: untap_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Counter,
        ReplacementHandlerEntry {
            matcher: counter_matcher,
            applier: counter_applier,
        },
    );
    registry.insert(
        ReplacementEvent::CreateToken,
        ReplacementHandlerEntry {
            matcher: create_token_matcher,
            applier: create_token_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Attached,
        ReplacementHandlerEntry {
            matcher: attached_matcher,
            applier: attached_applier,
        },
    );

    // Promoted from stubs to real handlers
    registry.insert(
        ReplacementEvent::DealtDamage,
        ReplacementHandlerEntry {
            matcher: dealt_damage_matcher,
            applier: dealt_damage_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Mill,
        ReplacementHandlerEntry {
            matcher: mill_matcher,
            applier: mill_applier,
        },
    );
    registry.insert(
        ReplacementEvent::PayLife,
        ReplacementHandlerEntry {
            matcher: pay_life_matcher,
            applier: pay_life_applier,
        },
    );
    // CR 106.3 + CR 614.1a: ProduceMana routes through the replacement pipeline
    // so cards like Contamination ("produces {B} instead") can rewrite produced
    // mana. The parser extracts the target type into `ReplacementDefinition::
    // mana_modification`; the applier substitutes it before the mana enters the
    // pool.
    registry.insert(
        ReplacementEvent::ProduceMana,
        ReplacementHandlerEntry {
            matcher: produce_mana_matcher,
            applier: produce_mana_applier,
        },
    );
    registry.insert(
        ReplacementEvent::TurnFaceUp,
        ReplacementHandlerEntry {
            matcher: turn_face_up_matcher,
            applier: turn_face_up_applier,
        },
    );

    // CR 614.1b + CR 614.10: BeginTurn skip replacements (Stranglehold, etc.)
    registry.insert(
        ReplacementEvent::BeginTurn,
        ReplacementHandlerEntry {
            matcher: begin_turn_matcher,
            applier: begin_turn_applier,
        },
    );
    // CR 614.1b: BeginPhase skip replacements.
    registry.insert(
        ReplacementEvent::BeginPhase,
        ReplacementHandlerEntry {
            matcher: begin_phase_matcher,
            applier: begin_phase_applier,
        },
    );

    // CR 703.4q + CR 614.1a + CR 614.6: LoseMana routes step-end empty-mana
    // events through the replacement pipeline so CR 616.1 player-choice
    // ordering applies when ≥2 handlers (Upwelling, Horizon Stone, Kruphix,
    // Omnath, …) match the same emptying event. The applier registered here
    // is a debug-assert stub because the path A carve-out
    // (`apply_empty_mana_pool_replacement` at the top of
    // `apply_single_replacement`) handles disposition mutation directly,
    // bypassing the registry applier dispatch.
    registry.insert(
        ReplacementEvent::LoseMana,
        ReplacementHandlerEntry {
            matcher: empty_mana_pool_matcher,
            applier: empty_mana_pool_applier,
        },
    );

    // CR 104.2b + CR 104.3b: GameLoss / GameWin are parser-emitted by
    // Platinum Angel, Lich's Mastery, Angel's Grace, etc. The effective
    // runtime enforcement for these cards is via first-class static-ability
    // variants: `StaticMode::CantLoseTheGame` (sba.rs::player_has_cant_lose)
    // and `StaticMode::CantWinTheGame` (effects/win_lose.rs::resolve_win).
    // The replacement-pipeline stub here is redundant but kept registered
    // so the parser's replacement-path output doesn't hit a dispatch miss.
    let stub_events: Vec<ReplacementEvent> =
        vec![ReplacementEvent::GameLoss, ReplacementEvent::GameWin];
    for ev in stub_events {
        registry.insert(ev, stub());
    }

    registry
}

// --- Prevention gating ---

/// CR 615.12: Check if damage prevention is disabled by a GameRestriction.
/// When active, prevention-type replacement effects are skipped in the pipeline.
fn is_prevention_disabled(state: &GameState, proposed: &ProposedEvent) -> bool {
    use crate::types::ability::{GameRestriction, RestrictionScope};

    state.restrictions.iter().any(|r| match r {
        GameRestriction::DamagePreventionDisabled { scope, .. } => match scope {
            None => {
                // Global — all damage prevention disabled
                matches!(proposed, ProposedEvent::Damage { .. })
            }
            Some(RestrictionScope::SpecificSource(id)) => {
                matches!(proposed, ProposedEvent::Damage { source_id, .. } if *source_id == *id)
            }
            Some(RestrictionScope::SourcesControlledBy(pid)) => {
                if let ProposedEvent::Damage { source_id, .. } = proposed {
                    state
                        .objects
                        .get(source_id)
                        .map(|obj| obj.controller == *pid)
                        .unwrap_or(false)
                } else {
                    false
                }
            }
            Some(RestrictionScope::DamageToTarget(tid)) => {
                matches!(proposed, ProposedEvent::Damage { target, .. }
                    if matches!(target, crate::types::ability::TargetRef::Object(oid) if *oid == *tid)
                    || matches!(target, crate::types::ability::TargetRef::Player(pid) if {
                        // For player targets, check if the player's "id object" matches
                        // This is a player target, not an object target, so tid doesn't apply
                        let _ = pid;
                        false
                    })
                )
            }
        },
        GameRestriction::ProhibitActivity { .. } => false,
    })
}

/// Check if a replacement definition is a damage prevention replacement.
/// Prevention replacements have a `Prevented` result (the event is fully stopped)
/// or are recognized prevention-type patterns from the parser.
fn is_damage_prevention_replacement(
    state: &GameState,
    rid: &ReplacementId,
    event: &ReplacementEvent,
) -> bool {
    // Only applies to DamageDone handlers
    let is_damage_event = matches!(event, ReplacementEvent::DamageDone)
        || matches!(event, ReplacementEvent::DealtDamage);
    if !is_damage_event {
        return false;
    }

    // Look up the replacement definition from either objects or pending_damage_replacements.
    let repl_def = if rid.source == ObjectId(0) {
        state.pending_damage_replacements.get(rid.index)
    } else {
        state
            .objects
            .get(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get(rid.index))
    };

    let Some(repl) = repl_def else {
        return false;
    };

    // CR 614.1a: Damage boost/reduction replacements are definitively not prevention effects
    if repl.damage_modification.is_some() {
        return false;
    }

    // Check for ShieldKind::Prevention or description-based prevention patterns
    // CR 615: Prevention shields created by prevent_damage.rs
    matches!(repl.shield_kind, ShieldKind::Prevention { .. })
    // Legacy: description-based prevention from parsed replacement definitions
    || repl.description.as_ref().is_some_and(|d| {
        let lower = d.to_lowercase();
        lower.contains("prevent") && lower.contains("damage")
    })
}

/// CR 614.1a: Check if a damage target matches the replacement's target filter.
fn matches_damage_target_filter(
    filter: &DamageTargetFilter,
    target: &TargetRef,
    repl_controller: PlayerId,
    repl_source: ObjectId,
    state: &GameState,
) -> bool {
    fn player_scope_matches(
        scope: &DamageTargetPlayerScope,
        player: PlayerId,
        repl_controller: PlayerId,
        repl_source: ObjectId,
        state: &GameState,
    ) -> bool {
        match scope {
            DamageTargetPlayerScope::Any => true,
            DamageTargetPlayerScope::Opponent => player != repl_controller,
            DamageTargetPlayerScope::Controller => player == repl_controller,
            DamageTargetPlayerScope::SourceChosenPlayer => {
                // CR 607.2d + CR 614.1a: A damage replacement can scope
                // "the chosen player" through the replacement source's linked
                // persisted choice.
                crate::game::game_object::source_chosen_player(state, repl_source)
                    .is_some_and(|chosen| player == chosen)
            }
            DamageTargetPlayerScope::Specific(specific) => player == *specific,
        }
    }

    match filter {
        DamageTargetFilter::Player { player } => match target {
            TargetRef::Player(pid) => {
                player_scope_matches(player, *pid, repl_controller, repl_source, state)
            }
            TargetRef::Object(_) => false,
        },
        DamageTargetFilter::PlayerOrPermanentsControlledBy { player } => match target {
            TargetRef::Player(pid) => {
                player_scope_matches(player, *pid, repl_controller, repl_source, state)
            }
            TargetRef::Object(oid) => state.objects.get(oid).is_some_and(|obj| {
                player_scope_matches(player, obj.controller, repl_controller, repl_source, state)
            }),
        },
        DamageTargetFilter::CreatureOnly => match target {
            TargetRef::Player(_) => false,
            TargetRef::Object(oid) => state
                .objects
                .get(oid)
                .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Creature)),
        },
    }
}

// --- Pipeline functions ---

/// CR 702.26f + CR 611.2b: "for as long as you control [source]" applicability
/// gate — true only while the captured originating source is on the battlefield,
/// still controlled by the captured installer, AND phased in. A "for as long as"
/// duration that tracks a permanent ends when that permanent phases out because
/// the effect can no longer see it (CR 702.26f); per CR 702.26b/d a phased-out
/// permanent is treated as not on the battlefield and not under its controller's
/// control even though phasing never changes its zone or controller, so it lapses
/// this duration (CR 611.2b: the duration ends and does not begin again).
/// CR 613.1b: the captured control reference is a Layer-2 control concept.
/// Single authority shared by the `ControllerControlsSource` condition arm (live
/// re-evaluation) and the layer-pass lapse prune
/// (`layers::prune_lapsed_controller_controls_source`), so both agree on exactly
/// when the CR 611.2b duration has ended.
pub(crate) fn controller_controls_source_gate(
    state: &GameState,
    source: ObjectId,
    installer: PlayerId,
) -> bool {
    state.objects.get(&source).is_some_and(|o| {
        // CR 702.26f: a "for as long as you control ~" continuous effect that
        // tracks a permanent ends when that permanent phases out, because the
        // effect can no longer see it (CR 611.2b: the duration ends and does not
        // begin again). CR 702.26b/d: phasing never changes zone or controller,
        // so the zone/controller checks alone would wrongly keep this gate true;
        // the phased-in requirement is load-bearing.
        o.zone == Zone::Battlefield && o.controller == installer && o.is_phased_in()
    })
}

/// Evaluate a replacement condition against the current game state.
/// Returns `true` if the replacement should apply, `false` if it should be skipped.
fn evaluate_replacement_condition(
    condition: &ReplacementCondition,
    controller: PlayerId,
    source_id: ObjectId,
    state: &GameState,
    affected_object_id: Option<ObjectId>,
    event: &ProposedEvent,
) -> bool {
    match condition {
        ReplacementCondition::And { conditions } => conditions.iter().all(|condition| {
            evaluate_replacement_condition(
                condition,
                controller,
                source_id,
                state,
                affected_object_id,
                event,
            )
        }),
        ReplacementCondition::UnlessControlsSubtype { subtypes } => {
            // "unless you control a [subtype]" → suppressed if controller has a matching permanent
            let controls_any = state.objects.values().any(|o| {
                o.zone == Zone::Battlefield
                    && o.controller == controller
                    && o.id != source_id
                    && subtypes.iter().any(|st| {
                        o.card_types
                            .subtypes
                            .iter()
                            .any(|s| s.eq_ignore_ascii_case(st))
                    })
            });
            // If the "unless" is satisfied (they DO control one), skip the replacement
            !controls_any
        }
        // CR 305.7 + CR 614.1c — fast lands enter tapped unless controller has
        // N or fewer other lands; condition evaluated as the replacement applies.
        ReplacementCondition::UnlessControlsOtherLeq { count, filter } => {
            let target_filter = TargetFilter::Typed(filter.clone());
            let ctx = FilterContext::from_source(state, source_id);
            let matching_count = state
                .objects
                .values()
                .filter(|o| {
                    o.zone == Zone::Battlefield
                        && matches_target_filter(state, o.id, &target_filter, &ctx)
                })
                .count() as u32;
            // "unless you control N or fewer" → suppressed when count ≤ N
            // Replacement applies (enters tapped) when count > N
            matching_count > *count
        }
        // CR 614.1d — "unless you control a [type phrase]" → suppressed if controller
        // has a matching permanent on the battlefield. ControllerRef::You is pre-set
        // in the filter by the parser.
        ReplacementCondition::UnlessControlsMatching { filter } => {
            let ctx = FilterContext::from_source_with_controller(source_id, controller);
            let controls_any = state.objects.values().any(|o| {
                o.zone == Zone::Battlefield
                    && o.id != source_id
                    && matches_target_filter(state, o.id, filter, &ctx)
            });
            !controls_any
        }
        // CR 614.1d + CR 810.9a: Bond lands — "unless a player has N or less
        // life". Each player's life reads the team total in a team format, so
        // the OR over players is "any team total <= N".
        ReplacementCondition::UnlessPlayerLifeAtMost { amount } => {
            let any_player_low = state
                .players
                .iter()
                .any(|p| crate::game::players::team_life_total(state, p.id) <= *amount as i32);
            !any_player_low
        }
        // CR 614.1d: Battlebond lands — "unless you have two or more opponents"
        ReplacementCondition::UnlessMultipleOpponents => {
            let opponent_count = state
                .players
                .iter()
                .filter(|p| p.id != controller && !p.is_eliminated)
                .count();
            opponent_count < 2
        }
        // CR 614.1d — "unless you control N or more [type]" → suppressed if controller
        // has at least `minimum` matching permanents on the battlefield.
        ReplacementCondition::UnlessControlsCountMatching { minimum, filter } => {
            let ctx = FilterContext::from_source_with_controller(source_id, controller);
            let matching_count = state
                .objects
                .values()
                .filter(|o| {
                    o.zone == Zone::Battlefield
                        && o.id != source_id
                        && matches_target_filter(state, o.id, filter, &ctx)
                })
                .count();
            matching_count < *minimum as usize
        }
        // CR 614.1d + CR 500: "unless it's your turn" — suppressed on controller's turn.
        ReplacementCondition::UnlessYourTurn => state.active_player != controller,
        // CR 614.1d: General quantity comparison — suppressed when comparison is true.
        ReplacementCondition::UnlessQuantity {
            lhs,
            comparator,
            rhs,
            active_player_req,
        } => {
            // Optional active-player gate: "it's your Nth turn" requires controller's turn;
            // "it's an opponent's Nth turn" requires opponent's turn; None = no gate.
            let turn_ok = match active_player_req {
                Some(ControllerRef::You) => state.active_player == controller,
                Some(ControllerRef::Opponent) => state.active_player != controller,
                // CR 109.4: TargetPlayer active-player gate is nonsensical at
                // replacement-check time (no ability context). Fail closed.
                Some(ControllerRef::ScopedPlayer) => false,
                Some(ControllerRef::TargetPlayer) => false,
                Some(ControllerRef::ParentTargetController) => false,
                Some(ControllerRef::ParentTargetOwner) => false,
                Some(ControllerRef::DefendingPlayer) => false,
                // CR 613.1: "the chosen player" is undefined at replacement-check
                // time here. Fail closed.
                Some(ControllerRef::SourceChosenPlayer) => false,
                // CR 109.4: Chosen-player scope is undefined at replacement-check
                // time (no resolution context). Fail closed.
                Some(ControllerRef::ChosenPlayer { .. }) => false,
                // CR 603.2 + CR 109.4: Triggering-player scope is undefined at
                // replacement-check time (no event context). Fail closed.
                Some(ControllerRef::TriggeringPlayer) => false,
                // CR 303.4b: Enchanted-player scope is undefined at replacement-check time. Fail closed.
                Some(ControllerRef::EnchantedPlayer) => false,
                None => true,
            };
            if !turn_ok {
                return true; // Turn requirement not met → replacement applies
            }
            let lhs_val =
                crate::game::quantity::resolve_quantity(state, lhs, controller, source_id);
            let rhs_val =
                crate::game::quantity::resolve_quantity(state, rhs, controller, source_id);
            !comparator.evaluate(lhs_val, rhs_val)
        }
        ReplacementCondition::OnlyIfQuantity {
            lhs,
            comparator,
            rhs,
            active_player_req,
        } => {
            let turn_ok = match active_player_req {
                Some(ControllerRef::You) => state.active_player == controller,
                Some(ControllerRef::Opponent) => state.active_player != controller,
                // CR 109.4: TargetPlayer active-player gate is nonsensical at
                // replacement-check time (no ability context). Fail closed.
                Some(ControllerRef::ScopedPlayer) => false,
                Some(ControllerRef::TargetPlayer) => false,
                Some(ControllerRef::ParentTargetController) => false,
                Some(ControllerRef::ParentTargetOwner) => false,
                Some(ControllerRef::DefendingPlayer) => false,
                // CR 613.1: "the chosen player" is undefined at replacement-check
                // time here. Fail closed.
                Some(ControllerRef::SourceChosenPlayer) => false,
                // CR 109.4: Chosen-player scope is undefined at replacement-check
                // time (no resolution context). Fail closed.
                Some(ControllerRef::ChosenPlayer { .. }) => false,
                // CR 603.2 + CR 109.4: Triggering-player scope is undefined at
                // replacement-check time (no event context). Fail closed.
                Some(ControllerRef::TriggeringPlayer) => false,
                // CR 303.4b: Enchanted-player scope is undefined at replacement-check time. Fail closed.
                Some(ControllerRef::EnchantedPlayer) => false,
                None => true,
            };
            if !turn_ok {
                return false;
            }
            let lhs_val =
                crate::game::quantity::resolve_quantity(state, lhs, controller, source_id);
            let rhs_val =
                crate::game::quantity::resolve_quantity(state, rhs, controller, source_id);
            comparator.evaluate(lhs_val, rhs_val)
        }
        ReplacementCondition::HasMaxSpeed => super::speed::has_max_speed(state, controller),
        // CR 702.138c: "escapes with" — applies only when the source was cast via escape.
        // Check cast_from_zone on the entering permanent as a proxy for escape.
        ReplacementCondition::CastViaEscape => state
            .objects
            .get(&source_id)
            .is_some_and(|o| o.cast_from_zone == Some(Zone::Graveyard)),
        // CR 702.188a: applies only when the source permanent's spell was cast
        // using the named alternative cost. Mirrors
        // `TriggerCondition::CastVariantPaid` (triggers.rs).
        ReplacementCondition::CastVariantPaid { variant } => state
            .objects
            .get(&source_id)
            .is_some_and(|o| o.cast_variant_paid == Some((*variant, state.turn_number))),
        // CR 603.4: "if you cast it from [zone]" — applies only when the source
        // permanent was cast from the gated zone. Equivalent to CastViaEscape
        // for arbitrary zones (Hand for Myojin, Exile for foretell-style, etc.).
        ReplacementCondition::CastFromZone { zone } => state
            .objects
            .get(&source_id)
            .is_some_and(|o| o.cast_from_zone == Some(*zone)),
        // CR 614.1d + CR 601: entry-origin gate on the ENTERING object
        // (`affected_object_id`), NOT the replacement source. The physical half
        // delegates to the shared `OriginConstraint::matches_from` predicate.
        // NOTE: `ProposedEvent::ZoneChange.from` is a non-optional `Zone`, so it
        // is wrapped as `Some(*from)` to match the predicate's `&Option<Zone>`
        // signature (the trigger-matcher caller passes a real `Option` because
        // CR 111.1 token entry has `from = None`). The cast half (CR 601) reads
        // the entering object's `cast_from_zone` — the "after being cast from
        // <zone>" case, where the object enters from the Stack but originated in
        // `cast_origin`. OR-combined: Don't Blink fires for both "enter from
        // exile" and "cast from exile then enter".
        ReplacementCondition::EnteredFromZone {
            origin_constraint,
            cast_origin,
        } => {
            // CR 614.1d: the physical half matches only when a physical origin
            // constraint is present. A cast-origin-only clause leaves
            // `origin_constraint` `None`, so the physical path is inert and the
            // condition can fire solely via the cast half below.
            let physical = matches!(
                event,
                ProposedEvent::ZoneChange { from, .. }
                    if origin_constraint
                        .as_ref()
                        .is_some_and(|c| c.matches_from(&Some(*from)))
            );
            let cast = cast_origin.is_some_and(|cz| {
                affected_object_id
                    .and_then(|oid| state.objects.get(&oid))
                    .is_some_and(|o| o.cast_from_zone == Some(cz))
            });
            physical || cast
        }
        // CR 207.2c (Raid): "if you attacked this turn" — applies only when
        // the controller's `creatures_attacked_this_turn` set is non-empty
        // for any owned creature. Tracked on GameState and reset each turn.
        ReplacementCondition::YouAttackedThisTurn => {
            state.creatures_attacked_this_turn.iter().any(|oid| {
                state
                    .objects
                    .get(oid)
                    .is_some_and(|o| o.controller == controller)
            })
        }
        // CR 702.54a (Bloodthirst): "if an opponent was dealt damage this turn"
        // — applies only when any opponent of `controller` is the target of a
        // damage record. Per CR 702.54a the damage source is irrelevant — ANY
        // damage to ANY opponent of the entering permanent's controller
        // satisfies the condition. `damage_dealt_this_turn` is cleared on
        // turn start (`start_next_turn`).
        ReplacementCondition::OpponentDamagedThisTurn => {
            let opponents = crate::game::players::opponents(state, controller);
            state
                .damage_dealt_this_turn
                .iter()
                .any(|r| opponents.contains(&r.target_controller))
        }
        // CR 702.33d + CR 702.33f: "if was kicked" — applies only when the
        // source permanent's spell was kicked. `kickers_paid` is populated at
        // cast resolution from `SpellContext.kickers_paid`. When `variant` is
        // `Some`, narrow to that specific kicker position; when `None`, any
        // kicker payment satisfies the gate. `kicker_cost` is parser metadata
        // that should be resolved by synthesis before runtime evaluation.
        ReplacementCondition::CastViaKicker {
            variant,
            kicker_cost,
        } => state.objects.get(&source_id).is_some_and(|o| {
            if kicker_cost.is_some() && variant.is_none() {
                false
            } else {
                match variant {
                    Some(v) => o.kickers_paid.contains(v),
                    None => !o.kickers_paid.is_empty(),
                }
            }
        }),
        ReplacementCondition::SourceTappedState { tapped } => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.tapped == *tapped),
        // CR 120.1 + CR 614.1a: Check whether the affected object was dealt
        // damage this turn by a source matching the replacement's source
        // filter. The filter is evaluated relative to the replacement source,
        // so `SelfRef` means "this source" and `AttachedTo` means the object
        // this Aura/Equipment is attached to.
        ReplacementCondition::DealtDamageThisTurnBySource { source } => {
            let Some(affected_id) = affected_object_id else {
                return false;
            };
            let ctx = FilterContext::from_source(state, source_id);
            state.damage_dealt_this_turn.iter().any(|record| {
                // CR 608.2i + CR 608.2h: match the damage source against its
                // damage-time snapshot (look-back), consistent with
                // DamageDealtThisTurn / OpponentDealtCombatDamage.
                record.target == TargetRef::Object(affected_id)
                    && matches_target_filter_on_damage_record_source(state, record, source, &ctx)
            })
        }
        ReplacementCondition::EventSourceControlledBy {
            controller: ctrl_ref,
        } => {
            let event_source = match event {
                ProposedEvent::Discard {
                    source_id: Some(source_id),
                    ..
                } => *source_id,
                _ => return false,
            };
            let event_source_controller = state
                .objects
                .get(&event_source)
                .map(|o| o.controller)
                .or_else(|| state.lki_cache.get(&event_source).map(|lki| lki.controller));
            let Some(event_source_controller) = event_source_controller else {
                return false;
            };
            match ctrl_ref {
                ControllerRef::You => event_source_controller == controller,
                ControllerRef::Opponent => event_source_controller != controller,
                ControllerRef::ScopedPlayer
                | ControllerRef::TargetPlayer
                | ControllerRef::ParentTargetController
                | ControllerRef::ParentTargetOwner
                | ControllerRef::DefendingPlayer
                | ControllerRef::SourceChosenPlayer
                | ControllerRef::ChosenPlayer { .. }
                | ControllerRef::TriggeringPlayer
                // CR 303.4b: Enchanted-player scope is undefined at replacement-check time. Fail closed.
                | ControllerRef::EnchantedPlayer => false,
            }
        }
        ReplacementCondition::EffectCausedDiscard => matches!(
            event,
            ProposedEvent::Discard {
                caused_by_effect: true,
                ..
            }
        ),
        // CR 500.7 + CR 614.10: Replacement applies only for extra turns.
        // Checks the event's `is_extra_turn` flag directly; returns `false` for
        // any non-`BeginTurn` event so a misattached `OnlyExtraTurn` doesn't
        // silently fire on unrelated replacements.
        ReplacementCondition::OnlyExtraTurn => matches!(
            event,
            ProposedEvent::BeginTurn {
                is_extra_turn: true,
                ..
            }
        ),
        // CR 614.1a + CR 111.1: "if you would create one or more <subtype> tokens" —
        // applies iff the proposed CreateToken event's spec subtypes overlap any
        // listed subtype. Non-CreateToken events never match this condition.
        ReplacementCondition::TokenSubtypeMatches { subtypes } => match event {
            ProposedEvent::CreateToken { spec, .. } => subtypes.iter().any(|wanted| {
                spec.characteristics
                    .subtypes
                    .iter()
                    .any(|got| got.eq_ignore_ascii_case(wanted))
            }),
            _ => false,
        },
        // CR 614.1a + CR 111.1: "if one or more <core type> tokens would be
        // created" — applies iff the proposed CreateToken event's spec core
        // types overlap any listed core type (Divine Visitation gates on
        // Creature). Non-CreateToken events never match this condition.
        ReplacementCondition::TokenCoreTypeMatches { core_types } => match event {
            ProposedEvent::CreateToken { spec, .. } => core_types
                .iter()
                .any(|wanted| spec.characteristics.core_types.contains(wanted)),
            _ => false,
        },
        // CR 121.1 + CR 504.1 + CR 614.6: "except the first one you draw in
        // each of your draw steps" — applies to every Draw EXCEPT the active
        // player's first draw of the draw step. Returns `false` (suppress
        // replacement) when this would be the first draw of the active player
        // in the draw step (`cards_drawn_this_step == 0`); `true` otherwise.
        ReplacementCondition::ExceptFirstDrawInDrawStep => match event {
            ProposedEvent::Draw { player_id, .. } => {
                let in_draw_step = state.phase == crate::types::phase::Phase::Draw;
                let drawer_is_active = *player_id == state.active_player;
                let already_drawn = state
                    .players
                    .iter()
                    .find(|p| p.id == *player_id)
                    .map(|p| p.cards_drawn_this_step)
                    .unwrap_or(0);
                // Suppress when this would be the FIRST draw of the active
                // player's draw step.
                !(in_draw_step && drawer_is_active && already_drawn == 0)
            }
            _ => false,
        },
        // CR 502.3 + CR 502.4: untap-step gate. Permanents untap as a turn-based
        // action during the untap step, and no player receives priority then, so
        // any `ProposedEvent::Untap` raised while `phase == Untap` is the
        // turn-based untap (effect-untaps like "untap target creature" occur in
        // phases that grant priority). Restricts the replacement to the untap
        // step exactly as the "during [its controller's / your] untap step"
        // wording requires.
        ReplacementCondition::DuringUntapStep => state.phase == crate::types::phase::Phase::Untap,
        // CR 614.1d: "if you control [N or more] [filter]" — replacement applies only
        // while the controller has at least `minimum` permanents matching `filter` on
        // the battlefield. minimum=1 covers the singular "a [type]" form (Worship);
        // higher values cover "N or more [type]" forms (Lair of the Hydra, etc.).
        //
        // Source-exclusion is handled by `FilterProp::Another` injected by the parser
        // when the Oracle text says "other" (e.g. "two or more other lands"). When the
        // text does NOT say "other" (e.g. Worship's "if you control a creature"), the
        // source MUST count toward its own condition — relevant when the source itself
        // satisfies the filter (e.g. Worship animated into a creature). Do not add a
        // hardcoded `o.id != source_id` here; it would silently override the filter.
        ReplacementCondition::IfControlsMatching { minimum, filter } => {
            let ctx = FilterContext::from_source_with_controller(source_id, controller);
            let matching_count = state
                .objects
                .values()
                .filter(|o| {
                    o.zone == Zone::Battlefield && matches_target_filter(state, o.id, filter, &ctx)
                })
                .count();
            matching_count >= *minimum as usize
        }
        // CR 611.3b + CR 716.2a + CR 614.1b: A Class-level static replacement applies
        // only while the source Class enchantment is on the battlefield and at the gated
        // level or higher. Unlike the shared `eval_class_level_ge` (used by
        // StaticCondition/TriggerCondition, where the functioning-abilities path already
        // constrains source availability), replacement effects can persist in lookup
        // tables beyond a source's zone change — so the battlefield zone guard here is
        // load-bearing and must NOT be factored out into the shared helper.
        ReplacementCondition::ClassLevelGE { level } => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.zone == Zone::Battlefield && obj.class_level >= Some(*level)),
        // CR 611.2b: "for as long as you control [source]" — the replacement
        // applies only while the captured source object is on the battlefield AND
        // still controlled by the captured installing player. Either departure
        // (leaving play, or a control swap) ends the continuous effect, matching
        // the Master Thief example. Both `source` and `controller` are captured at
        // install time and refer to the ORIGINATING source (e.g. Spider-Woman) and
        // its controller — NOT the host the replacement rides on, so the threaded
        // `controller`/`source_id` (which describe that host) are deliberately
        // ignored here.
        ReplacementCondition::ControllerControlsSource {
            source,
            controller: installer,
        } => controller_controls_source_gate(state, *source, *installer),
        // Unrecognized condition — always applies (enters tapped) as a safe default.
        // The engine recognizes the replacement but cannot evaluate the condition,
        // so it conservatively taps the land.
        ReplacementCondition::Unrecognized { .. } => true,
    }
}

/// CR 614.1d + CR 614.6: Evaluate the event-class-agnostic applicability gates
/// (`valid_card`, `destination_zone`, `condition`) for a replacement against an
/// event. Factored from the per-object scan (which runs the same three gates
/// inline) so the global state-level store can run identical logic for
/// non-damage events. `source` is the replacement's source object (the sentinel
/// `ObjectId(0)` for a global install); `source_controller` anchors
/// controller-relative filters/conditions. Returns `true` when all gates pass.
fn apply_state_level_gates(
    repl_def: &ReplacementDefinition,
    event: &ProposedEvent,
    source: ObjectId,
    source_controller: PlayerId,
    state: &GameState,
) -> bool {
    // CR 614.1d: valid_card filter — the event's affected object must match.
    if let Some(ref filter) = repl_def.valid_card {
        let ctx = FilterContext::from_source_with_controller(source, source_controller);
        let matches = if repl_def.event == ReplacementEvent::ChangeZone {
            matches_target_filter_on_battlefield_entry(state, event, filter, &ctx)
        } else {
            event
                .affected_object_id()
                .map(|oid| matches_target_filter(state, oid, filter, &ctx))
                .unwrap_or(false)
        };
        if !matches {
            return false;
        }
    }
    // CR 614.6: Zone-change replacements may be scoped to a specific destination.
    if let Some(ref dest_zone) = repl_def.destination_zone {
        let matches_dest = match event {
            ProposedEvent::ZoneChange { to, .. } => to == dest_zone,
            ProposedEvent::CreateToken { .. } => {
                repl_def.event == ReplacementEvent::ChangeZone && *dest_zone == Zone::Battlefield
            }
            _ => false,
        };
        if !matches_dest {
            return false;
        }
    }
    // CR 614.1d: Evaluate the replacement condition (e.g. EnteredFromZone).
    if let Some(ref cond) = repl_def.condition {
        if !evaluate_replacement_condition(
            cond,
            source_controller,
            source,
            state,
            event.affected_object_id(),
            event,
        ) {
            return false;
        }
    }
    true
}

pub fn find_applicable_replacements(
    state: &GameState,
    event: &ProposedEvent,
    registry: &IndexMap<ReplacementEvent, ReplacementHandlerEntry>,
) -> Vec<ReplacementId> {
    let mut candidates = Vec::new();

    match event {
        ProposedEvent::Destroy { object_id, .. }
            if object_has_shield_counter(state, *object_id) =>
        {
            let rid =
                shield_counter_replacement_id(*object_id, ShieldCounterReplacementKind::Destroy);
            if !event.already_applied(&rid) {
                candidates.push(rid);
            }
        }
        ProposedEvent::Damage {
            target: TargetRef::Object(object_id),
            amount,
            ..
        } if *amount > 0 && object_has_shield_counter(state, *object_id) => {
            let rid =
                shield_counter_replacement_id(*object_id, ShieldCounterReplacementKind::Damage);
            if !event.already_applied(&rid) {
                candidates.push(rid);
            }
        }
        _ => {}
    }

    // CR 702.150a: Compleated replaces the loyalty counters a permanent enters
    // with when life was paid for its Phyrexian mana symbols. In this engine,
    // ETB counters are delivered through the shared AddCounter replacement
    // authority (`apply_etb_counters`), so the intrinsic Compleated replacement
    // is exposed as a virtual AddCounter candidate there. This lets it order
    // correctly with Doubling Season-class count modifiers (CR 616.1).
    if let ProposedEvent::AddCounter {
        placement:
            CounterPlacement::Object {
                object_id,
                counter_type: CounterType::Loyalty,
                ..
            },
        count,
        ..
    } = event
    {
        let rid = compleated_replacement_id(*object_id);
        if *count > 0
            && compleated_life_paid(state, *object_id).is_some()
            && !event.already_applied(&rid)
        {
            candidates.push(rid);
        }
    }

    // CR 702.89a: Umbra armor — a destroy of a permanent enchanted by an Umbra is
    // a candidate for the virtual umbra-armor replacement. Offered independently of
    // the shield-counter match above so a permanent carrying both a shield counter
    // and an Umbra exposes both candidates for CR 616 ordering.
    if let ProposedEvent::Destroy { object_id, .. } = event {
        for umbra_id in umbra_armor_attachments(state, *object_id) {
            let rid = umbra_armor_replacement_id(umbra_id);
            if !event.already_applied(&rid) {
                candidates.push(rid);
            }
        }
    }

    // CR 614.10 + CR 614.10a + CR 506.1: Turn-scoped combat-phase skip (False
    // Peace / Empty City Ruse). When the active player has an `Active`
    // turn-scoped combat skip and a combat-phase step is beginning, expose the
    // virtual skip candidate so the CR 616 pipeline prevents the phase. Scoped
    // strictly to the active (begin-phase) player + combat steps so it never
    // over-matches; it persists for the whole turn (no `already_applied`
    // consumption beyond the standard per-event guard).
    if let ProposedEvent::BeginPhase {
        player_id, phase, ..
    } = event
    {
        if phase.is_combat()
            && state
                .combat_phase_skip_next_turn
                .get(player_id.0 as usize)
                .is_some_and(|skip| skip.active)
        {
            let rid = turn_scoped_combat_skip_replacement_id(*player_id);
            if !event.already_applied(&rid) {
                candidates.push(rid);
            }
        }
    }

    // CR 614.12: Self-replacement effects on a card entering the battlefield.
    // apply even though the card isn't on the battlefield yet. We must scan the
    // entering card in addition to battlefield/command zone permanents.
    let entering_object_id = match event {
        ProposedEvent::ZoneChange {
            object_id,
            to: Zone::Battlefield,
            ..
        } => Some(*object_id),
        _ => None,
    };
    let discarding_object_id = match event {
        ProposedEvent::Discard { object_id, .. } => Some(*object_id),
        _ => None,
    };
    // CR 608.2n + CR 614.1a + CR 614.12: A spell on the stack can carry its own
    // self-scoped `Moved` replacement that fires as it leaves the stack ("If
    // this spell would be put into your graveyard, exile it instead" — the
    // Invoke Calamity free-cast rider). The default `[Battlefield, Command]`
    // scan misses a stack-resident object, and the `is_entering` exception is
    // gated on `to: Battlefield`, so a stack→graveyard self-move would not
    // discover the object's own replacement. Mirror the entering-object
    // exception's shape: include the MOVING object as a candidate source when
    // its own move originates on the stack. This is a per-event check on the one
    // moving object (no extra zone sweep) — the loop below still iterates the
    // same `active_replacements` set; this only lets that one object pass the
    // zone gate, and the `is_stack_self_move && !in_scanned_zone` SelfRef guard
    // keeps it scoped to that object's own definitions.
    let stack_self_moving_object_id = match event {
        ProposedEvent::ZoneChange {
            object_id,
            from: Zone::Stack,
            ..
        } => Some(*object_id),
        _ => None,
    };

    let zones_to_scan = [Zone::Battlefield, Zone::Command];
    // CR 702.26b + CR 114.4: `active_replacements` owns the phased-out /
    // command-zone-emblem gate across all zones. Zone-of-function (CR 903.9 for
    // commander-zone, Leyline-class for hand) stays governed by the per-
    // replacement metadata checked inside this loop; here we preserve the
    // existing Battlefield/Command scan + entering-object exception.
    for (index, obj, repl_def) in super::functioning_abilities::active_replacements(state) {
        let in_scanned_zone = zones_to_scan.contains(&obj.zone);
        let is_entering = entering_object_id == Some(obj.id);
        let is_being_discarded = discarding_object_id == Some(obj.id);
        // CR 608.2n + CR 614.1a: the stack-resident object whose own move this
        // event represents (see `stack_self_moving_object_id` above).
        let is_stack_self_move = stack_self_moving_object_id == Some(obj.id);

        // CR 702.52a: Dredge functions only while the card is in a player's
        // graveyard. The default Battlefield/Command scan misses it, so include a
        // graveyard dredge card on its owner's draw — "as long as you have at
        // least N cards in your library" (CR 702.52b) is the offer gate.
        // Strictly additive: gated on `Keyword::Dredge` in the graveyard, so no
        // non-dredge object is affected.
        let replacement_player = replacement_source_player(obj);
        let is_applicable_dredge = matches!(repl_def.event, ReplacementEvent::Draw)
            && obj.zone == Zone::Graveyard
            && matches!(event, ProposedEvent::Draw { player_id, .. } if *player_id == replacement_player)
            && obj.keywords.iter().any(|k| {
                matches!(k, crate::types::keywords::Keyword::Dredge(n)
                    if state
                        .players
                        .iter()
                        .find(|p| p.id == replacement_player)
                        .is_some_and(|p| p.library.len() as u32 >= *n))
            });

        if !in_scanned_zone
            && !is_entering
            && !is_being_discarded
            && !is_applicable_dredge
            && !is_stack_self_move
        {
            continue;
        }

        {
            // CR 701.19: Skip consumed one-shot replacements (e.g., used regeneration shields).
            if repl_def.is_consumed {
                continue;
            }

            // Cards not yet on battlefield can only apply self-replacement effects
            if is_entering
                && !in_scanned_zone
                && repl_def.valid_card != Some(crate::types::ability::TargetFilter::SelfRef)
            {
                continue;
            }
            if is_being_discarded
                && !in_scanned_zone
                && repl_def.valid_card != Some(crate::types::ability::TargetFilter::SelfRef)
            {
                continue;
            }
            // CR 614.12 + CR 608.2n: a stack-resident object reached only via the
            // stack-self-move exception can apply only its own self-replacement
            // effects. Explicit `SelfRef` is the canonical marker (Invoke
            // Calamity rider, Nexus of Fate).
            if is_stack_self_move
                && !in_scanned_zone
                && repl_def.valid_card != Some(crate::types::ability::TargetFilter::SelfRef)
            {
                continue;
            }

            let rid = ReplacementId {
                source: obj.id,
                index,
            };

            if event.already_applied(&rid) {
                continue;
            }

            if let Some(handler) = registry.get(&repl_def.event) {
                if (handler.matcher)(event, obj.id, state) {
                    // Enforce valid_card filter: if set, the event's affected object
                    // must match the filter (e.g., SelfRef means only this card's own events)
                    if let Some(ref filter) = repl_def.valid_card {
                        let ctx =
                            FilterContext::from_source_with_controller(obj.id, replacement_player);
                        let matches = if repl_def.event == ReplacementEvent::ChangeZone {
                            matches_target_filter_on_battlefield_entry(state, event, filter, &ctx)
                        } else {
                            event
                                .affected_object_id()
                                .map(|oid| matches_target_filter(state, oid, filter, &ctx))
                                .unwrap_or(false)
                        };
                        if !matches {
                            continue;
                        }
                    }
                    // CR 614.6: Zone-change replacements may be scoped to a specific destination.
                    if let Some(ref dest_zone) = repl_def.destination_zone {
                        let matches_dest = match event {
                            ProposedEvent::ZoneChange { to, .. } => to == dest_zone,
                            ProposedEvent::CreateToken { .. } => {
                                repl_def.event == ReplacementEvent::ChangeZone
                                    && *dest_zone == Zone::Battlefield
                            }
                            // CR 614.6: Only zone-change events can match a destination zone scope.
                            _ => false,
                        };
                        if !matches_dest {
                            continue;
                        }
                    }
                    // Evaluate replacement condition (e.g. "unless you control a Mountain")
                    if let Some(ref cond) = repl_def.condition {
                        if !evaluate_replacement_condition(
                            cond,
                            replacement_player,
                            obj.id,
                            state,
                            event.affected_object_id(),
                            event,
                        ) {
                            continue;
                        }
                    }
                    // CR 614.1a: Damage source filter — matches the damage *source* object against the filter.
                    if let Some(ref sf) = repl_def.damage_source_filter {
                        if let ProposedEvent::Damage { source_id, .. } = event {
                            if !matches_target_filter(
                                state,
                                *source_id,
                                sf,
                                &FilterContext::from_source_with_controller(
                                    obj.id,
                                    replacement_player,
                                ),
                            ) {
                                continue;
                            }
                        }
                    }
                    // CR 614.1a: Combat/noncombat damage scope restriction.
                    if let Some(ref scope) = repl_def.combat_scope {
                        if let ProposedEvent::Damage { is_combat, .. } = event {
                            match scope {
                                CombatDamageScope::CombatOnly if !is_combat => continue,
                                CombatDamageScope::NoncombatOnly if *is_combat => continue,
                                _ => {}
                            }
                        }
                    }
                    // CR 614.1a: Damage target filter — restricts which damage recipients trigger this replacement.
                    if let Some(ref tf) = repl_def.damage_target_filter {
                        if let ProposedEvent::Damage { target, .. } = event {
                            if !matches_damage_target_filter(
                                tf,
                                target,
                                replacement_player,
                                obj.id,
                                state,
                            ) {
                                continue;
                            }
                        }
                    }
                    // CR 106.12b + CR 614.1a: Mana replacements can be scoped to
                    // production caused by tapping a permanent for mana.
                    if repl_def.mana_replacement_scope
                        == crate::types::ability::ManaReplacementScope::TappedForMana
                    {
                        match event {
                            ProposedEvent::ProduceMana {
                                tapped_for_mana, ..
                            } if *tapped_for_mana => {}
                            ProposedEvent::ProduceMana { .. } => continue,
                            _ => {}
                        }
                    }
                    // CR 615.12: Skip damage prevention replacements when prevention is disabled.
                    if is_damage_prevention_replacement(state, &rid, &repl_def.event)
                        && is_prevention_disabled(state, event)
                    {
                        continue;
                    }
                    // CR 614.1a: Token owner scope — restrict to tokens created under specific controller.
                    if let Some(ref scope) = repl_def.token_owner_scope {
                        if let ProposedEvent::CreateToken { owner, .. } = event {
                            let matches = match scope {
                                crate::types::ability::ControllerRef::You => {
                                    *owner == replacement_player
                                }
                                crate::types::ability::ControllerRef::Opponent => {
                                    *owner != replacement_player
                                }
                                // CR 109.4: Target-player scope has no meaning
                                // for static token-creation replacements. Fail
                                // closed — parser never emits this variant here.
                                crate::types::ability::ControllerRef::ScopedPlayer => false,
                                crate::types::ability::ControllerRef::TargetPlayer => false,
                                crate::types::ability::ControllerRef::ParentTargetController => {
                                    false
                                }
                                crate::types::ability::ControllerRef::ParentTargetOwner => false,
                                crate::types::ability::ControllerRef::DefendingPlayer => false,
                                // CR 613.1: chosen-player scope has no meaning
                                // for static token-creation replacements.
                                crate::types::ability::ControllerRef::SourceChosenPlayer => false,
                                // CR 109.4: Chosen-player scope has no meaning
                                // for static token-creation replacements.
                                crate::types::ability::ControllerRef::ChosenPlayer { .. } => false,
                                // CR 603.2 + CR 109.4: Triggering-player scope
                                // has no meaning for static token-creation
                                // replacements. Fail closed.
                                crate::types::ability::ControllerRef::TriggeringPlayer => false,
                                // CR 303.4b: Enchanted-player scope is undefined at replacement-check time. Fail closed.
                                crate::types::ability::ControllerRef::EnchantedPlayer => false,
                            };
                            if !matches {
                                continue;
                            }
                        }
                    }
                    // CR 614.1a: valid_player scope — restricts which player's events
                    // trigger this replacement. For GainLife events, determines whose life
                    // gain is replaced. Default (None) = source-player only.
                    if let ProposedEvent::LifeGain { player_id, .. }
                    | ProposedEvent::Draw { player_id, .. }
                    | ProposedEvent::Scry { player_id, .. }
                    | ProposedEvent::Mill { player_id, .. }
                    | ProposedEvent::Proliferate { player_id, .. }
                    | ProposedEvent::CoinFlip { player_id, .. } = event
                    {
                        let player_ok = match &repl_def.valid_player {
                            // CR 614.1a: opponent-scoped replacement (Tainted Remedy).
                            Some(crate::types::ability::ReplacementPlayerScope::Opponent) => {
                                *player_id != replacement_player
                            }
                            // Explicit controller scope.
                            Some(crate::types::ability::ReplacementPlayerScope::You) => {
                                *player_id == replacement_player
                            }
                            // CR 614.1a: all-players replacement (Rain of Gore) —
                            // applies regardless of who controls the source.
                            Some(crate::types::ability::ReplacementPlayerScope::AnyPlayer) => true,
                            None => {
                                // Default: source-player only (controller for permanents,
                                // owner for non-stack/non-battlefield cards).
                                *player_id == replacement_player
                            }
                        };
                        if !player_ok {
                            continue;
                        }
                    }
                    if let ProposedEvent::AddCounter { placement, .. } = event {
                        if placement.player_id().is_some() {
                            let Some(valid_player) = &repl_def.valid_player else {
                                continue;
                            };
                            let affected_player = placement.player_id().expect(
                                "CounterPlacement::player_id is Some for player counter events",
                            );
                            let player_ok = match valid_player {
                                crate::types::ability::ReplacementPlayerScope::Opponent => {
                                    affected_player != obj.controller
                                }
                                crate::types::ability::ReplacementPlayerScope::You => {
                                    affected_player == obj.controller
                                }
                                crate::types::ability::ReplacementPlayerScope::AnyPlayer => true,
                            };
                            if !player_ok {
                                continue;
                            }
                        } else if let Some(valid_player) = &repl_def.valid_player {
                            // Quantity-modifying counter replacements (Halving Season
                            // class) may scope by permanent controller; player-counter
                            // prohibitions with valid_player stay player-only.
                            if !matches!(
                                repl_def.quantity_modification,
                                Some(
                                    QuantityModification::Times { .. }
                                        | QuantityModification::Half
                                        | QuantityModification::Plus { .. }
                                        | QuantityModification::Minus { .. }
                                )
                            ) {
                                continue;
                            }
                            // CR 614.1a: Opponent-scoped counter replacements
                            // (Halving Season) apply to counters on permanents
                            // controlled by an opponent, not only player counters.
                            let Some(object_id) = placement.object_id() else {
                                continue;
                            };
                            let Some(affected_controller) =
                                state.objects.get(&object_id).map(|o| o.controller)
                            else {
                                continue;
                            };
                            let player_ok = match valid_player {
                                crate::types::ability::ReplacementPlayerScope::Opponent => {
                                    affected_controller != obj.controller
                                }
                                crate::types::ability::ReplacementPlayerScope::You => {
                                    affected_controller == obj.controller
                                }
                                crate::types::ability::ReplacementPlayerScope::AnyPlayer => true,
                            };
                            if !player_ok {
                                continue;
                            }
                        }
                    } else if repl_def.event == ReplacementEvent::AddCounter
                        && repl_def.valid_player.is_some()
                    {
                        continue;
                    }
                    // CR 614.7: Skip an Optional replacement whose decline branch is a
                    // no-op on the current event. E.g., a shock land whose `enter_tapped`
                    // is already set by an Earthbending return: declining would tap it,
                    // but it's tapping anyway — the player shouldn't be offered the
                    // dominated "pay 2 life to avoid a tap that isn't happening" choice.
                    if replacement_mode_is_optional(&repl_def.mode)
                        && optional_decline_is_noop(
                            event,
                            replacement_mode_decline(&repl_def.mode),
                            state,
                            obj.id,
                        )
                    {
                        continue;
                    }
                    // CR 122.1a + CR 614.1a: Counter-type filter on AddCounter
                    // replacements. Hardened Scales ("+1/+1 counters") must not
                    // fire on -1/-1 counter additions, and Vizier of Remedies
                    // ("-1/-1 counters") must not fire on +1/+1 counter additions
                    // — the printed Oracle text names a specific counter type as
                    // the discriminator, so the engine honors that here.
                    // `None` and `Some(CounterMatch::Any)` accept any counter
                    // type (Doubling Season, modern wording).
                    let event_counter_type = match (&repl_def.event, event) {
                        (
                            ReplacementEvent::AddCounter,
                            ProposedEvent::AddCounter {
                                placement:
                                    CounterPlacement::Object {
                                        counter_type: ev_ct,
                                        ..
                                    },
                                ..
                            },
                        )
                        | (
                            ReplacementEvent::AddCounter,
                            ProposedEvent::MoveCounter {
                                stage: CounterMoveStage::Add,
                                counter_type: ev_ct,
                                ..
                            },
                        )
                        | (
                            ReplacementEvent::RemoveCounter,
                            ProposedEvent::RemoveCounter {
                                counter_type: ev_ct,
                                ..
                            },
                        )
                        | (
                            ReplacementEvent::RemoveCounter,
                            ProposedEvent::MoveCounter {
                                stage: CounterMoveStage::Remove,
                                counter_type: ev_ct,
                                ..
                            },
                        ) => Some(ev_ct),
                        _ => None,
                    };
                    if let (Some(m), Some(ev_ct)) = (&repl_def.counter_match, event_counter_type) {
                        if !m.matches(ev_ct) {
                            continue;
                        }
                    }
                    candidates.push(rid);
                }
            }
        }
    }

    // CR 614.1a + CR 615.3: Also scan game-state-level (floating) replacements
    // installed by spells/abilities with a duration. These use a sentinel source
    // `ObjectId(0)` to distinguish them from object-attached replacements.
    //
    // Damage entries (prevention shields, damage modification — CR 615.3) run
    // ONLY the damage-specific gates, byte-for-byte identical to the prior
    // damage-only scan. Non-damage entries (zone-change/enter redirects —
    // CR 614.1a, the event is replaced "instead", e.g. enter-from-exile →
    // shuffle into owner's library, Don't Blink) run the
    // valid_card/destination_zone/condition gates shared with the per-object
    // loop via `apply_state_level_gates`.
    //
    // Safety: every existing pending entry's registry matcher is event-specific
    // (a damage entry uses `damage_done_matcher`, matching only `Damage`; a
    // zone-change entry uses `change_zone_matcher`, matching only
    // `ZoneChange{to: Battlefield}`/`CreateToken`). So a damage entry can never
    // be a candidate for a non-damage event and vice versa — the new gates are
    // reachable only by non-damage entries on non-damage events.
    {
        for (index, repl_def) in state.pending_damage_replacements.iter().enumerate() {
            if repl_def.is_consumed {
                continue;
            }

            let rid = ReplacementId {
                source: ObjectId(0),
                index,
            };

            if event.already_applied(&rid) {
                continue;
            }

            if let Some(handler) = registry.get(&repl_def.event) {
                if let ProposedEvent::Damage { .. } = event {
                    // CR 615.3: Check combat scope, target filters, and source filters.
                    // CR 614.1a: Damage source filter — matches the damage *source* object
                    // against the filter (e.g., "sources of the chosen color").
                    if let Some(ref sf) = repl_def.damage_source_filter {
                        if let ProposedEvent::Damage { source_id, .. } = event {
                            // CR 109.4 + CR 614.1a: The pending replacement lives under
                            // the sentinel `ObjectId(0)`, which has no entry in
                            // `state.objects`, so `from_source` cannot derive a
                            // controller. When the installing player was anchored at
                            // install time (`source_controller`), use it so a
                            // controller-relative source filter ("a source you control")
                            // resolves; otherwise fall back to the bare source context.
                            let ctx = match repl_def.source_controller {
                                Some(pid) => {
                                    FilterContext::from_source_with_controller(ObjectId(0), pid)
                                }
                                None => FilterContext::from_source(state, ObjectId(0)),
                            };
                            if !matches_target_filter(state, *source_id, sf, &ctx) {
                                continue;
                            }
                        }
                    }
                    if let Some(ref scope) = repl_def.combat_scope {
                        if let ProposedEvent::Damage { is_combat, .. } = event {
                            match scope {
                                CombatDamageScope::CombatOnly if !is_combat => continue,
                                CombatDamageScope::NoncombatOnly if *is_combat => continue,
                                _ => {}
                            }
                        }
                    }
                    if let Some(ref tf) = repl_def.damage_target_filter {
                        if let ProposedEvent::Damage { target, .. } = event {
                            if !matches_damage_target_filter(
                                tf,
                                target,
                                PlayerId(0),
                                ObjectId(0),
                                state,
                            ) {
                                continue;
                            }
                        }
                    }
                    if is_damage_prevention_replacement(state, &rid, &repl_def.event)
                        && is_prevention_disabled(state, event)
                    {
                        continue;
                    }
                } else {
                    // CR 614.1a + CR 614.1d: Non-damage floating replacements run
                    // the per-object applicability gates. `source_controller` is
                    // anchored at install time; fall back to the active player
                    // when absent (the EnteredFromZone condition reads the
                    // entering object, so the controller is not load-bearing for
                    // it, but a controller-relative valid_card filter would need
                    // it).
                    let source_controller =
                        repl_def.source_controller.unwrap_or(state.active_player);
                    if !apply_state_level_gates(
                        repl_def,
                        event,
                        ObjectId(0),
                        source_controller,
                        state,
                    ) {
                        continue;
                    }
                }
                // Verify the handler matcher still matches (DamageDone for damage
                // entries, ChangeZone for zone-redirect entries).
                if (handler.matcher)(event, ObjectId(0), state) {
                    candidates.push(rid);
                }
            }
        }
    }

    // CR 703.4q + CR 614.1a + CR 616.1: Step-end empty-mana sentinel scan.
    // Each entry in `pending_step_end_mana_handlers` is a candidate handler
    // for an `EmptyManaPool` event; addressed via sentinel source
    // `ObjectId(0)` + `index`. The per-handler filter is enforced here (not
    // in `empty_mana_pool_matcher`) because the matcher signature does not
    // carry a handler index.
    if let ProposedEvent::EmptyManaPool { units, .. } = event {
        for (index, entry) in state.pending_step_end_mana_handlers.iter().enumerate() {
            let rid = ReplacementId {
                source: ObjectId(0),
                index,
            };
            // CR 614.5: skip handlers that already applied to this event.
            if event.already_applied(&rid) {
                continue;
            }
            // CR 614.5 secondary correctness: handler applies iff at least one
            // unit has `Drop` disposition AND the filter accepts that unit's
            // color. Handlers do not re-act on units they have already
            // transformed (disposition is now Keep / Recolor).
            let applicable = units.iter().any(|u| {
                if !matches!(u.disposition, UnitDisposition::Drop) {
                    return false;
                }
                match entry.filter {
                    None => true,
                    Some(filter_color) => {
                        crate::types::mana::ManaType::from(filter_color) == u.color
                    }
                }
            });
            if applicable {
                candidates.push(rid);
            }
        }
    }

    candidates
}

const MAX_REPLACEMENT_DEPTH: u16 = 16;

/// Identifies which ability branch of a `ReplacementDefinition` is being applied.
/// CR 614.1a + CR 614.1c: `ReplacementMode::Optional` carries both an `execute` ability
/// (accept branch) and a `decline` ability (decline branch); both branches may introduce
/// ProposedEvent modifications (enter_tapped, counters) and must flow through the same
/// propagation logic so the replacement pipeline sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplacementBranch {
    Execute,
    Decline,
}

/// Extract ETB counter data from a replacement ability's effect.
/// Handles `PutCounter` and `AddCounter` effects, returning (counter_type, count) pairs.
///
/// `event` scopes the quantity resolution: for a `ZoneChange` to the battlefield
/// the entering object is threaded through `QuantityContext::entering`, so
/// self-scoped spell refs (`ManaSpentToCast` with self/trigger scopes
/// lookups) resolve against the spell that is ETB'ing rather than the static
/// replacement source. CR 614.1c treats these as replacement effects; CR 601.2h
/// guarantees `colors_spent_to_cast` is still populated at this point (the clear
/// happens later in `process_triggers`).
fn extract_etb_counters(
    ability: Option<&AbilityDefinition>,
    state: &GameState,
    source_id: ObjectId,
    event: &ProposedEvent,
) -> Vec<(CounterType, u32)> {
    let mut counters = Vec::new();
    let mut current = ability;
    // CR 614.1c: Only walk the event-modifier prefix of the ability chain.
    // `Effect::Choose` and other post-entry work live after that prefix and
    // must not have their `PutCounter` counts folded into `enter_with_counters`
    // before the choice resolves (Banner of Kinship: fellowship counters keyed
    // to the chosen creature type).
    while let Some(exec) = current {
        if !EventModifiers::is_event_modifier_effect(&exec.effect) {
            break;
        }
        counters.extend(extract_etb_counters_from_effect(
            &exec.effect,
            state,
            source_id,
            event,
        ));
        current = exec.sub_ability.as_deref();
    }
    counters
}

fn extract_etb_counters_from_effect(
    effect: &Effect,
    state: &GameState,
    source_id: ObjectId,
    event: &ProposedEvent,
) -> Vec<(CounterType, u32)> {
    match effect {
        Effect::PutCounter {
            counter_type,
            count,
            ..
        } => {
            // CR 107.3m + CR 614.1c: Resolve dynamic counts against the entering
            // object for ETB replacements. `CostXPaid` reads the spell's paid X
            // (stashed by `finalize_cast`); self-scoped spent-mana refs read the spell's
            // per-color mana tally; other dynamic refs resolve against current
            // state.
            let entering = match event {
                ProposedEvent::ZoneChange {
                    object_id,
                    to: Zone::Battlefield,
                    ..
                } => Some(*object_id),
                _ => None,
            };
            let ctx = crate::game::quantity::QuantityContext {
                entering,
                source: source_id,
                recipient: None,
                scoped_player: None,
            };
            let n = match count {
                QuantityExpr::Fixed { value } => (*value).max(0) as u32,
                other => {
                    let controller = state
                        .objects
                        .get(&source_id)
                        .map(|obj| obj.controller)
                        .unwrap_or(PlayerId(0));
                    crate::game::quantity::resolve_quantity_with_ctx(state, other, controller, ctx)
                        .max(0) as u32
                }
            };
            vec![(counter_type.clone(), n)]
        }
        Effect::ChangeZone {
            enter_with_counters,
            ..
        } => enter_with_counters
            .iter()
            .map(|(counter_type, count)| {
                let controller = state
                    .objects
                    .get(&source_id)
                    .map(|obj| obj.controller)
                    .unwrap_or(PlayerId(0));
                let ctx = crate::game::quantity::QuantityContext {
                    entering: event.affected_object_id(),
                    source: source_id,
                    recipient: None,
                    scoped_player: None,
                };
                let n =
                    crate::game::quantity::resolve_quantity_with_ctx(state, count, controller, ctx)
                        .max(0) as u32;
                (counter_type.clone(), n)
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// CR 614.1c + CR 614.12: ProposedEvent modifications that a replacement ability would
/// introduce onto a `ZoneChange` to the battlefield — enters-tapped, ETB counters, and
/// zone redirection. Used by `apply_single_replacement` to propagate the ability's effect
/// onto the ProposedEvent, and by `find_applicable_replacements` to detect Optional
/// replacements whose decline branch would be a no-op (CR 614.7).
#[derive(Debug, Clone, Default)]
pub(super) struct EventModifiers {
    etb_tap_state: EtbTapState,
    etb_counters: Vec<(CounterType, u32)>,
    redirect_zone: Option<Zone>,
    /// CR 110.2a: Controller override for a self-ETB replacement
    /// (`ReplacementDefinition::enters_under`). Carried as an unresolved
    /// `ControllerRef`; resolved to a concrete `PlayerId` and written onto the
    /// `ZoneChange`'s `controller_override` when the replacement is applied.
    controller_override: Option<ControllerRef>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct EnterReplacementModifiers {
    pub enter_tapped: Option<bool>,
    pub counters: Vec<(CounterType, u32)>,
}

impl EventModifiers {
    /// True if this single effect (ignoring sub_ability chain) is purely a
    /// ProposedEvent modifier with no additional resolution work.
    fn is_event_modifier_effect(effect: &Effect) -> bool {
        matches!(
            effect,
            // CR 701.26a/b: a SelfRef single tap/untap is purely an enters-tapped
            // event modifier (either polarity).
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                ..
            } | Effect::PutCounter {
                target: TargetFilter::SelfRef,
                ..
            } | Effect::ChangeZone { .. }
        )
    }

    /// True if this ability has any effect on the ProposedEvent beyond the event-modifier
    /// fields tracked here (i.e., it still needs to run as a post-replacement side effect).
    /// An ability that is *purely* a Tap SelfRef / PutCounter-SelfRef / ChangeZone has no
    /// remaining work after its modifiers are applied to the event.
    fn has_only_event_modifier(ability: Option<&AbilityDefinition>) -> bool {
        let Some(mut current) = ability else {
            return false;
        };
        loop {
            if !Self::is_event_modifier_effect(&current.effect) {
                return false;
            }
            let Some(next) = current.sub_ability.as_deref() else {
                return true;
            };
            current = next;
        }
    }

    /// CR 614.1c: Walk the ability's sub_ability chain and find the first effect
    /// that is NOT a pure event modifier. Returns `None` when the entire chain is
    /// modifiers (shock land class) or when there is no ability at all.
    pub(super) fn first_non_modifier_ability(
        ability: Option<&AbilityDefinition>,
    ) -> Option<&AbilityDefinition> {
        let mut current = ability?;
        loop {
            if !Self::is_event_modifier_effect(&current.effect) {
                return Some(current);
            }
            current = current.sub_ability.as_deref()?;
        }
    }
}

/// CR 614.1c: Compute the ProposedEvent modifications an ability would introduce.
/// Walks the sub_ability chain so composed replacements (e.g., Tap { SelfRef } →
/// BecomeCopy for Vesuva's "enter tapped as a copy") accumulate all modifier
/// effects onto the event, while non-modifier work is handled separately via
/// `apply_post_replacement_effect`.
fn event_modifiers_for_ability(
    ability: Option<&AbilityDefinition>,
    state: &GameState,
    source_id: ObjectId,
    event: &ProposedEvent,
) -> EventModifiers {
    let mut etb_tap_state = EtbTapState::Unspecified;
    let mut redirect = None;
    let mut current = ability;
    while let Some(def) = current {
        if etb_tap_state == EtbTapState::Unspecified {
            etb_tap_state = match &*def.effect {
                // CR 701.26a: SelfRef single tap → enters tapped.
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                } => EtbTapState::Tapped,
                // CR 701.26b: SelfRef single untap → enters untapped.
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Untap,
                } => EtbTapState::Untapped,
                _ => EtbTapState::Unspecified,
            };
        }
        if redirect.is_none() {
            if let Effect::ChangeZone { destination, .. } = &*def.effect {
                redirect = Some(*destination);
            }
        }
        if !EventModifiers::is_event_modifier_effect(&def.effect) {
            break;
        }
        current = def.sub_ability.as_deref();
    }
    let counters = extract_etb_counters(ability, state, source_id, event);
    EventModifiers {
        etb_tap_state,
        etb_counters: counters,
        redirect_zone: redirect,
        controller_override: None,
    }
}

/// CR 110.2a: Resolve the controller for a self-ETB controller-override
/// replacement (`ReplacementDefinition::enters_under`). The reference is resolved
/// relative to the entering object's *own* controller.
///
/// `ControllerRef::Opponent` ("enters under the control of an opponent of your
/// choice") is resolved here rather than via the canonical `controller_ref_player`,
/// which returns `None` for `Opponent` (ambiguous when more than one opponent
/// exists). In a two-player game this is the sole opponent — fully correct. In
/// multiplayer it picks the first opponent in seat order; a full controller choice
/// is a follow-up. Either way the permanent enters under an opponent's control
/// rather than its owner's, satisfying CR 110.2a.
fn resolve_self_enters_under_controller(
    state: &GameState,
    object_id: ObjectId,
    cref: &ControllerRef,
) -> Option<PlayerId> {
    let entering_controller = state.objects.get(&object_id)?.controller;
    match cref {
        ControllerRef::Opponent => crate::game::players::opponents(state, entering_controller)
            .into_iter()
            .next(),
        other => crate::game::filter::controller_ref_player(
            state,
            object_id,
            Some(entering_controller),
            None,
            other,
        ),
    }
}

/// CR 614.12 + CR 707.9: When an "enters as a copy" choice is made, the copy
/// effect determines the object's battlefield characteristics before other
/// self-replacement effects that modify how it enters are considered. The
/// engine's interactive `CopyTargetChoice` happens after the physical zone move,
/// so this helper re-runs only the copied object's current self ETB modifiers
/// (tap state and enter-with-counters) before SBAs/ETB triggers are checked.
pub(super) fn current_self_enter_replacement_modifiers(
    state: &GameState,
    source_id: ObjectId,
) -> EnterReplacementModifiers {
    let registry = build_replacement_registry();
    let event = ProposedEvent::zone_change(source_id, Zone::Battlefield, Zone::Battlefield, None);
    let mut result = EnterReplacementModifiers::default();

    for rid in find_applicable_replacements(state, &event, &registry)
        .into_iter()
        .filter(|rid| rid.source == source_id)
    {
        let Some(replacement) = state
            .objects
            .get(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get(rid.index))
        else {
            continue;
        };
        if replacement_mode_is_optional(&replacement.mode) {
            continue;
        }

        let modifiers =
            event_modifiers_for_ability(replacement.execute.as_deref(), state, source_id, &event);
        match modifiers.etb_tap_state {
            EtbTapState::Unspecified => {}
            EtbTapState::Tapped => result.enter_tapped = Some(true),
            EtbTapState::Untapped => result.enter_tapped = Some(false),
        }
        result.counters.extend(modifiers.etb_counters);
    }

    result
}

fn battlefield_entry_current_tapped(event: &ProposedEvent) -> Option<bool> {
    match event {
        ProposedEvent::ZoneChange { enter_tapped, .. } => Some(enter_tapped.resolve(false)),
        ProposedEvent::CreateToken {
            spec, enter_tapped, ..
        } => Some(enter_tapped.resolve(spec.tapped)),
        _ => None,
    }
}

fn battlefield_entry_counters(event: &ProposedEvent) -> Option<&Vec<(CounterType, u32)>> {
    match event {
        ProposedEvent::ZoneChange {
            enter_with_counters,
            ..
        } => Some(enter_with_counters),
        ProposedEvent::CreateToken { spec, .. } => Some(&spec.enter_with_counters),
        _ => None,
    }
}

/// CR 614.7: "If a replacement effect would replace an event, but that event never
/// happens, the replacement effect simply doesn't do anything."
///
/// An `Optional` replacement's decline branch is the player's "default" — what happens
/// if they decline the accept cost. If the decline branch is a pure ProposedEvent
/// modifier (e.g., shock-land `Tap SelfRef`) and every modification it would introduce
/// is already present on the event (e.g., `enter_tapped` is already `true` from an
/// earlier Earthbending return), declining would do nothing. Presenting the Optional
/// to the player becomes a dominated choice: accepting costs something (life, discard,
/// etc.) to avoid a modification that was going to happen anyway. Skip the Optional
/// entirely in that case — the event proceeds with its existing modifications.
///
/// The check only skips when the decline branch's work is fully subsumed. If decline
/// has any non-modifier effect (e.g., a choice, a draw) or a modification not already
/// present, the Optional remains applicable so the player can still be offered the
/// choice when it is meaningful.
fn optional_decline_is_noop(
    event: &ProposedEvent,
    decline: Option<&AbilityDefinition>,
    state: &GameState,
    source_id: ObjectId,
) -> bool {
    let Some(current_tapped) = battlefield_entry_current_tapped(event) else {
        return false;
    };
    let Some(enter_with_counters) = battlefield_entry_counters(event) else {
        return false;
    };

    // No decline branch at all → the Optional has nothing to do on decline. But it may
    // still have a meaningful accept branch, so do NOT dominate.
    let Some(def) = decline else {
        return false;
    };

    // If decline has any non-modifier effect, it still has real work on decline.
    if !EventModifiers::has_only_event_modifier(Some(def)) {
        return false;
    }

    let mods = event_modifiers_for_ability(Some(def), state, source_id, event);
    let tap_already = match mods.etb_tap_state {
        EtbTapState::Unspecified => true,
        EtbTapState::Tapped => current_tapped,
        EtbTapState::Untapped => !current_tapped,
    };
    let counters_already = mods.etb_counters.iter().all(|(ct, n)| {
        enter_with_counters
            .iter()
            .any(|(existing_ct, existing_n)| existing_ct == ct && existing_n >= n)
    });
    // Redirect: a redirect-bearing decline always has work to do, so it is never a
    // no-op regardless of the current `to` zone.
    let redirect_noop = mods.redirect_zone.is_none();

    tap_already && counters_already && redirect_noop
}

// clippy::result_large_err: see `apply_shield_counter_replacement` — the Err
// arm carries an inherent `ProposedEvent` from the shared replacement pipeline.
#[allow(clippy::result_large_err)]
fn apply_single_replacement(
    state: &mut GameState,
    mut proposed: ProposedEvent,
    rid: ReplacementId,
    branch: ReplacementBranch,
    registry: &IndexMap<ReplacementEvent, ReplacementHandlerEntry>,
    events: &mut Vec<GameEvent>,
) -> Result<ProposedEvent, ApplyResult> {
    // CR 703.4q + CR 614.1a: Path A carve-out for step-end empty-mana events.
    // Step-end mana handlers carry no `ReplacementDefinition` (no execute /
    // decline ability, no event-modifier sub-ability work, no runtime_execute)
    // so `branch` and `registry` are intentionally ignored — the carve-out IS
    // the applier. See `apply_empty_mana_pool_replacement` for the per-unit
    // disposition mutation. Discriminating on the event variant (rather than
    // on `state.pending_phase_transition_progress`) makes dispatch robust
    // against control-flow state being out-of-sync with event identity during
    // pipeline pauses.
    if matches!(proposed, ProposedEvent::EmptyManaPool { .. }) {
        return apply_empty_mana_pool_replacement(state, proposed, rid, events);
    }

    if is_compleated_replacement(rid) {
        return Ok(apply_compleated_replacement(state, proposed, rid, events));
    }

    if let Some(kind) = shield_counter_replacement_kind(rid) {
        return apply_shield_counter_replacement(state, proposed, rid, kind, events);
    }

    if is_umbra_armor_replacement(rid) {
        return apply_umbra_armor_replacement(state, proposed, rid, events);
    }

    // CR 614.10 + CR 614.10a: Turn-scoped combat-phase skip — "skip [the combat
    // phase]" is "instead of beginning it, do nothing." Yield `Prevented` so the
    // pipeline turns the BeginPhase event into `ReplacementResult::Prevented`,
    // which `advance_phase` consumes by not entering the phase. The marker is NOT
    // consumed here: it persists `Active` for the whole turn so every combat
    // phase that turn (including extra combat phases) is prevented; it is cleared
    // at the start of the player's following turn in `start_next_turn`.
    if is_turn_scoped_combat_skip_replacement(rid) {
        return Err(ApplyResult::Prevented);
    }

    // CR 615.3: Pending damage prevention shields use sentinel ObjectId(0).
    // Look up from game-state-level registry instead of object replacement_definitions.
    let repl_def_ref = if rid.source == ObjectId(0) {
        state.pending_damage_replacements.get(rid.index)
    } else {
        state
            .objects
            .get(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get(rid.index))
    };

    // Extract replacement metadata before mutably borrowing state for the applier.
    // CR 614.1c: ProposedEvent modifiers (enter_tapped, ETB counters, zone redirect)
    // come from whichever branch is being applied — `execute` on accept / mandatory,
    // `decline` on decline. Both must flow through the pipeline so dominance and
    // downstream replacements see a consistent ProposedEvent (CR 614.5).
    //
    // CR 614.12a: Mandatory replacement effects whose `execute` is non-modifier work
    // (e.g., `Effect::Choose { Opponent, persist: true }` for Siege protector /
    // Tribute) stash the execute as a `post_replacement_continuation` so it runs in
    // the same resolution step, right after the ZoneChange completes. Without this,
    // the chooser would never be prompted. Optional replacements set
    // `post_replacement_continuation` in `continue_replacement` when the player accepts.
    let (event_key, modifiers, mandatory_post_effect, consume_on_apply) = match repl_def_ref {
        Some(repl_def) => {
            let ability = match branch {
                ReplacementBranch::Execute => repl_def.execute.as_deref(),
                ReplacementBranch::Decline => replacement_mode_decline(&repl_def.mode),
            };
            // CR 510.2 + CR 615.13: A `Prevention::All` shield firing inside an
            // active combat-damage batch must NOT stash its rider per-source —
            // the rider fires once post-batch (`combat_damage.rs`) against the
            // summed prevented amount. Suppress the per-event stash here so the
            // batch step owns the single continuation.
            let batched_combat_all_shield = state.combat_prevention_tally.is_some()
                && matches!(
                    repl_def.shield_kind,
                    ShieldKind::Prevention {
                        amount: PreventionAmount::All
                    }
                );
            let post_effect = match (branch, &repl_def.mode) {
                (ReplacementBranch::Execute, ReplacementMode::Mandatory)
                    if !batched_combat_all_shield =>
                {
                    // CR 615.5: Damage prevention follow-ups (e.g. Phyrexian
                    // Hydra's "Put a -1/-1 counter on ~ for each 1 damage
                    // prevented this way") must always stash as a post-effect
                    // — the `has_only_event_modifier` heuristic that classifies
                    // self-targeted PutCounter as an ETB modifier does not
                    // apply to Damage events, where there is no `etb_counters`
                    // slot to absorb the counters into.
                    let is_damage = matches!(proposed, ProposedEvent::Damage { .. });
                    if let Some(runtime) = repl_def.runtime_execute.clone() {
                        Some(PostReplacementContinuation::Resolved(runtime))
                    } else {
                        repl_def.execute.as_deref().and_then(|def| {
                            // CR 608.2c + CR 614.11: Draw-count replacements with
                            // chained riders (Blood Scrivener: draw two, then lose
                            // 1 life) modify the draw via `draw_replacement_count`
                            // and stash only the rider chain for post-draw drain.
                            if matches!(*def.effect, Effect::Draw { .. })
                                && def.sub_ability.is_some()
                                && matches!(proposed, ProposedEvent::Draw { .. })
                                && draw_replacement_count(state, rid, &proposed).is_some()
                            {
                                return def
                                    .sub_ability
                                    .clone()
                                    .map(PostReplacementContinuation::Template);
                            }
                            // CR 614.1c: Walk past modifier-only effects (Tap/Untap/
                            // PutCounter/ChangeZone) in the sub_ability chain to find
                            // the first non-modifier work. Covers both the existing
                            // ChangeZone → sub_ability pattern (Nexus of Fate shuffle-
                            // back) and composed replacements like Tap → BecomeCopy
                            // (Vesuva "enter tapped as a copy").
                            match EventModifiers::first_non_modifier_ability(Some(def)) {
                                Some(real_work) => Some(PostReplacementContinuation::Template(
                                    Box::new(real_work.clone()),
                                )),
                                None if !is_damage
                                    && EventModifiers::has_only_event_modifier(Some(def)) =>
                                {
                                    None
                                }
                                _ => Some(PostReplacementContinuation::Template(Box::new(
                                    def.clone(),
                                ))),
                            }
                        })
                    }
                }
                _ => None,
            };
            // CR 614.6 + CR 614.11: When the branch being applied substitutes the
            // draw with a non-Draw chain (Jace's WinTheGame, Abundance's
            // reveal-until), zero the count here so `draw_applier` and
            // `apply_draw_after_replacement` see a no-op draw — the original draw
            // never happens (CR 614.6). Branch-aware via the `ability` binding
            // above, so an optional replacement's decline never pre-zeros against
            // the accept-side AST. The `draw_replacement_count` guard preserves
            // the count-modifier path (Alhammarret's Archive: count -> 2*count).
            if matches!(proposed, ProposedEvent::Draw { .. }) {
                if let Some(def) = ability {
                    let is_non_draw_substitute = !matches!(*def.effect, Effect::Draw { .. })
                        && !EventModifiers::has_only_event_modifier(Some(def))
                        && draw_replacement_count(state, rid, &proposed).is_none();
                    if is_non_draw_substitute {
                        if let ProposedEvent::Draw { count, .. } = &mut proposed {
                            *count = 0;
                        }
                    }
                }
            }
            // CR 614.6: When the applier itself substitutes the event with the
            // execute's effect (Draw count-modifier via `draw_replacement_count`,
            // Scry → Draw / Scry → Scry via `scry_applier`), the work is already
            // encoded in the substituted event — do NOT also stash the same
            // ability as a post-replacement continuation, or it will execute
            // twice (once via the applier-modified event, once via the drain).
            // Only the "residual work beyond the event substitution" case (a
            // sub_ability chain or a non-event-substituting effect like Choose /
            // WinTheGame) belongs in the continuation slot.
            let post_effect = post_effect.filter(|_| {
                let Some(def) = ability else {
                    return true;
                };
                if def.sub_ability.is_some() {
                    return true;
                }
                !matches!(
                    (&proposed, &*def.effect),
                    (ProposedEvent::Draw { .. }, Effect::Draw { .. })
                        | (ProposedEvent::Scry { .. }, Effect::Draw { .. })
                        | (ProposedEvent::Scry { .. }, Effect::Scry { .. })
                        | (ProposedEvent::Proliferate { .. }, Effect::Proliferate)
                        | (ProposedEvent::LifeGain { .. }, Effect::GainLife { .. })
                        // CR 614.1a + CR 111.1: Full token substitution
                        // (Divine Visitation) is performed inline by
                        // `create_token_applier`; stashing the same
                        // `Effect::Token` as a post-replacement continuation
                        // would re-propose token creation and re-enter the
                        // replacement pipeline (issue #4249 hang).
                        | (ProposedEvent::CreateToken { .. }, Effect::Token { .. })
                )
            });
            // CR 701.50a + CR 614.5: The connive applier runs the entire
            // replacement `execute` chain ("instead you draw a card, then that
            // creature connives") itself and returns `Prevented`. Stashing the
            // same chain as a post-replacement continuation would re-run it when
            // the continuation drains (e.g. after the connive's `ConniveDiscard`
            // choice resolves), executing the modified action twice. The applier
            // is the single authority for this event, so suppress the generic
            // stash. On its parking path the applier stashes its deferred connive
            // into the DEDICATED `state.pending_connive_reentry` slot (only the
            // deferred connive link, not the whole chain), so suppressing this
            // generic Template stash here does not drop the deferred connive.
            let post_effect =
                post_effect.filter(|_| !matches!(proposed, ProposedEvent::Connive { .. }));
            let mut modifiers = event_modifiers_for_ability(ability, state, rid.source, &proposed);
            // CR 110.2a: A self-ETB controller override is carried directly on the
            // replacement definition (not derived from `execute`), parallel to the
            // imperative `Effect::ChangeZone.enters_under` slot. Surface it as an
            // event modifier so it is written onto the `ZoneChange` below.
            modifiers.controller_override = repl_def.enters_under.clone();
            (
                repl_def.event.clone(),
                modifiers,
                post_effect,
                repl_def.consume_on_apply,
            )
        }
        None => return Ok(proposed),
    };

    // CR 615.5 + CR 609.7: Snapshot the *prevented event's* damage source
    // before the applier consumes `proposed`. Stashed below at the `Prevented`
    // arm so `TargetFilter::PostReplacementSourceController` can resolve "the
    // source's controller draws cards" follow-ups (Swans of Bryn Argoll class).
    let proposed_damage_source = match &proposed {
        ProposedEvent::Damage { source_id, .. } => Some(*source_id),
        _ => None,
    };
    let proposed_damage_target = match &proposed {
        ProposedEvent::Damage { target, .. } => Some(target.clone()),
        _ => None,
    };

    if let Some(handler) = registry.get(&event_key) {
        let event_type = event_key.to_string();
        match (handler.applier)(proposed, rid, state, events) {
            ApplyResult::Modified(mut new_event) => {
                if modifiers.etb_tap_state != EtbTapState::Unspecified {
                    if let Some(enter_tapped) = new_event.battlefield_entry_tap_state_mut() {
                        *enter_tapped = modifiers.etb_tap_state;
                    }
                }
                // CR 110.2a: Apply a self-ETB controller override onto the entering
                // ZoneChange (set before ETB triggers fire — the permanent never
                // enters under its owner's control first). Resolve the carried
                // `ControllerRef` against the entering object's own controller.
                if let Some(cref) = modifiers.controller_override.as_ref() {
                    if let ProposedEvent::ZoneChange {
                        object_id,
                        to: Zone::Battlefield,
                        controller_override,
                        ..
                    } = &mut new_event
                    {
                        if let Some(pid) =
                            resolve_self_enters_under_controller(state, *object_id, cref)
                        {
                            *controller_override = Some(pid);
                        }
                    }
                }
                // CR 614.6: Apply zone redirect (e.g., graveyard → exile for Rest in Peace).
                if let Some(zone) = modifiers.redirect_zone {
                    if let ProposedEvent::ZoneChange { ref mut to, .. } = new_event {
                        *to = zone;
                    }
                }
                // CR 614.1c: Applied branch carries ETB counter data; add to the zone change.
                if !modifiers.etb_counters.is_empty() {
                    match &mut new_event {
                        ProposedEvent::ZoneChange {
                            enter_with_counters,
                            ..
                        } => enter_with_counters.extend(modifiers.etb_counters.iter().cloned()),
                        ProposedEvent::CreateToken { spec, .. } => spec
                            .enter_with_counters
                            .extend(modifiers.etb_counters.iter().cloned()),
                        _ => {}
                    }
                }
                if consume_on_apply {
                    mark_replacement_consumed(state, rid);
                }
                // CR 614.12a: Stash the mandatory execute ability as a post-replacement
                // effect when it has work beyond the event modifiers (e.g., a Choose
                // prompt for Siege protector / Tribute opponent selection). Runs after
                // the ZoneChange completes. Only the first such stash in a chained
                // pipeline wins; this matches how Optional replacements queue their
                // accept-branch post-effect.
                if let Some(post) = mandatory_post_effect {
                    // CR 615.5 + CR 609.7: only the Prevented arm populates
                    // `post_replacement_event_source`; clear here so a prior
                    // prevention's source can't leak into a non-prevention stash.
                    stash_post_replacement_continuation(state, post, rid.source, None, None);
                }
                events.push(GameEvent::ReplacementApplied {
                    source_id: rid.source,
                    event_type,
                });
                return Ok(new_event);
            }
            ApplyResult::Prevented => {
                if consume_on_apply {
                    mark_replacement_consumed(state, rid);
                }
                // CR 615.5: A prevention effect's additional effect (e.g.
                // Phyrexian Hydra's "Put a -1/-1 counter on ~ for each 1 damage
                // prevented this way") is stashed as a post-replacement effect
                // and runs immediately after the prevention takes place. The
                // prevention applier has already stamped `last_effect_count`
                // with the prevented amount so `EventContextAmount` resolves
                // correctly when the follow-up effect fires.
                //
                // CR 615.5 + CR 609.7 + CR 614.12a: Stash the *prevented event's*
                // damage source so `TargetFilter::PostReplacementSourceController`
                // can resolve "the source's controller draws cards" follow-ups
                // (Swans of Bryn Argoll). Distinct from `post_replacement_source`,
                // which is the replacement's own source (Swans itself).
                if let Some(post) = mandatory_post_effect {
                    stash_post_replacement_continuation(
                        state,
                        post,
                        rid.source,
                        proposed_damage_source,
                        proposed_damage_target.clone(),
                    );
                }
                events.push(GameEvent::ReplacementApplied {
                    source_id: rid.source,
                    event_type,
                });
                return Err(ApplyResult::Prevented);
            }
        }
    }
    Ok(proposed)
}

/// CR 616.1: When two or more replacement and/or prevention effects apply to the
/// same event, the affected object's controller chooses one to apply, then the
/// process repeats (CR 616.1f) over the still-applicable effects. The engine
/// surfaces that choice as a prompt.
///
/// This predicate is a sound *observational-equivalence optimization*: the CR
/// has no "skip the prompt" provision, but when every candidate ordering yields
/// an identical final outcome the prompt is degenerate and may be skipped
/// without changing the result. The auto-resolve path still iterates per the
/// CR 616.1f repeat semantics — it only suppresses a player choice that cannot
/// affect anything.
/// A candidate set is *material* (the prompt must be shown) iff *either*:
/// - *any* candidate is an unconditionally order-sensitive shape — a
///   destination-redirecting `Effect::ChangeZone` (CR 614.6 — Rest in Peace
///   class; inspected via its own `destination`, not `is_event_modifier_effect`,
///   which classifies *all* `ChangeZone` as a pure modifier and would miss
///   exactly the material case), a controller override (CR 616.1b — "enters
///   under your control"), `Effect::BecomeCopy` / copy-as-it-enters
///   (CR 616.1c — Essence of the Wild), or a `null`-`execute` replacement
///   carrying an event-modifying side field (count/mana modification); *or*
/// - two or more candidates *modify the same* event field whose modifications do
///   not commute — e.g. a tapland's `Effect::Tap` and Spelunking's
///   `Effect::Untap` both write `enter_tapped` (last wins), or Doubling Season's
///   `Double` and Hardened Scales' `Plus` both modify an `AddCounter` count
///   (`Double` and `Plus` do not commute).
///
/// A single field-modifier with no peer is immaterial. Unrecognized effect
/// shapes default to MATERIAL — never auto-resolve a possibly order-sensitive
/// set; this conservative default also covers self-replacement effects
/// (CR 616.1a / CR 614.15).
pub(crate) fn replacement_ordering_is_material(
    state: &GameState,
    candidates: &[ReplacementId],
    proposed: &ProposedEvent,
) -> bool {
    let mut seen_writes: Vec<(EventField, CommuteClass)> = Vec::new();
    for rid in candidates {
        match candidate_materiality(state, *rid, proposed) {
            CandidateMateriality::Unconditional => return true,
            CandidateMateriality::Writes { field, commute } => {
                for (seen_field, seen_commute) in &seen_writes {
                    if *seen_field == field && !commute.commutes_with(*seen_commute) {
                        return true;
                    }
                }
                seen_writes.push((field, commute));
            }
            CandidateMateriality::Disjoint => {}
        }
    }
    false
}

/// An event field a non-redirecting replacement modifies. Two candidates
/// modifying the same field conflict when their modifications do not commute
/// (order-material, CR 616.1) — e.g. last-write-wins for `EnterTapped`, or
/// `Double` vs `Plus` for `Count`. Append-style fields (`enter_with_counters`
/// accumulates) are not collisions and are intentionally not modeled here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventField {
    /// `ZoneChange::enter_tapped` — overwritten by `Effect::Tap` / `Effect::Untap`.
    EnterTapped,
    /// The count of a count-bearing event (`AddCounter`, `CreateToken`, `Draw`,
    /// `Mill`, ...) — modified by a `quantity_modification` side field. Same-class
    /// arithmetic modifiers commute; mixed classes do not.
    Count,
    /// The produced mana type/amount of a `ProduceMana` event — modified by a
    /// `mana_modification` side field (`ReplaceWith` / `Multiply`).
    ManaType,
    /// The `amount` of a `ProposedEvent::Damage`, modified by a
    /// `damage_modification` side field (`Double` / `Triple` / `Plus` /
    /// `Minus` / `SetToSourcePower` / `SetTo`). Same-class arithmetic modifiers
    /// commute; mixed classes do not, e.g. Furnace of Rath `Double` + Torbran
    /// `Plus{2}`.
    Damage,
    /// `CreateToken::spec` — swapped by a full token-substitution replacement
    /// (`Effect::Token` execute payload, Divine Visitation class). Distinct from
    /// `Count`, which `quantity_modification` writers modify; the two commute
    /// when only multiplicative count modifiers are involved (double then
    /// substitute vs substitute then double yields the same batch).
    TokenSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommuteClass {
    NonCommuting,
    Multiplicative,
    Additive,
    Subtractive,
    /// Two replacements that set the same enter tap-state commute: the
    /// permanent enters with that state regardless of which is applied
    /// first, so the CR 616.1e/f ordering choice is immaterial. Keyed by
    /// the value written (not the direction) so that same-direction writes
    /// commute while opposite-direction writes (tap vs untap, where
    /// last-applied wins) stay `NonCommuting`.
    EnterTapped,
    EnterUntapped,
}

impl CommuteClass {
    fn commutes_with(self, other: Self) -> bool {
        self != Self::NonCommuting && self == other
    }
}

fn quantity_commute_class(modification: &QuantityModification) -> CommuteClass {
    match modification {
        // CR 616.1: all multiplicative modifiers commute with each other
        // (×2 then ×3 == ×3 then ×2), so Doubling Season + Ojer Taq auto-apply
        // without a degenerate ordering prompt — the same Multiplicative class
        // as `ManaModification::Multiply` and `DamageModification::Double/Triple`.
        QuantityModification::Times { .. } => CommuteClass::Multiplicative,
        // CR 616.1: integer halving (rounded down) does NOT commute with ×2 —
        // e.g. count 3 gives ×2÷2 = 3 but ÷2×2 = 2 — so it cannot share the
        // Multiplicative commuting class. The affected player must always choose
        // the application order (it is its own non-commuting class).
        QuantityModification::Half => CommuteClass::NonCommuting,
        QuantityModification::Plus { .. } => CommuteClass::Additive,
        QuantityModification::Minus { .. } => CommuteClass::Subtractive,
        QuantityModification::Prevent => CommuteClass::NonCommuting,
    }
}

fn damage_commute_class(modification: &DamageModification) -> CommuteClass {
    match modification {
        DamageModification::Double | DamageModification::Triple => CommuteClass::Multiplicative,
        DamageModification::Plus { .. } => CommuteClass::Additive,
        DamageModification::Minus { .. } => CommuteClass::Subtractive,
        DamageModification::SetToSourcePower
        | DamageModification::SetTo { .. }
        | DamageModification::LifeFloor { .. } => CommuteClass::NonCommuting,
    }
}

/// CR 106.12b + CR 616.1: Mana-production modifiers on the same `ProduceMana`
/// event. `Multiply` modifiers commute (×2 then ×3 == ×3 then ×2), so Mana
/// Reflection + Nyxbloom Ancient auto-apply without a degenerate ordering prompt.
fn mana_commute_class(modification: &crate::types::ability::ManaModification) -> CommuteClass {
    use crate::types::ability::ManaModification;
    match modification {
        ManaModification::Multiply { .. } => CommuteClass::Multiplicative,
        ManaModification::ReplaceWith { .. } => CommuteClass::NonCommuting,
    }
}

/// CR 616.1 classification of a single replacement candidate.
enum CandidateMateriality {
    /// An order-sensitive shape regardless of the other candidates (zone
    /// redirect, controller override, copy-as-it-enters).
    Unconditional,
    /// A pure event-field modifier. Immaterial alone; material iff another
    /// candidate modifies the same field with a non-commuting modification.
    Writes {
        field: EventField,
        commute: CommuteClass,
    },
    /// Touches no event field that another candidate could also touch
    /// (`Effect::Choose` post-effect, null/no-op pass-through with no side field).
    Disjoint,
}

/// CR 616.1: classify a candidate. A `null`-`execute` replacement is *not* a
/// guaranteed no-op — it can carry an event-modifying side field
/// (`quantity_modification` / `mana_modification` / `damage_modification`) that
/// mutates the event's count, mana type, or damage amount (Doubling Season,
/// Hardened Scales, Contamination, Furnace of Rath). When `execute` is present,
/// inspects the root `Effect` and walks `sub_ability` directly —
/// `first_non_modifier_ability` skips over `ChangeZone` links, so it cannot
/// surface the material redirect case. Unrecognized effect shapes default to
/// `Unconditional` (conservative — never auto-resolve a possibly order-sensitive
/// set).
///
/// CR 616.1d: `ProposedEvent::ZoneChange::enter_transformed` ("enters with its
/// back face up") is a forced-choice category, but it has no `*_modification`
/// side field on `ReplacementDefinition` and no replacement-pipeline write path
/// at all — it is an immutable event-construction property, set only when the
/// event is built (`stack.rs` / `triggers.rs` / `flip_coin.rs`) and never
/// mutated while replacements are applied. Two replacements therefore cannot
/// collide on it, so there is no `execute:null` collision to model and no
/// `EventField::Transformed`.
fn candidate_materiality(
    state: &GameState,
    rid: ReplacementId,
    proposed: &ProposedEvent,
) -> CandidateMateriality {
    let proposed_to = match proposed {
        ProposedEvent::ZoneChange { to, .. } => Some(*to),
        _ => None,
    };
    if is_compleated_replacement(rid) {
        return CandidateMateriality::Writes {
            field: EventField::Count,
            commute: CommuteClass::Subtractive,
        };
    }

    // CR 614.10: the turn-scoped combat skip fully prevents the BeginPhase event,
    // so it is unconditional like the umbra-armor / shield-counter destroy.
    if is_turn_scoped_combat_skip_replacement(rid) {
        return CandidateMateriality::Unconditional;
    }

    match shield_counter_replacement_kind(rid) {
        Some(ShieldCounterReplacementKind::Destroy) => return CandidateMateriality::Unconditional,
        Some(ShieldCounterReplacementKind::Damage) => {
            return CandidateMateriality::Writes {
                field: EventField::Damage,
                commute: CommuteClass::NonCommuting,
            }
        }
        None => {}
    }

    // CR 702.89a: Umbra armor fully replaces the destruction (prevents it), so it
    // is unconditional like the shield-counter destroy replacement.
    if is_umbra_armor_replacement(rid) {
        return CandidateMateriality::Unconditional;
    }

    let repl_def = state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index));
    let Some(repl_def) = repl_def else {
        // Unknown definition — be conservative.
        return CandidateMateriality::Unconditional;
    };
    // CR 615 + CR 616.1: A damage prevention shield modifies the damage amount,
    // so it writes the `Damage` field and is order-material against any other
    // `Damage` writer — a doubler (Furnace of Rath `Double`), Torbran (`Plus`),
    // or another prevention shield — because prevent-then-double and
    // double-then-prevent do not commute ((3-2)*2 = 2 vs (3*2)-2 = 4). A bare
    // prevention shield leaves `execute`/`damage_modification` unset, so without
    // this it fell through to `Disjoint` and the CR 616.1 order choice was
    // silently skipped.
    if matches!(repl_def.shield_kind, ShieldKind::Prevention { .. }) {
        return CandidateMateriality::Writes {
            field: EventField::Damage,
            commute: CommuteClass::NonCommuting,
        };
    }
    let Some(execute) = repl_def.execute.as_deref() else {
        // CR 616.1: a `null` `execute` is not a guaranteed no-op. A count-event
        // replacement (Doubling Season, Hardened Scales) modifies the count via
        // `quantity_modification`; a `ProduceMana` replacement (Contamination,
        // Mana Reflection) modifies the produced mana via `mana_modification`;
        // a damage replacement (Furnace of Rath, Fiery Emancipation, Torbran)
        // modifies the amount via `damage_modification`. Two such candidates on
        // one event are order-material — `Double` and `Plus` do not commute
        // ((x*2)+2 vs (x+2)*2). A `null` `execute` with no side field is a
        // genuine pass-through (test fixtures, structural placeholders).
        if let Some(modification) = repl_def.quantity_modification.as_ref() {
            return CandidateMateriality::Writes {
                field: EventField::Count,
                commute: quantity_commute_class(modification),
            };
        }
        if let Some(modification) = repl_def.mana_modification.as_ref() {
            return CandidateMateriality::Writes {
                field: EventField::ManaType,
                commute: mana_commute_class(modification),
            };
        }
        if let Some(modification) = repl_def.damage_modification.as_ref() {
            return CandidateMateriality::Writes {
                field: EventField::Damage,
                commute: damage_commute_class(modification),
            };
        }
        return CandidateMateriality::Disjoint;
    };
    // CR 616.1: a proliferate count-doubler ("proliferate twice instead",
    // Tekuthal) multiplies the proliferate action count via a `Multiply`
    // `repeat_for`. Two such doublers commute (x2 then x2 == x2 then x2 == x4),
    // so the ordering is immaterial and they must auto-apply — mirroring the
    // `QuantityModification::DOUBLE` -> `Multiplicative` count-write path. Without
    // this they fall to the conservative `Unconditional` default below and force
    // a degenerate CR 616.1 ordering choice. (A non-`Multiply` `repeat_for` is not
    // a doubler and correctly falls through to the conservative default.)
    if matches!(&*execute.effect, Effect::Proliferate)
        && matches!(execute.repeat_for, Some(QuantityExpr::Multiply { .. }))
    {
        return CandidateMateriality::Writes {
            field: EventField::Count,
            commute: CommuteClass::Multiplicative,
        };
    }
    let mut field: Option<EventField> = None;
    let mut enter_tapped_commute: Option<CommuteClass> = None;
    let mut current = Some(execute);
    while let Some(def) = current {
        match &*def.effect {
            // CR 614.6: a destination-redirecting ChangeZone (graveyard→exile,
            // etc.) is the material case. A ChangeZone whose destination equals
            // the proposed `to` zone is not a redirect.
            Effect::ChangeZone { destination, .. } if proposed_to != Some(*destination) => {
                return CandidateMateriality::Unconditional;
            }
            // CR 616.1b: a non-redirecting ChangeZone (destination matches the
            // proposed `to` zone) is not ordering-material on its own.
            Effect::ChangeZone { .. } => {}
            _ if effect_overrides_controller(&def.effect) => {
                return CandidateMateriality::Unconditional;
            }
            // CR 616.1c: copy-as-it-enters strips another replacement's source.
            Effect::BecomeCopy { .. } => return CandidateMateriality::Unconditional,
            // CR 614.1c: single-target `Tap`/`Untap` both overwrite the
            // `enter_tapped` field. CR 616.1e/f: ordering only matters when the
            // candidates would leave the permanent in *different* states.
            // Same-direction writes (two "enters tapped", or two "enters
            // untapped") are idempotent — the permanent enters with that state
            // regardless of order, so the choice is immaterial and no prompt is
            // shown. Opposite-direction writes (tapland + Spelunking / Archelos)
            // are last-applied-wins and stay `NonCommuting`. The mass scope is
            // not an ETB modifier and is not matched here.
            Effect::SetTapState {
                scope: EffectScope::Single,
                state,
                ..
            } => {
                field = Some(EventField::EnterTapped);
                // Keyed by the value written so opposite directions don't commute.
                enter_tapped_commute = Some(match state {
                    TapStateChange::Tap => CommuteClass::EnterTapped,
                    TapStateChange::Untap => CommuteClass::EnterUntapped,
                });
            }
            // ETB-counter replacements (`PutCounter`) only *append* to
            // `enter_with_counters`, so they never conflict. `Effect::Choose`
            // (the as-enters color choice) runs after the ZoneChange and
            // touches no shared event field. Both are explicitly recognized as
            // order-independent so they do NOT fall through to the conservative
            // material default below.
            Effect::PutCounter { .. } | Effect::Choose { .. } => {}
            // CR 614.1a + CR 111.1: Full token substitution on a CreateToken
            // event rewrites `CreateToken::spec` in the applier. Two different
            // substitutions on one event are last-applied-wins and stay
            // order-material; a single substitution commutes with count-only
            // writers on the `Count` field (Elspeth + Divine Visitation).
            // On non-CreateToken events (Draw→Token instead, Words of Wilding
            // class), the substitution fully replaces the event type — order
            // against a count modifier on the original event is material and
            // must stay conservative.
            Effect::Token { .. } if matches!(proposed, ProposedEvent::CreateToken { .. }) => {
                field = Some(EventField::TokenSpec);
                enter_tapped_commute = Some(CommuteClass::NonCommuting);
            }
            // CR 616.1: any unrecognized effect shape defaults to MATERIAL —
            // never auto-resolve a set whose order-sensitivity is unproven.
            _ => return CandidateMateriality::Unconditional,
        }
        current = def.sub_ability.as_deref();
    }
    match field {
        Some(field) => CandidateMateriality::Writes {
            field,
            commute: enter_tapped_commute.unwrap_or(CommuteClass::NonCommuting),
        },
        None => CandidateMateriality::Disjoint,
    }
}

/// CR 616.1b: True if an effect moves an object onto the battlefield under a
/// controller other than its owner ("enters under your control" class).
fn effect_overrides_controller(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::ChangeZone {
            enters_under: Some(_),
            ..
        }
    )
}

fn is_counter_placement_event(event: &ProposedEvent) -> bool {
    matches!(event, ProposedEvent::AddCounter { count, .. } if *count > 0)
        || matches!(
            event,
            ProposedEvent::MoveCounter {
                stage: CounterMoveStage::Add,
                add_count,
                ..
            } if *add_count > 0
        )
}

fn counter_placement_prevention_applies(state: &GameState, candidates: &[ReplacementId]) -> bool {
    candidates.iter().any(|rid| {
        state
            .objects
            .get(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get(rid.index))
            .is_some_and(|def| {
                def.event == ReplacementEvent::AddCounter
                    && def.quantity_modification == Some(QuantityModification::Prevent)
                    && !replacement_mode_is_optional(&def.mode)
            })
    })
}

fn pipeline_loop(
    state: &mut GameState,
    mut proposed: ProposedEvent,
    mut depth: u16,
    registry: &IndexMap<ReplacementEvent, ReplacementHandlerEntry>,
    events: &mut Vec<GameEvent>,
) -> ReplacementResult {
    loop {
        if depth >= MAX_REPLACEMENT_DEPTH {
            break;
        }

        let candidates = find_applicable_replacements(state, &proposed, registry);

        if candidates.is_empty() {
            break;
        }

        // CR 614.17c + CR 122.1: If a matching "can't get/have counters put
        // on" effect prevents this counter-placement event, non-self
        // replacement/prevention effects such as Doubling Season or Hardened
        // Scales cannot modify or replace it. The event simply cannot happen,
        // so there is no CR 616 ordering prompt.
        if is_counter_placement_event(&proposed)
            && counter_placement_prevention_applies(state, &candidates)
        {
            return ReplacementResult::Prevented;
        }

        if candidates.len() == 1 {
            let rid = candidates[0];

            // Check if this single candidate is Optional — if so, present as a choice
            let is_optional = state
                .objects
                .get(&rid.source)
                .and_then(|obj| obj.replacement_definitions.get(rid.index))
                .map(|repl| replacement_mode_is_optional(&repl.mode))
                .unwrap_or(false);

            if is_optional {
                let affected = proposed.affected_player(state);
                state.pending_replacement = Some(PendingReplacement {
                    proposed,
                    candidates,
                    depth,
                    is_optional: true,
                    // CR 701.24a: set by the W3 library-placement arm after parking
                    // (the pipeline doesn't know the caller's placement here).
                    library_placement: None,
                    // CR 614.12a: first park of this choice — no MayCost has been
                    // paid yet. Set only when re-parking after a paused accept.
                    may_cost_paid: false,
                    may_cost_remaining: None,
                });
                return ReplacementResult::NeedsChoice(affected);
            }

            proposed.mark_applied(rid);
            match apply_single_replacement(
                state,
                proposed,
                rid,
                ReplacementBranch::Execute,
                registry,
                events,
            ) {
                Ok(new_event) => proposed = new_event,
                Err(ApplyResult::Prevented) => return ReplacementResult::Prevented,
                Err(ApplyResult::Modified(_)) => unreachable!(),
            }
        } else if replacement_ordering_is_material(state, &candidates, &proposed) {
            // CR 616.1: If multiple replacement effects apply, the affected player
            // or controller of the affected object chooses which one to apply first,
            // even when every candidate is mandatory.
            let affected = proposed.affected_player(state);
            state.pending_replacement = Some(PendingReplacement {
                proposed,
                candidates,
                depth,
                is_optional: false,
                // CR 701.24a: set by the W3 library-placement arm after parking.
                library_placement: None,
                // CR 614.12a: distinct-replacement choices carry no MayCost.
                may_cost_paid: false,
                may_cost_remaining: None,
            });
            return ReplacementResult::NeedsChoice(affected);
        } else {
            // CR 616.1: the choice is degenerate here — every candidate ordering
            // yields an observationally identical outcome — so the prompt is
            // skipped. Auto-resolve: apply candidates[0] and re-loop, which
            // preserves the CR 616.1f repeat semantics (apply one, then repeat
            // over the still-applicable effects). All candidates still apply
            // exactly once.
            let rid = candidates[0];
            proposed.mark_applied(rid);
            match apply_single_replacement(
                state,
                proposed,
                rid,
                ReplacementBranch::Execute,
                registry,
                events,
            ) {
                Ok(new_event) => proposed = new_event,
                Err(ApplyResult::Prevented) => return ReplacementResult::Prevented,
                Err(ApplyResult::Modified(_)) => unreachable!(),
            }
        }

        depth += 1;
    }

    ReplacementResult::Execute(proposed)
}

pub fn replace_event(
    state: &mut GameState,
    proposed: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> ReplacementResult {
    let registry = build_replacement_registry();
    pipeline_loop(state, proposed, 0, &registry, events)
}

/// CR 510.2 + CR 615.7 + CR 615.13: Run the replacement pipeline over a whole
/// simultaneous combat-damage batch.
///
/// Each proposed `Damage` event is passed through `replace_event` individually
/// (the pipeline is inherently per-event), but for the duration of the batch
/// `state.combat_prevention_tally` is active: the damage-replacement applier's
/// `Prevention::All` branch routes each prevented amount into a per-shield
/// aggregate keyed by `ReplacementId` instead of stamping `last_effect_count`
/// or emitting a per-source `DamagePrevented`. `Prevention::Next(N)` shields
/// keep the existing per-event sequential path — depletion-style shields are
/// not aggregated here.
///
/// `// strict-failure: CR 615.7 multi-source Next(N) prevention requires a
/// player choice — out of scope (#314 is Prevention::All)`. When two or more
/// `Next(N)` shields apply to the same simultaneous batch, CR 615.7 requires
/// the shielded player to choose which damage each shield prevents; that
/// player-choice path is not modeled — the shields apply per-event in pipeline
/// order instead.
///
/// Returns a vector aligned 1:1 with `proposed`: `Some(event)` is a survivor
/// post-replacement `Damage` event for `combat_damage.rs` Phase C to apply;
/// `None` means that source's damage was fully prevented or skipped. The
/// `HashMap` is the per-`Prevention::All`-shield aggregate prevented amount.
pub(crate) fn replace_combat_damage_batch(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    proposed: Vec<ProposedEvent>,
) -> (Vec<Option<ProposedEvent>>, HashMap<ReplacementId, i32>) {
    let registry = build_replacement_registry();

    // CR 510.2: Activate the batch tally so the applier aggregates per shield.
    let restore_tally = state.combat_prevention_tally.take();
    state.combat_prevention_tally = Some(HashMap::new());

    let mut survivors = Vec::with_capacity(proposed.len());
    for event in proposed {
        let result = pipeline_loop(state, event, 0, &registry, events);
        // CR 615.5: A `Prevention::Next(N)` shield's rider is stashed per-event
        // by the applier (the `Prevention::All` batch path suppresses its stash
        // and fires once post-batch instead). Resolve any such per-event
        // continuation inline — for both full prevention (`Prevented`) and
        // partial prevention (`Modified` → `Execute`) — so a depletion-shield
        // rider fires "immediately afterward" and never leaks past the batch.
        if !matches!(result, ReplacementResult::NeedsChoice(_))
            && state.post_replacement_continuation.is_some()
        {
            let _ = crate::game::engine_replacement::apply_pending_post_replacement_effect(
                state, None, None, None, events,
            );
        }
        match result {
            ReplacementResult::Execute(survivor) => survivors.push(Some(survivor)),
            ReplacementResult::Prevented => {
                survivors.push(None);
            }
            ReplacementResult::NeedsChoice(_) => {
                // CR 510.2: Combat damage cannot pause for a replacement
                // ordering choice. Mirror the legacy per-event behavior
                // (`apply_damage_to_target`'s combat `NeedsChoice` arm) — skip
                // this source's damage. Clear the pending pause so it does not
                // leak out of the batch.
                state.pending_replacement = None;
                survivors.push(None);
            }
        }
    }

    let tally = state.combat_prevention_tally.take().unwrap_or_default();
    state.combat_prevention_tally = restore_tally;
    (survivors, tally)
}

pub fn continue_replacement(
    state: &mut GameState,
    chosen_index: usize,
    events: &mut Vec<GameEvent>,
) -> ReplacementResult {
    let pending = match state.pending_replacement.take() {
        Some(p) => p,
        None => {
            return ReplacementResult::Execute(ProposedEvent::Draw {
                player_id: PlayerId(0),
                count: 0,
                applied: std::collections::HashSet::new(),
            });
        }
    };

    let registry = build_replacement_registry();

    // Optional replacement: index 0 = accept, index 1 = decline
    if pending.is_optional {
        let rid = pending.candidates[0];
        let payer = pending.proposed.affected_player(state);
        // CR 614.12a: a `true` flag means this is the post-choice resume of an
        // accept whose `MayCost` payment paused for an interactive sub-choice
        // (e.g. a `DiscardChoice`). Re-park fields are captured up front so a
        // fresh pause can re-stash the same record.
        let resuming_after_paid_cost = pending.may_cost_paid;
        let remaining_may_cost = pending.may_cost_remaining.clone();
        let reparked_candidates = pending.candidates.clone();
        let reparked_depth = pending.depth;
        let reparked_library_placement = pending.library_placement.clone();
        let mut proposed = pending.proposed;
        proposed.mark_applied(rid);

        // Extract the accept/decline effects before applying
        let (accept_effect, decline_effect, may_cost) = state
            .objects
            .get(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get(rid.index))
            .map(|repl| {
                let accept = repl.execute.clone();
                let decline = replacement_mode_decline_cloned(&repl.mode);
                let may_cost = match &repl.mode {
                    ReplacementMode::MayCost { cost, .. } => Some(cost.clone()),
                    ReplacementMode::Mandatory | ReplacementMode::Optional { .. } => None,
                };
                (accept, decline, may_cost)
            })
            .unwrap_or((None, None, None));

        // CR 614.12a: on accept, pay the MayCost (skipped on a paid resume). A
        // `PausedForChoice` outcome means the payment surfaced an interactive
        // sub-choice (`WaitingFor` already set) — re-park the SAME pending record
        // with `may_cost_paid: true` plus any unpaid suffix so the post-choice
        // resume re-enters here, continues payment, and finishes entering the
        // permanent. The permanent must NOT enter until the card actually leaves
        // the hand.
        let pay_outcome = if chosen_index != 0 {
            MayCostOutcome::Unpaid
        } else if resuming_after_paid_cost {
            match &remaining_may_cost {
                None => MayCostOutcome::Paid,
                Some(cost) => pay_replacement_may_cost(state, payer, rid.source, cost, events),
            }
        } else {
            match &may_cost {
                None => MayCostOutcome::Paid,
                Some(cost) => pay_replacement_may_cost(state, payer, rid.source, cost, events),
            }
        };

        let paid_may_cost = match pay_outcome {
            MayCostOutcome::Paid => true,
            MayCostOutcome::Unpaid => false,
            MayCostOutcome::PausedForChoice { remaining_cost } => {
                // CR 614.12a: the payment surfaced an interactive sub-choice (e.g. a
                // `DiscardChoice`); `state.waiting_for` is already set to it. Re-park
                // the SAME pending record with `may_cost_paid: true` and flag the
                // pause so `handle_replacement_choice` surfaces the live sub-choice
                // (not a fresh ReplacementChoice). The permanent enters only when
                // the resume finishes any `may_cost_remaining`. The carried
                // `Execute` payload is inert — the flag short-circuits the caller
                // before it is read.
                state.pending_replacement = Some(crate::types::game_state::PendingReplacement {
                    proposed: proposed.clone(),
                    candidates: reparked_candidates,
                    depth: reparked_depth,
                    is_optional: true,
                    library_placement: reparked_library_placement,
                    may_cost_paid: true,
                    may_cost_remaining: remaining_cost,
                });
                state.replacement_may_cost_paused = true;
                return ReplacementResult::Execute(proposed);
            }
        };

        let (branch, post_effect) = if chosen_index == 0 && paid_may_cost {
            // CR 614.1c: Accept path — walk past modifier-only effects (already
            // applied to ProposedEvent by event_modifiers_for_ability) to find the
            // first non-modifier as the real post-replacement work. Covers composed
            // replacements like Tap → BecomeCopy (Vesuva "enter tapped as a copy").
            let real_work = accept_effect.as_deref().and_then(|def| {
                EventModifiers::first_non_modifier_ability(Some(def))
                    .map(|work| Box::new(work.clone()))
            });
            let post = if real_work.is_some() {
                real_work
            } else if EventModifiers::has_only_event_modifier(accept_effect.as_deref()) {
                None
            } else {
                accept_effect
            };
            (ReplacementBranch::Execute, post)
        } else {
            // CR 614.1c + CR 614.12: Decline's ProposedEvent modifications (enter_tapped,
            // counters, zone redirect) must flow through the replacement pipeline so the
            // next iteration sees the current state of the event. If the decline branch
            // is a pure event modifier (e.g., shock-land Tap SelfRef), no post-effect is
            // needed — the modifier has already been applied to the ProposedEvent.
            // If the decline branch has non-modifier work (e.g., a choice side-effect),
            // it is retained as a post-replacement side effect.
            let post = if EventModifiers::has_only_event_modifier(decline_effect.as_deref()) {
                None
            } else {
                decline_effect
            };
            (ReplacementBranch::Decline, post)
        };

        // CR 614.12a: Optional accept/decline branches always derive a Template
        // continuation — the post-effect is built from the ReplacementDefinition's
        // `execute`/`decline` AST, never from a captured runtime resolution.
        // Set BEFORE `apply_single_replacement` so per-event appliers (e.g.,
        // `draw_applier`) can see the continuation slot and suppress the
        // original event when its replacement is a non-modifier chain
        // (CR 614.6: the draw never happens when fully replaced).
        if post_effect.is_some() {
            state.post_replacement_source = Some(rid.source);
            // CR 615.5 + CR 609.7: Optional/decline post-effects don't carry
            // prevention-event-source semantics — clear so a prior prevention
            // can't leak into a non-prevention stash.
            state.post_replacement_event_source = None;
            state.post_replacement_event_target = None;
        }
        state.post_replacement_continuation =
            post_effect.map(PostReplacementContinuation::Template);

        match apply_single_replacement(state, proposed, rid, branch, &registry, events) {
            Ok(new_event) => proposed = new_event,
            Err(ApplyResult::Prevented) => return ReplacementResult::Prevented,
            Err(ApplyResult::Modified(_)) => unreachable!(),
        }

        return pipeline_loop(state, proposed, pending.depth + 1, &registry, events);
    }

    if chosen_index >= pending.candidates.len() {
        return ReplacementResult::Execute(pending.proposed);
    }

    let rid = pending.candidates[chosen_index];
    let mut proposed = pending.proposed;
    proposed.mark_applied(rid);

    match apply_single_replacement(
        state,
        proposed,
        rid,
        ReplacementBranch::Execute,
        &registry,
        events,
    ) {
        Ok(new_event) => proposed = new_event,
        Err(ApplyResult::Prevented) => return ReplacementResult::Prevented,
        Err(ApplyResult::Modified(_)) => unreachable!(),
    }

    pipeline_loop(state, proposed, pending.depth + 1, &registry, events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::effects::token::apply_create_token_after_replacement;
    use crate::game::game_object::{AttachTarget, GameObject};
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, CastManaObjectScope, CastManaSpentMetric,
        ChosenAttribute, ControllerRef, Effect, FilterProp, OriginConstraint, QuantityExpr,
        QuantityModification, QuantityRef, ReplacementDefinition, ReplacementMode,
        ReplacementPlayerScope, TargetFilter, TargetRef, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{DamageRecord, ManaSpentSourceSnapshot};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;
    use crate::types::proposed_event::{EtbTapState, TokenSpec};
    use crate::types::replacements::ReplacementEvent;
    use std::collections::HashSet;

    fn make_repl(event: ReplacementEvent) -> ReplacementDefinition {
        ReplacementDefinition::new(event)
    }

    /// Placeholder event for `evaluate_replacement_condition` callers that
    /// aren't exercising event-contextual conditions (`OnlyExtraTurn`). A
    /// natural-turn BeginTurn is inert against all state-based conditions.
    fn dummy_begin_turn_event() -> ProposedEvent {
        ProposedEvent::begin_turn(PlayerId(0), false)
    }

    #[test]
    fn extract_etb_counters_walks_sub_ability_chain() {
        let state = GameState::new_two_player(42);
        let mut first = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
        );
        first.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Generic("shield".to_string()),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
        )));
        let event = ProposedEvent::zone_change(ObjectId(1), Zone::Stack, Zone::Battlefield, None);

        assert_eq!(
            extract_etb_counters(Some(&first), &state, ObjectId(1), &event),
            vec![
                (CounterType::Plus1Plus1, 1),
                (CounterType::Generic("shield".to_string()), 1)
            ]
        );
    }

    #[test]
    fn choose_then_chosen_dependent_counter_defers_to_post_replacement() {
        let choose = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: crate::types::ability::ChoiceType::CreatureType,
                persist: true,
                selection: crate::types::ability::TargetSelectionMode::Chosen,
            },
        );
        let mut execute = choose;
        execute.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Generic("fellowship".to_string()),
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(
                            TypedFilter::creature()
                                .controller(crate::types::ability::ControllerRef::You)
                                .properties(vec![FilterProp::IsChosenCreatureType]),
                        ),
                    },
                },
                target: TargetFilter::SelfRef,
            },
        )));
        let repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(execute)
            .valid_card(TargetFilter::SelfRef);
        let mut state = test_state_with_object(ObjectId(10), Zone::Hand, vec![repl]);
        let mut events = Vec::new();
        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Hand, Zone::Battlefield, None);

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::ZoneChange {
            enter_with_counters,
            ..
        }) = result
        else {
            panic!("expected Execute with ZoneChange, got {result:?}");
        };

        assert!(
            enter_with_counters.is_empty(),
            "chosen-dependent counters must not fold pre-choice"
        );
        assert!(
            state.post_replacement_continuation.is_some(),
            "Choose + chosen-dependent PutCounter must stash post-replacement work"
        );
    }

    #[test]
    fn chained_etb_modifiers_do_not_stash_post_replacement_continuation() {
        let mut enter_tapped = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        );
        enter_tapped.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Stun,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
        )));
        let repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(enter_tapped)
            .valid_card(TargetFilter::SelfRef);
        let mut state = test_state_with_object(ObjectId(10), Zone::Hand, vec![repl]);
        let mut events = Vec::new();
        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Hand, Zone::Battlefield, None);

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::ZoneChange {
            enter_tapped,
            enter_with_counters,
            ..
        }) = result
        else {
            panic!("expected Execute with ZoneChange, got {result:?}");
        };

        assert!(enter_tapped.resolve(false));
        assert_eq!(enter_with_counters, vec![(CounterType::Stun, 1)]);
        assert!(
            state.post_replacement_continuation.is_none(),
            "pure ETB modifier chains must not be replayed after the event"
        );
    }

    fn test_state_with_object(
        obj_id: ObjectId,
        zone: Zone,
        replacements: Vec<ReplacementDefinition>,
    ) -> GameState {
        let mut state = GameState::new_two_player(42);
        let mut obj = GameObject::new(obj_id, CardId(1), PlayerId(0), "Test".to_string(), zone);
        obj.replacement_definitions = replacements.into();
        state.objects.insert(obj_id, obj);
        if zone == Zone::Battlefield {
            state.battlefield.push_back(obj_id);
        }
        state
    }

    fn resolve_first_replacement_choice(
        state: &mut GameState,
        result: ReplacementResult,
        events: &mut Vec<GameEvent>,
    ) -> ReplacementResult {
        match result {
            ReplacementResult::NeedsChoice(_) => continue_replacement(state, 0, events),
            other => other,
        }
    }

    fn may_cost_tapped_replacement(amount: i32) -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .mode(ReplacementMode::MayCost {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: amount },
                },
                decline: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SetTapState {
                        target: TargetFilter::SelfRef,
                        scope: EffectScope::Single,
                        state: TapStateChange::Tap,
                    },
                ))),
            })
            .valid_card(TargetFilter::SelfRef)
    }

    #[test]
    fn may_cost_replacement_accept_pays_cost_and_keeps_event_untapped() {
        let repl = may_cost_tapped_replacement(2);
        let mut state = test_state_with_object(ObjectId(10), Zone::Hand, vec![repl]);
        let mut events = Vec::new();
        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Hand, Zone::Battlefield, None);

        let result = replace_event(&mut state, proposed, &mut events);
        assert!(matches!(
            result,
            ReplacementResult::NeedsChoice(PlayerId(0))
        ));

        let result = continue_replacement(&mut state, 0, &mut events);
        let ReplacementResult::Execute(ProposedEvent::ZoneChange { enter_tapped, .. }) = result
        else {
            panic!("expected zone change execute");
        };
        assert!(!enter_tapped.resolve(false));
        assert_eq!(state.players[0].life, 18);
    }

    #[test]
    fn may_cost_replacement_decline_applies_decline_branch() {
        let repl = may_cost_tapped_replacement(2);
        let mut state = test_state_with_object(ObjectId(10), Zone::Hand, vec![repl]);
        let mut events = Vec::new();
        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Hand, Zone::Battlefield, None);

        let result = replace_event(&mut state, proposed, &mut events);
        assert!(matches!(
            result,
            ReplacementResult::NeedsChoice(PlayerId(0))
        ));

        let result = continue_replacement(&mut state, 1, &mut events);
        let ReplacementResult::Execute(ProposedEvent::ZoneChange { enter_tapped, .. }) = result
        else {
            panic!("expected zone change execute");
        };
        assert!(enter_tapped.resolve(false));
        assert_eq!(state.players[0].life, 20);
    }

    #[test]
    fn test_single_replacement_zone_change() {
        // Creature with Moved replacement (no params means handler applies with default behavior)
        let repl = make_repl(ReplacementEvent::Moved);
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Battlefield, Zone::Graveyard, None);

        let result = replace_event(&mut state, proposed, &mut events);

        // With empty params, the Moved handler applies default behavior (fallback: stay in origin)
        match result {
            ReplacementResult::Execute(ProposedEvent::ZoneChange { .. }) => {
                // Replacement was applied
            }
            other => panic!("expected Execute with ZoneChange, got {:?}", other),
        }
        // Should have emitted a ReplacementApplied event
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::ReplacementApplied {
                event_type,
                ..
            } if event_type == "Moved"
        )));
    }

    #[test]
    fn test_once_per_event_enforcement() {
        // CR 616.1f: two bare (null/no-op) mandatory Moved replacements on the
        // same object are immaterial — neither can change the other's
        // applicability — so the pipeline auto-resolves without a prompt. The
        // once-per-event invariant (each applies exactly once) is unchanged.
        let repl1 = make_repl(ReplacementEvent::Moved);
        let repl2 = make_repl(ReplacementEvent::Moved);
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl1, repl2]);
        let mut events = Vec::new();

        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Battlefield, Zone::Graveyard, None);

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(event) = result else {
            panic!("expected Execute (immaterial auto-resolve), got {result:?}");
        };
        assert_eq!(
            event.applied_set().len(),
            2,
            "both replacements should have been applied exactly once"
        );
    }

    #[test]
    fn test_multiple_immaterial_replacements_auto_resolve() {
        // CR 616.1f: two bare Moved replacements on *different* objects are also
        // immaterial — the pipeline auto-resolves both without a prompt.
        let repl = make_repl(ReplacementEvent::Moved);

        let mut state = GameState::new_two_player(42);

        let mut obj1 = GameObject::new(
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Obj1".to_string(),
            Zone::Battlefield,
        );
        obj1.replacement_definitions = vec![repl.clone()].into();

        let mut obj2 = GameObject::new(
            ObjectId(20),
            CardId(2),
            PlayerId(0),
            "Obj2".to_string(),
            Zone::Battlefield,
        );
        obj2.replacement_definitions = vec![repl].into();

        state.objects.insert(ObjectId(10), obj1);
        state.objects.insert(ObjectId(20), obj2);
        state.battlefield.push_back(ObjectId(10));
        state.battlefield.push_back(ObjectId(20));

        let target = GameObject::new(
            ObjectId(30),
            CardId(3),
            PlayerId(0),
            "Target".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(30), target);

        let mut events = Vec::new();
        let proposed = ProposedEvent::ZoneChange {
            object_id: ObjectId(30),
            from: Zone::Battlefield,
            to: Zone::Graveyard,
            cause: None,
            attach_to: None,
            enter_tapped: EtbTapState::Unspecified,
            enter_with_counters: Vec::new(),
            controller_override: None,
            enter_transformed: false,
            applied: HashSet::new(),
            face_down_profile: None,
        };
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(event) = result else {
            panic!("expected Execute (immaterial auto-resolve), got {result:?}");
        };
        assert_eq!(
            event.applied_set().len(),
            2,
            "both replacements should have applied"
        );
    }

    /// Build a Moved replacement whose `execute` redirects a zone change to a
    /// specific destination — a genuine destination-redirecting `ChangeZone`
    /// (Rest in Peace class). Such replacements are ordering-material (CR 614.6).
    fn redirect_repl(destination: Zone) -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Moved).execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: Vec::new(),
                face_down_profile: None,
            },
        ))
    }

    #[test]
    fn test_material_replacement_ordering_still_prompts() {
        // CR 616.1f: two genuine zone-redirect replacements on different sources,
        // each sending the object to a *different* destination zone. Applying one
        // changes whether the other still applies, so the ordering is material —
        // the CR 616.1 prompt must still be surfaced.
        let mut state = GameState::new_two_player(42);

        let mut obj1 = GameObject::new(
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "RedirectToExile".to_string(),
            Zone::Battlefield,
        );
        obj1.replacement_definitions = vec![redirect_repl(Zone::Exile)].into();

        let mut obj2 = GameObject::new(
            ObjectId(20),
            CardId(2),
            PlayerId(0),
            "RedirectToLibrary".to_string(),
            Zone::Battlefield,
        );
        obj2.replacement_definitions = vec![redirect_repl(Zone::Library)].into();

        state.objects.insert(ObjectId(10), obj1);
        state.objects.insert(ObjectId(20), obj2);
        state.battlefield.push_back(ObjectId(10));
        state.battlefield.push_back(ObjectId(20));

        let target = GameObject::new(
            ObjectId(30),
            CardId(3),
            PlayerId(0),
            "Target".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(30), target);

        let mut events = Vec::new();
        let proposed =
            ProposedEvent::zone_change(ObjectId(30), Zone::Battlefield, Zone::Graveyard, None);
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice for material ordering, got {result:?}");
        };
        assert_eq!(player, PlayerId(0));
    }

    fn compleated_doubling_order_result(choice: usize) -> u32 {
        let compleated = ObjectId(10);
        let doubling_season = ObjectId(20);

        let mut state = GameState::new_two_player(42);
        let mut walker = GameObject::new(
            compleated,
            CardId(1),
            PlayerId(0),
            "Compleated Walker".to_string(),
            Zone::Battlefield,
        );
        walker.card_types.core_types.push(CoreType::Planeswalker);
        walker.keywords.push(Keyword::Compleated);
        walker.phyrexian_life_paid = 3;
        state.objects.insert(compleated, walker);
        state.battlefield.push_back(compleated);

        let mut doubler = GameObject::new(
            doubling_season,
            CardId(2),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        doubler.replacement_definitions =
            vec![ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .quantity_modification(QuantityModification::DOUBLE)]
            .into();
        state.objects.insert(doubling_season, doubler);
        state.battlefield.push_back(doubling_season);

        let proposed = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: compleated,
                counter_type: CounterType::Loyalty,
            },
            count: 5,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected Compleated/Doubling replacement choice, got {result:?}");
        };
        assert_eq!(player, PlayerId(0));

        let result = continue_replacement(&mut state, choice, &mut events);
        let ReplacementResult::Execute(ProposedEvent::AddCounter { count, .. }) = result else {
            panic!("expected accepted AddCounter after replacement choice, got {result:?}");
        };
        count
    }

    #[test]
    fn compleated_and_doubling_season_order_is_material() {
        // CR 702.150a + CR 616.1: Compleated's loyalty reduction and a Doubling
        // Season-class counter doubler do not commute. Loyalty 5 with three
        // Phyrexian symbols paid by life is either (5 - 6) * 2 = 0 or
        // (5 * 2) - 6 = 4 depending on the affected player's chosen order.
        assert_eq!(compleated_doubling_order_result(0), 0);
        assert_eq!(compleated_doubling_order_result(1), 4);
    }

    #[test]
    fn tap_untap_field_collision_prompts_for_order() {
        // CR 616.1: two `Moved` replacements that both modify the `enter_tapped`
        // field of a single `ZoneChange` event — one `Effect::Tap` (the
        // tapland's own "enters tapped"), one `Effect::Untap` (a Spelunking-style
        // "lands enter untapped"). The modifications do not commute (last wins),
        // so the ordering is material and the prompt must be surfaced. Directly
        // exercises the `Writes`-collision branch.
        let tap_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield);
        let untap_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Untap,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield);
        let mut state =
            test_state_with_object(ObjectId(10), Zone::Hand, vec![tap_repl, untap_repl]);
        let mut events = Vec::new();
        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Hand, Zone::Battlefield, None);

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice for enter_tapped field collision, got {result:?}");
        };
        assert_eq!(player, PlayerId(0));
    }

    #[test]
    fn two_identical_untap_replacements_auto_apply_without_choice() {
        // CR 616.1f: Duplicate "lands enter untapped" replacements commute (#1340).
        let untap_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Untap,
                },
            ))
            .valid_card(TargetFilter::Typed(
                TypedFilter::land().controller(ControllerRef::You),
            ))
            .destination_zone(Zone::Battlefield);
        let mut state = test_state_with_object(
            ObjectId(1),
            Zone::Battlefield,
            vec![untap_repl.clone(), untap_repl],
        );
        let land_id = ObjectId(10);
        state.objects.insert(
            land_id,
            GameObject::new(
                land_id,
                CardId(2),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Hand,
            ),
        );

        let mut events = Vec::new();
        let proposed = ProposedEvent::zone_change(land_id, Zone::Hand, Zone::Battlefield, None);
        let result = replace_event(&mut state, proposed, &mut events);
        assert!(
            matches!(result, ReplacementResult::Execute(_)),
            "identical untap replacements must auto-apply without ordering prompt, got {result:?}"
        );
    }

    #[test]
    fn two_identical_tap_replacements_auto_apply_without_choice() {
        // CR 616.1e/f: Two "enters tapped" replacements (Kismet + Frozen Aether)
        // are idempotent — the permanent enters tapped regardless of order, so
        // the ordering choice is immaterial and no prompt is shown. This is the
        // symmetric counterpart of the untap case (#1340): materiality keys on
        // the value written, not the tap-direction.
        let tap_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ))
            .destination_zone(Zone::Battlefield);
        let mut state = test_state_with_object(
            ObjectId(1),
            Zone::Battlefield,
            vec![tap_repl.clone(), tap_repl],
        );
        let perm_id = ObjectId(10);
        state.objects.insert(
            perm_id,
            GameObject::new(
                perm_id,
                CardId(2),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Hand,
            ),
        );

        let mut events = Vec::new();
        let proposed = ProposedEvent::zone_change(perm_id, Zone::Hand, Zone::Battlefield, None);
        let result = replace_event(&mut state, proposed, &mut events);
        assert!(
            matches!(result, ReplacementResult::Execute(_)),
            "identical tap replacements must auto-apply without ordering prompt, got {result:?}"
        );
    }

    #[test]
    fn opposite_tap_state_replacements_prompt_for_order() {
        // CR 616.1e/f: One "enters tapped" + one "enters untapped" replacement
        // leave the permanent in *different* states depending on which is applied
        // last, so the ordering is material and the controller must choose
        // (tapland + Spelunking / Archelos). Guards against over-commuting the
        // value-keyed classes.
        let tap_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ))
            .destination_zone(Zone::Battlefield);
        let untap_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Untap,
                },
            ))
            .destination_zone(Zone::Battlefield);
        let mut state =
            test_state_with_object(ObjectId(1), Zone::Battlefield, vec![tap_repl, untap_repl]);
        let perm_id = ObjectId(10);
        state.objects.insert(
            perm_id,
            GameObject::new(
                perm_id,
                CardId(2),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Hand,
            ),
        );

        let mut events = Vec::new();
        let proposed = ProposedEvent::zone_change(perm_id, Zone::Hand, Zone::Battlefield, None);
        let result = replace_event(&mut state, proposed, &mut events);
        assert!(
            matches!(result, ReplacementResult::NeedsChoice(_)),
            "opposite tap-state replacements must prompt for order, got {result:?}"
        );
    }

    #[test]
    fn quantity_modification_field_collision_prompts_for_order() {
        // CR 616.1: Doubling Season (`Double`) and Hardened Scales (`Plus{1}`)
        // both modify the count of a single `AddCounter` event via the
        // `quantity_modification` side field — and these modifications do NOT
        // commute: (1+1)*2 = 4 vs (1*2)+1 = 3. Both replacements have a `null`
        // `execute`, so they would have classified `Disjoint` before the
        // side-field fix. The set must be material and surface the prompt.
        use crate::types::ability::QuantityModification;
        use crate::types::counter::CounterType;

        let doubling_season = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::DOUBLE);
        let hardened_scales = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Plus { value: 1 });

        let mut state = GameState::new_two_player(42);
        let mut src1 = GameObject::new(
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        src1.replacement_definitions = vec![doubling_season].into();
        let mut src2 = GameObject::new(
            ObjectId(20),
            CardId(2),
            PlayerId(0),
            "Hardened Scales".to_string(),
            Zone::Battlefield,
        );
        src2.replacement_definitions = vec![hardened_scales].into();
        state.objects.insert(ObjectId(10), src1);
        state.objects.insert(ObjectId(20), src2);
        state.battlefield.push_back(ObjectId(10));
        state.battlefield.push_back(ObjectId(20));

        let target = GameObject::new(
            ObjectId(30),
            CardId(3),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(30), target);

        let mut events = Vec::new();
        let proposed = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: ObjectId(30),
                counter_type: CounterType::Plus1Plus1,
            },
            count: 1,
            applied: HashSet::new(),
        };
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice for non-commuting count modification, got {result:?}");
        };
        assert_eq!(player, PlayerId(0));
    }

    /// CR 614.17c + CR 122.1: A matching "can't have counters put on it"
    /// effect makes the counter-placement event impossible before ordinary
    /// counter replacement ordering. Count modifiers such as Doubling Season
    /// therefore cannot create a CR 616 prompt against the prohibition.
    #[test]
    fn counter_prohibition_short_circuits_count_modifier_prompt() {
        let prevent_repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Prevent);
        let double_repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::DOUBLE);

        let mut state = GameState::new_two_player(42);
        let mut solemnity = GameObject::new(
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Solemnity".to_string(),
            Zone::Battlefield,
        );
        solemnity.replacement_definitions = vec![prevent_repl].into();
        let mut doubling_season = GameObject::new(
            ObjectId(20),
            CardId(2),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        doubling_season.replacement_definitions = vec![double_repl].into();
        state.objects.insert(ObjectId(10), solemnity);
        state.objects.insert(ObjectId(20), doubling_season);
        state.battlefield.push_back(ObjectId(10));
        state.battlefield.push_back(ObjectId(20));

        let target = GameObject::new(
            ObjectId(30),
            CardId(3),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(30), target);

        let proposed = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: ObjectId(30),
                counter_type: CounterType::Plus1Plus1,
            },
            count: 1,
            applied: HashSet::new(),
        };

        let mut events = Vec::new();
        assert_eq!(
            replace_event(&mut state, proposed, &mut events),
            ReplacementResult::Prevented
        );
    }

    #[test]
    fn damage_modification_field_collision_prompts_for_order() {
        // CR 616.1: Furnace of Rath (`Double`) and Torbran (`Plus{2}`) both
        // modify the `amount` of a single `ProposedEvent::Damage` via the
        // `damage_modification` side field — and these do NOT commute:
        // (x*2)+2 vs (x+2)*2. Both replacements have a `null` `execute`, so
        // they would classify `Disjoint` without the `damage_modification`
        // arm. The set must be material and surface the prompt.
        use crate::types::ability::DamageModification;

        let furnace_of_rath = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Double);
        let torbran = ReplacementDefinition::new(ReplacementEvent::DamageDone).damage_modification(
            DamageModification::Plus {
                value: QuantityExpr::Fixed { value: 2 },
            },
        );

        let mut state = GameState::new_two_player(42);
        let mut src1 = GameObject::new(
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Furnace of Rath".to_string(),
            Zone::Battlefield,
        );
        src1.replacement_definitions = vec![furnace_of_rath].into();
        let mut src2 = GameObject::new(
            ObjectId(20),
            CardId(2),
            PlayerId(0),
            "Torbran, Thane of Red Fell".to_string(),
            Zone::Battlefield,
        );
        src2.replacement_definitions = vec![torbran].into();
        state.objects.insert(ObjectId(10), src1);
        state.objects.insert(ObjectId(20), src2);
        state.battlefield.push_back(ObjectId(10));
        state.battlefield.push_back(ObjectId(20));

        let mut events = Vec::new();
        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice for non-commuting damage modification, got {result:?}");
        };
        assert_eq!(player, PlayerId(1));
    }

    #[test]
    fn prevention_shield_and_damage_doubler_prompt_for_order() {
        // CR 615 + CR 616.1e: A prevention shield ("prevent the next 2") and a
        // damage doubler (Furnace of Rath `Double`) both modify the amount of a
        // single `ProposedEvent::Damage`, and they do NOT commute:
        // (3-2)*2 = 2 vs (3*2)-2 = 4. The affected player must choose the order.
        // Before the fix the prevention shield classified `Disjoint` (its
        // `execute`/`damage_modification` are unset), so the set was deemed
        // immaterial and the CR 616.1 order prompt was skipped.
        let mut state = GameState::new_two_player(42);
        let mut furnace = GameObject::new(
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Furnace of Rath".to_string(),
            Zone::Battlefield,
        );
        furnace.replacement_definitions =
            vec![ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .damage_modification(DamageModification::Double)]
            .into();
        state.objects.insert(ObjectId(10), furnace);
        state.battlefield.push_back(ObjectId(10));

        // Global prevention shield ("prevent the next 2 damage").
        state.pending_damage_replacements.push(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .prevention_shield(PreventionAmount::Next(2)),
        );

        let mut events = Vec::new();
        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        let result = replace_event(&mut state, proposed, &mut events);
        assert!(
            matches!(result, ReplacementResult::NeedsChoice(_)),
            "prevention shield + doubler must prompt for order per CR 616.1e, got {result:?}"
        );
    }

    #[test]
    fn shield_counter_and_damage_doubler_prompt_for_order() {
        // CR 122.1c + CR 616.1e: A shield counter's prevention effect and a
        // damage doubler both modify the damage event. The shield counter must be
        // a pipeline candidate so the affected object's controller chooses the
        // order instead of the counter always preempting the doubler.
        use crate::types::ability::DamageModification;
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let mut doubler = GameObject::new(
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Furnace of Rath".to_string(),
            Zone::Battlefield,
        );
        doubler.replacement_definitions =
            vec![ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .damage_modification(DamageModification::Double)]
            .into();
        state.objects.insert(ObjectId(10), doubler);
        state.battlefield.push_back(ObjectId(10));

        let mut target = GameObject::new(
            ObjectId(30),
            CardId(3),
            PlayerId(1),
            "Shielded Bear".to_string(),
            Zone::Battlefield,
        );
        target.counters.insert(CounterType::Shield, 1);
        state.objects.insert(ObjectId(30), target);
        state.battlefield.push_back(ObjectId(30));

        let mut events = Vec::new();
        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Object(ObjectId(30)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice for shield counter + doubler, got {result:?}");
        };
        assert_eq!(player, PlayerId(1));
    }

    #[test]
    fn shield_counter_and_regeneration_prompt_for_destroy_order() {
        // CR 122.1c + CR 614.8 + CR 616.1e: Shield counters and regeneration
        // shields are both destruction replacements with different observable
        // outcomes (remove a counter vs. consume regeneration/tap/remove from
        // combat). The affected object's controller must choose.
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let mut target = GameObject::new(
            ObjectId(30),
            CardId(3),
            PlayerId(1),
            "Shielded Bear".to_string(),
            Zone::Battlefield,
        );
        target.counters.insert(CounterType::Shield, 1);
        target.replacement_definitions =
            vec![ReplacementDefinition::new(ReplacementEvent::Destroy)
                .valid_card(TargetFilter::SelfRef)
                .regeneration_shield()]
            .into();
        state.objects.insert(ObjectId(30), target);
        state.battlefield.push_back(ObjectId(30));

        let mut events = Vec::new();
        let proposed = ProposedEvent::Destroy {
            object_id: ObjectId(30),
            source: Some(ObjectId(50)),
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice for shield counter + regeneration, got {result:?}");
        };
        assert_eq!(player, PlayerId(1));
    }

    #[test]
    fn shield_counter_on_unpreventable_damage_removes_counter_without_preventing() {
        // CR 615.12: A prevention effect is still applied to unpreventable damage,
        // but it prevents no damage. For CR 122.1c shield counters, the additional
        // "remove a shield counter" effect still happens.
        use crate::types::ability::{GameRestriction, RestrictionExpiry};
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let mut target = GameObject::new(
            ObjectId(30),
            CardId(3),
            PlayerId(1),
            "Shielded Bear".to_string(),
            Zone::Battlefield,
        );
        target.counters.insert(CounterType::Shield, 1);
        state.objects.insert(ObjectId(30), target);
        state.battlefield.push_back(ObjectId(30));
        state
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source: ObjectId(99),
                expiry: RestrictionExpiry::EndOfTurn,
                scope: None,
            });

        let mut events = Vec::new();
        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Object(ObjectId(30)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        let result = replace_event(&mut state, proposed, &mut events);

        assert!(
            matches!(
                result,
                ReplacementResult::Execute(ProposedEvent::Damage { amount: 3, .. })
            ),
            "unpreventable damage must survive shield-counter replacement, got {result:?}"
        );
        assert_eq!(
            state.objects[&ObjectId(30)]
                .counters
                .get(&CounterType::Shield),
            None
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, GameEvent::DamagePrevented { .. })),
            "unpreventable damage must not emit DamagePrevented"
        );
    }

    #[test]
    fn gate_land_enters_tapped_and_prompts_color_without_modal() {
        // Issue #482 Defect A: a Gate land has two mandatory `Moved` ETB
        // replacements — `Tap SelfRef` (enters tapped) and a `Choose` (as it
        // enters, choose a color). Their application order is immaterial, so the
        // pipeline must auto-resolve without a spurious CR 616.1 modal. Both
        // replacements still apply: the land enters tapped, and the color
        // `Choose` is stashed as a post-replacement continuation.
        let tap_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield);
        let choose_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Choose {
                    choice_type: crate::types::ability::ChoiceType::color_excluding(vec![
                        crate::types::mana::ManaColor::Green,
                    ]),
                    persist: true,
                    selection: crate::types::ability::TargetSelectionMode::Chosen,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield);
        let mut state =
            test_state_with_object(ObjectId(10), Zone::Hand, vec![tap_repl, choose_repl]);
        let mut events = Vec::new();
        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Hand, Zone::Battlefield, None);

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::ZoneChange { enter_tapped, .. }) = result
        else {
            panic!("expected Execute with ZoneChange (no modal), got {result:?}");
        };
        assert!(
            enter_tapped.resolve(false),
            "Gate land should enter the battlefield tapped"
        );
        assert!(
            state.post_replacement_continuation.is_some(),
            "the as-enters color Choose should be stashed as a post-replacement continuation"
        );
    }

    #[test]
    fn replacement_choice_label_derives_outcome_from_execute_effect() {
        // Building-block test for `replacement_choice_label` across its input
        // range, including the SelfRef boundary (R1).
        let tap =
            ReplacementDefinition::new(ReplacementEvent::Moved).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ));
        assert_eq!(replacement_choice_label(&tap), "Enters tapped");

        let untap =
            ReplacementDefinition::new(ReplacementEvent::Moved).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Untap,
                },
            ));
        assert_eq!(replacement_choice_label(&untap), "Enters untapped");

        // A non-SelfRef tap is NOT an enters-tapped modifier — must fall
        // through to the raw-text fallback (proves the SelfRef constraint).
        let non_self_tap = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::Any,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ))
            .description("X".to_string());
        assert_eq!(replacement_choice_label(&non_self_tap), "X");

        // An unrecognized effect falls through to the raw-text fallback.
        let other = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            ))
            .description("X".to_string());
        assert_eq!(replacement_choice_label(&other), "X");

        // No `execute` and no `description` → non-empty generic fallback so
        // the candidate vec is never shorter than `candidate_count`.
        let bare = ReplacementDefinition::new(ReplacementEvent::Moved);
        assert_eq!(replacement_choice_label(&bare), "Replacement effect");
    }

    #[test]
    fn competing_enter_tap_replacements_get_outcome_labels() {
        // Issue #505: two competing distinct `Moved` ETB replacements — one
        // `Untap SelfRef` ("lands you control enter untapped", Horizon
        // Explorer) and one `Tap SelfRef` (a tapland's own "enters tapped").
        // They both write `enter_tapped`, so CR 616.1 pops a distinct-
        // replacement choice. The two option labels must state the *outcome*
        // ("Enters tapped" / "Enters untapped"), not raw Oracle text.
        let untap_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Untap,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield)
            .description("Lands you control enter untapped.".to_string());
        let tap_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield)
            .description("This land enters the battlefield tapped.".to_string());
        let mut state =
            test_state_with_object(ObjectId(10), Zone::Hand, vec![untap_repl, tap_repl]);
        let mut events = Vec::new();
        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Hand, Zone::Battlefield, None);

        let result = replace_event(&mut state, proposed, &mut events);
        assert!(
            matches!(result, ReplacementResult::NeedsChoice(PlayerId(0))),
            "two competing enter-tap replacements must pop a CR 616.1 choice, got {result:?}"
        );

        let WaitingFor::ReplacementChoice {
            candidate_count,
            candidate_descriptions,
            ..
        } = replacement_choice_waiting_for(PlayerId(0), &state)
        else {
            panic!("expected ReplacementChoice waiting_for");
        };
        assert_eq!(candidate_count, 2);
        // After `filter_map`→`map` the vec length equals `candidate_count` by
        // construction (`map` cannot drop elements); this is a weak guard —
        // the label-set assertion below is the real regression discriminator.
        assert_eq!(candidate_descriptions.len(), 2);
        let labels: HashSet<&str> = candidate_descriptions.iter().map(String::as_str).collect();
        assert_eq!(
            labels,
            HashSet::from(["Enters tapped", "Enters untapped"]),
            "labels must be outcome-descriptive, not raw Oracle text"
        );
        for label in &candidate_descriptions {
            assert!(!label.is_empty(), "no label may be empty");
            assert!(
                !label.contains("Lands you control"),
                "label must not be a raw Oracle-text blob: {label:?}"
            );
        }
    }

    /// CR 702.136a: Riot — the optional ETB replacement offers "+1/+1 counter"
    /// (accept) vs "gains haste" (decline). The prompt must label each option by
    /// its OWN outcome, not the card's rules-text `description` for accept and a
    /// bare "Decline" for the haste branch (the reported bug: clicking the rules
    /// text gave the counter and "decline" silently gave haste).
    #[test]
    fn riot_optional_replacement_labels_each_branch_by_outcome() {
        let counter_branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
        )
        .description("This permanent enters with an additional +1/+1 counter on it".to_string());
        let haste_branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        )
        .description("It gains haste".to_string());

        let riot_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(counter_branch)
            .mode(ReplacementMode::Optional {
                decline: Some(Box::new(haste_branch)),
            })
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield)
            .description(
                "CR 702.136a: Riot — this permanent may enter with an additional +1/+1 \
                 counter; otherwise it gains haste."
                    .to_string(),
            );

        let mut state = test_state_with_object(ObjectId(20), Zone::Hand, vec![riot_repl]);
        // Drive the prompt state directly (the CR 616.1 accept/decline choice a
        // single optional replacement produces): candidate 0 is the real Riot
        // replacement, decline is synthetic. This isolates the label builder.
        state.pending_replacement = Some(PendingReplacement {
            proposed: ProposedEvent::zone_change(ObjectId(20), Zone::Hand, Zone::Battlefield, None),
            candidates: vec![ReplacementId {
                source: ObjectId(20),
                index: 0,
            }],
            depth: 0,
            is_optional: true,
            library_placement: None,
            may_cost_paid: false,
            may_cost_remaining: None,
        });

        let WaitingFor::ReplacementChoice {
            candidate_count,
            candidate_descriptions,
            ..
        } = replacement_choice_waiting_for(PlayerId(0), &state)
        else {
            panic!("expected ReplacementChoice waiting_for");
        };
        assert_eq!(candidate_count, 2);
        // Index 0 = accept: the replacement's own `description`, which names its
        // source keyword ("Riot — ...") so the prompt is identifiable (the
        // issue_709 granted-keyword contract). Index 1 = decline: the distinct
        // outcome ("It gains haste") rather than a bare "Decline" — the reported
        // bug was that declining silently granted haste with no indication.
        assert_eq!(
            candidate_descriptions,
            vec![
                "CR 702.136a: Riot — this permanent may enter with an additional +1/+1 \
                 counter; otherwise it gains haste."
                    .to_string(),
                "It gains haste".to_string(),
            ],
            "accept identifies the source (Riot); decline shows its outcome (haste), not a bare \"Decline\""
        );
    }

    #[test]
    fn gain_life_replacement_doubles_via_multiply_expr() {
        // Alhammarret's Archive / Boon Reflection / Rhox Faithmender:
        // "If you would gain life, you gain twice that much life instead."
        // Parser emits `Multiply { factor: 2, inner: EventContextAmount }`.
        let repl =
            ReplacementDefinition::new(ReplacementEvent::GainLife).execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Multiply {
                        factor: 2,
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                    },
                    player: TargetFilter::Controller,
                },
            ));
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::LifeGain {
            player_id: PlayerId(0),
            amount: 3,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::LifeGain { amount, .. }) => {
                assert_eq!(amount, 6);
            }
            other => panic!("expected Execute with LifeGain, got {:?}", other),
        }
        // CR 614.6: the applier substituted the amount; the `post_effect`
        // filter must suppress stashing the same execute ability as a
        // continuation. A leaked Template here is the same defect class as
        // the Jace empty-library win bug.
        assert!(
            state.post_replacement_continuation.is_none(),
            "GainLife→GainLife amount-substitution must not leak a post-replacement \
             continuation; found {:?}",
            state.post_replacement_continuation
        );
    }

    #[test]
    fn gain_life_replacement_offset_via_plus_expr() {
        // Heron of Hope / Angel of Vitality:
        // "If you would gain life, you gain that much life plus 1 instead."
        // Parser emits `Offset { inner: EventContextAmount, offset: 1 }`.
        let repl =
            ReplacementDefinition::new(ReplacementEvent::GainLife).execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                        offset: 1,
                    },
                    player: TargetFilter::Controller,
                },
            ));
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::LifeGain {
            player_id: PlayerId(0),
            amount: 3,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::LifeGain { amount, .. }) => {
                assert_eq!(amount, 4);
            }
            other => panic!("expected Execute with LifeGain, got {:?}", other),
        }
    }

    #[test]
    fn draw_replacement_uses_event_context_amount_with_offset() {
        let repl =
            ReplacementDefinition::new(ReplacementEvent::Draw).execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                        offset: 1,
                    },
                    target: TargetFilter::Controller,
                },
            ));
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 3,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::Draw { count, .. }) => {
                assert_eq!(count, 4);
            }
            other => panic!("expected Execute with Draw, got {:?}", other),
        }
    }

    #[test]
    fn mill_replacement_uses_event_context_amount_multiplier() {
        let repl =
            ReplacementDefinition::new(ReplacementEvent::Mill).execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::Mill {
                    count: QuantityExpr::Multiply {
                        factor: 2,
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                    },
                    target: TargetFilter::Controller,
                    destination: Zone::Graveyard,
                },
            ));
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::Mill {
            player_id: PlayerId(0),
            count: 3,
            destination: Zone::Graveyard,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::Mill { count, .. }) => {
                assert_eq!(count, 6);
            }
            other => panic!("expected Execute with Mill, got {:?}", other),
        }
    }

    #[test]
    fn scry_replacement_can_replace_scry_with_draw() {
        let repl =
            ReplacementDefinition::new(ReplacementEvent::Scry).execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::EventContextAmount,
                    },
                    target: TargetFilter::Controller,
                },
            ));
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::Scry {
            player_id: PlayerId(0),
            count: 3,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::Draw { count, .. }) => {
                assert_eq!(count, 3);
            }
            other => panic!("expected Execute with Draw, got {:?}", other),
        }
    }

    #[test]
    fn scry_replacement_can_modify_scry_count() {
        let repl =
            ReplacementDefinition::new(ReplacementEvent::Scry).execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::Scry {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                        offset: 1,
                    },
                    target: TargetFilter::Controller,
                },
            ));
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::Scry {
            player_id: PlayerId(0),
            count: 2,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::Scry { count, .. }) => {
                assert_eq!(count, 3);
            }
            other => panic!("expected Execute with Scry, got {:?}", other),
        }
    }

    #[test]
    fn scry_replacement_defaults_to_controller_scope() {
        let repl =
            ReplacementDefinition::new(ReplacementEvent::Scry).execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::EventContextAmount,
                    },
                    target: TargetFilter::Controller,
                },
            ));
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let registry = build_replacement_registry();
        let controller_event = ProposedEvent::Scry {
            player_id: PlayerId(0),
            count: 1,
            applied: HashSet::new(),
        };
        let opponent_event = ProposedEvent::Scry {
            player_id: PlayerId(1),
            count: 1,
            applied: HashSet::new(),
        };

        assert_eq!(
            find_applicable_replacements(&state, &controller_event, &registry).len(),
            1
        );
        assert!(find_applicable_replacements(&state, &opponent_event, &registry).is_empty());
    }

    // CR 702.52a: a Dredge draw-replacement shaped like `synthesize_dredge`'s.
    fn dredge_draw_replacement_def() -> ReplacementDefinition {
        let return_to_hand = AbilityDefinition::new(
            crate::types::ability::AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Hand,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        );
        let mut mill = AbilityDefinition::new(
            crate::types::ability::AbilityKind::Spell,
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
        );
        mill.sub_ability = Some(Box::new(return_to_hand));
        let mut repl = ReplacementDefinition::new(ReplacementEvent::Draw);
        repl.mode = ReplacementMode::Optional { decline: None };
        repl.execute = Some(Box::new(mill));
        repl
    }

    fn dredge_state(library_size: usize) -> GameState {
        let mut state = test_state_with_object(
            ObjectId(10),
            Zone::Graveyard,
            vec![dredge_draw_replacement_def()],
        );
        state
            .objects
            .get_mut(&ObjectId(10))
            .unwrap()
            .keywords
            .push(crate::types::keywords::Keyword::Dredge(2));
        let lib = &mut state.players[0].library;
        lib.clear();
        for i in 0..library_size {
            let object_id = ObjectId(100 + i as u64);
            lib.push_back(object_id);
            state.objects.insert(
                object_id,
                GameObject::new(
                    object_id,
                    CardId(100 + i as u64),
                    PlayerId(0),
                    format!("Library Card {i}"),
                    Zone::Library,
                ),
            );
        }
        state
    }

    /// CR 702.52a: a graveyard dredge card's draw-replacement applies on its
    /// owner's draw when the library has at least N cards — even though the
    /// scanner's default zones are Battlefield/Command.
    #[test]
    fn dredge_applies_from_graveyard_on_owner_draw_with_enough_library() {
        let state = dredge_state(2);
        let registry = build_replacement_registry();
        let owner_draw = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 1,
            applied: HashSet::new(),
        };
        assert_eq!(
            find_applicable_replacements(&state, &owner_draw, &registry).len(),
            1,
            "dredge must apply on the owner's draw with library >= N"
        );
        // CR 614.1a default scope is source-player only: an opponent's draw
        // never offers your dredge card.
        let opponent_draw = ProposedEvent::Draw {
            player_id: PlayerId(1),
            count: 1,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &opponent_draw, &registry).is_empty(),
            "dredge must not apply to an opponent's draw"
        );
    }

    /// CR 702.52b: with fewer than N cards in library, dredge is not offered.
    #[test]
    fn dredge_not_applicable_when_library_smaller_than_n() {
        let state = dredge_state(1); // 1 < Dredge 2
        let registry = build_replacement_registry();
        let owner_draw = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 1,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &owner_draw, &registry).is_empty(),
            "CR 702.52b: dredge must not apply when the library has fewer than N cards"
        );
    }

    /// CR 109.4 + CR 108.4a + CR 702.52a: once a stolen card is in its owner's
    /// graveyard, it has no controller; Dredge belongs to the owner, not the
    /// last battlefield controller.
    #[test]
    fn dredge_graveyard_scope_uses_owner_not_stale_controller() {
        let mut state = dredge_state(2);
        state.objects.get_mut(&ObjectId(10)).unwrap().controller = PlayerId(1);
        let registry = build_replacement_registry();

        let owner_draw = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 1,
            applied: HashSet::new(),
        };
        assert_eq!(
            find_applicable_replacements(&state, &owner_draw, &registry).len(),
            1,
            "dredge must be offered to the graveyard card's owner"
        );

        let stale_controller_draw = ProposedEvent::Draw {
            player_id: PlayerId(1),
            count: 1,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &stale_controller_draw, &registry).is_empty(),
            "dredge must not follow the card's stale battlefield controller"
        );
    }

    #[test]
    fn opponent_mill_replacement_does_not_apply_to_controller() {
        let mut repl =
            ReplacementDefinition::new(ReplacementEvent::Mill).execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::Mill {
                    count: QuantityExpr::Multiply {
                        factor: 2,
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                    },
                    target: TargetFilter::Controller,
                    destination: Zone::Graveyard,
                },
            ));
        repl.valid_player = Some(ReplacementPlayerScope::Opponent);
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let registry = build_replacement_registry();

        let controller_event = ProposedEvent::Mill {
            player_id: PlayerId(0),
            count: 3,
            destination: Zone::Graveyard,
            applied: HashSet::new(),
        };
        let opponent_event = ProposedEvent::Mill {
            player_id: PlayerId(1),
            count: 3,
            destination: Zone::Graveyard,
            applied: HashSet::new(),
        };

        assert!(find_applicable_replacements(&state, &controller_event, &registry).is_empty());
        assert_eq!(
            find_applicable_replacements(&state, &opponent_event, &registry).len(),
            1
        );
    }

    /// CR 614.1a: a `valid_player: Some(AnyPlayer)` replacement (Rain of Gore)
    /// applies to EVERY player's event — both the source controller's and a
    /// non-controller's. The non-controller case is the bug all-players scope
    /// fixes (the controller-only default would have skipped it).
    #[test]
    fn any_player_gain_life_replacement_applies_to_every_player() {
        let mut repl =
            ReplacementDefinition::new(ReplacementEvent::GainLife).execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::LoseLife {
                    amount: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::EventContextAmount,
                    },
                    target: Some(TargetFilter::Controller),
                },
            ));
        repl.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let registry = build_replacement_registry();

        let controller_event = ProposedEvent::LifeGain {
            player_id: PlayerId(0),
            amount: 3,
            applied: HashSet::new(),
        };
        let opponent_event = ProposedEvent::LifeGain {
            player_id: PlayerId(1),
            amount: 3,
            applied: HashSet::new(),
        };

        assert_eq!(
            find_applicable_replacements(&state, &controller_event, &registry).len(),
            1,
            "AnyPlayer scope must apply to the source controller"
        );
        assert_eq!(
            find_applicable_replacements(&state, &opponent_event, &registry).len(),
            1,
            "AnyPlayer scope must also apply to a non-controller (the fixed bug)"
        );
    }

    #[test]
    fn draw_replacement_does_not_apply_when_quantity_gate_is_false() {
        let repl = ReplacementDefinition::new(ReplacementEvent::Draw)
            .condition(ReplacementCondition::OnlyIfQuantity {
                lhs: QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::HandSize {
                        player: crate::types::ability::PlayerScope::Controller,
                    },
                },
                comparator: crate::types::ability::Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 1 },
                active_player_req: None,
            })
            .execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                        offset: 1,
                    },
                    target: TargetFilter::Controller,
                },
            ));
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        state.players[0].hand.extend([ObjectId(20), ObjectId(21)]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 3,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::Draw { count, .. }) => {
                assert_eq!(count, 3);
            }
            other => panic!("expected Execute with Draw, got {:?}", other),
        }
    }

    #[test]
    fn draw_replacement_does_not_apply_to_zero_card_draws() {
        let repl =
            ReplacementDefinition::new(ReplacementEvent::Draw).execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                        offset: 1,
                    },
                    target: TargetFilter::Controller,
                },
            ));
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let proposed = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 0,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        assert!(
            find_applicable_replacements(&state, &proposed, &registry).is_empty(),
            "draw replacements with 'one or more' semantics should not apply to zero-card draws"
        );
    }

    #[test]
    fn test_continue_replacement_after_choice() {
        // CR 616.1f: two *material* (zone-redirecting) replacements surface an
        // ordering choice, and resolving one choice lets the pipeline finish the
        // remaining replacement. Bare/no-op replacements would auto-resolve, so
        // genuine destination-redirecting `ChangeZone` replacements are used.
        let repl1 = redirect_repl(Zone::Exile);
        let repl2 = redirect_repl(Zone::Library);

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl1, repl2]);
        let mut events = Vec::new();

        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Battlefield, Zone::Graveyard, None);

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("mandatory replacements should prompt for order, got {result:?}");
        };
        assert_eq!(player, PlayerId(0));

        let final_result = continue_replacement(&mut state, 0, &mut events);
        assert!(
            matches!(final_result, ReplacementResult::Execute(_)),
            "pipeline should finish after resolving the replacement choice, got {final_result:?}"
        );
    }

    #[test]
    fn test_depth_cap() {
        // A replacement that always matches (Moved with no params filter)
        // but once-per-event tracking should prevent infinite loop anyway.
        let repl = make_repl(ReplacementEvent::Moved);

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Battlefield, Zone::Graveyard, None);

        // Should complete without hanging (once-per-event prevents re-application)
        let result = replace_event(&mut state, proposed, &mut events);
        assert!(
            matches!(result, ReplacementResult::Execute(_)),
            "should complete even with broadly-matching replacement"
        );
    }

    #[test]
    fn test_damage_replacement_matches() {
        // DamageDone replacement matches damage events
        let repl = make_repl(ReplacementEvent::DamageDone);

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(99),
            target: TargetRef::Player(PlayerId(0)),
            amount: 5,
            is_combat: false,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        // Without Prevent param, the handler modifies (passes through)
        assert!(
            matches!(result, ReplacementResult::Execute(_)),
            "damage replacement should apply (passthrough without Prevent param)"
        );
    }

    #[test]
    fn test_no_replacements_passthrough() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let proposed = ProposedEvent::ZoneChange {
            object_id: ObjectId(99),
            from: Zone::Battlefield,
            to: Zone::Graveyard,
            cause: None,
            attach_to: None,
            enter_tapped: EtbTapState::Unspecified,
            enter_with_counters: Vec::new(),
            controller_override: None,
            enter_transformed: false,
            applied: HashSet::new(),
            face_down_profile: None,
        };

        let result = replace_event(&mut state, proposed.clone(), &mut events);
        match result {
            ReplacementResult::Execute(event) => {
                assert_eq!(event, proposed);
            }
            other => panic!("expected Execute passthrough, got {:?}", other),
        }
        assert!(
            events.is_empty(),
            "no events should be emitted for passthrough"
        );
    }

    #[test]
    fn test_dealt_damage_replacement_matches_damage_to_source() {
        // DealtDamage replacement on a creature matches damage dealt to it
        let repl = make_repl(ReplacementEvent::DealtDamage);

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(99),
            target: TargetRef::Object(ObjectId(10)),
            amount: 5,
            is_combat: false,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        // DealtDamage matcher checks target matches source_id, so it should match
        // Without Prevent param, it passes through as modified
        match result {
            ReplacementResult::Execute(_) | ReplacementResult::Prevented => {
                // Handler was invoked (either modified or prevented depending on implementation)
            }
            other => panic!("unexpected result: {:?}", other),
        }
    }

    #[test]
    fn test_dealt_damage_does_not_match_damage_to_other() {
        // DealtDamage on ObjectId(10) should NOT match damage targeting ObjectId(20)
        let repl = make_repl(ReplacementEvent::DealtDamage);

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(99),
            target: TargetRef::Object(ObjectId(20)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        // Should pass through since the target doesn't match the replacement source
        assert!(matches!(result, ReplacementResult::Execute(_)));
    }

    #[test]
    fn test_registry_has_all_types() {
        let registry = build_replacement_registry();
        // Count reflects first-class matchers (including ProduceMana — CR 106.3 +
        // CR 614.1a wiring for Contamination-class cards) + placeholders for
        // parser-emitted but not-yet-typed events (TurnFaceUp) + stubs for
        // parser-emitted events whose semantics live in statics (GameLoss,
        // GameWin). Phantom ReplacementEvent variants with zero parser
        // emission are intentionally NOT registered — their absence is a
        // fail-fast signal if a future parser path starts producing them
        // without wiring a handler.
        assert!(
            registry.len() >= 25,
            "registry should have 25+ entries, got {}",
            registry.len()
        );

        // Verify all expected keys
        let expected: Vec<ReplacementEvent> = vec![
            ReplacementEvent::DamageDone,
            ReplacementEvent::ChangeZone,
            ReplacementEvent::Moved,
            ReplacementEvent::Discard,
            ReplacementEvent::Destroy,
            ReplacementEvent::Draw,
            ReplacementEvent::DrawCards,
            ReplacementEvent::GainLife,
            ReplacementEvent::LifeReduced,
            ReplacementEvent::LoseLife,
            ReplacementEvent::AddCounter,
            ReplacementEvent::RemoveCounter,
            ReplacementEvent::Tap,
            ReplacementEvent::Untap,
            ReplacementEvent::Counter,
            ReplacementEvent::CreateToken,
            ReplacementEvent::Attached,
            ReplacementEvent::BeginPhase,
            ReplacementEvent::BeginTurn,
            ReplacementEvent::DealtDamage,
            ReplacementEvent::Mill,
            ReplacementEvent::PayLife,
            ReplacementEvent::ProduceMana,
            ReplacementEvent::TurnFaceUp,
            ReplacementEvent::GameLoss,
            ReplacementEvent::GameWin,
        ];
        for key in &expected {
            assert!(registry.contains_key(key), "registry missing key: {}", key);
        }
    }

    #[test]
    fn restriction_prevents_damage_prevention() {
        use crate::types::ability::{GameRestriction, ReplacementDefinition, RestrictionExpiry};

        // Create a state with a damage prevention replacement on an object
        let obj_id = ObjectId(1);
        let prevent_repl = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .description("Prevent all damage that would be dealt to you.".to_string());
        let mut state = test_state_with_object(obj_id, Zone::Battlefield, vec![prevent_repl]);

        // Add a DamagePreventionDisabled restriction
        state
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source: ObjectId(99),
                expiry: RestrictionExpiry::EndOfTurn,
                scope: None, // Global
            });

        // Create a damage proposed event
        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };

        // The prevention replacement should be skipped
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            candidates.is_empty(),
            "Prevention replacement should be skipped when DamagePreventionDisabled is active"
        );
    }

    #[test]
    fn restriction_does_not_block_non_prevention_replacements() {
        use crate::types::ability::{GameRestriction, ReplacementDefinition, RestrictionExpiry};

        // Create a state with a non-prevention damage replacement
        let obj_id = ObjectId(1);
        let non_prevent_repl = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .description("If a source would deal damage, it deals double instead.".to_string());
        let mut state = test_state_with_object(obj_id, Zone::Battlefield, vec![non_prevent_repl]);

        // Add a DamagePreventionDisabled restriction
        state
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source: ObjectId(99),
                expiry: RestrictionExpiry::EndOfTurn,
                scope: None,
            });

        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };

        // Non-prevention replacements should still apply
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "Non-prevention damage replacements should not be blocked"
        );
    }

    // ── destination_zone filter tests (CR 614.6) ──

    fn rip_replacement() -> ReplacementDefinition {
        use crate::types::ability::{AbilityKind, TargetFilter};
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    origin: None,
                    target: TargetFilter::Any,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                },
            ))
            .destination_zone(Zone::Graveyard)
    }

    fn authority_replacement() -> ReplacementDefinition {
        use crate::types::ability::{AbilityKind, ControllerRef, TargetFilter, TypedFilter};
        ReplacementDefinition::new(ReplacementEvent::ChangeZone)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ))
            .valid_card(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::Opponent),
            ))
            .destination_zone(Zone::Battlefield)
    }

    fn spelunking_replacement() -> ReplacementDefinition {
        use crate::types::ability::{AbilityKind, ControllerRef, TargetFilter, TypedFilter};
        ReplacementDefinition::new(ReplacementEvent::ChangeZone)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Untap,
                },
            ))
            .valid_card(TargetFilter::Typed(
                TypedFilter::new(crate::types::ability::TypeFilter::Land)
                    .controller(ControllerRef::You),
            ))
            .destination_zone(Zone::Battlefield)
    }

    fn uphill_battle_replacement() -> ReplacementDefinition {
        use crate::types::ability::{
            AbilityKind, ControllerRef, FilterProp, TargetFilter, TypedFilter,
        };
        ReplacementDefinition::new(ReplacementEvent::ChangeZone)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ))
            .valid_card(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::Opponent)
                    .properties(vec![FilterProp::WasPlayed]),
            ))
            .destination_zone(Zone::Battlefield)
    }

    fn test_token_spec(
        owner_controller: PlayerId,
        core_type: crate::types::card_type::CoreType,
    ) -> TokenSpec {
        use crate::types::proposed_event::TokenCharacteristics;
        TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Test Token".to_string(),
                power: Some(1),
                toughness: Some(1),
                core_types: vec![core_type],
                subtypes: vec!["Soldier".to_string()],
                supertypes: Vec::new(),
                colors: vec![crate::types::mana::ManaColor::White],
                keywords: Vec::new(),
            },
            script_name: "w_1_1_soldier".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(999),
            controller: owner_controller,
            attach_to: None,
        }
    }

    #[test]
    fn destination_zone_rip_matches_graveyard() {
        // Battlefield → Graveyard with RIP replacement → should be a candidate
        let repl = rip_replacement();
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        let proposed =
            ProposedEvent::zone_change(ObjectId(99), Zone::Battlefield, Zone::Graveyard, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "RIP should match zone change TO graveyard"
        );
    }

    #[test]
    fn destination_zone_rip_hand_to_graveyard() {
        // Hand → Graveyard (discard) with RIP → should match
        let repl = rip_replacement();
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        let proposed = ProposedEvent::zone_change(ObjectId(99), Zone::Hand, Zone::Graveyard, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "RIP should match discard (hand → graveyard)"
        );
    }

    #[test]
    fn destination_zone_rip_library_to_graveyard() {
        // Library → Graveyard (mill) with RIP → should match
        let repl = rip_replacement();
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        let proposed =
            ProposedEvent::zone_change(ObjectId(99), Zone::Library, Zone::Graveyard, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "RIP should match mill (library → graveyard)"
        );
    }

    #[test]
    fn destination_zone_rip_stack_to_graveyard() {
        // Stack → Graveyard (countered spell) with RIP → should match
        let repl = rip_replacement();
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        let proposed = ProposedEvent::zone_change(ObjectId(99), Zone::Stack, Zone::Graveyard, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "RIP should match countered spell (stack → graveyard)"
        );
    }

    #[test]
    fn destination_zone_rip_does_not_match_exile() {
        // Battlefield → Exile — RIP (destination_zone: Graveyard) should NOT match
        let repl = rip_replacement();
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        let proposed =
            ProposedEvent::zone_change(ObjectId(99), Zone::Battlefield, Zone::Exile, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            candidates.is_empty(),
            "RIP should NOT match zone change to exile"
        );
    }

    #[test]
    fn destination_zone_no_rip_passthrough() {
        // Zone change to graveyard without RIP → no replacement
        let state = GameState::new_two_player(42);
        let proposed =
            ProposedEvent::zone_change(ObjectId(99), Zone::Battlefield, Zone::Graveyard, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            candidates.is_empty(),
            "No replacement should match without RIP on battlefield"
        );
    }

    fn make_creature(id: ObjectId, owner: PlayerId, zone: Zone) -> GameObject {
        use crate::types::card_type::{CardType, CoreType};
        let mut obj = GameObject::new(id, CardId(3), owner, "Test Creature".to_string(), zone);
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        };
        obj
    }

    #[test]
    fn destination_zone_authority_matches_battlefield() {
        // Opponent creature entering battlefield with Authority → should match
        let repl = authority_replacement();
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        // Create the entering creature (owned/controlled by opponent = PlayerId(1))
        let creature = make_creature(ObjectId(30), PlayerId(1), Zone::Hand);
        state.objects.insert(ObjectId(30), creature);

        let proposed =
            ProposedEvent::zone_change(ObjectId(30), Zone::Hand, Zone::Battlefield, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "Authority should match opponent creature entering battlefield"
        );
    }

    #[test]
    fn destination_zone_authority_own_creature_not_affected() {
        // Own creature entering battlefield with Authority → should NOT match (controller filter)
        let repl = authority_replacement();
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        // Create own creature (PlayerId(0), same as Authority's controller)
        let creature = make_creature(ObjectId(30), PlayerId(0), Zone::Hand);
        state.objects.insert(ObjectId(30), creature);

        let proposed =
            ProposedEvent::zone_change(ObjectId(30), Zone::Hand, Zone::Battlefield, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            candidates.is_empty(),
            "Authority should NOT match own creature entering battlefield"
        );
    }

    #[test]
    fn destination_zone_authority_matches_token_battlefield_entry() {
        let repl = authority_replacement();
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(1),
            count: 1,
            spec: Box::new(test_token_spec(
                PlayerId(0),
                crate::types::card_type::CoreType::Creature,
            )),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "Authority should match opponent-controlled creature token entry"
        );
    }

    #[test]
    fn destination_zone_authority_own_token_not_affected() {
        let repl = authority_replacement();
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            count: 1,
            spec: Box::new(test_token_spec(
                PlayerId(1),
                crate::types::card_type::CoreType::Creature,
            )),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            candidates.is_empty(),
            "Authority should not match tokens entering under your control"
        );
    }

    #[test]
    fn source_tapped_state_condition_matches_object_state() {
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, Vec::new());
        state.objects.get_mut(&ObjectId(10)).unwrap().tapped = true;

        assert!(evaluate_replacement_condition(
            &ReplacementCondition::SourceTappedState { tapped: true },
            PlayerId(0),
            ObjectId(10),
            &state,
            None,
            &dummy_begin_turn_event(),
        ));
        assert!(!evaluate_replacement_condition(
            &ReplacementCondition::SourceTappedState { tapped: false },
            PlayerId(0),
            ObjectId(10),
            &state,
            None,
            &dummy_begin_turn_event(),
        ));
    }

    #[test]
    fn and_condition_requires_all_children() {
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, Vec::new());
        state.objects.get_mut(&ObjectId(10)).unwrap().tapped = true;

        let condition = ReplacementCondition::And {
            conditions: vec![
                ReplacementCondition::SourceTappedState { tapped: true },
                ReplacementCondition::UnlessYourTurn,
            ],
        };

        state.active_player = PlayerId(1);
        assert!(evaluate_replacement_condition(
            &condition,
            PlayerId(0),
            ObjectId(10),
            &state,
            None,
            &dummy_begin_turn_event(),
        ));

        state.active_player = PlayerId(0);
        assert!(!evaluate_replacement_condition(
            &condition,
            PlayerId(0),
            ObjectId(10),
            &state,
            None,
            &dummy_begin_turn_event(),
        ));
    }

    #[test]
    fn class_level_condition_requires_battlefield_source_at_level() {
        let source = ObjectId(10);
        let mut state = test_state_with_object(source, Zone::Battlefield, Vec::new());
        state.objects.get_mut(&source).unwrap().class_level = Some(3);
        let condition = ReplacementCondition::ClassLevelGE { level: 3 };

        assert!(evaluate_replacement_condition(
            &condition,
            PlayerId(0),
            source,
            &state,
            None,
            &dummy_begin_turn_event(),
        ));

        state.objects.get_mut(&source).unwrap().zone = Zone::Graveyard;
        assert!(!evaluate_replacement_condition(
            &condition,
            PlayerId(0),
            source,
            &state,
            None,
            &dummy_begin_turn_event(),
        ));
    }

    /// CR 614.1d: `IfControlsMatching` with `minimum: 1` and a "creature" filter
    /// must count the source itself when the source satisfies the filter and the
    /// Oracle text does NOT say "other" (no `FilterProp::Another`). Models
    /// Worship's "if you control a creature" once Worship has been animated into
    /// a creature — the condition is self-satisfying and the replacement still
    /// applies. Regression guard: a previous revision hardcoded
    /// `o.id != source_id`, which silently broke this case.
    #[test]
    fn if_controls_matching_counts_self_when_filter_lacks_another() {
        use crate::types::ability::{ControllerRef, TargetFilter, TypedFilter};
        use crate::types::card_type::CoreType;

        let source = ObjectId(10);
        let mut state = test_state_with_object(source, Zone::Battlefield, Vec::new());
        // Animate the source into a creature — the only creature on the battlefield.
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let cond = ReplacementCondition::IfControlsMatching {
            minimum: 1,
            filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        };

        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                source,
                &state,
                None,
                &dummy_begin_turn_event(),
            ),
            "source itself must count toward 'if you control a creature' when no \
             FilterProp::Another is present (Worship-when-animated case)"
        );
    }

    /// CR 614.1d: `IfControlsMatching` with `FilterProp::Another` in the filter
    /// must NOT count the source — exclusion is filter-driven, not hardcoded.
    /// Models Lair of the Hydra's "if you control two or more other lands": the
    /// land itself, plus exactly one other land, must NOT satisfy `minimum: 2`.
    #[test]
    fn if_controls_matching_excludes_self_via_another_prop() {
        use crate::types::ability::{
            ControllerRef, FilterProp, TargetFilter, TypeFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;

        let source = ObjectId(10);
        let other_land = ObjectId(11);
        let mut state = test_state_with_object(source, Zone::Battlefield, Vec::new());
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let mut other = GameObject::new(
            other_land,
            CardId(2),
            PlayerId(0),
            "Other Land".to_string(),
            Zone::Battlefield,
        );
        other.card_types.core_types.push(CoreType::Land);
        state.objects.insert(other_land, other);
        state.battlefield.push_back(other_land);

        let cond = ReplacementCondition::IfControlsMatching {
            minimum: 2,
            filter: TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                type_filters: vec![TypeFilter::Land],
                properties: vec![FilterProp::Another],
            }),
        };

        // With only one OTHER land, condition is false (source excluded by Another).
        assert!(
            !evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                source,
                &state,
                None,
                &dummy_begin_turn_event(),
            ),
            "FilterProp::Another must exclude the source from the count"
        );

        // Add a second other land — now condition is true.
        let third = ObjectId(12);
        let mut third_obj = GameObject::new(
            third,
            CardId(3),
            PlayerId(0),
            "Third Land".to_string(),
            Zone::Battlefield,
        );
        third_obj.card_types.core_types.push(CoreType::Land);
        state.objects.insert(third, third_obj);
        state.battlefield.push_back(third);

        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                source,
                &state,
                None,
                &dummy_begin_turn_event(),
            ),
            "two other lands satisfy `minimum: 2` with Another excluding source"
        );
    }

    /// CR 614.1d + CR 810.9a: Bond-land "unless a player has N or less life"
    /// reads each player's TEAM total in 2HG. Both teams at 20 (10+10) → no
    /// team is at or below 15, so the condition is true (not suppressed) even
    /// though every individual is at 10. Reverting Site 5 to `p.life` would see
    /// individuals at 10 (<= 15) and wrongly suppress (return false).
    #[test]
    fn unless_player_life_at_most_reads_team_total_in_2hg() {
        let mut state =
            GameState::new(crate::types::format::FormatConfig::two_headed_giant(), 4, 0);
        for p in &mut state.players {
            p.life = 10; // each team total = 20
        }
        let cond = ReplacementCondition::UnlessPlayerLifeAtMost { amount: 15 };
        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(0),
                &state,
                None,
                &dummy_begin_turn_event(),
            ),
            "no team total (20) is <= 15, so the replacement is not suppressed"
        );

        // A single low individual on an otherwise-healthy team must NOT trip the
        // condition: player 0 at 8 + teammate at 20 → team 28 > 15.
        state.players[0].life = 8;
        state.players[1].life = 20;
        state.players[2].life = 20;
        state.players[3].life = 20;
        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(0),
                &state,
                None,
                &dummy_begin_turn_event(),
            ),
            "an individual at 8 must not trip the condition when its team is at 28"
        );

        // When a TEAM total drops to <= 15, the condition is satisfied (false).
        state.players[0].life = 5;
        state.players[1].life = 5; // team 10 <= 15
        assert!(!evaluate_replacement_condition(
            &cond,
            PlayerId(0),
            ObjectId(0),
            &state,
            None,
            &dummy_begin_turn_event(),
        ));
    }

    #[test]
    fn cast_variant_paid_condition_matches_web_slinging_tag() {
        // CR 702.188a: Scarlet Spider's "Sensational Save" replacement applies
        // only when the source's spell was cast using web-slinging.
        use crate::types::ability::CastVariantPaid;
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, Vec::new());
        let cond = ReplacementCondition::CastVariantPaid {
            variant: CastVariantPaid::WebSlinging,
        };

        // Untagged (cast normally) → condition false, no counters.
        assert!(!evaluate_replacement_condition(
            &cond,
            PlayerId(0),
            ObjectId(10),
            &state,
            None,
            &dummy_begin_turn_event(),
        ));

        // Tagged this turn with web-slinging → condition true.
        state
            .objects
            .get_mut(&ObjectId(10))
            .unwrap()
            .cast_variant_paid = Some((CastVariantPaid::WebSlinging, state.turn_number));
        assert!(evaluate_replacement_condition(
            &cond,
            PlayerId(0),
            ObjectId(10),
            &state,
            None,
            &dummy_begin_turn_event(),
        ));

        // Tagged with a different variant → condition false.
        state
            .objects
            .get_mut(&ObjectId(10))
            .unwrap()
            .cast_variant_paid = Some((CastVariantPaid::Evoke, state.turn_number));
        assert!(!evaluate_replacement_condition(
            &cond,
            PlayerId(0),
            ObjectId(10),
            &state,
            None,
            &dummy_begin_turn_event(),
        ));
    }

    #[test]
    fn dealt_damage_by_source_condition_matches_exact_source() {
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, Vec::new());
        let victim = GameObject::new(
            ObjectId(20),
            CardId(2),
            PlayerId(0),
            "Victim".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(20), victim);
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(10),
            source_controller: PlayerId(0),
            target: TargetRef::Object(ObjectId(20)),
            target_controller: PlayerId(0),
            amount: 1,
            is_combat: false,
            ..Default::default()
        });

        let cond = ReplacementCondition::DealtDamageThisTurnBySource {
            source: TargetFilter::SelfRef,
        };

        assert!(evaluate_replacement_condition(
            &cond,
            PlayerId(0),
            ObjectId(10),
            &state,
            Some(ObjectId(20)),
            &dummy_begin_turn_event(),
        ));
        assert!(!evaluate_replacement_condition(
            &cond,
            PlayerId(0),
            ObjectId(10),
            &state,
            Some(ObjectId(30)),
            &dummy_begin_turn_event(),
        ));
    }

    #[test]
    fn opponent_damaged_condition_uses_recorded_target_controller() {
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, Vec::new());
        let mut victim = GameObject::new(
            ObjectId(20),
            CardId(2),
            PlayerId(1),
            "Victim".to_string(),
            Zone::Battlefield,
        );
        victim.controller = PlayerId(0);
        state.objects.insert(ObjectId(20), victim);
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(10),
            source_controller: PlayerId(0),
            target: TargetRef::Object(ObjectId(20)),
            target_controller: PlayerId(1),
            amount: 1,
            is_combat: false,
            ..Default::default()
        });

        assert!(evaluate_replacement_condition(
            &ReplacementCondition::OpponentDamagedThisTurn,
            PlayerId(0),
            ObjectId(10),
            &state,
            None,
            &dummy_begin_turn_event(),
        ));
        assert!(!evaluate_replacement_condition(
            &ReplacementCondition::OpponentDamagedThisTurn,
            PlayerId(1),
            ObjectId(10),
            &state,
            None,
            &dummy_begin_turn_event(),
        ));
    }

    #[test]
    fn dealt_damage_by_source_condition_matches_attached_to_source() {
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, Vec::new());
        let enchanted = GameObject::new(
            ObjectId(20),
            CardId(2),
            PlayerId(0),
            "Enchanted".to_string(),
            Zone::Battlefield,
        );
        let victim = GameObject::new(
            ObjectId(30),
            CardId(3),
            PlayerId(0),
            "Victim".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(20), enchanted);
        state.objects.insert(ObjectId(30), victim);
        state.objects.get_mut(&ObjectId(10)).unwrap().attached_to =
            Some(AttachTarget::Object(ObjectId(20)));
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(20),
            source_controller: PlayerId(0),
            target: TargetRef::Object(ObjectId(30)),
            target_controller: PlayerId(0),
            amount: 1,
            is_combat: false,
            ..Default::default()
        });

        assert!(evaluate_replacement_condition(
            &ReplacementCondition::DealtDamageThisTurnBySource {
                source: TargetFilter::AttachedTo,
            },
            PlayerId(0),
            ObjectId(10),
            &state,
            Some(ObjectId(30)),
            &dummy_begin_turn_event(),
        ));
    }

    /// CR 608.2i + CR 608.2h: `DealtDamageThisTurnBySource` matches the damage
    /// source against its damage-time *snapshot*, not the live object. A Dragon
    /// deals damage this turn and is then transformed into a non-Dragon (or
    /// leaves the battlefield). A live-object source match would now read the
    /// current characteristics and fail; the snapshot match still recognizes
    /// the source was a Dragon when the damage was dealt. This is the
    /// discriminating regression guard for the lookback unification — it would
    /// FAIL under the previous `matches_target_filter(state, record.source_id,
    /// ..)` live read.
    #[test]
    fn dealt_damage_by_source_uses_damage_time_snapshot() {
        use crate::types::ability::{TargetFilter, TypedFilter};
        use crate::types::card_type::CoreType;

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, Vec::new());
        let dragon_id = ObjectId(20);
        let victim_id = ObjectId(30);

        // The damage source: a Dragon creature controlled by PlayerId(0) at damage time.
        let mut dragon = GameObject::new(
            dragon_id,
            CardId(2),
            PlayerId(0),
            "Shivan Dragon".to_string(),
            Zone::Battlefield,
        );
        dragon.card_types.core_types.push(CoreType::Creature);
        dragon.card_types.subtypes.push("Dragon".to_string());
        state.objects.insert(dragon_id, dragon);
        state.battlefield.push_back(dragon_id);

        let victim = GameObject::new(
            victim_id,
            CardId(3),
            PlayerId(0),
            "Victim".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(victim_id, victim);

        // Record damage with the Dragon characteristics captured at damage time.
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: dragon_id,
            source_controller: PlayerId(0),
            target: TargetRef::Object(victim_id),
            target_controller: PlayerId(0),
            amount: 3,
            is_combat: false,
            source_subtypes: vec!["Dragon".to_string()],
            source_core_types: vec![CoreType::Creature],
            source_controller_snapshot: PlayerId(0),
            source_owner: PlayerId(0),
            ..Default::default()
        });

        // Now mutate the LIVE source: strip its Dragon subtype (transformed into
        // a non-Dragon permanent). A live-object match would no longer see a Dragon.
        let live = state.objects.get_mut(&dragon_id).unwrap();
        live.card_types.subtypes.clear();
        live.card_types.core_types.clear();

        let dragon_filter =
            TargetFilter::Typed(TypedFilter::default().subtype("Dragon".to_string()));
        let cond = ReplacementCondition::DealtDamageThisTurnBySource {
            source: dragon_filter,
        };

        // The snapshot says the source was a Dragon at damage time → matches.
        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(10),
                &state,
                Some(victim_id),
                &dummy_begin_turn_event(),
            ),
            "source matched its damage-time Dragon snapshot even after the live \
             object lost the Dragon subtype (CR 608.2i lookback)"
        );

        // A non-matching filter (Goblin) must NOT match the Dragon snapshot —
        // confirms the swap discriminates on snapshot characteristics, not Any.
        let goblin_cond = ReplacementCondition::DealtDamageThisTurnBySource {
            source: TargetFilter::Typed(TypedFilter::default().subtype("Goblin".to_string())),
        };
        assert!(
            !evaluate_replacement_condition(
                &goblin_cond,
                PlayerId(0),
                ObjectId(10),
                &state,
                Some(victim_id),
                &dummy_begin_turn_event(),
            ),
            "Dragon snapshot must not satisfy a Goblin source filter"
        );
    }

    #[test]
    fn untap_override_replaces_seeded_zone_change_tap_state() {
        let repl = spelunking_replacement();
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let registry = build_replacement_registry();
        let mut events = Vec::new();

        let proposed = ProposedEvent::ZoneChange {
            object_id: ObjectId(20),
            from: Zone::Hand,
            to: Zone::Battlefield,
            cause: None,
            attach_to: None,
            enter_tapped: EtbTapState::Tapped,
            enter_with_counters: Vec::new(),
            controller_override: None,
            enter_transformed: false,
            applied: HashSet::new(),
            face_down_profile: None,
        };

        let replaced = apply_single_replacement(
            &mut state,
            proposed,
            ReplacementId {
                source: ObjectId(10),
                index: 0,
            },
            ReplacementBranch::Execute,
            &registry,
            &mut events,
        )
        .expect("Spelunking untap replacement should modify the event");

        assert_eq!(
            replaced.battlefield_entry_tap_state(),
            Some(EtbTapState::Untapped)
        );
    }

    #[test]
    fn later_tap_state_modifier_overwrites_earlier_one() {
        let tap_repl = authority_replacement();
        let untap_repl = spelunking_replacement();
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![tap_repl]);
        let mut other_source = GameObject::new(
            ObjectId(11),
            CardId(2),
            PlayerId(0),
            "Spelunking".to_string(),
            Zone::Battlefield,
        );
        other_source.replacement_definitions = vec![untap_repl].into();
        state.objects.insert(ObjectId(11), other_source);
        state.battlefield.push_back(ObjectId(11));

        let registry = build_replacement_registry();
        let mut events = Vec::new();
        let proposed =
            ProposedEvent::zone_change(ObjectId(20), Zone::Hand, Zone::Battlefield, None);

        let tapped_event = apply_single_replacement(
            &mut state,
            proposed,
            ReplacementId {
                source: ObjectId(10),
                index: 0,
            },
            ReplacementBranch::Execute,
            &registry,
            &mut events,
        )
        .expect("tap replacement should apply");
        assert_eq!(
            tapped_event.battlefield_entry_tap_state(),
            Some(EtbTapState::Tapped)
        );

        let untapped_event = apply_single_replacement(
            &mut state,
            tapped_event,
            ReplacementId {
                source: ObjectId(11),
                index: 0,
            },
            ReplacementBranch::Execute,
            &registry,
            &mut events,
        )
        .expect("untap replacement should apply");
        assert_eq!(
            untapped_event.battlefield_entry_tap_state(),
            Some(EtbTapState::Untapped)
        );

        let retapped_event = apply_single_replacement(
            &mut state,
            untapped_event,
            ReplacementId {
                source: ObjectId(10),
                index: 0,
            },
            ReplacementBranch::Execute,
            &registry,
            &mut events,
        )
        .expect("later tap replacement should overwrite prior untap");
        assert_eq!(
            retapped_event.battlefield_entry_tap_state(),
            Some(EtbTapState::Tapped)
        );
    }

    #[test]
    fn authority_taps_creature_tokens_after_replacement() {
        let repl = authority_replacement();
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(1),
            count: 1,
            spec: Box::new(test_token_spec(
                PlayerId(0),
                crate::types::card_type::CoreType::Creature,
            )),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            applied: HashSet::new(),
        };

        let ReplacementResult::Execute(event) = replace_event(&mut state, proposed, &mut events)
        else {
            panic!("expected authority token replacement to auto-apply");
        };
        apply_create_token_after_replacement(&mut state, event, &mut events);

        let created_id = *state
            .battlefield
            .iter()
            .find(|id| state.objects.get(id).is_some_and(|obj| obj.is_token))
            .expect("token should be created");
        let created = state.objects.get(&created_id).unwrap();
        assert!(
            created.tapped,
            "Authority should make creature tokens enter tapped"
        );
    }

    #[test]
    fn spelunking_untaps_seeded_land_tokens_after_replacement() {
        let repl = spelunking_replacement();
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();
        let mut spec = test_token_spec(PlayerId(1), crate::types::card_type::CoreType::Land);
        spec.tapped = true;
        spec.characteristics.power = None;
        spec.characteristics.toughness = None;
        spec.script_name = "c_a_clue".to_string();
        spec.characteristics.display_name = "Land Token".to_string();
        spec.characteristics.subtypes.clear();

        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            count: 1,
            spec: Box::new(spec),
            copy: None,
            enter_tapped: EtbTapState::Tapped,
            applied: HashSet::new(),
        };

        let ReplacementResult::Execute(event) = replace_event(&mut state, proposed, &mut events)
        else {
            panic!("expected spelunking token replacement to auto-apply");
        };
        apply_create_token_after_replacement(&mut state, event, &mut events);

        let created_id = *state
            .battlefield
            .iter()
            .find(|id| state.objects.get(id).is_some_and(|obj| obj.is_token))
            .expect("token should be created");
        let created = state.objects.get(&created_id).unwrap();
        assert!(
            !created.tapped,
            "Spelunking should make your land tokens enter untapped"
        );
    }

    #[test]
    fn zone_redirect_applied_in_apply_single_replacement() {
        // Test that the zone redirect in apply_single_replacement mutates the destination
        let repl = rip_replacement();
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        // Add the object being moved
        let target = GameObject::new(
            ObjectId(30),
            CardId(3),
            PlayerId(0),
            "Dying Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(30), target);
        state.battlefield.push_back(ObjectId(30));

        let mut events = Vec::new();
        let proposed =
            ProposedEvent::zone_change(ObjectId(30), Zone::Battlefield, Zone::Graveyard, None);
        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::ZoneChange { to, .. }) => {
                assert_eq!(to, Zone::Exile, "RIP should redirect graveyard → exile");
            }
            other => panic!("expected Execute with ZoneChange, got {:?}", other),
        }
    }

    // ── Damage modification applier tests ──

    fn damage_event(amount: u32) -> ProposedEvent {
        ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(1)),
            amount,
            is_combat: false,
            applied: HashSet::new(),
        }
    }

    fn damage_repl(modification: DamageModification) -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::DamageDone).damage_modification(modification)
    }

    #[test]
    fn consume_on_apply_prevention_is_consumed_when_damage_fully_prevented() {
        // CR 614.5 + CR 615.1a: A one-shot replacement that fully prevents damage
        // still successfully applied, so the live replacement must be consumed.
        let mut repl = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .prevention_shield(PreventionAmount::All);
        repl.consume_on_apply = true;
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let result = replace_event(&mut state, damage_event(3), &mut events);

        assert!(matches!(result, ReplacementResult::Prevented));
        let obj = state.objects.get(&ObjectId(10)).unwrap();
        assert!(
            obj.replacement_definitions[0].is_consumed,
            "consume_on_apply replacement should be consumed after full prevention"
        );
    }

    fn test_state_with_damage_repl(
        obj_id: ObjectId,
        controller: PlayerId,
        repls: Vec<ReplacementDefinition>,
    ) -> GameState {
        let mut state = GameState::new_two_player(42);
        let mut obj = GameObject::new(
            obj_id,
            CardId(1),
            controller,
            "Test".to_string(),
            Zone::Battlefield,
        );
        obj.replacement_definitions = repls.into();
        state.objects.insert(obj_id, obj);
        state.battlefield.push_back(obj_id);
        state
    }

    #[test]
    fn damage_applier_double() {
        let repl = damage_repl(DamageModification::Double);
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        let result = damage_done_applier(damage_event(3), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 6);
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_applier_triple() {
        let repl = damage_repl(DamageModification::Triple);
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        let result = damage_done_applier(damage_event(3), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 9);
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    /// MSH-F Sub-Plan B (B2): the additive damage offset reads the replacement
    /// source's LIVE power through the full `replace_event` pipeline. Hawkeye
    /// (the source) power 2 + a 3-damage noncombat source you control →
    /// opponent takes 5; raising Hawkeye's power to 4 makes the next event add 4
    /// (proves a live re-read, not a snapshot). Combat damage and damage to your
    /// own permanent are NOT amplified (NoncombatOnly + opponent target filter).
    /// Revert-fail: with the parser/type lift reverted the offset is frozen to
    /// `Fixed(0)`, so the opponent would take 3 on both events.
    #[test]
    fn damage_applier_plus_dynamic_source_power_is_live() {
        use crate::types::ability::{
            ControllerRef, ObjectScope, QuantityExpr, QuantityRef, TypedFilter,
        };
        use crate::types::card_type::CoreType;

        // Hawkeye-shaped replacement: +X where X is the source's (Hawkeye's)
        // current power; only noncombat damage to an opponent / their permanents.
        let repl = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Plus {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Source,
                    },
                },
            })
            .combat_scope(CombatDamageScope::NoncombatOnly)
            .damage_source_filter(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
            .damage_target_filter(DamageTargetFilter::PlayerOrPermanentsControlledBy {
                player: DamageTargetPlayerScope::Opponent,
            });

        // Hawkeye = ObjectId(10), controlled by P0, power 2.
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        state.objects.get_mut(&ObjectId(10)).unwrap().power = Some(2);

        // A noncombat damage source P0 controls (ObjectId(50)).
        let mut src = GameObject::new(
            ObjectId(50),
            CardId(2),
            PlayerId(0),
            "Ping".to_string(),
            Zone::Battlefield,
        );
        src.power = Some(1);
        state.objects.insert(ObjectId(50), src);
        state.battlefield.push_back(ObjectId(50));

        let noncombat_to_opp = || ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };

        let mut events = Vec::new();
        // Power 2 → 3 + 2 = 5.
        match replace_event(&mut state, noncombat_to_opp(), &mut events) {
            ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 5, "3 + live Hawkeye power(2)")
            }
            other => panic!("expected modified damage, got {other:?}"),
        }

        // Raise Hawkeye's power to 4 → the next event re-reads it live: 3 + 4 = 7.
        state.objects.get_mut(&ObjectId(10)).unwrap().power = Some(4);
        match replace_event(&mut state, noncombat_to_opp(), &mut events) {
            ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 7, "live re-read: 3 + Hawkeye power(4)")
            }
            other => panic!("expected modified damage, got {other:?}"),
        }

        // NEGATIVE: combat damage is not amplified (NoncombatOnly scope).
        let combat = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: true,
            applied: HashSet::new(),
        };
        match replace_event(&mut state, combat, &mut events) {
            ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 3, "combat damage must not be amplified")
            }
            other => panic!("expected unmodified combat damage, got {other:?}"),
        }

        // NEGATIVE: noncombat damage to YOUR OWN permanent is not amplified
        // (target filter is opponent-only).
        let mut own = GameObject::new(
            ObjectId(60),
            CardId(3),
            PlayerId(0),
            "Own Creature".to_string(),
            Zone::Battlefield,
        );
        own.card_types.core_types.push(CoreType::Creature);
        state.objects.insert(ObjectId(60), own);
        state.battlefield.push_back(ObjectId(60));
        let to_own = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Object(ObjectId(60)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        match replace_event(&mut state, to_own, &mut events) {
            ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(
                    amount, 3,
                    "damage to your own permanent must not be amplified"
                )
            }
            other => panic!("expected unmodified self-damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_applier_plus() {
        let repl = damage_repl(DamageModification::Plus {
            value: QuantityExpr::Fixed { value: 2 },
        });
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        let result = damage_done_applier(damage_event(3), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 5);
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_applier_minus() {
        let repl = damage_repl(DamageModification::Minus { value: 1 });
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        let result = damage_done_applier(damage_event(3), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 2);
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_applier_minus_saturates_at_zero() {
        let repl = damage_repl(DamageModification::Minus { value: 5 });
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        let result = damage_done_applier(damage_event(1), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 0);
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_applier_life_floor_does_not_increase_damage() {
        let repl = damage_repl(DamageModification::LifeFloor { minimum: 1 });
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        state.players[1].life = 10;
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };

        let result = damage_done_applier(damage_event(2), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 2);
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_applier_life_floor_caps_damage_that_would_go_below_floor() {
        let repl = damage_repl(DamageModification::LifeFloor { minimum: 1 });
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        state.players[1].life = 5;
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };

        let result = damage_done_applier(damage_event(10), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 4);
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_applier_life_floor_does_not_apply_when_already_below_floor() {
        let repl = damage_repl(DamageModification::LifeFloor { minimum: 1 });
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        state.players[1].life = 0;
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };

        let result = damage_done_applier(damage_event(3), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 3);
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_double_chaining_two_doublers() {
        // CR 616.1: Two pure damage doublers commute just like two pure token
        // doublers, so the replacement pipeline can auto-resolve without a
        // player ordering prompt.
        let repl1 = damage_repl(DamageModification::Double);
        let repl2 = damage_repl(DamageModification::Double);
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl1, repl2]);
        let mut events = Vec::new();
        let proposed = damage_event(3);
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected auto-resolved Damage with no CR 616.1 prompt, got {result:?}");
        };
        assert_eq!(amount, 12, "Two doublers should quadruple: 3 * 2 * 2 = 12");
    }

    // ── Damage pipeline filter tests ──

    #[test]
    fn damage_source_filter_blocks_wrong_controller() {
        // Replacement on P0's object requires "source you control" but damage source is P1's
        use crate::types::ability::{ControllerRef, TypedFilter};
        let repl = damage_repl(DamageModification::Double).damage_source_filter(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
        );
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);

        // Add a damage source owned by P1
        let mut source_obj = GameObject::new(
            ObjectId(50),
            CardId(2),
            PlayerId(1),
            "Enemy Source".to_string(),
            Zone::Battlefield,
        );
        source_obj.controller = PlayerId(1);
        state.objects.insert(ObjectId(50), source_obj);
        state.battlefield.push_back(ObjectId(50));

        let proposed = damage_event(3);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            candidates.is_empty(),
            "Should not match: source controller differs"
        );
    }

    #[test]
    fn damage_source_filter_allows_correct_controller() {
        use crate::types::ability::{ControllerRef, TypedFilter};
        let repl = damage_repl(DamageModification::Double).damage_source_filter(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
        );
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);

        // Damage source owned by P0 (same as replacement controller)
        let source_obj = GameObject::new(
            ObjectId(50),
            CardId(2),
            PlayerId(0),
            "Own Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(50), source_obj);
        state.battlefield.push_back(ObjectId(50));

        let proposed = damage_event(3);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "Should match: source controller matches"
        );
    }

    #[test]
    fn damage_target_filter_opponent_blocks_self() {
        let repl = damage_repl(DamageModification::Plus {
            value: QuantityExpr::Fixed { value: 2 },
        })
        .damage_target_filter(DamageTargetFilter::PlayerOrPermanentsControlledBy {
            player: DamageTargetPlayerScope::Opponent,
        });
        // Replacement on P0's object
        let state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);

        // Damage targets P0 (self) — should not match
        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(candidates.is_empty(), "Should not match damage to self");
    }

    #[test]
    fn damage_target_filter_opponent_allows_opponent() {
        let repl = damage_repl(DamageModification::Plus {
            value: QuantityExpr::Fixed { value: 2 },
        })
        .damage_target_filter(DamageTargetFilter::PlayerOrPermanentsControlledBy {
            player: DamageTargetPlayerScope::Opponent,
        });
        let state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);

        // Damage targets P1 (opponent) — should match
        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(!candidates.is_empty(), "Should match damage to opponent");
    }

    #[test]
    fn damage_target_filter_opponent_allows_opponents_permanent() {
        use crate::types::card_type::CoreType;
        let repl = damage_repl(DamageModification::Plus {
            value: QuantityExpr::Fixed { value: 2 },
        })
        .damage_target_filter(DamageTargetFilter::PlayerOrPermanentsControlledBy {
            player: DamageTargetPlayerScope::Opponent,
        });
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);

        // Add opponent's creature
        let mut opp_creature = GameObject::new(
            ObjectId(60),
            CardId(3),
            PlayerId(1),
            "Opp Creature".to_string(),
            Zone::Battlefield,
        );
        opp_creature.card_types.core_types.push(CoreType::Creature);
        state.objects.insert(ObjectId(60), opp_creature);
        state.battlefield.push_back(ObjectId(60));

        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Object(ObjectId(60)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "Should match damage to opponent's permanent"
        );
    }

    #[test]
    fn damage_target_filter_source_chosen_player_scopes_replacement() {
        let repl = damage_repl(DamageModification::Double).damage_target_filter(
            DamageTargetFilter::PlayerOrPermanentsControlledBy {
                player: DamageTargetPlayerScope::SourceChosenPlayer,
            },
        );
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        state
            .objects
            .get_mut(&ObjectId(10))
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Player(PlayerId(1)));
        let registry = build_replacement_registry();

        let chosen_player_damage = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        assert!(
            !find_applicable_replacements(&state, &chosen_player_damage, &registry).is_empty(),
            "damage to the source's chosen player should match"
        );

        let unchosen_player_damage = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &unchosen_player_damage, &registry).is_empty(),
            "damage to another player should not match"
        );
    }

    #[test]
    fn damage_target_filter_source_chosen_player_matches_their_permanent() {
        let repl = damage_repl(DamageModification::Double).damage_target_filter(
            DamageTargetFilter::PlayerOrPermanentsControlledBy {
                player: DamageTargetPlayerScope::SourceChosenPlayer,
            },
        );
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        state
            .objects
            .get_mut(&ObjectId(10))
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Player(PlayerId(1)));

        let chosen_permanent = GameObject::new(
            ObjectId(60),
            CardId(3),
            PlayerId(1),
            "Chosen Player Permanent".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(60), chosen_permanent);
        state.battlefield.push_back(ObjectId(60));

        let other_permanent = GameObject::new(
            ObjectId(61),
            CardId(4),
            PlayerId(0),
            "Other Permanent".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(61), other_permanent);
        state.battlefield.push_back(ObjectId(61));

        let registry = build_replacement_registry();
        let chosen_permanent_damage = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Object(ObjectId(60)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        assert!(
            !find_applicable_replacements(&state, &chosen_permanent_damage, &registry).is_empty(),
            "damage to a permanent the source's chosen player controls should match"
        );

        let other_permanent_damage = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Object(ObjectId(61)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &other_permanent_damage, &registry).is_empty(),
            "damage to another player's permanent should not match"
        );
    }

    #[test]
    fn damage_boost_not_blocked_by_prevention_disabled() {
        use crate::types::ability::{GameRestriction, RestrictionExpiry};
        // Damage boost with damage_modification should still apply even when prevention is disabled
        let repl = damage_repl(DamageModification::Double);
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        state
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source: ObjectId(99),
                expiry: RestrictionExpiry::EndOfTurn,
                scope: None,
            });

        let proposed = damage_event(3);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "Damage boost should not be blocked by prevention disabled"
        );
    }

    // ── Regeneration shield tests ──

    /// Helper: create a creature on the battlefield with a regeneration shield.
    fn create_creature_with_regen_shield(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
    ) -> ObjectId {
        let id = crate::game::zones::create_object(
            state,
            CardId(1),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);

            let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
                .valid_card(TargetFilter::SelfRef)
                .description("Regenerate".to_string())
                .regeneration_shield();
            obj.replacement_definitions.push(shield);
        }
        id
    }

    fn create_creature_with_umbra(state: &mut GameState, owner: PlayerId) -> (ObjectId, ObjectId) {
        let creature = crate::game::zones::create_object(
            state,
            CardId(1),
            owner,
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }
        let umbra = crate::game::zones::create_object(
            state,
            CardId(2),
            owner,
            "Hyena Umbra".to_string(),
            Zone::Battlefield,
        );
        {
            let aura = state.objects.get_mut(&umbra).unwrap();
            aura.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Enchantment);
            aura.card_types.subtypes.push("Aura".to_string());
            aura.keywords
                .push(crate::types::keywords::Keyword::TotemArmor);
            aura.attached_to = Some(creature.into());
        }
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .attachments
            .push(umbra);
        (creature, umbra)
    }

    #[test]
    fn umbra_armor_replaces_destruction_and_destroys_the_aura() {
        let mut state = GameState::new_two_player(42);
        let (creature, umbra) = create_creature_with_umbra(&mut state, PlayerId(0));
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.damage_marked = 5;
            obj.dealt_deathtouch_damage = true;
            obj.tapped = false;
        }

        let proposed = ProposedEvent::Destroy {
            object_id: creature,
            source: Some(ObjectId(100)),
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        // CR 702.89a: the destruction is replaced.
        assert_eq!(result, ReplacementResult::Prevented);
        // The enchanted creature survives with all damage removed, and — unlike
        // regeneration (CR 701.19b) — is NOT tapped.
        assert!(state.battlefield.contains(&creature));
        let obj = state.objects.get(&creature).unwrap();
        assert_eq!(obj.damage_marked, 0);
        assert!(!obj.dealt_deathtouch_damage);
        assert!(
            !obj.tapped,
            "umbra armor does not tap (unlike regeneration)"
        );
        // CR 702.89a: the Umbra Aura is destroyed.
        assert!(
            !state.battlefield.contains(&umbra),
            "the Umbra Aura should be destroyed"
        );
    }

    #[test]
    fn umbra_armor_applies_even_when_cant_regenerate() {
        // CR 702.89a: umbra armor is a replacement, not regeneration, so a
        // "can't be regenerated" destruction does NOT bypass it.
        let mut state = GameState::new_two_player(42);
        let (creature, umbra) = create_creature_with_umbra(&mut state, PlayerId(0));

        let proposed = ProposedEvent::Destroy {
            object_id: creature,
            source: None,
            cant_regenerate: true,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        assert_eq!(result, ReplacementResult::Prevented);
        assert!(state.battlefield.contains(&creature));
        assert!(!state.battlefield.contains(&umbra));
    }

    #[test]
    fn multiple_umbra_armor_auras_prompt_for_aura_choice() {
        // CR 616.1 + CR 702.89a: each Umbra on the enchanted permanent creates
        // its own replacement effect. The controller chooses which Aura is
        // destroyed; the engine must not deterministically pick the first.
        let mut state = GameState::new_two_player(42);
        let (creature, hyena_umbra) = create_creature_with_umbra(&mut state, PlayerId(0));
        let bear_umbra = crate::game::zones::create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear Umbra".to_string(),
            Zone::Battlefield,
        );
        {
            let aura = state.objects.get_mut(&bear_umbra).unwrap();
            aura.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Enchantment);
            aura.card_types.subtypes.push("Aura".to_string());
            aura.keywords
                .push(crate::types::keywords::Keyword::TotemArmor);
            aura.attached_to = Some(creature.into());
        }
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .attachments
            .push(bear_umbra);

        let proposed = ProposedEvent::Destroy {
            object_id: creature,
            source: Some(ObjectId(100)),
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice for two Umbra armor replacements, got {result:?}");
        };
        assert_eq!(player, PlayerId(0));

        let WaitingFor::ReplacementChoice {
            candidate_count,
            candidate_descriptions,
            ..
        } = replacement_choice_waiting_for(player, &state)
        else {
            panic!("expected ReplacementChoice waiting_for");
        };
        assert_eq!(candidate_count, 2);
        let labels: HashSet<&str> = candidate_descriptions.iter().map(String::as_str).collect();
        assert_eq!(
            labels,
            HashSet::from([
                "Umbra armor: destroy Hyena Umbra instead",
                "Umbra armor: destroy Bear Umbra instead",
            ])
        );
        assert!(state.battlefield.contains(&hyena_umbra));
        assert!(state.battlefield.contains(&bear_umbra));
    }

    #[test]
    fn zero_candidate_replacement_choice_returns_priority_not_softlock() {
        // Issue #4277: a `ReplacementChoice` parked with `candidate_count == 0`
        // is unactionable — `candidate_actions_exact` enumerates
        // `(0..candidate_count)` (empty) and the frontend `ReplacementModal`
        // renders nothing on count 0, wedging the game ("Waiting for:
        // ReplacementChoice, Stuck players: 0"). The builder must never emit
        // such a choice: when there is no pending replacement record to choose
        // among (e.g. an upstream resume/drain re-parked after the record was
        // already consumed), it must hand control back to priority so the drain
        // machinery resumes instead of softlocking.
        let state = GameState::new_two_player(42);
        assert!(
            state.pending_replacement.is_none(),
            "precondition: no pending replacement"
        );

        let waiting_for = replacement_choice_waiting_for(PlayerId(0), &state);

        assert!(
            matches!(waiting_for, WaitingFor::Priority { .. }),
            "a no-candidate replacement choice must resolve to Priority, not an \
             actionless ReplacementChoice; got {waiting_for:?}"
        );

        // Defense-in-depth: whatever it is, it must not be a wedged
        // ReplacementChoice. This is the exact softlock the diagnostic reported.
        assert!(
            !matches!(
                waiting_for,
                WaitingFor::ReplacementChoice {
                    candidate_count: 0,
                    ..
                }
            ),
            "must never park on a zero-candidate ReplacementChoice"
        );
    }

    #[test]
    fn empty_candidates_replacement_record_returns_priority_not_softlock() {
        // Issue #4277, sibling count-0 producer: the softlock arises not only when
        // `pending_replacement` is None, but also when a `Some(record)` carries an
        // empty `candidates` list. `replacement_choice_waiting_for` takes the
        // `_ =>` arm and computes `count = candidates.len() == 0` (replacement.rs
        // ~298), which must still route to Priority rather than an actionless
        // ReplacementChoice — covering the non-None branch of the guard.
        let mut state = GameState::new_two_player(42);
        state.pending_replacement = Some(PendingReplacement {
            proposed: ProposedEvent::zone_change(ObjectId(20), Zone::Hand, Zone::Battlefield, None),
            candidates: vec![],
            depth: 0,
            is_optional: false,
            library_placement: None,
            may_cost_paid: false,
            may_cost_remaining: None,
        });

        let waiting_for = replacement_choice_waiting_for(PlayerId(0), &state);

        assert!(
            matches!(waiting_for, WaitingFor::Priority { .. }),
            "an empty-candidates replacement record must resolve to Priority, not an \
             actionless ReplacementChoice; got {waiting_for:?}"
        );
    }

    #[test]
    fn umbra_armor_noop_without_umbra() {
        let mut state = GameState::new_two_player(42);
        let creature = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);

        let proposed = ProposedEvent::Destroy {
            object_id: creature,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        // No Umbra → the destruction is not replaced.
        assert!(matches!(result, ReplacementResult::Execute(_)));
    }

    #[test]
    fn regen_shield_prevents_targeted_destruction() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");

        let proposed = ProposedEvent::Destroy {
            object_id: bear_id,
            source: Some(ObjectId(100)),
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        assert_eq!(result, ReplacementResult::Prevented);
        // CR 701.19: Creature stays on battlefield
        assert!(state.battlefield.contains(&bear_id));
        // CR 701.19: Damage removed and tapped
        let obj = state.objects.get(&bear_id).unwrap();
        assert_eq!(obj.damage_marked, 0);
        assert!(obj.tapped);
        // Shield consumed
        assert!(obj.replacement_definitions[0].is_consumed);
        // Regenerated event emitted
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::Regenerated { object_id } if *object_id == bear_id)));
    }

    #[test]
    fn regen_shield_removes_damage_and_deathtouch() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");

        // Mark damage including deathtouch
        {
            let obj = state.objects.get_mut(&bear_id).unwrap();
            obj.damage_marked = 3;
            obj.dealt_deathtouch_damage = true;
        }

        let proposed = ProposedEvent::Destroy {
            object_id: bear_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        replace_event(&mut state, proposed, &mut events);

        let obj = state.objects.get(&bear_id).unwrap();
        assert_eq!(obj.damage_marked, 0);
        assert!(!obj.dealt_deathtouch_damage);
    }

    #[test]
    fn cant_regenerate_bypasses_shield() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");

        let proposed = ProposedEvent::Destroy {
            object_id: bear_id,
            source: Some(ObjectId(100)),
            cant_regenerate: true,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        // Should pass through — not prevented
        assert!(
            matches!(
                result,
                ReplacementResult::Execute(ProposedEvent::Destroy { .. })
            ),
            "cant_regenerate should bypass shield, got {:?}",
            result
        );
        // Shield not consumed
        let obj = state.objects.get(&bear_id).unwrap();
        assert!(!obj.replacement_definitions[0].is_consumed);
    }

    /// CR 701.19c: A creature marked with `StaticMode::CantBeRegenerated`
    /// (granted by the standalone "[creature] can't be regenerated this turn"
    /// effect — Hurr Jackal, Furnace Brood, Lim-Dûl's Cohort) has its
    /// regeneration shield bypassed at destroy time, even though the Destroy
    /// event itself carries `cant_regenerate: false`. Mirrors
    /// `cant_regenerate_bypasses_shield` but exercises the static-driven path.
    #[test]
    fn cant_be_regenerated_static_bypasses_shield() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");

        // Grant the regeneration prohibition onto the creature, mirroring the
        // transient until-end-of-turn continuous effect's `AddStaticMode`
        // propagation onto the affected creature's `static_definitions`.
        state
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .static_definitions
            .push(
                crate::types::ability::StaticDefinition::new(
                    crate::types::statics::StaticMode::CantBeRegenerated,
                )
                .affected(TargetFilter::SelfRef),
            );

        // Helper observes the active mark.
        assert!(object_has_active_cant_be_regenerated(&state, bear_id));

        let proposed = ProposedEvent::Destroy {
            object_id: bear_id,
            source: Some(ObjectId(100)),
            // Note: the inline flag is false — the bypass is driven purely by the
            // static mark, not by the destroy event.
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        // Destruction proceeds; the shield does NOT save the creature.
        assert!(
            matches!(
                result,
                ReplacementResult::Execute(ProposedEvent::Destroy { .. })
            ),
            "CantBeRegenerated static should bypass the shield, got {:?}",
            result
        );
        // CR 701.19c: shields are not applied, not consumed.
        let obj = state.objects.get(&bear_id).unwrap();
        assert!(!obj.replacement_definitions[0].is_consumed);
    }

    /// Negative control for `object_has_active_cant_be_regenerated`: a creature
    /// with no regeneration prohibition is not reported as marked.
    #[test]
    fn object_without_cant_be_regenerated_is_not_marked() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");
        assert!(!object_has_active_cant_be_regenerated(&state, bear_id));
    }

    #[test]
    fn regen_shield_consumption_one_of_two() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");

        // Add a second shield
        {
            let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
                .valid_card(TargetFilter::SelfRef)
                .description("Regenerate 2".to_string())
                .regeneration_shield();
            state
                .objects
                .get_mut(&bear_id)
                .unwrap()
                .replacement_definitions
                .push(shield);
        }

        // First destruction — one shield consumed
        let proposed = ProposedEvent::Destroy {
            object_id: bear_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let initial_result = replace_event(&mut state, proposed, &mut events);
        let result = resolve_first_replacement_choice(&mut state, initial_result, &mut events);
        assert_eq!(result, ReplacementResult::Prevented);

        let obj = state.objects.get(&bear_id).unwrap();
        let consumed_count = obj
            .replacement_definitions
            .iter_all()
            .filter(|r| r.is_consumed)
            .count();
        let active_count = obj
            .replacement_definitions
            .iter_all()
            .filter(|r| r.shield_kind.is_shield() && !r.is_consumed)
            .count();
        assert_eq!(consumed_count, 1, "One shield should be consumed");
        assert_eq!(active_count, 1, "One shield should remain active");

        // Second destruction — second shield consumed
        let proposed2 = ProposedEvent::Destroy {
            object_id: bear_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let initial_result2 = replace_event(&mut state, proposed2, &mut events);
        let result2 = resolve_first_replacement_choice(&mut state, initial_result2, &mut events);
        assert_eq!(result2, ReplacementResult::Prevented);

        let obj = state.objects.get(&bear_id).unwrap();
        let all_consumed = obj
            .replacement_definitions
            .iter_all()
            .filter(|r| r.shield_kind.is_shield())
            .all(|r| r.is_consumed);
        assert!(all_consumed, "Both shields should be consumed now");
    }

    #[test]
    fn regen_shield_removes_from_combat_attacker() {
        use crate::game::combat::{AttackerInfo, CombatState};

        let mut state = GameState::new_two_player(42);
        let attacker_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Attacker");

        // Set up combat with the creature as an attacker
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker_id, PlayerId(1))],
            ..Default::default()
        });

        let proposed = ProposedEvent::Destroy {
            object_id: attacker_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        replace_event(&mut state, proposed, &mut events);

        // CR 701.19c: Removed from combat
        let combat = state.combat.as_ref().unwrap();
        assert!(
            combat.attackers.is_empty(),
            "Regenerated attacker should be removed from combat"
        );
    }

    #[test]
    fn regen_shield_removes_from_combat_blocker() {
        use crate::game::combat::{AttackerInfo, CombatState};
        use std::collections::HashMap;

        let mut state = GameState::new_two_player(42);
        let blocker_id = create_creature_with_regen_shield(&mut state, PlayerId(1), "Blocker");
        let attacker_id = crate::game::zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        // Set up combat with the creature as a blocker
        let mut blocker_assignments = HashMap::new();
        blocker_assignments.insert(attacker_id, vec![blocker_id]);
        let mut blocker_to_attacker = HashMap::new();
        blocker_to_attacker.insert(blocker_id, vec![attacker_id]);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker_id, PlayerId(1))],
            blocker_assignments,
            blocker_to_attacker,
            ..Default::default()
        });

        let proposed = ProposedEvent::Destroy {
            object_id: blocker_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        replace_event(&mut state, proposed, &mut events);

        let combat = state.combat.as_ref().unwrap();
        assert!(
            !combat.blocker_to_attacker.contains_key(&blocker_id),
            "Regenerated blocker should be removed from blocker_to_attacker"
        );
        // Blocker removed from the attacker's blocker list
        let blockers = combat.blocker_assignments.get(&attacker_id).unwrap();
        assert!(
            !blockers.contains(&blocker_id),
            "Regenerated blocker should be removed from blocker list"
        );
    }

    #[test]
    fn regen_shield_taps_already_tapped_creature() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");

        // Already tapped
        state.objects.get_mut(&bear_id).unwrap().tapped = true;

        let proposed = ProposedEvent::Destroy {
            object_id: bear_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        assert_eq!(result, ReplacementResult::Prevented);
        // Still tapped (no-op on already-tapped)
        assert!(state.objects.get(&bear_id).unwrap().tapped);
    }

    #[test]
    fn consumed_shield_skipped_by_find_applicable() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");

        // Pre-consume the shield
        state
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .replacement_definitions[0]
            .is_consumed = true;

        let proposed = ProposedEvent::Destroy {
            object_id: bear_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);

        assert!(
            candidates.is_empty(),
            "Consumed shield should not be a candidate"
        );
    }

    #[test]
    fn unless_your_turn_untapped_on_controllers_turn() {
        let state = GameState::new_two_player(42);
        // active_player is PlayerId(0) by default
        let cond = ReplacementCondition::UnlessYourTurn;
        // Controller is active player → replacement suppressed (enters untapped)
        assert!(
            !evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should be suppressed (untapped) on controller's turn"
        );
    }

    #[test]
    fn unless_your_turn_tapped_on_opponents_turn() {
        let state = GameState::new_two_player(42);
        let cond = ReplacementCondition::UnlessYourTurn;
        // Controller is NOT active player → replacement applies (enters tapped)
        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(1),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should apply (tapped) on opponent's turn"
        );
    }

    #[test]
    fn unless_quantity_turn_count_untapped_within_threshold() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.players[0].turns_taken = 2;
        let cond = ReplacementCondition::UnlessQuantity {
            lhs: QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::TurnsTaken,
            },
            comparator: crate::types::ability::Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 3 },
            active_player_req: Some(ControllerRef::You),
        };
        // turns_taken=2 ≤ 3 on controller's turn → suppressed (untapped)
        assert!(
            !evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should be suppressed (untapped) when turns_taken <= threshold"
        );
    }

    #[test]
    fn unless_quantity_turn_count_tapped_beyond_threshold() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.players[0].turns_taken = 4;
        let cond = ReplacementCondition::UnlessQuantity {
            lhs: QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::TurnsTaken,
            },
            comparator: crate::types::ability::Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 3 },
            active_player_req: Some(ControllerRef::You),
        };
        // turns_taken=4 > 3 → replacement applies (tapped)
        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should apply (tapped) when turns_taken > threshold"
        );
    }

    #[test]
    fn unless_quantity_tapped_on_opponents_turn_regardless_of_count() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(1); // Opponent's turn
        state.players[0].turns_taken = 1; // Controller's count is low
        let cond = ReplacementCondition::UnlessQuantity {
            lhs: QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::TurnsTaken,
            },
            comparator: crate::types::ability::Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 3 },
            active_player_req: Some(ControllerRef::You),
        };
        // Not controller's turn → replacement applies (tapped) even though turns_taken ≤ 3
        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should apply (tapped) when not controller's turn"
        );
    }

    #[test]
    fn unless_quantity_no_turn_req_works_on_any_turn() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(1); // Opponent's turn
        state.players[0].turns_taken = 2;
        let cond = ReplacementCondition::UnlessQuantity {
            lhs: QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::TurnsTaken,
            },
            comparator: crate::types::ability::Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 3 },
            active_player_req: None, // No turn requirement
        };
        // No turn gate, turns_taken=2 ≤ 3 → suppressed regardless of active player
        assert!(
            !evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should be suppressed (untapped) with no turn requirement"
        );
    }

    #[test]
    fn only_if_quantity_applies_when_condition_is_true() {
        let mut state = GameState::new_two_player(42);
        let h = &mut state.players[0].hand;
        if h.len() > 1 {
            h.truncate(1);
        }
        let cond = ReplacementCondition::OnlyIfQuantity {
            lhs: QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::HandSize {
                    player: crate::types::ability::PlayerScope::Controller,
                },
            },
            comparator: crate::types::ability::Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 1 },
            active_player_req: None,
        };
        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should apply while hand size is one or fewer"
        );
    }

    #[test]
    fn has_max_speed_condition_tracks_controller_speed() {
        let mut state = GameState::new_two_player(42);
        let condition = ReplacementCondition::HasMaxSpeed;

        assert!(!evaluate_replacement_condition(
            &condition,
            PlayerId(0),
            ObjectId(1),
            &state,
            None,
            &dummy_begin_turn_event()
        ));

        state.players[0].speed = Some(4);

        assert!(evaluate_replacement_condition(
            &condition,
            PlayerId(0),
            ObjectId(1),
            &state,
            None,
            &dummy_begin_turn_event()
        ));
    }

    #[test]
    fn only_if_quantity_is_filtered_for_opponent_draws() {
        let repl = ReplacementDefinition::new(ReplacementEvent::Draw)
            .condition(ReplacementCondition::OnlyIfQuantity {
                lhs: QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::HandSize {
                        player: crate::types::ability::PlayerScope::Controller,
                    },
                },
                comparator: crate::types::ability::Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 1 },
                active_player_req: None,
            })
            .execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                        offset: 1,
                    },
                    target: TargetFilter::Controller,
                },
            ));
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let h = &mut state.players[0].hand;
        if h.len() > 1 {
            h.truncate(1);
        }

        let proposed = ProposedEvent::Draw {
            player_id: PlayerId(1),
            count: 2,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        assert!(
            find_applicable_replacements(&state, &proposed, &registry).is_empty(),
            "Controller-only draw replacement should not apply to opponent draws"
        );
    }

    #[test]
    fn damage_applier_set_to_source_power_replaces_when_less() {
        let repl = damage_repl(DamageModification::SetToSourcePower);
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        // Set replacement source's power to 4
        state.objects.get_mut(&ObjectId(10)).unwrap().power = Some(4);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        // Damage amount 2 < power 4 → should be replaced to 4
        let result = damage_done_applier(damage_event(2), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 4, "Damage should be set to source power");
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_applier_set_to_source_power_no_change_when_greater() {
        let repl = damage_repl(DamageModification::SetToSourcePower);
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        state.objects.get_mut(&ObjectId(10)).unwrap().power = Some(4);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        // Damage amount 5 >= power 4 → should NOT be replaced
        let result = damage_done_applier(damage_event(5), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 5, "Damage should pass through unchanged");
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_target_filter_opponent_only() {
        let repl = damage_repl(DamageModification::Plus {
            value: QuantityExpr::Fixed { value: 1 },
        })
        .damage_target_filter(DamageTargetFilter::Player {
            player: DamageTargetPlayerScope::Opponent,
        });
        let state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);

        // Damage to opponent (P1) — should match
        let proposed_opp = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        assert!(
            !find_applicable_replacements(&state, &proposed_opp, &registry).is_empty(),
            "Should match damage to opponent"
        );

        // Damage to self (P0) — should NOT match
        let proposed_self = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &proposed_self, &registry).is_empty(),
            "Should not match damage to self"
        );

        // Damage to a creature — should NOT match (opponent player filter is player-only)
        let mut state2 = state.clone();
        let mut creature = GameObject::new(
            ObjectId(60),
            CardId(3),
            PlayerId(1),
            "Opp Creature".to_string(),
            Zone::Battlefield,
        );
        creature.card_types.core_types.push(CoreType::Creature);
        state2.objects.insert(ObjectId(60), creature);
        state2.battlefield.push_back(ObjectId(60));

        let proposed_creature = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Object(ObjectId(60)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state2, &proposed_creature, &registry).is_empty(),
            "opponent player filter should not match damage to creatures"
        );
    }

    #[test]
    fn damage_target_filter_controller_only() {
        let repl = damage_repl(DamageModification::Plus {
            value: QuantityExpr::Fixed { value: 1 },
        })
        .damage_target_filter(DamageTargetFilter::Player {
            player: DamageTargetPlayerScope::Controller,
        });
        let state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        let registry = build_replacement_registry();

        let proposed_self = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        assert!(
            !find_applicable_replacements(&state, &proposed_self, &registry).is_empty(),
            "controller player filter should match damage to the replacement source controller"
        );

        let proposed_opponent = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &proposed_opponent, &registry).is_empty(),
            "controller player filter should not match damage to opponents"
        );
    }

    // --- BeginTurn / BeginPhase (CR 614.1b, CR 614.10) ---

    #[test]
    fn only_extra_turn_condition_fires_only_on_extra_turn() {
        // CR 500.7 + CR 614.10: Stranglehold-class replacement with OnlyExtraTurn
        // must pass the condition check on extra turns and fail on natural turns.
        // Condition gating lives in `evaluate_replacement_condition` (the matcher
        // only filters by event shape); this test exercises the condition directly.
        let state = GameState::new_two_player(42);
        let cond = ReplacementCondition::OnlyExtraTurn;

        let extra_turn_event = ProposedEvent::begin_turn(PlayerId(0), true);
        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &extra_turn_event
            ),
            "OnlyExtraTurn should apply when is_extra_turn=true"
        );

        let natural_turn_event = ProposedEvent::begin_turn(PlayerId(0), false);
        assert!(
            !evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &natural_turn_event
            ),
            "OnlyExtraTurn should NOT apply when is_extra_turn=false"
        );
    }

    #[test]
    fn begin_turn_matcher_matches_event_shape_only() {
        // Matcher checks event shape; per-def gating runs in the outer pipeline.
        let state = GameState::new_two_player(42);
        let begin_turn = ProposedEvent::begin_turn(PlayerId(0), true);
        let draw = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 1,
            applied: HashSet::new(),
        };
        assert!(begin_turn_matcher(&begin_turn, ObjectId(1), &state));
        assert!(!begin_turn_matcher(&draw, ObjectId(1), &state));
    }

    #[test]
    fn begin_turn_applier_returns_prevented() {
        // CR 614.10: "skip" means unconditionally skip — applier must return Prevented.
        let repl =
            make_repl(ReplacementEvent::BeginTurn).condition(ReplacementCondition::OnlyExtraTurn);
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        let mut events = Vec::new();
        let proposed = ProposedEvent::begin_turn(PlayerId(0), true);

        let result = begin_turn_applier(proposed, rid, &mut state, &mut events);
        assert!(matches!(result, ApplyResult::Prevented));
    }

    #[test]
    fn begin_turn_replacement_does_not_consume_shield() {
        // CR 614.10 + ShieldKind::None: permanent statics fire every time their
        // predicate matches — the replacement definition is NOT marked consumed
        // after the pipeline applies it.
        let repl =
            make_repl(ReplacementEvent::BeginTurn).condition(ReplacementCondition::OnlyExtraTurn);
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();
        let proposed = ProposedEvent::begin_turn(PlayerId(0), true);

        let result = replace_event(&mut state, proposed, &mut events);
        assert!(matches!(result, ReplacementResult::Prevented));

        let obj = state.objects.get(&ObjectId(10)).unwrap();
        assert!(
            !obj.replacement_definitions[0].is_consumed,
            "permanent static skip replacement must not be consumed after use"
        );
    }

    #[test]
    fn begin_phase_matcher_fires_for_bare_begin_phase_def() {
        // CR 614.1b: Unconditional BeginPhase replacement should match the event.
        let repl = make_repl(ReplacementEvent::BeginPhase);
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let proposed = ProposedEvent::begin_phase(PlayerId(0), crate::types::phase::Phase::Upkeep);

        assert!(begin_phase_matcher(&proposed, ObjectId(10), &state));
    }

    #[test]
    fn produce_mana_replacement_replaces_type() {
        // CR 106.3 + CR 614.1a: Contamination-style replacement rewrites Green → Black.
        use crate::types::ability::ManaModification;
        use crate::types::mana::ManaType;

        let land_id = ObjectId(10);
        let contamination_id = ObjectId(20);
        let repl = ReplacementDefinition::new(ReplacementEvent::ProduceMana).mana_modification(
            ManaModification::ReplaceWith {
                mana_type: ManaType::Black,
            },
        );
        let mut state = test_state_with_object(contamination_id, Zone::Battlefield, vec![repl]);
        // Add the land as a separate object so `valid_card` gating isn't exercised here.
        let land = GameObject::new(
            land_id,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(land_id, land);
        state.battlefield.push_back(land_id);

        let mut events = Vec::new();
        let proposed = ProposedEvent::produce_mana(land_id, PlayerId(0), ManaType::Green);
        let result = replace_event(&mut state, proposed, &mut events);

        match result {
            ReplacementResult::Execute(ProposedEvent::ProduceMana { mana_type, .. }) => {
                assert_eq!(
                    mana_type,
                    ManaType::Black,
                    "Green should be rewritten to Black"
                );
            }
            other => panic!("expected Execute(ProduceMana), got {:?}", other),
        }
    }

    #[test]
    fn produce_mana_replacement_multiplies_tapped_for_mana_amount() {
        // CR 106.12b + CR 614.1a: Nyxbloom-style replacements multiply only
        // mana produced by tapping a permanent for mana.
        use crate::types::ability::{
            ControllerRef, ManaModification, ManaReplacementScope, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::mana::ManaType;

        let land_id = ObjectId(10);
        let nyxbloom_id = ObjectId(20);
        let repl = ReplacementDefinition::new(ReplacementEvent::ProduceMana)
            .mana_modification(ManaModification::Multiply { factor: 3 })
            .mana_replacement_scope(ManaReplacementScope::TappedForMana)
            .valid_card(TargetFilter::Typed(
                TypedFilter::permanent().controller(ControllerRef::You),
            ));
        let mut state = test_state_with_object(nyxbloom_id, Zone::Battlefield, vec![repl]);
        let mut land = GameObject::new(
            land_id,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        land.card_types.core_types.push(CoreType::Land);
        state.objects.insert(land_id, land);
        state.battlefield.push_back(land_id);

        let mut events = Vec::new();
        let tapped_event =
            ProposedEvent::produce_mana_with_context(land_id, PlayerId(0), ManaType::Green, true);
        let result = replace_event(&mut state, tapped_event, &mut events);

        match result {
            ReplacementResult::Execute(ProposedEvent::ProduceMana { count, .. }) => {
                assert_eq!(count, 3);
            }
            other => panic!("expected Execute(ProduceMana), got {:?}", other),
        }

        let untapped_event =
            ProposedEvent::produce_mana_with_context(land_id, PlayerId(0), ManaType::Green, false);
        let result = replace_event(&mut state, untapped_event, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::ProduceMana { count, .. }) => {
                assert_eq!(count, 1);
            }
            other => panic!("expected Execute(ProduceMana), got {:?}", other),
        }
    }

    #[test]
    fn produce_mana_no_replacement_passthrough() {
        // CR 106.3: Without any ProduceMana replacement, the event passes through unchanged.
        use crate::types::mana::ManaType;

        let land_id = ObjectId(10);
        let mut state = test_state_with_object(land_id, Zone::Battlefield, vec![]);
        let mut events = Vec::new();
        let proposed = ProposedEvent::produce_mana(land_id, PlayerId(0), ManaType::Green);
        let result = replace_event(&mut state, proposed, &mut events);

        match result {
            ReplacementResult::Execute(ProposedEvent::ProduceMana { mana_type, .. }) => {
                assert_eq!(mana_type, ManaType::Green, "no replacement → pass through");
            }
            other => panic!("expected Execute(ProduceMana), got {:?}", other),
        }
    }

    /// CR 614.1c + CR 601.2h: Wildgrowth Archaic requires `colors_spent_to_cast`
    /// on the entering spell object to remain populated while the ZoneChange→Battlefield
    /// replacement pipeline runs. `process_triggers` clears this field AFTER all
    /// replacements have applied (see `triggers.rs` post-collection cleanup), so the
    /// replacement pipeline is the correct place to read it. This test asserts the
    /// invariant by driving a Moved replacement on a spell object whose colors are
    /// populated, and confirming the field is still there after `replace_event` returns.
    #[test]
    fn colors_spent_to_cast_persists_through_zone_change_replacement() {
        use crate::types::mana::ManaColor;

        // Source of the replacement (static permanent on battlefield).
        let repl_source = ObjectId(10);
        let mut state = test_state_with_object(
            repl_source,
            Zone::Battlefield,
            vec![make_repl(ReplacementEvent::Moved)],
        );

        // Spell object on the stack with 3 distinct colors of mana spent.
        let spell_id = ObjectId(20);
        let mut spell = crate::game::game_object::GameObject::new(
            spell_id,
            CardId(99),
            PlayerId(0),
            "Test Creature Spell".to_string(),
            Zone::Stack,
        );
        spell.colors_spent_to_cast.add(ManaColor::White, 1);
        spell.colors_spent_to_cast.add(ManaColor::Blue, 1);
        spell.colors_spent_to_cast.add(ManaColor::Red, 1);
        state.objects.insert(spell_id, spell);

        let mut events = Vec::new();
        let proposed = ProposedEvent::zone_change(spell_id, Zone::Stack, Zone::Battlefield, None);

        let _ = replace_event(&mut state, proposed, &mut events);

        // The invariant: `colors_spent_to_cast` is still intact after replacement.
        // (process_triggers clears it later, not the replacement pipeline.)
        let after = &state.objects[&spell_id].colors_spent_to_cast;
        assert_eq!(after.get(ManaColor::White), 1);
        assert_eq!(after.get(ManaColor::Blue), 1);
        assert_eq!(after.get(ManaColor::Red), 1);
        assert_eq!(after.get(ManaColor::Black), 0);
        assert_eq!(after.get(ManaColor::Green), 0);
    }

    /// CR 614.1c + CR 601.2h + CR 202.2: Wildgrowth Archaic's replacement places
    /// `N` P1P1 counters on the entering creature, where N is the number of
    /// distinct colors of mana spent to cast it. The replacement source is the
    /// Archaic itself (static permanent on battlefield); the quantity must
    /// resolve against the *entering* object's `colors_spent_to_cast`, not the
    /// source's. This test builds that exact scenario and asserts the resulting
    /// `ZoneChange.enter_with_counters` carries `("P1P1", 3)` for a 3-color cast.
    #[test]
    fn colors_spent_on_self_resolves_against_entering_object() {
        use crate::types::ability::{AbilityKind, Effect, QuantityExpr, QuantityRef, TargetFilter};
        use crate::types::mana::ManaColor;

        let archaic_id = ObjectId(10);
        let creature_id = ObjectId(20);

        let etb_counter_ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                target: TargetFilter::SelfRef,
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ManaSpentToCast {
                        scope: crate::types::ability::CastManaObjectScope::SelfObject,
                        metric: crate::types::ability::CastManaSpentMetric::DistinctColors,
                    },
                },
            },
        );

        let creature_filter = TargetFilter::Typed(
            crate::types::ability::TypedFilter::creature()
                .controller(crate::types::ability::ControllerRef::You),
        );

        let repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(etb_counter_ability)
            .valid_card(creature_filter);

        let mut state = test_state_with_object(archaic_id, Zone::Battlefield, vec![repl]);

        // Entering creature spell with 3 distinct colors tallied.
        let mut spell = crate::game::game_object::GameObject::new(
            creature_id,
            CardId(99),
            PlayerId(0),
            "3-color creature".to_string(),
            Zone::Stack,
        );
        spell
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        spell.colors_spent_to_cast.add(ManaColor::White, 1);
        spell.colors_spent_to_cast.add(ManaColor::Blue, 1);
        spell.colors_spent_to_cast.add(ManaColor::Red, 1);
        state.objects.insert(creature_id, spell);

        let mut events = Vec::new();
        let proposed =
            ProposedEvent::zone_change(creature_id, Zone::Stack, Zone::Battlefield, None);

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::ZoneChange {
                enter_with_counters,
                ..
            }) => {
                assert_eq!(
                    enter_with_counters,
                    vec![(CounterType::Plus1Plus1, 3u32)],
                    "expected 3 P1P1 counters (3 distinct colors spent)"
                );
            }
            other => panic!("expected Execute(ZoneChange), got {:?}", other),
        }
    }

    /// CR 614.1c + CR 601.2h: Coin of Mastery — artifact-source mana spent to
    /// cast the entering creature resolves via payment-time source snapshots on
    /// the spell object, not the static replacement source.
    #[test]
    fn artifact_mana_spent_on_self_resolves_against_entering_object() {
        let coin_id = ObjectId(10);
        let creature_id = ObjectId(20);
        let treasure_id = ObjectId(30);

        let etb_counter_ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                target: TargetFilter::SelfRef,
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ManaSpentToCast {
                        scope: CastManaObjectScope::SelfObject,
                        metric: CastManaSpentMetric::FromSource {
                            source_filter: TargetFilter::Typed(TypedFilter::new(
                                TypeFilter::Artifact,
                            )),
                        },
                    },
                },
            },
        );

        let creature_filter =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You));

        let repl = ReplacementDefinition::new(ReplacementEvent::ChangeZone)
            .execute(etb_counter_ability)
            .valid_card(creature_filter)
            .destination_zone(Zone::Battlefield);

        let mut state = test_state_with_object(coin_id, Zone::Battlefield, vec![repl]);

        let mut treasure = GameObject::new(
            treasure_id,
            CardId(98),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        treasure.card_types.core_types.push(CoreType::Artifact);
        treasure.card_types.subtypes.push("Treasure".to_string());
        state.objects.insert(treasure_id, treasure);

        let mut spell = GameObject::new(
            creature_id,
            CardId(99),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Stack,
        );
        spell.card_types.core_types.push(CoreType::Creature);
        spell.mana_spent_source_snapshots = vec![
            ManaSpentSourceSnapshot {
                source_id: treasure_id,
                lki: state.objects[&treasure_id].snapshot_for_mana_spent(),
            },
            ManaSpentSourceSnapshot {
                source_id: treasure_id,
                lki: state.objects[&treasure_id].snapshot_for_mana_spent(),
            },
        ];
        state.objects.insert(creature_id, spell);

        let mut events = Vec::new();
        let proposed =
            ProposedEvent::zone_change(creature_id, Zone::Stack, Zone::Battlefield, None);

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::ZoneChange {
                enter_with_counters,
                ..
            }) => {
                assert_eq!(
                    enter_with_counters,
                    vec![(CounterType::Plus1Plus1, 2u32)],
                    "expected 2 P1P1 counters (2 artifact-source mana units spent)"
                );
            }
            other => panic!("expected Execute(ZoneChange), got {:?}", other),
        }
    }

    /// Regression: when a self-scoped spent-mana quantity is used outside an ETB
    /// context (no entering object), it resolves against the static source. This
    /// keeps `CountersOnSelf`-style refs working for static abilities that inspect
    /// their own source without reach-around via the replacement pipeline.
    #[test]
    fn colors_spent_on_self_falls_back_to_source_without_entering() {
        use crate::types::ability::{QuantityExpr, QuantityRef};
        use crate::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);
        let source = ObjectId(10);
        let mut obj = crate::game::game_object::GameObject::new(
            source,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        obj.colors_spent_to_cast.add(ManaColor::Green, 1);
        obj.colors_spent_to_cast.add(ManaColor::Red, 1);
        state.objects.insert(source, obj);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ManaSpentToCast {
                scope: crate::types::ability::CastManaObjectScope::SelfObject,
                metric: crate::types::ability::CastManaSpentMetric::DistinctColors,
            },
        };
        // No entering object — resolves against `source` directly.
        let n = crate::game::quantity::resolve_quantity(&state, &expr, PlayerId(0), source);
        assert_eq!(n, 2);
    }

    /// CR 614.1a + CR 111.1: Chatterfang-class replacement emits additional
    /// tokens alongside the primary CreateToken event. Two Plant tokens enter
    /// plus two Squirrel tokens, all under the primary owner's control.
    #[test]
    fn create_token_applier_emits_additional_token_spec_batch() {
        use crate::types::proposed_event::TokenCharacteristics;
        let chatterfang = ObjectId(500);
        let squirrel_spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Squirrel".to_string(),
                power: Some(1),
                toughness: Some(1),
                core_types: vec![crate::types::card_type::CoreType::Creature],
                subtypes: vec!["Squirrel".to_string()],
                supertypes: Vec::new(),
                colors: vec![crate::types::mana::ManaColor::Green],
                keywords: Vec::new(),
            },
            script_name: "Squirrel".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(0),
            controller: PlayerId(0),
            attach_to: None,
        };
        let repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .token_owner_scope(ControllerRef::You)
            .additional_token_spec(squirrel_spec);
        let mut state = test_state_with_object(chatterfang, Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let plant_spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Plant".to_string(),
                power: Some(0),
                toughness: Some(2),
                core_types: vec![crate::types::card_type::CoreType::Creature],
                subtypes: vec!["Plant".to_string()],
                supertypes: Vec::new(),
                colors: vec![crate::types::mana::ManaColor::Green],
                keywords: Vec::new(),
            },
            script_name: "Plant".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: chatterfang,
            controller: PlayerId(0),
            attach_to: None,
        };
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(plant_spec),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            count: 2,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(primary) = result else {
            panic!("expected Execute; got {:?}", result);
        };
        crate::game::effects::token::apply_create_token_after_replacement(
            &mut state,
            primary,
            &mut events,
        );

        let plant_count = state
            .objects
            .values()
            .filter(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == "Plant"))
            .count();
        let squirrel_count = state
            .objects
            .values()
            .filter(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == "Squirrel"))
            .count();
        assert_eq!(plant_count, 2, "primary Plant batch materializes");
        assert_eq!(
            squirrel_count, 2,
            "additional_token_spec emits matching Squirrel batch"
        );
        assert!(state
            .objects
            .values()
            .filter(|o| o.is_token)
            .all(|o| o.owner == PlayerId(0)));
    }

    /// CR 614.1a + CR 111.1: Manufactor's "ensure one of each" — when the
    /// proposed event creates a Treasure, the applier emits Clue and Food
    /// recursively, but does NOT re-emit Treasure (already present in the
    /// primary spec). Idempotence: the spawned Clue/Food events carry the
    /// Manufactor `ReplacementId` in `applied`, so a second Manufactor on the
    /// battlefield does not re-fire on its own output (CR 616.1).
    #[test]
    fn create_token_applier_ensure_specs_emits_only_missing_subtypes_cr_614_1a() {
        fn artifact_spec(name: &str) -> TokenSpec {
            use crate::types::proposed_event::TokenCharacteristics;
            TokenSpec {
                characteristics: TokenCharacteristics {
                    display_name: name.to_string(),
                    power: None,
                    toughness: None,
                    core_types: vec![crate::types::card_type::CoreType::Artifact],
                    subtypes: vec![name.to_string()],
                    supertypes: Vec::new(),
                    colors: Vec::new(),
                    keywords: Vec::new(),
                },
                script_name: name.to_string(),
                static_abilities: Vec::new(),
                enter_with_counters: Vec::new(),
                tapped: false,
                enters_attacking: false,
                sacrifice_at: None,
                source_id: ObjectId(0),
                controller: PlayerId(0),
                attach_to: None,
            }
        }

        let manufactor = ObjectId(700);
        let repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .condition(ReplacementCondition::TokenSubtypeMatches {
                subtypes: vec![
                    "Clue".to_string(),
                    "Food".to_string(),
                    "Treasure".to_string(),
                ],
            })
            .ensure_token_specs(vec![
                artifact_spec("Clue"),
                artifact_spec("Food"),
                artifact_spec("Treasure"),
            ]);
        let mut state = test_state_with_object(manufactor, Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let mut treasure = artifact_spec("Treasure");
        treasure.source_id = manufactor;
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(treasure),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(primary) = result else {
            panic!("expected Execute; got {:?}", result);
        };
        crate::game::effects::token::apply_create_token_after_replacement(
            &mut state,
            primary,
            &mut events,
        );

        let count_subtype = |sub: &str| {
            state
                .objects
                .values()
                .filter(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == sub))
                .count()
        };
        assert_eq!(
            count_subtype("Treasure"),
            1,
            "primary Treasure materializes"
        );
        assert_eq!(
            count_subtype("Clue"),
            1,
            "missing Clue emitted by ensure-all"
        );
        assert_eq!(
            count_subtype("Food"),
            1,
            "missing Food emitted by ensure-all"
        );
    }

    /// CR 616.1: Multiple pure `Double` token doublers commute and should not
    /// trigger a CR 616.1 ordering prompt. Three doublers (Doubling Season,
    /// Adrix and Nev, Primal Vigor) on a single token creation should auto-resolve
    /// and multiply correctly: 1 * 2 * 2 * 2 = 8.
    #[test]
    fn multiple_pure_token_doublers_commute_no_prompt() {
        use crate::types::ability::QuantityModification;
        use crate::types::proposed_event::TokenCharacteristics;

        let doubling_season = ObjectId(10);
        let adrix_nev = ObjectId(20);
        let primal_vigor = ObjectId(30);

        let doubler_repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .quantity_modification(QuantityModification::DOUBLE);

        let mut state = GameState::new_two_player(42);
        let mut ds = GameObject::new(
            doubling_season,
            CardId(1),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        ds.replacement_definitions = vec![doubler_repl.clone()].into();
        let mut an = GameObject::new(
            adrix_nev,
            CardId(2),
            PlayerId(0),
            "Adrix and Nev".to_string(),
            Zone::Battlefield,
        );
        an.replacement_definitions = vec![doubler_repl.clone()].into();
        let mut pv = GameObject::new(
            primal_vigor,
            CardId(3),
            PlayerId(0),
            "Primal Vigor".to_string(),
            Zone::Battlefield,
        );
        pv.replacement_definitions = vec![doubler_repl].into();

        state.objects.insert(doubling_season, ds);
        state.objects.insert(adrix_nev, an);
        state.objects.insert(primal_vigor, pv);
        state.battlefield.push_back(doubling_season);
        state.battlefield.push_back(adrix_nev);
        state.battlefield.push_back(primal_vigor);

        let food_spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Food".to_string(),
                power: None,
                toughness: None,
                core_types: vec![crate::types::card_type::CoreType::Artifact],
                subtypes: vec!["Food".to_string()],
                supertypes: Vec::new(),
                colors: Vec::new(),
                keywords: Vec::new(),
            },
            script_name: "Food".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(0),
            controller: PlayerId(0),
            attach_to: None,
        };

        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(food_spec),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        // Should auto-resolve without a prompt since all doublers commute
        let ReplacementResult::Execute(primary) = result else {
            panic!("expected Execute (auto-resolve), got {:?}", result);
        };

        let ProposedEvent::CreateToken { count, .. } = primary else {
            panic!("expected CreateToken event");
        };

        assert_eq!(
            count, 8,
            "Three doublers should multiply: 1 * 2 * 2 * 2 = 8"
        );
    }

    /// CR 616.1: Elspeth, Storm Slayer's token doubler and Divine Visitation's
    /// creature-token substitution commute (double-then-substitute and
    /// substitute-then-double both yield the same batch). The prompt is
    /// degenerate and must auto-resolve; applying the substitution must not
    /// also stash its `Effect::Token` as a post-replacement continuation
    /// (issue #4249 re-prompt loop).
    #[test]
    fn token_doubler_and_creature_substitution_commute_no_prompt() {
        use crate::parser::oracle_replacement::parse_replacement_line;

        let doubler = parse_replacement_line(
            "If one or more tokens would be created under your control, twice that many of those tokens are created instead.",
            "Elspeth, Storm Slayer",
        )
        .expect("doubler parses");
        let visitation = parse_replacement_line(
            "If one or more creature tokens would be created under your control, that many 4/4 white Angel creature tokens with flying and vigilance are created instead.",
            "Divine Visitation",
        )
        .expect("substitution parses");

        let elspeth = ObjectId(10);
        let visitation_id = ObjectId(20);

        let mut state = GameState::new_two_player(42);
        let mut es = GameObject::new(
            elspeth,
            CardId(1),
            PlayerId(0),
            "Elspeth, Storm Slayer".to_string(),
            Zone::Battlefield,
        );
        es.replacement_definitions = vec![doubler].into();
        let mut dv = GameObject::new(
            visitation_id,
            CardId(2),
            PlayerId(0),
            "Divine Visitation".to_string(),
            Zone::Battlefield,
        );
        dv.replacement_definitions = vec![visitation].into();
        state.objects.insert(elspeth, es);
        state.objects.insert(visitation_id, dv);
        state.battlefield.push_back(elspeth);
        state.battlefield.push_back(visitation_id);

        let soldier = test_token_spec(PlayerId(0), CoreType::Creature);
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(soldier),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(primary) = result else {
            panic!("expected Execute (commuting auto-resolve), got {result:?}");
        };

        assert!(
            state.post_replacement_continuation.is_none(),
            "token substitution must not stash a post-replacement continuation"
        );

        apply_create_token_after_replacement(&mut state, primary, &mut events);

        let tokens: Vec<_> = state.objects.values().filter(|o| o.is_token).collect();
        assert_eq!(
            tokens.len(),
            2,
            "1 soldier doubled and substituted → 2 Angels"
        );
        assert!(tokens
            .iter()
            .all(|t| t.power == Some(4) && t.toughness == Some(4)));
    }

    /// CR 616.1: `Effect::Token` execute on a Draw event fully substitutes the
    /// draw (Words of Wilding class). That is order-material against a draw-count
    /// modifier — substitute-first removes the draw, double-first changes how many
    /// draws are replaced — so it must NOT be classified as an immaterial
    /// `TokenSpec` write unless the proposed event is `CreateToken`.
    #[test]
    fn draw_to_token_substitution_does_not_commute_with_draw_count_modifier() {
        use crate::types::ability::PtValue;

        let doubler = ReplacementDefinition::new(ReplacementEvent::Draw)
            .quantity_modification(QuantityModification::DOUBLE);
        let draw_to_token =
            ReplacementDefinition::new(ReplacementEvent::Draw).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Token {
                    name: "Beast".to_string(),
                    power: PtValue::Fixed(3),
                    toughness: PtValue::Fixed(3),
                    types: vec!["Creature".to_string()],
                    colors: vec![],
                    keywords: vec![],
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    owner: TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    supertypes: vec![],
                    static_abilities: vec![],
                    enter_with_counters: vec![],
                },
            ));

        let mut state = GameState::new_two_player(42);
        let mut doubler_src = GameObject::new(
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Draw Doubler".to_string(),
            Zone::Battlefield,
        );
        doubler_src.replacement_definitions = vec![doubler].into();
        let mut token_src = GameObject::new(
            ObjectId(20),
            CardId(2),
            PlayerId(0),
            "Words of Wilding".to_string(),
            Zone::Battlefield,
        );
        token_src.replacement_definitions = vec![draw_to_token].into();
        state.objects.insert(ObjectId(10), doubler_src);
        state.objects.insert(ObjectId(20), token_src);
        state.battlefield.push_back(ObjectId(10));
        state.battlefield.push_back(ObjectId(20));

        let proposed = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 1,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert_eq!(
            candidates.len(),
            2,
            "both draw replacements must be applicable"
        );
        assert!(
            replacement_ordering_is_material(&state, &candidates, &proposed),
            "Draw→Token substitution must stay order-material against a draw-count modifier"
        );

        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice for draw doubler + draw→token, got {result:?}");
        };
        assert_eq!(player, PlayerId(0));
    }

    /// Build a `TokenSpec` of the given core type for replacement-pipeline tests.
    fn token_spec_of(name: &str, core: CoreType, subtype: &str) -> TokenSpec {
        use crate::types::proposed_event::TokenCharacteristics;
        TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: name.to_string(),
                power: (core == CoreType::Creature).then_some(1),
                toughness: (core == CoreType::Creature).then_some(1),
                core_types: vec![core],
                subtypes: vec![subtype.to_string()],
                supertypes: Vec::new(),
                colors: Vec::new(),
                keywords: Vec::new(),
            },
            script_name: name.to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(0),
            controller: PlayerId(0),
            attach_to: None,
        }
    }

    /// Run the Ojer Taq creature-token replacement against `spec` with the given
    /// proposed `count`, returning the post-replacement count. Parses the real
    /// Oracle line so the test exercises parser → pipeline end-to-end.
    fn ojer_taq_replaced_count(spec: TokenSpec, count: u32) -> u32 {
        let parsed = crate::parser::oracle::parse_oracle_text(
            "If one or more creature tokens would be created under your control, \
             three times that many of those tokens are created instead.",
            "Ojer Taq, Deepest Foundation",
            &[],
            &["Creature".to_string()],
            &["God".to_string()],
        );
        assert_eq!(
            parsed.replacements.len(),
            1,
            "Ojer Taq token-multiplier line must parse to exactly one replacement"
        );
        let repl = parsed.replacements[0].clone();
        // CR 614.1a: the multiplier is the parameterized ×N factor (×3 here),
        // not the legacy ×2 `Double`.
        assert_eq!(
            repl.quantity_modification,
            Some(QuantityModification::Times { factor: 3 }),
            "Ojer Taq must parse to Times {{ factor: 3 }}"
        );

        let ojer = ObjectId(10);
        let mut state = GameState::new_two_player(42);
        let mut obj = GameObject::new(
            ojer,
            CardId(1),
            PlayerId(0),
            "Ojer Taq, Deepest Foundation".to_string(),
            Zone::Battlefield,
        );
        obj.replacement_definitions = vec![repl].into();
        state.objects.insert(ojer, obj);
        state.battlefield.push_back(ojer);

        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(spec),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            count,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        match replace_event(&mut state, proposed, &mut events) {
            ReplacementResult::Execute(ProposedEvent::CreateToken { count, .. }) => count,
            other => panic!("expected Execute(CreateToken), got {other:?}"),
        }
    }

    /// CR 614.1a + CR 111.1: Ojer Taq, Deepest Foundation triplicates creature
    /// tokens created under its controller ("three times that many"). Drives the
    /// real parser output through `replace_event`: a proposed 2 creature tokens
    /// resolves to 6. Reverting the ×N parameterization (factor 3 → the old ×2
    /// `Double`) would yield 4, and dropping the replacement entirely yields 2 —
    /// so the `== 6` assertion flips on either regression.
    #[test]
    fn ojer_taq_triplicates_creature_tokens() {
        let spec = token_spec_of("Soldier", CoreType::Creature, "Soldier");
        assert_eq!(
            ojer_taq_replaced_count(spec, 2),
            6,
            "Ojer Taq must triple creature-token creation: 2 * 3 = 6"
        );
    }

    /// CR 111.1: Ojer Taq's multiplier is gated on creature tokens ("if one or
    /// more CREATURE tokens would be created") via `TokenCoreTypeMatches`. A
    /// non-creature (Treasure artifact) token is NOT triplicated — the proposed
    /// count passes through unchanged. Discriminates the core-type gate: without
    /// it, the artifact count would become 6.
    #[test]
    fn ojer_taq_does_not_multiply_noncreature_tokens() {
        let spec = token_spec_of("Treasure", CoreType::Artifact, "Treasure");
        assert_eq!(
            ojer_taq_replaced_count(spec, 2),
            2,
            "Ojer Taq must leave non-creature token creation untouched"
        );
    }

    /// CR 305.1 + CR 601.2a: Uphill Battle WasPlayed filter discriminates cast
    /// creatures from tokens and from nontokens put onto the battlefield.
    #[test]
    fn uphill_battle_was_played_filter_matches_cast_creature_not_token() {
        use crate::types::card_type::CoreType;

        let uphill_id = ObjectId(10);
        let mut state = test_state_with_object(
            uphill_id,
            Zone::Battlefield,
            vec![uphill_battle_replacement()],
        );
        let registry = build_replacement_registry();

        let cast_creature = ObjectId(20);
        let mut creature = GameObject::new(
            cast_creature,
            CardId(2),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        creature.card_types.core_types.push(CoreType::Creature);
        creature.cast_from_zone = Some(Zone::Hand);
        state.objects.insert(cast_creature, creature);

        let cast_event = ProposedEvent::ZoneChange {
            object_id: cast_creature,
            from: Zone::Hand,
            to: Zone::Battlefield,
            cause: None,
            attach_to: None,
            enter_tapped: EtbTapState::Unspecified,
            enter_with_counters: Vec::new(),
            controller_override: None,
            enter_transformed: false,
            face_down_profile: None,
            applied: HashSet::new(),
        };
        let cast_matches = find_applicable_replacements(&state, &cast_event, &registry);
        assert!(
            cast_matches.iter().any(|rid| rid.source == uphill_id),
            "cast creature must match Uphill Battle WasPlayed filter"
        );

        let token_event = ProposedEvent::CreateToken {
            owner: PlayerId(1),
            count: 1,
            spec: Box::new(test_token_spec(PlayerId(1), CoreType::Creature)),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            applied: HashSet::new(),
        };
        let token_matches = find_applicable_replacements(&state, &token_event, &registry);
        assert!(
            !token_matches.iter().any(|rid| rid.source == uphill_id),
            "tokens put directly onto the battlefield must not match WasPlayed filter"
        );

        let put_creature = ObjectId(30);
        let mut put_obj = GameObject::new(
            put_creature,
            CardId(3),
            PlayerId(1),
            "Runeclaw Bear".to_string(),
            Zone::Hand,
        );
        put_obj.card_types.core_types.push(CoreType::Creature);
        state.objects.insert(put_creature, put_obj);

        let put_event = ProposedEvent::ZoneChange {
            object_id: put_creature,
            from: Zone::Hand,
            to: Zone::Battlefield,
            cause: None,
            attach_to: None,
            enter_tapped: EtbTapState::Unspecified,
            enter_with_counters: Vec::new(),
            controller_override: None,
            enter_transformed: false,
            face_down_profile: None,
            applied: HashSet::new(),
        };
        let put_matches = find_applicable_replacements(&state, &put_event, &registry);
        assert!(
            !put_matches.iter().any(|rid| rid.source == uphill_id),
            "nontoken creatures put onto the battlefield without being cast must not match WasPlayed filter"
        );
    }

    /// CR 614.1a + CR 111.1: Halving Season halves opponent token batches.
    #[test]
    fn halving_season_halves_opponent_token_creation() {
        use crate::types::ability::QuantityModification;
        use crate::types::proposed_event::{TokenCharacteristics, TokenSpec};

        let halving_season = ObjectId(10);
        let halver_repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .quantity_modification(QuantityModification::Half)
            .token_owner_scope(ControllerRef::Opponent);

        let mut state = GameState::new_two_player(42);
        let mut hs = GameObject::new(
            halving_season,
            CardId(1),
            PlayerId(0),
            "Halving Season".to_string(),
            Zone::Battlefield,
        );
        hs.replacement_definitions = vec![halver_repl].into();
        state.objects.insert(halving_season, hs);
        state.battlefield.push_back(halving_season);

        let soldier_spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Soldier".to_string(),
                power: Some(1),
                toughness: Some(1),
                core_types: vec![crate::types::card_type::CoreType::Creature],
                subtypes: vec!["Soldier".to_string()],
                supertypes: Vec::new(),
                colors: Vec::new(),
                keywords: Vec::new(),
            },
            script_name: "Soldier".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(0),
            controller: PlayerId(1),
            attach_to: None,
        };

        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(1),
            spec: Box::new(soldier_spec),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            count: 5,
            applied: HashSet::new(),
        };

        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(primary) = result else {
            panic!("expected Execute, got {:?}", result);
        };
        let ProposedEvent::CreateToken { count, .. } = primary else {
            panic!("expected CreateToken");
        };
        assert_eq!(count, 2, "five tokens halved (rounded down) → two");
    }

    /// CR 614.1a: Halving Season halves opponent counter batches on permanents.
    #[test]
    fn halving_season_halves_opponent_counter_placement_on_permanents() {
        use crate::types::ability::QuantityModification;
        use crate::types::counter::CounterType;
        use crate::types::proposed_event::CounterPlacement;

        let halving_season = ObjectId(10);
        let opponent_creature = ObjectId(20);
        let halver_repl = {
            let mut repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .quantity_modification(QuantityModification::Half);
            repl.valid_player = Some(ReplacementPlayerScope::Opponent);
            repl
        };

        let mut state = GameState::new_two_player(42);
        let mut hs = GameObject::new(
            halving_season,
            CardId(1),
            PlayerId(0),
            "Halving Season".to_string(),
            Zone::Battlefield,
        );
        hs.replacement_definitions = vec![halver_repl].into();
        state.objects.insert(halving_season, hs);
        state.battlefield.push_back(halving_season);

        let creature = GameObject::new(
            opponent_creature,
            CardId(2),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(opponent_creature, creature);
        state.battlefield.push_back(opponent_creature);

        let proposed = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(1),
                object_id: opponent_creature,
                counter_type: CounterType::Plus1Plus1,
            },
            count: 5,
            applied: HashSet::new(),
        };

        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(primary) = result else {
            panic!("expected Execute, got {:?}", result);
        };
        let ProposedEvent::AddCounter { count, .. } = primary else {
            panic!("expected AddCounter");
        };
        assert_eq!(count, 2, "five counters halved (rounded down) → two");
    }

    /// CR 614.1a: Halving Season must not halve counters on permanents you control.
    #[test]
    fn halving_season_skips_controller_owned_permanent_counters() {
        use crate::types::ability::QuantityModification;
        use crate::types::counter::CounterType;
        use crate::types::proposed_event::CounterPlacement;

        let halving_season = ObjectId(10);
        let own_creature = ObjectId(20);
        let halver_repl = {
            let mut repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .quantity_modification(QuantityModification::Half);
            repl.valid_player = Some(ReplacementPlayerScope::Opponent);
            repl
        };

        let mut state = GameState::new_two_player(42);
        let mut hs = GameObject::new(
            halving_season,
            CardId(1),
            PlayerId(0),
            "Halving Season".to_string(),
            Zone::Battlefield,
        );
        hs.replacement_definitions = vec![halver_repl].into();
        state.objects.insert(halving_season, hs);
        state.battlefield.push_back(halving_season);

        let creature = GameObject::new(
            own_creature,
            CardId(2),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(own_creature, creature);
        state.battlefield.push_back(own_creature);

        let proposed = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: own_creature,
                counter_type: CounterType::Plus1Plus1,
            },
            count: 5,
            applied: HashSet::new(),
        };

        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::AddCounter { count, .. }) = result else {
            panic!("expected Execute, got {:?}", result);
        };
        assert_eq!(
            count, 5,
            "controller-owned counters must pass through unchanged"
        );
    }

    /// CR 614.1a: Bloodletter of Aclazotz doubles opponent life loss on the
    /// source controller's turn via the LoseLife replacement pipeline.
    #[test]
    fn bloodletter_doubles_opponent_life_loss_during_your_turn() {
        let bloodletter = ObjectId(10);
        let repl = {
            let mut repl = ReplacementDefinition::new(ReplacementEvent::LoseLife)
                .quantity_modification(QuantityModification::DOUBLE);
            repl.valid_player = Some(ReplacementPlayerScope::Opponent);
            repl
        };

        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        let mut card = GameObject::new(
            bloodletter,
            CardId(1),
            PlayerId(0),
            "Bloodletter of Aclazotz".to_string(),
            Zone::Battlefield,
        );
        card.replacement_definitions = vec![repl].into();
        state.objects.insert(bloodletter, card);
        state.battlefield.push_back(bloodletter);

        let proposed = ProposedEvent::LifeLoss {
            player_id: PlayerId(1),
            amount: 3,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::LifeLoss { amount, .. }) = result else {
            panic!("expected doubled LifeLoss, got {:?}", result);
        };
        assert_eq!(amount, 6);
    }

    /// CR 614.1a: Bloodletter only doubles during the source controller's turn.
    #[test]
    fn bloodletter_does_not_double_on_opponents_turn() {
        let bloodletter = ObjectId(10);
        let repl = {
            let mut repl = ReplacementDefinition::new(ReplacementEvent::LoseLife)
                .quantity_modification(QuantityModification::DOUBLE);
            repl.valid_player = Some(ReplacementPlayerScope::Opponent);
            repl
        };

        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(1);
        let mut card = GameObject::new(
            bloodletter,
            CardId(1),
            PlayerId(0),
            "Bloodletter of Aclazotz".to_string(),
            Zone::Battlefield,
        );
        card.replacement_definitions = vec![repl].into();
        state.objects.insert(bloodletter, card);
        state.battlefield.push_back(bloodletter);

        let proposed = ProposedEvent::LifeLoss {
            player_id: PlayerId(1),
            amount: 3,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::LifeLoss { amount, .. }) = result else {
            panic!("expected LifeLoss passthrough, got {:?}", result);
        };
        assert_eq!(amount, 3);
    }

    /// CR 616.1: Mixed `Double` and `Plus` quantity modifications do NOT commute
    /// and should trigger a CR 616.1 ordering prompt. Doubling Season (`Double`)
    /// and Hardened Scales (`Plus{1}`) on a counter placement must prompt the player.
    #[test]
    fn mixed_double_and_plus_do_not_commute_prompt_required() {
        use crate::types::ability::QuantityModification;
        use crate::types::counter::CounterType;

        let doubling_season = ObjectId(10);
        let hardened_scales = ObjectId(20);

        let doubler_repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::DOUBLE);
        let plus_repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Plus { value: 1 });

        let mut state = GameState::new_two_player(42);
        let mut ds = GameObject::new(
            doubling_season,
            CardId(1),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        ds.replacement_definitions = vec![doubler_repl].into();
        let mut hs = GameObject::new(
            hardened_scales,
            CardId(2),
            PlayerId(0),
            "Hardened Scales".to_string(),
            Zone::Battlefield,
        );
        hs.replacement_definitions = vec![plus_repl].into();

        state.objects.insert(doubling_season, ds);
        state.objects.insert(hardened_scales, hs);
        state.battlefield.push_back(doubling_season);
        state.battlefield.push_back(hardened_scales);

        let target = GameObject::new(
            ObjectId(30),
            CardId(3),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(30), target);

        let proposed = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: ObjectId(30),
                counter_type: CounterType::Plus1Plus1,
            },
            count: 1,
            applied: HashSet::new(),
        };

        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        // Should trigger a prompt since Double and Plus do not commute
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!(
                "expected NeedsChoice for non-commuting Double+Plus, got {:?}",
                result
            );
        };
        assert_eq!(player, PlayerId(0));
    }

    #[test]
    fn mixed_double_and_half_do_not_commute_prompt_required() {
        // CR 616.1: ×2 and ÷2-rounded-down do NOT commute (count 3 → ×2÷2 = 3
        // but ÷2×2 = 2), so Halving Season + a doubler on the same counter event
        // must prompt the affected player to choose the order — Half must NOT
        // share the Multiplicative commuting class with Double.
        use crate::types::ability::QuantityModification;
        use crate::types::counter::CounterType;

        let doubler_repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::DOUBLE);
        let halver_repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Half);

        let mut state = GameState::new_two_player(42);
        let mut ds = GameObject::new(
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        ds.replacement_definitions = vec![doubler_repl].into();
        let mut hs = GameObject::new(
            ObjectId(20),
            CardId(2),
            PlayerId(0),
            "Halving Season".to_string(),
            Zone::Battlefield,
        );
        hs.replacement_definitions = vec![halver_repl].into();
        state.objects.insert(ObjectId(10), ds);
        state.objects.insert(ObjectId(20), hs);
        state.battlefield.push_back(ObjectId(10));
        state.battlefield.push_back(ObjectId(20));

        let target = GameObject::new(
            ObjectId(30),
            CardId(3),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(30), target);

        let proposed = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: ObjectId(30),
                counter_type: CounterType::Plus1Plus1,
            },
            count: 1,
            applied: HashSet::new(),
        };

        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        let ReplacementResult::NeedsChoice(player) = result else {
            panic!(
                "expected NeedsChoice for non-commuting Double+Half, got {:?}",
                result
            );
        };
        assert_eq!(player, PlayerId(0));
    }

    /// CR 614.5 + CR 614.1a: Academy Manufactor's recursive token events should
    /// inherit the primary event's `applied` set to prevent Doubling Season from
    /// re-applying to the recursive batches. With Manufactor + Doubling Season,
    /// creating 1 Food should result in exactly 2 Foods, 2 Clues, and 2 Treasures
    /// (not 4 of each, which would indicate incorrect re-application).
    #[test]
    fn academy_manufactor_plus_doubling_season_correct_stacking() {
        use crate::types::ability::QuantityModification;
        use crate::types::proposed_event::TokenCharacteristics;

        fn artifact_spec(name: &str) -> TokenSpec {
            TokenSpec {
                characteristics: TokenCharacteristics {
                    display_name: name.to_string(),
                    power: None,
                    toughness: None,
                    core_types: vec![crate::types::card_type::CoreType::Artifact],
                    subtypes: vec![name.to_string()],
                    supertypes: Vec::new(),
                    colors: Vec::new(),
                    keywords: Vec::new(),
                },
                script_name: name.to_string(),
                static_abilities: Vec::new(),
                enter_with_counters: Vec::new(),
                tapped: false,
                enters_attacking: false,
                sacrifice_at: None,
                source_id: ObjectId(0),
                controller: PlayerId(0),
                attach_to: None,
            }
        }

        let manufactor = ObjectId(700);
        let doubling_season = ObjectId(10);

        let manufactor_repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .condition(ReplacementCondition::TokenSubtypeMatches {
                subtypes: vec![
                    "Clue".to_string(),
                    "Food".to_string(),
                    "Treasure".to_string(),
                ],
            })
            .ensure_token_specs(vec![
                artifact_spec("Clue"),
                artifact_spec("Food"),
                artifact_spec("Treasure"),
            ]);

        let doubler_repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .quantity_modification(QuantityModification::DOUBLE);

        let mut state = GameState::new_two_player(42);
        let mut m = GameObject::new(
            manufactor,
            CardId(1),
            PlayerId(0),
            "Academy Manufactor".to_string(),
            Zone::Battlefield,
        );
        m.replacement_definitions = vec![manufactor_repl].into();
        let mut ds = GameObject::new(
            doubling_season,
            CardId(2),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        ds.replacement_definitions = vec![doubler_repl].into();

        state.objects.insert(manufactor, m);
        state.objects.insert(doubling_season, ds);
        state.battlefield.push_back(manufactor);
        state.battlefield.push_back(doubling_season);

        let mut treasure = artifact_spec("Treasure");
        treasure.source_id = manufactor;
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(treasure),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(primary) = result else {
            panic!("expected Execute; got {:?}", result);
        };
        crate::game::effects::token::apply_create_token_after_replacement(
            &mut state,
            primary,
            &mut events,
        );

        let count_subtype = |sub: &str| {
            state
                .objects
                .values()
                .filter(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == sub))
                .count()
        };

        // With correct applied set inheritance, Doubling Season applies once
        // to the primary event (1 → 2) and does NOT re-apply to the recursive
        // Manufactor batches. Result: 2 of each subtype.
        assert_eq!(
            count_subtype("Treasure"),
            2,
            "primary Treasure doubled once"
        );
        assert_eq!(count_subtype("Clue"), 2, "Clue batch doubled once");
        assert_eq!(count_subtype("Food"), 2, "Food batch doubled once");
    }

    /// CR 614.1a + CR 109.5: Academy Manufactor's "If *you* would create..."
    /// is scoped to the source's controller. When a different player creates a
    /// Treasure token, the replacement must NOT fire — only the single Treasure
    /// is created, with no Clue or Food (issue #1967). Mirrors the
    /// `token_owner_scope` enforcement in the main applicability loop.
    #[test]
    fn academy_manufactor_does_not_apply_to_other_players_tokens_cr_614_1a() {
        use crate::types::proposed_event::TokenCharacteristics;

        fn artifact_spec(name: &str) -> TokenSpec {
            TokenSpec {
                characteristics: TokenCharacteristics {
                    display_name: name.to_string(),
                    power: None,
                    toughness: None,
                    core_types: vec![crate::types::card_type::CoreType::Artifact],
                    subtypes: vec![name.to_string()],
                    supertypes: Vec::new(),
                    colors: Vec::new(),
                    keywords: Vec::new(),
                },
                script_name: name.to_string(),
                static_abilities: Vec::new(),
                enter_with_counters: Vec::new(),
                tapped: false,
                enters_attacking: false,
                sacrifice_at: None,
                source_id: ObjectId(0),
                controller: PlayerId(0),
                attach_to: None,
            }
        }

        let manufactor = ObjectId(700);
        // CR 614.1a + CR 109.5: `token_owner_scope(You)` is what the parser now
        // emits for the "if you would create" Manufactor shape.
        let manufactor_repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .condition(ReplacementCondition::TokenSubtypeMatches {
                subtypes: vec![
                    "Clue".to_string(),
                    "Food".to_string(),
                    "Treasure".to_string(),
                ],
            })
            .token_owner_scope(ControllerRef::You)
            .ensure_token_specs(vec![
                artifact_spec("Clue"),
                artifact_spec("Food"),
                artifact_spec("Treasure"),
            ]);

        // Manufactor is controlled by PlayerId(0); the opponent PlayerId(1)
        // will be the one creating a Treasure.
        let mut state =
            test_state_with_object(manufactor, Zone::Battlefield, vec![manufactor_repl]);

        let mut treasure = artifact_spec("Treasure");
        treasure.source_id = manufactor;
        treasure.controller = PlayerId(1);
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(1),
            spec: Box::new(treasure),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(primary) = result else {
            panic!("expected Execute; got {:?}", result);
        };
        crate::game::effects::token::apply_create_token_after_replacement(
            &mut state,
            primary,
            &mut events,
        );

        let count_subtype = |sub: &str| {
            state
                .objects
                .values()
                .filter(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == sub))
                .count()
        };

        // The opponent's lone Treasure is created unmodified; Manufactor does
        // not bolt on a Clue and a Food because it does not own the event.
        assert_eq!(
            count_subtype("Treasure"),
            1,
            "opponent's single Treasure is created unmodified"
        );
        assert_eq!(
            count_subtype("Clue"),
            0,
            "Manufactor must not add a Clue to another player's token creation"
        );
        assert_eq!(
            count_subtype("Food"),
            0,
            "Manufactor must not add a Food to another player's token creation"
        );
    }

    /// CR 616.1: When candidates have both commuting Count modifications
    /// AND non-commutative EnterTapped modifications, the set must still
    /// be material and trigger a prompt. This catches the early-return bug
    /// where commuting Count would incorrectly return false before checking
    /// other candidates.
    #[test]
    fn commuting_count_plus_non_commuting_entertapped_material() {
        use crate::types::ability::{AbilityKind, Effect, QuantityModification};

        let doubler1 = ObjectId(10);
        let doubler2 = ObjectId(15);
        let tap_effect1 = ObjectId(20);
        let tap_effect2 = ObjectId(25);

        let doubler_repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .quantity_modification(QuantityModification::DOUBLE);

        let tap_repl = ReplacementDefinition::new(ReplacementEvent::CreateToken).execute(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ),
        );

        let untap_repl = ReplacementDefinition::new(ReplacementEvent::CreateToken).execute(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Untap,
                },
            ),
        );

        let mut state = GameState::new_two_player(42);
        let mut ds1 = GameObject::new(
            doubler1,
            CardId(1),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        ds1.replacement_definitions = vec![doubler_repl.clone()].into();
        let mut ds2 = GameObject::new(
            doubler2,
            CardId(2),
            PlayerId(0),
            "Adrix and Nev".to_string(),
            Zone::Battlefield,
        );
        ds2.replacement_definitions = vec![doubler_repl].into();
        let mut te1 = GameObject::new(
            tap_effect1,
            CardId(3),
            PlayerId(0),
            "Tap Effect".to_string(),
            Zone::Battlefield,
        );
        te1.replacement_definitions = vec![tap_repl].into();
        let mut te2 = GameObject::new(
            tap_effect2,
            CardId(4),
            PlayerId(0),
            "Untap Effect".to_string(),
            Zone::Battlefield,
        );
        te2.replacement_definitions = vec![untap_repl].into();

        state.objects.insert(doubler1, ds1);
        state.objects.insert(doubler2, ds2);
        state.objects.insert(tap_effect1, te1);
        state.objects.insert(tap_effect2, te2);
        state.battlefield.push_back(doubler1);
        state.battlefield.push_back(doubler2);
        state.battlefield.push_back(tap_effect1);
        state.battlefield.push_back(tap_effect2);

        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(TokenSpec {
                characteristics: crate::types::proposed_event::TokenCharacteristics {
                    display_name: "Token".to_string(),
                    power: None,
                    toughness: None,
                    core_types: vec![crate::types::card_type::CoreType::Creature],
                    subtypes: Vec::new(),
                    supertypes: Vec::new(),
                    colors: Vec::new(),
                    keywords: Vec::new(),
                },
                script_name: "Token".to_string(),
                static_abilities: Vec::new(),
                enter_with_counters: Vec::new(),
                tapped: false,
                enters_attacking: false,
                sacrifice_at: None,
                source_id: ObjectId(0),
                controller: PlayerId(0),
                attach_to: None,
            }),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        // Should trigger a prompt since EnterTapped is non-commutative
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!(
                "expected NeedsChoice for non-commutative EnterTapped, got {:?}",
                result
            );
        };
        assert_eq!(player, PlayerId(0));
    }

    /// CR 121.1 + CR 504.1 + CR 614.6 — Alhammarret's Archive's
    /// `ExceptFirstDrawInDrawStep` replacement gates the "draw two cards
    /// instead" replacement so it does NOT apply to the active player's
    /// mandatory first draw of their draw step. Subsequent draws in the same
    /// step (extra draws, draws outside the draw step, opponent draws, etc.)
    /// all replace normally. The first-draw identity is read from
    /// `Player.cards_drawn_this_step` (0 ⇒ this would be the first).
    #[test]
    fn except_first_draw_in_draw_step_suppresses_only_active_first_draw() {
        let condition = ReplacementCondition::ExceptFirstDrawInDrawStep;
        let source = ObjectId(10);

        let make_state = |phase: crate::types::phase::Phase, p0_drawn: u32| {
            let mut state = GameState::new_two_player(42);
            state.active_player = PlayerId(0);
            state.phase = phase;
            state.players[0].cards_drawn_this_step = p0_drawn;
            state
        };

        let draw_event = |player_id: PlayerId| ProposedEvent::Draw {
            player_id,
            count: 1,
            applied: HashSet::new(),
        };

        // Active player about to make their FIRST draw of the draw step → suppress.
        let state = make_state(crate::types::phase::Phase::Draw, 0);
        assert!(
            !evaluate_replacement_condition(
                &condition,
                PlayerId(0),
                source,
                &state,
                None,
                &draw_event(PlayerId(0)),
            ),
            "the mandatory first draw of the active player's draw step must NOT replace"
        );

        // Active player making a SECOND draw during their draw step → replace.
        let state = make_state(crate::types::phase::Phase::Draw, 1);
        assert!(
            evaluate_replacement_condition(
                &condition,
                PlayerId(0),
                source,
                &state,
                None,
                &draw_event(PlayerId(0)),
            ),
            "any subsequent draw during the active player's draw step must replace"
        );

        // Outside the draw step — first draw of any other step still replaces.
        let state = make_state(crate::types::phase::Phase::Upkeep, 0);
        assert!(
            evaluate_replacement_condition(
                &condition,
                PlayerId(0),
                source,
                &state,
                None,
                &draw_event(PlayerId(0)),
            ),
            "first draw outside the draw step must replace"
        );

        // Draw step but the NON-active player is drawing — exception only
        // excuses the active player's mandatory draw, so this still replaces.
        let state = make_state(crate::types::phase::Phase::Draw, 0);
        assert!(
            evaluate_replacement_condition(
                &condition,
                PlayerId(1),
                source,
                &state,
                None,
                &draw_event(PlayerId(1)),
            ),
            "draw step draws by the non-active player must replace"
        );
    }

    /// CR 122.1a + CR 614.1a: A counter-replacement that names "+1/+1
    /// counters" in its Oracle text (Hardened Scales) must NOT fire on a
    /// -1/-1 counter addition. The runtime gate honors `counter_match`
    /// when the proposed event is `AddCounter`.
    #[test]
    fn counter_match_filters_hardened_scales_from_minus_one_minus_one_event() {
        use crate::types::counter::{CounterMatch, CounterType};

        let source = ObjectId(1);
        let target = ObjectId(2);

        let repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(crate::types::ability::QuantityModification::Plus { value: 1 })
            .counter_match(CounterMatch::OfType(CounterType::Plus1Plus1));
        let mut state = test_state_with_object(source, Zone::Battlefield, vec![repl]);
        // The proposed AddCounter event targets a separate creature on the
        // battlefield owned by the same player so any controller-scoped
        // checks in the registry pass through unchanged.
        let mut creature = crate::game::game_object::GameObject::new(
            target,
            CardId(2),
            PlayerId(0),
            "C".into(),
            Zone::Battlefield,
        );
        creature
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        state.objects.insert(target, creature);
        state.battlefield.push_back(target);

        let registry = build_replacement_registry();
        let proposed = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: target,
                counter_type: CounterType::Minus1Minus1,
            },
            count: 1,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &proposed, &registry).is_empty(),
            "Hardened-Scales-class replacement must not fire on -1/-1 counter additions"
        );

        // Sanity: the same replacement DOES fire on a +1/+1 counter event.
        let proposed_p1p1 = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: target,
                counter_type: CounterType::Plus1Plus1,
            },
            count: 1,
            applied: HashSet::new(),
        };
        assert_eq!(
            find_applicable_replacements(&state, &proposed_p1p1, &registry).len(),
            1,
            "Hardened-Scales-class replacement must fire on +1/+1 counter additions"
        );
    }

    /// CR 122.1a + CR 614.1a: Vizier of Remedies's "-1/-1 counters"
    /// replacement must fire on a -1/-1 counter addition, but not on a
    /// +1/+1 counter addition. Mirrors the Hardened Scales test in the
    /// opposite direction.
    #[test]
    fn counter_match_filters_vizier_from_plus_one_plus_one_event() {
        use crate::types::counter::{CounterMatch, CounterType};

        let source = ObjectId(10);
        let target = ObjectId(20);

        let repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(crate::types::ability::QuantityModification::Minus { value: 1 })
            .counter_match(CounterMatch::OfType(CounterType::Minus1Minus1));
        let mut state = test_state_with_object(source, Zone::Battlefield, vec![repl]);
        let mut creature = crate::game::game_object::GameObject::new(
            target,
            CardId(2),
            PlayerId(0),
            "C".into(),
            Zone::Battlefield,
        );
        creature
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        state.objects.insert(target, creature);
        state.battlefield.push_back(target);

        let registry = build_replacement_registry();

        let proposed_p1p1 = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: target,
                counter_type: CounterType::Plus1Plus1,
            },
            count: 1,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &proposed_p1p1, &registry).is_empty(),
            "Vizier-class replacement must not fire on +1/+1 counter additions"
        );

        let proposed_m1m1 = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: target,
                counter_type: CounterType::Minus1Minus1,
            },
            count: 1,
            applied: HashSet::new(),
        };
        assert_eq!(
            find_applicable_replacements(&state, &proposed_m1m1, &registry).len(),
            1,
            "Vizier-class replacement must fire on -1/-1 counter additions"
        );
    }

    /// CR 614.1a + CR 122.1a: Counter-agnostic replacements (Doubling Season's
    /// modern wording: "those counters") leave `counter_match = None` and
    /// continue to match every counter type — current behavior is preserved.
    #[test]
    fn counter_match_none_matches_any_counter_type() {
        use crate::types::counter::CounterType;

        let source = ObjectId(30);
        let target = ObjectId(40);

        let repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(crate::types::ability::QuantityModification::DOUBLE);
        // Note: counter_match is left as None.
        let mut state = test_state_with_object(source, Zone::Battlefield, vec![repl]);
        let mut creature = crate::game::game_object::GameObject::new(
            target,
            CardId(2),
            PlayerId(0),
            "C".into(),
            Zone::Battlefield,
        );
        creature
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        state.objects.insert(target, creature);
        state.battlefield.push_back(target);

        let registry = build_replacement_registry();
        for ct in [
            CounterType::Plus1Plus1,
            CounterType::Minus1Minus1,
            CounterType::Loyalty,
            CounterType::Generic("charge".to_string()),
        ] {
            let proposed = ProposedEvent::AddCounter {
                placement: CounterPlacement::Object {
                    actor: PlayerId(0),
                    object_id: target,
                    counter_type: ct.clone(),
                },
                count: 1,
                applied: HashSet::new(),
            };
            assert_eq!(
                find_applicable_replacements(&state, &proposed, &registry).len(),
                1,
                "counter_match=None must accept any counter type, including {ct:?}"
            );
        }
    }

    /// CR 614.6 + CR 303.4b: Blossombind — "Enchanted creature can't have
    /// counters put on it" lowers to an AddCounter-prevention replacement scoped
    /// to the Aura's enchanted host (CR 303.4b). Parsed from the real Oracle text, installed
    /// on an attached Aura, and driven through `replace_event`: a counter on the
    /// enchanted creature is Prevented, while a counter on an unrelated creature
    /// is not. Reverting the "enchanted creature" subject arm in
    /// `parse_no_counters_replacement` (or the Priority-6e split that routes
    /// Blossombind's compound line) leaves no replacement and the prevention
    /// assertion fails.
    #[test]
    fn blossombind_prevents_counters_on_enchanted_creature_only() {
        let mut state = GameState::new_two_player(42);

        let host = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bound Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let other = crate::game::zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Free Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&other)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Parse the real Blossombind static line and pull the counter-prohibition
        // replacement out of the cross-layer split.
        let parsed = crate::parser::parse_oracle_text(
            "Enchant creature\nWhen this Aura enters, tap enchanted creature.\nEnchanted creature can't become untapped and can't have counters put on it.",
            "Blossombind",
            &[],
            &["Enchantment".to_string()],
            &["Aura".to_string()],
        );
        assert!(
            !parsed.replacements.is_empty(),
            "Blossombind must yield a counter-prohibition replacement"
        );

        let aura = crate::game::zones::create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Blossombind".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.replacement_definitions = parsed.replacements.clone().into();
            obj.attached_to = Some(host.into());
        }
        state.objects.get_mut(&host).unwrap().attachments.push(aura);

        let on_host = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: host,
                counter_type: CounterType::Plus1Plus1,
            },
            count: 1,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        assert_eq!(
            replace_event(&mut state, on_host, &mut events),
            ReplacementResult::Prevented,
            "counters on the enchanted creature must be prevented"
        );

        let on_other = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: other,
                counter_type: CounterType::Plus1Plus1,
            },
            count: 1,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        assert!(
            find_applicable_replacements(&state, &on_other, &registry).is_empty(),
            "counters on a non-enchanted creature must not be prevented"
        );
    }

    #[test]
    fn global_object_counter_prohibition_prevents_listed_types_only() {
        let source = ObjectId(90);
        let target = ObjectId(91);
        let unrelated = ObjectId(92);
        let type_filter = TypeFilter::AnyOf(vec![
            TypeFilter::Artifact,
            TypeFilter::Creature,
            TypeFilter::Enchantment,
            TypeFilter::Land,
        ]);
        let repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .valid_card(TargetFilter::Typed(
                TypedFilter::new(type_filter).properties(vec![FilterProp::InZone {
                    zone: Zone::Battlefield,
                }]),
            ))
            .quantity_modification(QuantityModification::Prevent);
        let mut state = test_state_with_object(source, Zone::Battlefield, vec![repl]);

        let mut artifact = GameObject::new(
            target,
            CardId(91),
            PlayerId(1),
            "Target Artifact".to_string(),
            Zone::Battlefield,
        );
        artifact.card_types.core_types = vec![CoreType::Artifact];
        state.objects.insert(target, artifact);
        state.battlefield.push_back(target);

        let mut planeswalker = GameObject::new(
            unrelated,
            CardId(92),
            PlayerId(1),
            "Unrelated Planeswalker".to_string(),
            Zone::Battlefield,
        );
        planeswalker.card_types.core_types = vec![CoreType::Planeswalker];
        state.objects.insert(unrelated, planeswalker);
        state.battlefield.push_back(unrelated);

        let exiled_artifact_id = ObjectId(93);
        let mut exiled_artifact = GameObject::new(
            exiled_artifact_id,
            CardId(93),
            PlayerId(1),
            "Exiled Artifact".to_string(),
            Zone::Exile,
        );
        exiled_artifact.card_types.core_types = vec![CoreType::Artifact];
        state.objects.insert(exiled_artifact_id, exiled_artifact);
        state.exile.push_back(exiled_artifact_id);

        let registry = build_replacement_registry();
        let listed_type_event = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: target,
                counter_type: CounterType::Plus1Plus1,
            },
            count: 1,
            applied: HashSet::new(),
        };
        assert!(
            !find_applicable_replacements(&state, &listed_type_event, &registry).is_empty(),
            "artifact counter placement should match Solemnity's listed-type prohibition"
        );
        let mut events = Vec::new();
        assert_eq!(
            replace_event(&mut state, listed_type_event, &mut events),
            ReplacementResult::Prevented
        );

        let unlisted_type_event = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: unrelated,
                counter_type: CounterType::Loyalty,
            },
            count: 1,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &unlisted_type_event, &registry).is_empty(),
            "planeswalker counter placement should not match the artifact/creature/enchantment/land filter"
        );

        let exiled_artifact_event = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: exiled_artifact_id,
                counter_type: CounterType::Generic("egg".to_string()),
            },
            count: 1,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &exiled_artifact_event, &registry).is_empty(),
            "unqualified artifact/creature/enchantment/land wording must only match battlefield permanents"
        );
    }

    #[test]
    fn optional_counter_prevention_prompts_instead_of_auto_preventing() {
        let source = ObjectId(90);
        let target = ObjectId(91);
        let mut repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .valid_card(TargetFilter::Any)
            .quantity_modification(QuantityModification::Prevent);
        repl.mode = ReplacementMode::Optional { decline: None };
        let mut state = test_state_with_object(source, Zone::Battlefield, vec![repl]);

        let mut creature = GameObject::new(
            target,
            CardId(91),
            PlayerId(1),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        creature.card_types.core_types = vec![CoreType::Creature];
        state.objects.insert(target, creature);
        state.battlefield.push_back(target);

        let event = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: target,
                counter_type: CounterType::Plus1Plus1,
            },
            count: 1,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();

        assert_eq!(
            replace_event(&mut state, event, &mut events),
            ReplacementResult::NeedsChoice(PlayerId(1)),
            "optional counter prevention must use the normal replacement choice path"
        );
        assert!(state
            .pending_replacement
            .as_ref()
            .is_some_and(|pending| pending.is_optional));
    }

    #[test]
    fn player_counter_prohibition_does_not_match_object_counter_placement() {
        let source = ObjectId(90);
        let planeswalker_id = ObjectId(91);
        let mut repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Prevent);
        repl.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
        let mut state = test_state_with_object(source, Zone::Battlefield, vec![repl]);

        let mut planeswalker = GameObject::new(
            planeswalker_id,
            CardId(91),
            PlayerId(1),
            "Target Planeswalker".to_string(),
            Zone::Battlefield,
        );
        planeswalker.card_types.core_types = vec![CoreType::Planeswalker];
        state.objects.insert(planeswalker_id, planeswalker);
        state.battlefield.push_back(planeswalker_id);

        let registry = build_replacement_registry();
        let event = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: planeswalker_id,
                counter_type: CounterType::Loyalty,
            },
            count: 1,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &event, &registry).is_empty(),
            "player-scoped counter replacements must not match object counter placement"
        );
    }

    #[test]
    fn player_counter_replacement_scope_uses_recipient_not_actor() {
        let source = ObjectId(90);
        let mut repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Prevent);
        repl.valid_player = Some(ReplacementPlayerScope::You);
        let state = test_state_with_object(source, Zone::Battlefield, vec![repl]);
        let registry = build_replacement_registry();

        let controlled_actor_puts_counter_on_opponent = ProposedEvent::AddCounter {
            placement: CounterPlacement::Player {
                actor: PlayerId(0),
                player_id: PlayerId(1),
                counter_kind: crate::types::player::PlayerCounterKind::Poison,
            },
            count: 1,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(
                &state,
                &controlled_actor_puts_counter_on_opponent,
                &registry
            )
            .is_empty(),
            "controller-scoped player-counter replacement must not match only the actor placing counters"
        );

        let opponent_actor_puts_counter_on_controller = ProposedEvent::AddCounter {
            placement: CounterPlacement::Player {
                actor: PlayerId(1),
                player_id: PlayerId(0),
                counter_kind: crate::types::player::PlayerCounterKind::Poison,
            },
            count: 1,
            applied: HashSet::new(),
        };
        assert!(
            !find_applicable_replacements(
                &state,
                &opponent_actor_puts_counter_on_controller,
                &registry
            )
            .is_empty(),
            "controller-scoped player-counter replacement should match the recipient receiving counters"
        );
    }

    #[test]
    fn object_counter_replacement_without_player_scope_ignores_player_counter_events() {
        let source = ObjectId(90);
        let repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::DOUBLE);
        let state = test_state_with_object(source, Zone::Battlefield, vec![repl]);
        let registry = build_replacement_registry();

        let poison_event = ProposedEvent::AddCounter {
            placement: CounterPlacement::Player {
                actor: PlayerId(0),
                player_id: PlayerId(0),
                counter_kind: crate::types::player::PlayerCounterKind::Poison,
            },
            count: 1,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &poison_event, &registry).is_empty(),
            "object-counter replacement without valid_player must not match player counters"
        );

        let energy_event = ProposedEvent::AddCounter {
            placement: CounterPlacement::Energy {
                actor: PlayerId(0),
                player_id: PlayerId(0),
            },
            count: 1,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &energy_event, &registry).is_empty(),
            "object-counter replacement without valid_player must not match energy counters"
        );
    }

    /// SHAPE: `empty_mana_pool_matcher` returns true for an EmptyManaPool event
    /// with at least one `Drop`-disposition unit, false when every unit is
    /// already `Keep` or `Recolor(_)` (the per-event applicability gate; the
    /// per-handler filter is enforced in `find_applicable_replacements`'s
    /// sentinel block).
    #[test]
    fn empty_mana_pool_matcher_predicate() {
        use crate::types::mana::{ManaType, UnitDecision, UnitDisposition};

        let state = GameState::new_two_player(0);

        let with_drop = ProposedEvent::EmptyManaPool {
            player_id: PlayerId(0),
            units: vec![
                UnitDecision {
                    pool_index: 0,
                    color: ManaType::Green,
                    disposition: UnitDisposition::Keep,
                },
                UnitDecision {
                    pool_index: 1,
                    color: ManaType::Red,
                    disposition: UnitDisposition::Drop,
                },
            ],
            applied: HashSet::new(),
        };
        assert!(empty_mana_pool_matcher(&with_drop, ObjectId(0), &state));

        let all_kept = ProposedEvent::EmptyManaPool {
            player_id: PlayerId(0),
            units: vec![UnitDecision {
                pool_index: 0,
                color: ManaType::Green,
                disposition: UnitDisposition::Recolor(ManaType::Colorless),
            }],
            applied: HashSet::new(),
        };
        assert!(!empty_mana_pool_matcher(&all_kept, ObjectId(0), &state));

        // Non-EmptyManaPool events never match.
        let damage = ProposedEvent::Damage {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        assert!(!empty_mana_pool_matcher(&damage, ObjectId(0), &state));
    }

    /// SHAPE: `build_replacement_registry` registers `LoseMana` with the real
    /// `empty_mana_pool_matcher` (not the placeholder `stub_matcher`). Verified
    /// by feeding a synthetic event through the registered matcher and
    /// asserting it discriminates on the variant.
    #[test]
    fn lose_mana_registry_is_not_stub() {
        use crate::types::mana::{ManaType, UnitDecision, UnitDisposition};
        let registry = build_replacement_registry();
        let entry = registry
            .get(&ReplacementEvent::LoseMana)
            .expect("LoseMana must be registered");
        let state = GameState::new_two_player(0);

        // A real matcher rejects non-EmptyManaPool events (stub_matcher would
        // also reject, but would also reject EmptyManaPool — so the
        // discrimination below is what actually proves promotion).
        let damage = ProposedEvent::Damage {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 1,
            is_combat: false,
            applied: HashSet::new(),
        };
        assert!(!(entry.matcher)(&damage, ObjectId(0), &state));

        // A real matcher ACCEPTS an EmptyManaPool with a Drop unit.
        let pool = ProposedEvent::EmptyManaPool {
            player_id: PlayerId(0),
            units: vec![UnitDecision {
                pool_index: 0,
                color: ManaType::Green,
                disposition: UnitDisposition::Drop,
            }],
            applied: HashSet::new(),
        };
        assert!(
            (entry.matcher)(&pool, ObjectId(0), &state),
            "LoseMana registry must use the promoted empty_mana_pool_matcher, not the stub"
        );
    }

    // ---- Don't Blink: floating zone-redirect replacement (CR 614.1a/d, CR 601) ----

    /// Build the Don't Blink global `ChangeZone` redirect: a floating
    /// replacement installed under the sentinel `ObjectId(0)` that redirects a
    /// creature entering the battlefield to its owner's library, gated by
    /// `EnteredFromZone { Equals(Exile), cast_origin: Exile }`.
    fn dont_blink_global_replacement() -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::ChangeZone)
            .valid_card(TargetFilter::Typed(TypedFilter::creature()))
            .destination_zone(Zone::Battlefield)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Library,
                    target: TargetFilter::SelfRef,
                    owner_library: true,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: Default::default(),
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: Vec::new(),
                    face_down_profile: None,
                },
            ))
            .condition(ReplacementCondition::EnteredFromZone {
                origin_constraint: Some(OriginConstraint::Equals(Zone::Exile)),
                cast_origin: Some(Zone::Exile),
            })
    }

    /// A cast-origin-ONLY redirect: the clause carried no physical "would enter
    /// from <zone>" half, so `origin_constraint` is `None`. Mirrors
    /// `dont_blink_global_replacement` but isolates the cast half — used to
    /// prove the physical path stays inert when there is no physical constraint.
    fn cast_origin_only_global_replacement() -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::ChangeZone)
            .valid_card(TargetFilter::Typed(TypedFilter::creature()))
            .destination_zone(Zone::Battlefield)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Library,
                    target: TargetFilter::SelfRef,
                    owner_library: true,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: Default::default(),
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: Vec::new(),
                    face_down_profile: None,
                },
            ))
            .condition(ReplacementCondition::EnteredFromZone {
                origin_constraint: None,
                cast_origin: Some(Zone::Exile),
            })
    }

    /// Insert a creature object and return a two-player state holding it on the
    /// battlefield-bound entry path. `cast_from_zone` seeds the cast-origin half.
    fn state_with_entering_creature(
        obj_id: ObjectId,
        from: Zone,
        cast_from_zone: Option<Zone>,
    ) -> GameState {
        let mut state = GameState::new_two_player(42);
        let mut obj = GameObject::new(obj_id, CardId(1), PlayerId(0), "Creature".to_string(), from);
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.cast_from_zone = cast_from_zone;
        state.objects.insert(obj_id, obj);
        state
    }

    #[test]
    fn dont_blink_matches_creature_entering_from_exile() {
        // CR 614.1d: physical-from half — a creature moving from exile to the
        // battlefield is a candidate for the global redirect.
        let registry = build_replacement_registry();
        let mut state = state_with_entering_creature(ObjectId(20), Zone::Exile, None);
        state
            .pending_damage_replacements
            .push(dont_blink_global_replacement());
        let event = ProposedEvent::zone_change(ObjectId(20), Zone::Exile, Zone::Battlefield, None);
        let candidates = find_applicable_replacements(&state, &event, &registry);
        assert_eq!(
            candidates,
            vec![ReplacementId {
                source: ObjectId(0),
                index: 0
            }],
            "creature entering from exile must match the global zone redirect"
        );
    }

    #[test]
    fn dont_blink_rejects_creature_entering_from_hand() {
        // The EnteredFromZone gate must exclude non-exile origins; the Some(*from)
        // wrap correctly rejects Some(Hand) against Equals(Exile), and the cast
        // half is inert (no cast_from_zone).
        let registry = build_replacement_registry();
        let mut state = state_with_entering_creature(ObjectId(20), Zone::Hand, None);
        state
            .pending_damage_replacements
            .push(dont_blink_global_replacement());
        let event = ProposedEvent::zone_change(ObjectId(20), Zone::Hand, Zone::Battlefield, None);
        let candidates = find_applicable_replacements(&state, &event, &registry);
        assert!(
            candidates.is_empty(),
            "creature entering from hand must NOT match (got {candidates:?})"
        );
    }

    #[test]
    fn dont_blink_matches_creature_cast_from_exile_entering_from_stack() {
        // CR 601: cast-origin half (HARD GATE). A creature cast from exile enters
        // the battlefield FROM THE STACK (from = Stack, so the physical half is
        // Some(Stack) != Some(Exile) and is false), but cast_from_zone == Exile.
        // This isolates the cast half and proves the condition reads
        // affected_object_id (the entering object), NOT source_id (the sentinel
        // ObjectId(0), which has no cast_from_zone). Without this the cast arm
        // would ship dead.
        let registry = build_replacement_registry();
        let mut state = state_with_entering_creature(ObjectId(20), Zone::Stack, Some(Zone::Exile));
        state
            .pending_damage_replacements
            .push(dont_blink_global_replacement());
        let event = ProposedEvent::zone_change(ObjectId(20), Zone::Stack, Zone::Battlefield, None);
        let candidates = find_applicable_replacements(&state, &event, &registry);
        assert_eq!(
            candidates,
            vec![ReplacementId {
                source: ObjectId(0),
                index: 0
            }],
            "creature cast from exile (entering from stack) must match via the cast half"
        );
    }

    #[test]
    fn cast_origin_only_rejects_ordinary_exile_entry_without_cast_from_zone() {
        // CR 614.1d (blocker guard, PR #3419): a cast-origin-ONLY clause
        // (`origin_constraint: None`) must NOT match an ordinary creature
        // entering from exile that was not cast from exile. Pre-fix the absent
        // physical half collapsed to `OriginConstraint::Any`, so the OR-combined
        // physical path matched EVERY entry — this entry would have wrongly
        // matched. With the physical half modelled as `None`, only the cast half
        // is live, and this object has no `cast_from_zone`.
        let registry = build_replacement_registry();
        let mut state = state_with_entering_creature(ObjectId(20), Zone::Exile, None);
        state
            .pending_damage_replacements
            .push(cast_origin_only_global_replacement());
        let event = ProposedEvent::zone_change(ObjectId(20), Zone::Exile, Zone::Battlefield, None);
        let candidates = find_applicable_replacements(&state, &event, &registry);
        assert!(
            candidates.is_empty(),
            "cast-origin-only condition must NOT match an ordinary exile entry \
             with no cast_from_zone (pre-fix this matched via the Any physical \
             half); got {candidates:?}"
        );
    }

    #[test]
    fn cast_origin_only_matches_creature_cast_from_exile() {
        // CR 601: the live half of a cast-origin-only clause — a creature cast
        // from exile (entering from the stack) matches via `cast_from_zone`,
        // confirming the condition is not inert after the physical half became
        // optional.
        let registry = build_replacement_registry();
        let mut state = state_with_entering_creature(ObjectId(20), Zone::Stack, Some(Zone::Exile));
        state
            .pending_damage_replacements
            .push(cast_origin_only_global_replacement());
        let event = ProposedEvent::zone_change(ObjectId(20), Zone::Stack, Zone::Battlefield, None);
        let candidates = find_applicable_replacements(&state, &event, &registry);
        assert_eq!(
            candidates,
            vec![ReplacementId {
                source: ObjectId(0),
                index: 0
            }],
            "cast-origin-only condition must still match a creature cast from exile"
        );
    }

    #[test]
    fn dont_blink_excludes_noncreature_via_valid_card_gate() {
        // The valid_card gate (Typed creature) runs for non-damage global
        // entries: a land entering from exile must NOT match.
        let registry = build_replacement_registry();
        let mut state = GameState::new_two_player(42);
        let mut land = GameObject::new(
            ObjectId(21),
            CardId(2),
            PlayerId(0),
            "Land".to_string(),
            Zone::Exile,
        );
        land.card_types.core_types = vec![CoreType::Land];
        state.objects.insert(ObjectId(21), land);
        state
            .pending_damage_replacements
            .push(dont_blink_global_replacement());
        let event = ProposedEvent::zone_change(ObjectId(21), Zone::Exile, Zone::Battlefield, None);
        let candidates = find_applicable_replacements(&state, &event, &registry);
        assert!(
            candidates.is_empty(),
            "non-creature must be excluded by the valid_card gate (got {candidates:?})"
        );
    }

    #[test]
    fn global_store_damage_path_ignores_valid_card_filter() {
        // REGRESSION (BLOCKER guard): the prevent_damage typed-recipient shield
        // sets a `valid_card` recipient filter that is DELIBERATELY not enforced
        // for damage (prevent_damage.rs: "global shields must match any damage
        // event"). The generalized scan must still prevent a damage event whose
        // recipient does NOT match that typed filter — i.e. the new valid_card
        // gate must NOT run on the Damage path.
        let registry = build_replacement_registry();
        let mut state = GameState::new_two_player(42);
        // Global prevention shield carrying a typed recipient valid_card filter
        // that the damage target will NOT match.
        let shield = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .prevention_shield(PreventionAmount::Next(2))
            .valid_card(TargetFilter::Typed(TypedFilter::creature()));
        state.pending_damage_replacements.push(shield);
        let event = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        let candidates = find_applicable_replacements(&state, &event, &registry);
        assert_eq!(
            candidates,
            vec![ReplacementId {
                source: ObjectId(0),
                index: 0
            }],
            "damage prevention shield must remain a candidate despite a non-matching valid_card recipient filter"
        );
    }
}
