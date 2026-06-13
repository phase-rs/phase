//! CR 702.113a: Awaken — append the awaken resolution rider to a spell.
//!
//! "Awaken N—[cost]" represents two abilities (CR 702.113a): a static ability
//! that lets you pay the awaken cost instead of the spell's mana cost (wired in
//! `casting.rs`), and a spell ability — "If this spell's awaken cost was paid,
//! put N +1/+1 counters on target land you control. That land becomes a 0/0
//! Elemental creature with haste. It's still a land."
//!
//! When a spell is cast with `CastingVariant::Awaken`, [`append_awaken_rider`]
//! appends that spell ability to the TAIL of the spell's existing ability tree
//! at cast-preparation time. Appending (rather than transforming) preserves
//! CR 702.113a's "ALSO put ..." ordering: the printed effect resolves first,
//! then the awaken rider. The rider is only appended on the awaken variant, so a
//! normal cast leaves the ability tree untouched and requests no land target
//! (CR 702.113b: the awaken target exists only if the awaken cost was paid).
//!
//! Single authority: every call site routes through [`append_awaken_rider`].
//! The rider is composed entirely from existing effect primitives —
//! `Effect::PutCounter` surfaces the one land target slot (CR 601.2c), and a
//! chained `Effect::Animate` sub-ability inherits that chosen land via the
//! unconditional sub-target inheritance in `effects::resolve_ability_chain` (it
//! references `TargetFilter::ParentTarget`, a context ref that surfaces no extra
//! slot). No new `Effect`, `CounterType`, `Duration`, or `ContinuousModification`
//! variant is introduced.

use crate::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Duration, Effect, PtValue, QuantityExpr,
    TargetFilter, TypedFilter,
};
use crate::types::counter::CounterType;
use crate::types::keywords::Keyword;

/// CR 702.113a: "put N +1/+1 counters on target land you control. That land
/// becomes a 0/0 Elemental creature with haste. It's still a land." Builds the
/// awaken spell ability as a parent `PutCounter` (which surfaces the single
/// "target land you control" slot per CR 601.2c) with a chained `Animate`
/// sub-ability that inherits the chosen land via `TargetFilter::ParentTarget`.
///
/// The `Animate` effect sets P/T to 0/0 (CR 613.4b, Layer 7b), adds the Creature
/// core type and Elemental creature subtype (CR 613.1d, Layer 4), and grants
/// Haste (CR 613.1f, Layer 6). `remove_types` is empty so the Land card type and
/// its mana ability are preserved — CR 205.1b ("it's still a land"). The
/// animation's duration is `Duration::Permanent` (CR 611.2a: a continuous effect
/// from a resolving spell with no stated duration lasts until the end of the
/// game) — set on the sub-ability's `duration` field because `Effect::Animate`
/// itself carries no duration and `animate::resolve` reads `ability.duration`
/// (defaulting to `UntilEndOfTurn` when absent).
fn build_awaken_rider(count: u32) -> AbilityDefinition {
    let land_you_control = TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));

    // CR 613.4b + CR 613.1d + CR 613.1f + CR 205.1b: 0/0 Elemental creature with
    // haste that's still a land. `ParentTarget` inherits the land chosen by the
    // parent `PutCounter` (CR 608.2c) without surfacing a second target slot.
    let animate = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Animate {
            power: Some(PtValue::Fixed(0)),
            toughness: Some(PtValue::Fixed(0)),
            // "Creature" → AddType (CoreType::Creature); "Elemental" → AddSubtype.
            // Both are required to make the land a 0/0 *Elemental creature*; the
            // Land core type is retained because `remove_types` is empty.
            types: vec!["Creature".to_string(), "Elemental".to_string()],
            remove_types: vec![],
            target: TargetFilter::ParentTarget,
            keywords: vec![Keyword::Haste],
        },
    )
    // CR 611.2a: no stated duration → lasts until end of the game. `animate::resolve`
    // reads this field; without it the animation would default to UntilEndOfTurn.
    .duration(Duration::Permanent);

    // CR 702.113a + CR 601.2c: "put N +1/+1 counters on target land you control".
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed {
                value: count as i32,
            },
            target: land_you_control,
        },
    )
    .sub_ability(animate)
}

/// CR 702.113a: Append the awaken spell ability to the TAIL of `def`'s existing
/// ability tree so the printed effect resolves first, then the awaken rider.
/// Walks the `sub_ability` chain to its deepest leaf and attaches the rider
/// there; handles `def` having any `effect` variant and no pre-existing
/// `sub_ability`.
pub fn append_awaken_rider(def: &mut AbilityDefinition, count: u32) {
    let mut tail = def;
    while tail.sub_ability.is_some() {
        tail = tail
            .sub_ability
            .as_deref_mut()
            .expect("checked is_some above");
    }
    tail.sub_ability = Some(Box::new(build_awaken_rider(count)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Effect, TargetFilter, TypeFilter};

    /// CR 702.113a: The appended rider's chain tail is
    /// `PutCounter{Plus1Plus1, Fixed(N), land you control}` →
    /// `Animate{0/0, [Creature, Elemental], [Haste], ParentTarget}` with the
    /// sub-ability duration set to `Permanent`. The original printed effect is
    /// unchanged and resolves first.
    #[test]
    fn append_awaken_rider_builds_counter_then_animate_tail() {
        // Printed spell effect: a plain instant ("draw a card"-shaped leaf).
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Controller,
            },
        );
        append_awaken_rider(&mut def, 2);

        // Original printed effect is unchanged and first.
        assert!(
            matches!(*def.effect, Effect::Draw { .. }),
            "printed effect must remain the chain head"
        );

        // Chain tail head: PutCounter{Plus1Plus1, Fixed(2), land you control}.
        let put = def.sub_ability.as_deref().expect("rider appended");
        match &*put.effect {
            Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(*counter_type, CounterType::Plus1Plus1);
                assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
                match target {
                    TargetFilter::Typed(tf) => {
                        assert!(tf.type_filters.contains(&TypeFilter::Land));
                        assert_eq!(tf.controller, Some(ControllerRef::You));
                    }
                    other => panic!("expected land-you-control filter, got {other:?}"),
                }
            }
            other => panic!("expected PutCounter rider head, got {other:?}"),
        }
        // PutCounter must not request a duration (it's not a continuous effect).
        assert!(put.duration.is_none());

        // Sub-ability: Animate{0/0, [Creature, Elemental], [Haste], ParentTarget}
        // with Permanent duration.
        let anim = put.sub_ability.as_deref().expect("animate sub-ability");
        match &*anim.effect {
            Effect::Animate {
                power,
                toughness,
                types,
                remove_types,
                target,
                keywords,
            } => {
                assert_eq!(*power, Some(PtValue::Fixed(0)));
                assert_eq!(*toughness, Some(PtValue::Fixed(0)));
                assert!(types.contains(&"Creature".to_string()));
                assert!(types.contains(&"Elemental".to_string()));
                assert!(
                    remove_types.is_empty(),
                    "CR 205.1b: land type must be preserved"
                );
                assert_eq!(*target, TargetFilter::ParentTarget);
                assert!(keywords.contains(&Keyword::Haste));
            }
            other => panic!("expected Animate sub-ability, got {other:?}"),
        }
        // CR 611.2a: animation persists until end of game (Permanent), not EOT.
        assert_eq!(anim.duration, Some(Duration::Permanent));
    }

    /// The rider attaches at the deepest tail when the spell already has a
    /// `sub_ability` chain, leaving the existing chain intact and ordered first.
    #[test]
    fn append_awaken_rider_attaches_at_deepest_tail() {
        let inner = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        )
        .sub_ability(inner);
        append_awaken_rider(&mut def, 4);

        // head: DealDamage, then existing Draw sub, then PutCounter rider.
        assert!(matches!(*def.effect, Effect::DealDamage { .. }));
        let draw = def.sub_ability.as_deref().expect("existing sub intact");
        assert!(matches!(*draw.effect, Effect::Draw { .. }));
        let put = draw.sub_ability.as_deref().expect("rider at deepest tail");
        match &*put.effect {
            Effect::PutCounter { count, .. } => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 4 })
            }
            other => panic!("expected PutCounter at tail, got {other:?}"),
        }
    }
}
