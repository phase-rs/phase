//! Regression snapshot for `effect_classify::effect_polarity`.
//!
//! `effect_polarity` was made exhaustive over `Effect` (the trailing `_ =>
//! Contextual` wildcard was replaced with an explicit enumeration of every
//! formerly-unclassified variant, all mapped to `Contextual`). Two guards
//! protect that change:
//!
//! 1. **Compile-time exhaustiveness** — because the wildcard is gone, a newly
//!    added `Effect` variant fails to compile in `effect_polarity` until it is
//!    deliberately classified. That is the forcing function; it cannot be a
//!    runtime assertion (there is no `EnumIter` on `Effect`).
//! 2. **This snapshot** — locks the concrete classifications so a future edit
//!    that accidentally moves a `Beneficial`/`Harmful` variant into the
//!    Contextual bulk (or vice versa) fails. Constructing all 210 variants is
//!    impractical without `EnumIter`, so this samples representatives of every
//!    polarity outcome, both branch-dependent variants (`ChangeZone`,
//!    `SetTapState`), and a spread of formerly-wildcarded variants now proven
//!    to return `Contextual`.

use engine::types::ability::{Effect, EffectScope, QuantityExpr, TapStateChange, TargetFilter};
use engine::types::counter::CounterType;
use engine::types::zones::{EtbTapState, Zone};

use crate::policies::effect_classify::{effect_polarity, EffectPolarity};

fn change_zone(destination: Zone) -> Effect {
    Effect::ChangeZone {
        origin: None,
        destination,
        target: TargetFilter::Any,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
    }
}

#[test]
fn beneficial_classifications_unchanged() {
    assert_eq!(
        effect_polarity(&Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        }),
        EffectPolarity::Beneficial
    );
    assert_eq!(
        effect_polarity(&Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        }),
        EffectPolarity::Beneficial
    );
    // CR 122.1: +1/+1 counter placement is beneficial to the bearer.
    assert_eq!(
        effect_polarity(&Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
        }),
        EffectPolarity::Beneficial
    );
    // Branch-dependent: ChangeZone → Battlefield is beneficial.
    assert_eq!(
        effect_polarity(&change_zone(Zone::Battlefield)),
        EffectPolarity::Beneficial
    );
    // Guarded arm: Single + Untap is beneficial.
    assert_eq!(
        effect_polarity(&Effect::SetTapState {
            target: TargetFilter::Any,
            scope: EffectScope::Single,
            state: TapStateChange::Untap,
        }),
        EffectPolarity::Beneficial
    );
}

#[test]
fn harmful_classifications_unchanged() {
    assert_eq!(
        effect_polarity(&Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        }),
        EffectPolarity::Harmful
    );
    assert_eq!(
        effect_polarity(&Effect::Mill {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Player,
            destination: Zone::Graveyard,
        }),
        EffectPolarity::Harmful
    );
    // Branch-dependent: ChangeZone → Exile is harmful.
    assert_eq!(
        effect_polarity(&change_zone(Zone::Exile)),
        EffectPolarity::Harmful
    );
    assert_eq!(
        effect_polarity(&change_zone(Zone::Graveyard)),
        EffectPolarity::Harmful
    );
    // Guarded arm: Single + Tap is harmful.
    assert_eq!(
        effect_polarity(&Effect::SetTapState {
            target: TargetFilter::Any,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        }),
        EffectPolarity::Harmful
    );
}

#[test]
fn contextual_classifications_unchanged() {
    // Previously explicit Contextual arms.
    assert_eq!(
        effect_polarity(&Effect::Proliferate),
        EffectPolarity::Contextual
    );

    // Formerly caught by the removed `_` wildcard — now explicitly enumerated,
    // still Contextual. If any of these ever needs a real polarity, the arm
    // must move deliberately and this assertion will flag the change.
    assert_eq!(
        effect_polarity(&Effect::GainEnergy {
            amount: QuantityExpr::Fixed { value: 1 },
        }),
        EffectPolarity::Contextual
    );
    assert_eq!(
        effect_polarity(&Effect::Amass {
            subtype: "Zombie".to_string(),
            count: QuantityExpr::Fixed { value: 1 },
        }),
        EffectPolarity::Contextual
    );
    // Mass (non-Single) SetTapState scope falls through to the Contextual bulk.
    assert_eq!(
        effect_polarity(&Effect::SetTapState {
            target: TargetFilter::Any,
            scope: EffectScope::All,
            state: TapStateChange::Tap,
        }),
        EffectPolarity::Contextual
    );
    for effect in [
        Effect::NoOp,
        Effect::Cascade,
        Effect::TimeTravel,
        Effect::ManifestDread,
        Effect::Forage,
        Effect::Learn,
        Effect::Planeswalk,
        Effect::Encore,
        Effect::Myriad,
        Effect::EndTheTurn,
        Effect::TakeTheInitiative,
        Effect::VentureIntoDungeon,
        Effect::RingTemptsYou,
        Effect::Specialize,
        Effect::SolveCase,
        Effect::HeistExile,
        Effect::ProcessRadCounters,
    ] {
        assert_eq!(
            effect_polarity(&effect),
            EffectPolarity::Contextual,
            "{effect:?} must remain Contextual",
        );
    }
}
