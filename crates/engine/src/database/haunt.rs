//! Haunt (CR 702.55) — synthesis of the keyword's triggered abilities.
//!
//! `Keyword::Haunt` carries no parameters; the keyword's ability and its
//! reminder text are otherwise dropped by the parser, so without synthesis a
//! haunt card is a silent no-op. This module builds, for every `Keyword::Haunt`
//! face:
//!
//! 1. **The haunt ability (CR 702.55a)** — a `ChangesZone` triggered ability
//!    whose effect is [`Effect::ExileHaunting`] (resolved by `game/haunt.rs`):
//!    - permanent (creature): "When this permanent is put into a graveyard from
//!      the battlefield, exile it haunting target creature" — origin
//!      `Battlefield`, destination `Graveyard` (a dies trigger);
//!    - instant/sorcery: "When this spell is put into a graveyard during its
//!      resolution, exile it haunting target creature" — origin `Stack`,
//!      destination `Graveyard`, fired from the graveyard via `trigger_zones`.
//!
//! 2. **The haunt-payoff (CR 702.55c)** — a `HauntedCreatureDies` trigger that
//!    fires from exile when the haunted creature dies. For a creature, the
//!    payoff is the same effect as the card's "enters **or** the creature it
//!    haunts dies" ability, whose ETB half the parser already produced; this
//!    module clones that effect into the `HauntedCreatureDies` trigger (a single
//!    `TriggerDefinition` cannot hold both the ETB and the haunted-dies trigger
//!    conditions). For an instant/sorcery the payoff ("When the creature this
//!    card haunts dies, …") is produced directly by the parser as a
//!    `HauntedCreatureDies` trigger; this module only ensures it fires from
//!    exile.

use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, TargetFilter, TriggerDefinition, TypeFilter,
    TypedFilter,
};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::keywords::Keyword;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

/// CR 702.55: Synthesize the haunt ability and the haunt-payoff trigger for a
/// `Keyword::Haunt` face. Idempotent — re-running `synthesize_all` does not stack
/// duplicate triggers.
pub fn synthesize_haunt(face: &mut CardFace) {
    if !face.keywords.iter().any(|k| matches!(k, Keyword::Haunt)) {
        return;
    }
    // Idempotency: the synthesized haunt ability is the only `ExileHaunting`
    // emitter, so its presence means synthesis already ran.
    if face.triggers.iter().any(|t| {
        trigger_chain_has(t, |a| {
            matches!(a.effect.as_ref(), Effect::ExileHaunting { .. })
        })
    }) {
        return;
    }

    let is_creature = face.card_type.core_types.contains(&CoreType::Creature);

    // CR 702.55c: the creature-form payoff is the same effect as the card's ETB
    // self-trigger ("enters or the creature it haunts dies"). Build the
    // `HauntedCreatureDies` clones from the existing ETB self-triggers BEFORE
    // appending the haunt ability (which is itself a self zone-change trigger
    // and must not be mistaken for an ETB payoff).
    let payoff_clones: Vec<TriggerDefinition> = if is_creature {
        face.triggers
            .iter()
            .filter(|t| is_etb_self_trigger(t))
            .filter_map(|t| {
                t.execute
                    .as_ref()
                    .map(|e| haunt_payoff_trigger((**e).clone()))
            })
            .collect()
    } else {
        Vec::new()
    };

    // CR 702.55a: the haunt ability.
    face.triggers.push(exile_haunting_trigger(is_creature));
    face.triggers.extend(payoff_clones);

    // CR 702.55c: spell-form payoff is produced by the parser as a
    // `HauntedCreatureDies` trigger; ensure it functions in the exile zone.
    if !is_creature {
        for t in face.triggers.iter_mut() {
            if matches!(t.mode, TriggerMode::HauntedCreatureDies) && t.trigger_zones.is_empty() {
                t.trigger_zones = vec![Zone::Exile];
            }
        }
    }
}

/// CR 702.55a: "exile it haunting target creature." The haunted creature is a
/// real target chosen as the haunt trigger goes on the stack.
fn exile_haunting_effect() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ExileHaunting {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
        },
    )
    .description("CR 702.55a: Exile it haunting target creature.".to_string())
}

/// CR 702.55a: The haunt ability — a `ChangesZone` "put into a graveyard"
/// trigger. A creature's fires on leaving the battlefield (dies); a spell's
/// fires on leaving the stack (resolving to the graveyard), scanned from the
/// graveyard via `trigger_zones`.
fn exile_haunting_trigger(is_creature: bool) -> TriggerDefinition {
    let origin = if is_creature {
        Zone::Battlefield
    } else {
        Zone::Stack
    };
    let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .origin(origin)
        .destination(Zone::Graveyard)
        .valid_card(TargetFilter::SelfRef)
        .execute(exile_haunting_effect())
        .description(if is_creature {
            "CR 702.55a: When this permanent is put into a graveyard from the battlefield, \
             exile it haunting target creature."
                .to_string()
        } else {
            "CR 702.55a: When this spell is put into a graveyard during its resolution, \
             exile it haunting target creature."
                .to_string()
        });
    if !is_creature {
        // CR 113.6k: the source is in the graveyard when this self zone-change
        // from the stack is scanned, so the trigger must function there.
        trigger.trigger_zones = vec![Zone::Graveyard];
    }
    trigger
}

/// CR 702.55c: Build the haunt-payoff trigger from the card's parsed payoff
/// effect. Fires from exile when the haunted creature dies.
fn haunt_payoff_trigger(effect: AbilityDefinition) -> TriggerDefinition {
    let mut trigger = TriggerDefinition::new(TriggerMode::HauntedCreatureDies)
        .valid_card(TargetFilter::SelfRef)
        .execute(effect)
        .description(
            "CR 702.55c: When the creature this card haunts dies, trigger the haunt payoff."
                .to_string(),
        );
    trigger.trigger_zones = vec![Zone::Exile];
    trigger
}

/// CR 702.55c: A card's ETB self-trigger: `ChangesZone` to the battlefield matching itself,
/// or the haunt creature compound "enters or the creature it haunts dies".
/// For a haunt creature this is the haunt payoff whose effect is cloned into
/// exile by synthesis.
fn is_etb_self_trigger(trigger: &TriggerDefinition) -> bool {
    trigger.valid_card == Some(TargetFilter::SelfRef)
        && trigger.destination == Some(Zone::Battlefield)
        && matches!(
            trigger.mode,
            TriggerMode::ChangesZone | TriggerMode::EntersOrHauntedCreatureDies
        )
}

/// Walk a trigger's `execute` ability chain, testing `pred` on each step.
fn trigger_chain_has(
    trigger: &TriggerDefinition,
    pred: impl Fn(&AbilityDefinition) -> bool,
) -> bool {
    fn walk(ability: &AbilityDefinition, pred: &impl Fn(&AbilityDefinition) -> bool) -> bool {
        pred(ability)
            || ability
                .sub_ability
                .as_deref()
                .is_some_and(|s| walk(s, pred))
    }
    trigger.execute.as_ref().is_some_and(|a| walk(a, &pred))
}
