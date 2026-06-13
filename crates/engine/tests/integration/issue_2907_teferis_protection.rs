//! Regression for issue #2907: Teferi's Protection mass phase-out targeting.
//!
//! https://github.com/phase-rs/phase/issues/2907
//!
//! "All permanents you control phase out" must not prompt for a single target at
//! cast time; the effect expands its mass filter at resolution.

use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ControllerRef, Effect, TargetFilter, TypeFilter, TypedFilter};
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const TEFERIS_PROTECTION_ORACLE: &str = "\
Until your next turn, your life total can't change and you gain protection from everything. All permanents you control phase out.\n\
Exile Teferi's Protection.";

fn phase_out_def_from_parsed() -> engine::types::ability::AbilityDefinition {
    let parsed = parse_oracle_text(
        TEFERIS_PROTECTION_ORACLE,
        "Teferi's Protection",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let mut node = &parsed.abilities[0];
    loop {
        if matches!(node.effect.as_ref(), Effect::PhaseOut { .. }) {
            return node.clone();
        }
        node = node
            .sub_ability
            .as_ref()
            .expect("Teferi's Protection must chain to PhaseOut");
    }
}

#[test]
fn teferis_protection_mass_phase_out_builds_no_target_slots() {
    let mut state = GameState::new_two_player(42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Teferi's Protection".to_string(),
        Zone::Stack,
    );
    let phase_out = phase_out_def_from_parsed();
    let resolved = build_resolved_from_def(&phase_out, source, PlayerId(0));
    let slots = build_target_slots(&state, &resolved).expect("slot build");
    assert!(
        slots.is_empty(),
        "mass phase-out must not require target selection, got {slots:?}"
    );
}

#[test]
fn teferis_protection_parsed_phase_out_uses_mass_permanent_filter() {
    let phase_out = phase_out_def_from_parsed();
    let Effect::PhaseOut { target } = phase_out.effect.as_ref() else {
        panic!("expected PhaseOut, got {:?}", phase_out.effect);
    };
    assert_eq!(
        *target,
        TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You))
    );
    assert!(matches!(
        target,
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: Some(ControllerRef::You),
            ..
        }) if type_filters.contains(&TypeFilter::Permanent)
    ));
}
