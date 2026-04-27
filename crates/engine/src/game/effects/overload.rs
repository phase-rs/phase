//! CR 702.96b: Overload — transform every `target` in a spell's text to `each`.
//!
//! When a spell is cast with `CastingVariant::Overload`, its ability tree is
//! rewritten at cast-preparation time: target-bearing effects are promoted to
//! their all-matching counterparts. Per CR 702.96c the overloaded spell has
//! no targets, so the transformed effects carry no `TargetRef` slots and
//! target selection is naturally skipped.
//!
//! Single authority: every call site routes through [`transform_ability_def`].
//! No scattered `target → each` logic is permitted elsewhere.
//!
//! Effects transformed (covers the printed Overload corpus):
//! - `Destroy { target, cant_regenerate }` → `DestroyAll { target, cant_regenerate }`
//! - `Pump { power, toughness, target }` → `PumpAll { power, toughness, target }`
//! - `DealDamage { amount, target, damage_source }` → `DamageAll { amount, target, player_filter: None }`
//!   (the `damage_source` override is dropped: `DamageAll` always resolves
//!   with the resolving spell as the source per CR 120.3, which matches every
//!   overload card in the current corpus.)
//! - `Tap { target }` → `TapAll { target }`
//! - `Bounce { target, destination }` → `BounceAll { target, destination }`
//!   (the canonical mass-bounce variant; mirrors `Destroy` → `DestroyAll`
//!   and `Pump` → `PumpAll` in shape — see CR 400.7 + CR 611.2c).
//! - `ChangeZone { destination, target, ... }` → `ChangeZoneAll { origin, destination, target }`
//!   (Winds of Abandon: "Exile target creature you don't control" → exile
//!   each. The single-target flags `enter_tapped`/`enter_transformed`/
//!   `under_your_control`/`enters_attacking`/`up_to`/`enter_with_counters`
//!   are dropped — `ChangeZoneAll` does not carry them and the overload
//!   corpus exiles to a hidden zone where they have no semantics.)
//!
//! Effects with no all-matching counterpart (e.g. `Counter` — Counterflux)
//! are preserved unchanged; the overloaded cast simply has no useful effect
//! for those. CR 702.96a's clarification that the transformation applies
//! only to the word "target" in the spell's text matches this behavior.

use crate::types::ability::{AbilityDefinition, Effect};

/// Transform an ability definition tree in place: rewrite every target-bearing
/// effect into its all-matching counterpart and recurse into `sub_ability`,
/// `else_ability`, and `mode_abilities`.
pub fn transform_ability_def(def: &mut AbilityDefinition) {
    transform_effect_in_place(def.effect.as_mut());
    if let Some(sub) = def.sub_ability.as_mut() {
        transform_ability_def(sub);
    }
    if let Some(els) = def.else_ability.as_mut() {
        transform_ability_def(els);
    }
    for mode in def.mode_abilities.iter_mut() {
        transform_ability_def(mode);
    }
}

/// CR 702.96b: Rewrite a single `Effect` in place. Leaves non-target-bearing
/// variants untouched.
fn transform_effect_in_place(effect: &mut Effect) {
    // Replace `*effect` only when we need to rebuild the enum variant. We use
    // `std::mem::replace` against a placeholder so we can move the owned
    // fields out of the old variant without cloning.
    let placeholder = Effect::Unimplemented {
        name: String::new(),
        description: None,
    };
    let owned = std::mem::replace(effect, placeholder);
    *effect = match owned {
        Effect::Destroy {
            target,
            cant_regenerate,
        } => Effect::DestroyAll {
            target,
            cant_regenerate,
        },
        Effect::Pump {
            power,
            toughness,
            target,
        } => Effect::PumpAll {
            power,
            toughness,
            target,
        },
        Effect::DealDamage {
            amount,
            target,
            damage_source: _,
        } => Effect::DamageAll {
            amount,
            target,
            player_filter: None,
        },
        Effect::Tap { target } => Effect::TapAll { target },
        // CR 702.96b + CR 400.7 + CR 611.2c: Cyclonic Rift overload — promote
        // single-target Bounce to the canonical mass-bounce variant. Preserves
        // `destination` so top-of-library overloads (none in current corpus
        // but type-system-supported) thread through unchanged.
        Effect::Bounce {
            target,
            destination,
        } => Effect::BounceAll {
            target,
            destination,
        },
        // CR 702.96b + CR 701.13a: Winds of Abandon overload — promote the
        // single-target `ChangeZone` to its mass counterpart so "exile target
        // creature you don't control" becomes "exile each creature you don't
        // control". Single-target-only flags (enter_tapped, enter_transformed,
        // under_your_control, enters_attacking, up_to, enter_with_counters)
        // are dropped — `ChangeZoneAll` carries no equivalents and the
        // overload corpus uses this only for hidden-zone exile where these
        // modifiers have no semantics.
        Effect::ChangeZone {
            origin,
            destination,
            target,
            // Single-target-only modifiers — `ChangeZoneAll` carries no
            // equivalents and the overload corpus uses ChangeZone only for
            // hidden-zone exile where these have no semantics. Bind each
            // field by name (no `..`) so any new `ChangeZone` field added
            // upstream forces a deliberate decision here.
            owner_library: _, // dropped: ChangeZoneAll always uses target's library scope
            enter_transformed: _, // dropped: hidden-zone exile, no battlefield-side effect
            under_your_control: _, // dropped: hidden-zone exile, no controller swap
            enter_tapped: _,  // dropped: hidden-zone exile, tap state irrelevant
            enters_attacking: _, // dropped: hidden-zone exile, combat irrelevant
            up_to: _,         // dropped: ChangeZoneAll has no count semantics
            enter_with_counters: _, // dropped: hidden-zone exile, no counters
        } => Effect::ChangeZoneAll {
            origin,
            destination,
            target,
        },
        // Effects without an all-matching counterpart (e.g. `Counter` for
        // Counterflux) are preserved as-is. No overload corpus card has a
        // meaningful transformation for these today.
        other => other,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PtValue, QuantityExpr, TargetFilter, TypeFilter,
        TypedFilter,
    };
    use crate::types::Zone;

    fn creature_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![],
        })
    }

    fn leaf(effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, effect)
    }

    #[test]
    fn destroy_becomes_destroy_all() {
        let mut def = leaf(Effect::Destroy {
            target: creature_filter(),
            cant_regenerate: true,
        });
        transform_ability_def(&mut def);
        match *def.effect {
            Effect::DestroyAll {
                cant_regenerate, ..
            } => assert!(cant_regenerate),
            other => panic!("expected DestroyAll, got {other:?}"),
        }
    }

    #[test]
    fn pump_becomes_pump_all() {
        let mut def = leaf(Effect::Pump {
            power: PtValue::Fixed(-4),
            toughness: PtValue::Fixed(0),
            target: creature_filter(),
        });
        transform_ability_def(&mut def);
        assert!(matches!(*def.effect, Effect::PumpAll { .. }));
    }

    #[test]
    fn deal_damage_becomes_damage_all() {
        let mut def = leaf(Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 4 },
            target: creature_filter(),
            damage_source: None,
        });
        transform_ability_def(&mut def);
        assert!(matches!(*def.effect, Effect::DamageAll { .. }));
    }

    #[test]
    fn tap_becomes_tap_all() {
        let mut def = leaf(Effect::Tap {
            target: creature_filter(),
        });
        transform_ability_def(&mut def);
        assert!(matches!(*def.effect, Effect::TapAll { .. }));
    }

    #[test]
    fn bounce_becomes_bounce_all_with_destination_preserved() {
        // CR 702.96b + CR 400.7 + CR 611.2c: Cyclonic Rift overload promotes
        // single-target `Bounce` to the canonical mass-bounce variant. Default
        // destination (`None`) means owner's hand at resolve time, matching
        // the single-target Bounce semantics.
        let mut def = leaf(Effect::Bounce {
            target: creature_filter(),
            destination: None,
        });
        transform_ability_def(&mut def);
        match *def.effect {
            Effect::BounceAll {
                destination,
                ref target,
            } => {
                assert!(destination.is_none(), "default destination preserved");
                assert!(matches!(target, TargetFilter::Typed(_)));
            }
            ref other => panic!("expected BounceAll, got {other:?}"),
        }

        // Confirm the destination override threads through unchanged.
        let mut def_lib = leaf(Effect::Bounce {
            target: creature_filter(),
            destination: Some(Zone::Library),
        });
        transform_ability_def(&mut def_lib);
        match *def_lib.effect {
            Effect::BounceAll {
                destination: Some(dest),
                ..
            } => assert_eq!(dest, Zone::Library),
            ref other => panic!("expected BounceAll {{ destination: Library }}, got {other:?}"),
        }
    }

    /// CR 702.96b + CR 701.13a: Winds of Abandon overload — single-target
    /// `ChangeZone(exile target opponent's creature)` must promote to
    /// `ChangeZoneAll(exile each creature you don't control)`. The filter
    /// (controller=Opponent) survives unchanged so the mass exile only hits
    /// opponents' creatures, never the caster's own.
    #[test]
    fn change_zone_becomes_change_zone_all() {
        use crate::types::ability::ControllerRef;
        let mut def = leaf(Effect::ChangeZone {
            origin: None,
            destination: Zone::Exile,
            target: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::Opponent),
                properties: vec![],
            }),
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        });
        transform_ability_def(&mut def);
        match *def.effect {
            Effect::ChangeZoneAll {
                origin,
                destination,
                ref target,
            } => {
                assert!(origin.is_none());
                assert_eq!(destination, Zone::Exile);
                match target {
                    TargetFilter::Typed(tf) => {
                        assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                        assert!(tf.type_filters.contains(&TypeFilter::Creature));
                    }
                    other => panic!("expected typed creature filter, got {other:?}"),
                }
            }
            ref other => panic!("expected ChangeZoneAll, got {other:?}"),
        }
    }

    #[test]
    fn counter_preserved_unchanged() {
        let mut def = leaf(Effect::Counter {
            target: creature_filter(),
            source_static: None,
            unless_payment: None,
        });
        transform_ability_def(&mut def);
        assert!(matches!(*def.effect, Effect::Counter { .. }));
    }

    #[test]
    fn recurses_into_sub_ability() {
        let sub = leaf(Effect::Destroy {
            target: creature_filter(),
            cant_regenerate: false,
        });
        let mut parent = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Tap {
                target: creature_filter(),
            },
        )
        .sub_ability(sub);
        transform_ability_def(&mut parent);
        assert!(matches!(*parent.effect, Effect::TapAll { .. }));
        let sub_ref = parent.sub_ability.as_ref().expect("sub present");
        assert!(matches!(*sub_ref.effect, Effect::DestroyAll { .. }));
    }
}
