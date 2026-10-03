//! Combat-trigger awareness for the AI's combat math.
//!
//! Two families of triggered abilities decide a combat the AI is evaluating,
//! yet neither shows up in a creature's printed power and toughness:
//!
//! 1. **Block-time P/T triggers.** They fire when blockers are declared and
//!    resolve in the declare blockers step, before combat damage, so they change
//!    who survives the exchange. Examples: Flanking (CR 702.25a), Bushido
//!    (CR 702.45a), Rampage (CR 702.23a), and printed "whenever this creature
//!    blocks or becomes blocked, it gets +N/+N" abilities. The keyword forms
//!    are synthesized into ordinary `TriggerDefinition`s by the card database,
//!    so one structural reader covers the keyword and printed forms alike. It
//!    does not match card names.
//! 2. **Connect triggers.** These are payoffs for dealing combat damage to a
//!    player or for going unblocked, like "whenever this creature deals combat
//!    damage to a player, draw a card." Blocking the creature denies the
//!    payoff, which is part of what a block is worth.
//!
//! Both readers only admit triggers they can price soundly. Anything else
//! (a conditional trigger, a pump aimed at a chosen target, an unmodelled
//! quantity) contributes nothing, so the combat math falls back to printed
//! stats. It never invents a shift the engine would not produce.

use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::GameObject;
use engine::types::ability::{
    AbilityDefinition, DamageKindFilter, Effect, FilterProp, PtValue, QuantityExpr, QuantityRef,
    TargetFilter, TriggerDefinition,
};
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::triggers::TriggerMode;

/// A P/T adjustment one combatant receives from block-time triggers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PtShift {
    pub power: i32,
    pub toughness: i32,
}

impl PtShift {
    fn add(&mut self, power: i32, toughness: i32) {
        self.power += power;
        self.toughness += toughness;
    }
}

/// Block-time P/T shifts for one hypothetical block declaration: one attacker
/// and the creatures blocking it. `blockers[i]` is the shift for
/// `blocker_ids[i]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BlockShifts {
    pub attacker: PtShift,
    pub blockers: Vec<PtShift>,
}

/// Which combatant a block-time trigger belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Attacker,
    Blocker(usize),
}

/// CR 509.1h + CR 509.3c + CR 509.3d + CR 603.3: projects the P/T shifts that
/// block-time triggers on the combatants would apply if `blocker_ids` were
/// declared as the blockers of `attacker_id`. Those triggers resolve in the
/// declare blockers step, before any combat damage is dealt, so the exchange
/// is fought at the shifted stats.
///
/// Only the combatants' own self-scoped triggers are read. A trigger that
/// watches some other creature ("whenever a creature you control becomes
/// blocked") lives on a permanent outside the exchange and is not modelled.
pub(crate) fn block_trigger_shifts(
    state: &GameState,
    attacker_id: ObjectId,
    blocker_ids: &[ObjectId],
) -> BlockShifts {
    let mut shifts = BlockShifts {
        attacker: PtShift::default(),
        blockers: vec![PtShift::default(); blocker_ids.len()],
    };
    let Some(attacker) = state.objects.get(&attacker_id) else {
        return shifts;
    };
    // Cheap exit for the overwhelmingly common case: nobody in the exchange has
    // any trigger at all.
    let any_triggers = !attacker.trigger_definitions.is_empty()
        || blocker_ids.iter().any(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|b| !b.trigger_definitions.is_empty())
        });
    if !any_triggers || blocker_ids.is_empty() {
        return shifts;
    }
    let blocker_count = blocker_ids.len() as i32;

    // The attacker's own "becomes blocked" triggers.
    for trigger in self_scoped_triggers(attacker) {
        if !fires_on_becoming_blocked(&trigger.mode) {
            continue;
        }
        match combat_filter(trigger) {
            // CR 509.3d: "becomes blocked by a creature [with quality]" fires
            // once for each blocker that matches the quality.
            Some(filter) => {
                let ctx = FilterContext::from_source(state, attacker_id);
                for (index, &blocker_id) in blocker_ids.iter().enumerate() {
                    if matches_target_filter(state, blocker_id, filter, &ctx) {
                        apply_trigger(
                            &mut shifts,
                            trigger,
                            Side::Attacker,
                            Some(Side::Blocker(index)),
                            blocker_count,
                        );
                    }
                }
            }
            // CR 509.3c: the bare "becomes blocked" form fires once however
            // many creatures block. With several blockers, "the blocking
            // creature" is ambiguous, so an effect aimed at it is skipped.
            None => {
                let triggering = (blocker_ids.len() == 1).then_some(Side::Blocker(0));
                apply_trigger(
                    &mut shifts,
                    trigger,
                    Side::Attacker,
                    triggering,
                    blocker_count,
                );
            }
        }
    }

    // Each blocker's own "blocks" triggers.
    for (index, &blocker_id) in blocker_ids.iter().enumerate() {
        let Some(blocker) = state.objects.get(&blocker_id) else {
            continue;
        };
        for trigger in self_scoped_triggers(blocker) {
            if !fires_on_blocking(&trigger.mode) {
                continue;
            }
            // CR 509.3b: "blocks a <quality> creature" — the attacker must
            // satisfy the quality.
            if let Some(filter) = combat_filter(trigger) {
                let ctx = FilterContext::from_source(state, blocker_id);
                if !matches_target_filter(state, attacker_id, filter, &ctx) {
                    continue;
                }
            }
            // CR 509.1h: the triggering object of a blocker declaration is the
            // blocker itself (engine `extract_source_from_event`), not the
            // creature it blocks.
            apply_trigger(
                &mut shifts,
                trigger,
                Side::Blocker(index),
                Some(Side::Blocker(index)),
                blocker_count,
            );
        }
    }
    shifts
}

/// CR 509.1h: trigger modes that fire for an attacker when it becomes blocked.
fn fires_on_becoming_blocked(mode: &TriggerMode) -> bool {
    matches!(
        mode,
        TriggerMode::BecomesBlocked
            | TriggerMode::BlocksOrBecomesBlocked
            | TriggerMode::AttackerBlocked
    )
}

/// CR 509.1h: trigger modes that fire for a creature when it is declared as a
/// blocker.
fn fires_on_blocking(mode: &TriggerMode) -> bool {
    matches!(
        mode,
        TriggerMode::Blocks | TriggerMode::BlocksOrBecomesBlocked | TriggerMode::AttacksOrBlocks
    )
}

/// The creature's own unconditional triggers: `valid_card` is absent or names
/// the creature itself. Conditional triggers are left out because their
/// condition cannot be evaluated against a hypothetical block.
fn self_scoped_triggers(obj: &GameObject) -> impl Iterator<Item = &TriggerDefinition> {
    obj.trigger_definitions
        .iter_unchecked()
        .map(|entry| &entry.definition)
        .filter(|trigger| {
            trigger
                .valid_card
                .as_ref()
                .is_none_or(|filter| matches!(filter, TargetFilter::SelfRef))
                && trigger.condition.is_none()
        })
}

/// CR 509.3d: the real blocker/attacker qualifier on a combat trigger. A
/// `TargetFilter::Player` in `valid_target` is the effect's player target
/// surfaced by the lowering, never a combat qualifier (mirrors the engine's
/// `trigger_matchers::combat_filter`).
fn combat_filter(trigger: &TriggerDefinition) -> Option<&TargetFilter> {
    trigger
        .valid_target
        .as_ref()
        .filter(|filter| !matches!(filter, TargetFilter::Player))
}

/// Folds the P/T pumps in `trigger`'s effect chain into `shifts`. `source` is
/// the combatant that owns the trigger; `triggering` is the combatant the
/// engine binds `TriggeringSource` to for this firing — the blocker, for both a
/// per-blocker "becomes blocked by a creature" firing and a "blocks" firing —
/// or `None` when that binding is ambiguous.
fn apply_trigger(
    shifts: &mut BlockShifts,
    trigger: &TriggerDefinition,
    source: Side,
    triggering: Option<Side>,
    blocker_count: i32,
) {
    let Some(execute) = trigger.execute.as_deref() else {
        return;
    };
    if execute.condition.is_some() {
        return;
    }
    let mut link: Option<&AbilityDefinition> = Some(execute);
    while let Some(ability) = link {
        if let Effect::Pump {
            power,
            toughness,
            target,
        } = &*ability.effect
        {
            let recipient = match target {
                TargetFilter::SelfRef => Some(source),
                TargetFilter::TriggeringSource => triggering,
                _ => None,
            };
            let amounts = pt_amount(power, blocker_count).zip(pt_amount(toughness, blocker_count));
            if let (Some(side), Some((dp, dt))) = (recipient, amounts) {
                match side {
                    Side::Attacker => shifts.attacker.add(dp, dt),
                    Side::Blocker(index) => {
                        if let Some(shift) = shifts.blockers.get_mut(index) {
                            shift.add(dp, dt);
                        }
                    }
                }
            }
        }
        link = ability.sub_ability.as_deref();
    }
}

fn pt_amount(value: &PtValue, blocker_count: i32) -> Option<i32> {
    match value {
        PtValue::Fixed(n) => Some(*n),
        PtValue::Quantity(expr) => block_quantity(expr, blocker_count),
        PtValue::Variable(_) => None,
    }
}

/// Evaluates a pump quantity against a hypothetical block. The only dynamic
/// reference understood is the count of creatures blocking the source (CR
/// 702.23a Rampage's "for each creature blocking it beyond the first"), which
/// is known exactly for a hypothetical declaration. Any other reference makes
/// the quantity unknown.
fn block_quantity(expr: &QuantityExpr, blocker_count: i32) -> Option<i32> {
    match expr {
        QuantityExpr::Fixed { value } => Some(*value),
        QuantityExpr::Offset { inner, offset } => {
            Some(block_quantity(inner, blocker_count)? + offset)
        }
        // CR 107.1b: a negative result is replaced by the clamp floor.
        QuantityExpr::ClampMin { inner, minimum } => {
            Some(block_quantity(inner, blocker_count)?.max(*minimum))
        }
        QuantityExpr::Multiply { factor, inner } => {
            Some(factor * block_quantity(inner, blocker_count)?)
        }
        QuantityExpr::Sum { exprs } => exprs
            .iter()
            .map(|expr| block_quantity(expr, blocker_count))
            .sum(),
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } if counts_blockers_of_source(filter) => Some(blocker_count),
        _ => None,
    }
}

fn counts_blockers_of_source(filter: &TargetFilter) -> bool {
    matches!(filter, TargetFilter::Typed(typed) if typed.properties.contains(&FilterProp::BlockingSource))
}

/// Combat-value units credited per card a connect trigger draws. The combat
/// math prices bodies at `1.5 * power + toughness` (a 2/2 is 5.0), and a card
/// is worth at least a small creature, so a card is priced just above a 1/1.
const CONNECT_CARD_VALUE: f64 = 3.0;

/// CR 510.1 + CR 120.3 + CR 509.1h: the value, in combat-eval units, of the
/// payoff `obj` collects when it connects in combat, from "deals combat damage
/// to a player" triggers and "attacks and isn't blocked" triggers. A block
/// denies it, so it counts toward what blocking this attacker is worth and
/// toward what an unblocked swing is worth to its controller.
///
/// Only established positive payoffs are priced. At block time the damage
/// event does not exist yet, so an intervening-if condition (CR 603.4), a
/// damage-amount threshold, a trigger constraint or an "unless [a player]
/// pays" rider (CR 118.12a) cannot be confirmed — such a trigger is worth 0,
/// as is any chain containing an effect this reader does not price (a harmful
/// rider, a loot's discard, an opponent's draw).
pub(crate) fn connect_trigger_value(obj: &GameObject) -> f64 {
    if obj.trigger_definitions.is_empty() {
        return 0.0;
    }
    obj.trigger_definitions
        .iter_unchecked()
        .map(|entry| &entry.definition)
        .filter(|trigger| is_connect_trigger(trigger) && fires_unconditionally(trigger))
        .filter_map(|trigger| trigger.execute.as_deref())
        .filter_map(connect_payoff_value)
        .sum()
}

/// No trigger-level gate that could stop the payoff once the source connects.
fn fires_unconditionally(trigger: &TriggerDefinition) -> bool {
    trigger.condition.is_none()
        && trigger.constraint.is_none()
        && trigger.unless_pay.is_none()
        && trigger.damage_amount.is_none()
}

/// The value of a connect trigger's effect chain, or `None` when any link is
/// conditional or not a recognised positive outcome for the controller.
fn connect_payoff_value(execute: &AbilityDefinition) -> Option<f64> {
    let mut total = 0.0;
    let mut link: Option<&AbilityDefinition> = Some(execute);
    while let Some(ability) = link {
        if ability.condition.is_some()
            || ability.unless_pay.is_some()
            || ability.repeat_for.is_some()
            || ability.modal.is_some()
        {
            return None;
        }
        total += match &*ability.effect {
            Effect::Draw {
                count: QuantityExpr::Fixed { value },
                target: TargetFilter::Controller,
            } if *value > 0 => f64::from(*value) * CONNECT_CARD_VALUE,
            // A P/T pump on an unblocked attacker is already paid for by the
            // damage it adds, which the caller counts through power.
            Effect::Pump { .. } => 0.0,
            _ => return None,
        };
        link = ability.sub_ability.as_deref();
    }
    Some(total)
}

fn is_connect_trigger(trigger: &TriggerDefinition) -> bool {
    match trigger.mode {
        // CR 120.3 + CR 510.2: the source dealt combat damage to a player.
        TriggerMode::DamageDone | TriggerMode::DamageDoneOnce => {
            let from_self = trigger
                .valid_source
                .as_ref()
                .is_none_or(|filter| matches!(filter, TargetFilter::SelfRef));
            let to_player = matches!(trigger.valid_target, Some(TargetFilter::Player));
            let combat = matches!(
                trigger.damage_kind,
                DamageKindFilter::CombatOnly | DamageKindFilter::Any
            );
            from_self && to_player && combat
        }
        // CR 509.1h: the source attacked and is an unblocked creature.
        TriggerMode::AttackerUnblocked | TriggerMode::AttackerUnblockedOnce => trigger
            .valid_card
            .as_ref()
            .is_none_or(|filter| matches!(filter, TargetFilter::SelfRef)),
        _ => false,
    }
}
