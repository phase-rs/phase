//! Issue #5264 — Sun Titan: "Whenever this creature enters or attacks, you may
//! return target permanent card with mana value 3 or less from your graveyard to
//! the battlefield."
//!
//! https://github.com/phase-rs/phase/issues/5264
//!
//! Investigation (confirmed with the maintainer): there is no engine defect. The
//! reported symptom is a graveyard that holds no MV≤3 *permanent* card, so the
//! optional targeted trigger correctly finds no legal target. What was genuinely
//! UNPINNED is that nothing drove Sun Titan's real, verbatim printed Oracle text
//! through [`parse_oracle_text`] — the exact entry point `database::synthesis`
//! uses to build `card-data.json`. The nearest existing "Sun Titan" fixture
//! (`issue_3309_rise_etb_returns`) feeds an ETB-only fabrication that drops the
//! "or attacks" half, so the enters-OR-attacks shape was never guarded through
//! the real synthesis pipeline. This test closes that gap: it parses the verbatim
//! text and pins the single `EntersOrAttacks` trigger (both halves) plus its
//! optional graveyard-return target filter. A parser regression that split,
//! dropped, or retyped either half would flip this test red.

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    Comparator, ControllerRef, Effect, FilterProp, QuantityExpr, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

/// Verbatim Sun Titan Oracle text (Scryfall, verified 2026-07-14). The "or
/// attacks" half is load-bearing — dropping it silently regresses the card to an
/// ETB-only reanimator, which is exactly the misread the fabricated `issue_3309`
/// fixture bakes in.
const SUN_TITAN_ORACLE: &str = "Vigilance\nWhenever this creature enters or attacks, you may return target permanent card with mana value 3 or less from your graveyard to the battlefield.";

#[test]
fn sun_titan_verbatim_parses_enters_or_attacks_graveyard_return() {
    let parsed = parse_oracle_text(
        SUN_TITAN_ORACLE,
        "Sun Titan",
        &[],
        &["Creature".to_string()],
        &["Giant".to_string()],
    );

    // Exactly one trigger, in the combined enters-OR-attacks mode. This single
    // mode IS both halves (CR 603.2: one triggered ability with two trigger
    // events); a regression that split it into two triggers or dropped a half
    // would change the count or the mode.
    assert_eq!(
        parsed.triggers.len(),
        1,
        "Sun Titan must parse to exactly one trigger, got {:?}",
        parsed.triggers
    );
    let trigger = &parsed.triggers[0];
    assert_eq!(
        trigger.mode,
        TriggerMode::EntersOrAttacks,
        "the \"enters or attacks\" clause must produce a single EntersOrAttacks trigger"
    );

    // The optional graveyard→battlefield return, targeting a controller-You
    // permanent card of mana value 3 or less that lives in the graveyard.
    let execute = trigger
        .execute
        .as_ref()
        .expect("the trigger must carry an execute ability");
    assert!(
        execute.optional,
        "\"you may return\" must parse as optional"
    );
    let target = match execute.effect.as_ref() {
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            target,
            ..
        } => target,
        other => panic!("expected a graveyard→battlefield ChangeZone, got {other:?}"),
    };
    match target {
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            properties,
        }) => {
            assert!(
                type_filters.contains(&TypeFilter::Permanent),
                "target must be a permanent card, got {type_filters:?}"
            );
            assert_eq!(
                *controller,
                Some(ControllerRef::You),
                "\"your graveyard\" must resolve to controller: You"
            );
            assert!(
                properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Cmc {
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 3 },
                    }
                )),
                "must require mana value 3 or less, got {properties:?}"
            );
            assert!(
                properties.iter().any(|p| matches!(
                    p,
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }
                )),
                "must be restricted to the graveyard zone, got {properties:?}"
            );
        }
        other => panic!("expected a Typed target filter, got {other:?}"),
    }
}
