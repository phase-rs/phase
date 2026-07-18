//! Nettling Imp / Norritt / Arcum's Whistle continuity target clause — PR #5463
//! fix round, addressing the maintainer's "test doesn't cover the actual
//! regression" finding.
//!
//! The two pre-existing tests are each insufficient alone:
//!   * `parse_target_active_player_controlled_continuously_since_turn_began`
//!     (oracle_target.rs) only checks the isolated `parse_target` AST shape — it
//!     never reaches target selection.
//!   * `build_target_slots_active_player_controlled_continuously_since_turn_began`
//!     (ability_utils.rs) hand-constructs the `FilterProp`, bypassing the parser
//!     entirely — reverting the new parser arm leaves it green.
//!
//! This end-to-end test closes the gap. It parses Nettling Imp's REAL Oracle
//! text through the top-level `parse_oracle_text` entry point and drives the
//! resulting PARSER-PRODUCED `AbilityDefinition` through the production
//! `build_resolved_from_def` -> `build_target_slots` path (the same path used by
//! the live activation flow). If the decomposed continuity arm in
//! `parse_ownership_or_controller_suffix` is reverted or broken, the parsed
//! filter degrades to non-Wall-creature-only (no `ActivePlayer` controller, no
//! `ControlledContinuouslySinceTurnBegan` property), so `build_target_slots`
//! would then wrongly admit the summoning-sick and non-active-player fixtures —
//! failing the exact `legal_targets` assertion below.

use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ControllerRef, FilterProp, TargetFilter, TargetRef};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::zones::Zone;
use engine::types::PlayerId;

/// Verbatim Scryfall Oracle text for Nettling Imp.
const NETTLING_IMP_ORACLE: &str = "{T}: Choose target non-Wall creature the active player has controlled continuously since the beginning of the turn. That creature attacks this turn if able. Destroy it at the beginning of the next end step if it didn't attack this turn. Activate only during an opponent's turn, before attackers are declared.";

#[test]
fn nettling_imp_parsed_continuity_filter_restricts_target_slots_end_to_end() {
    // --- Parse the real card through the real top-level entry point. ---
    let parsed = parse_oracle_text(
        NETTLING_IMP_ORACLE,
        "Nettling Imp",
        &[],
        &["Creature".to_string()],
        &["Imp".to_string()],
    );
    assert_eq!(
        parsed.abilities.len(),
        1,
        "expected the single {{T}} activated ability"
    );
    let ability_def = &parsed.abilities[0];

    // Positive reach-guard: prove the parse produced the FULL continuity filter
    // (active-player controller + continuity property), not the degraded
    // non-Wall-creature-only fallback. This is the exact seam the Finding 1
    // decomposition parses; if that arm regresses, both facts vanish here and
    // the slot assertion below would no longer be a meaningful discriminator.
    let parsed_filter = ability_def
        .effect
        .target_filter()
        .expect("Nettling Imp surfaces a target-selection filter");
    let TargetFilter::Typed(typed) = parsed_filter else {
        panic!("expected a Typed target filter, got {parsed_filter:?}");
    };
    assert_eq!(
        typed.controller,
        Some(ControllerRef::ActivePlayer),
        "continuity clause must pin the ActivePlayer controller scope"
    );
    assert!(
        typed
            .properties
            .contains(&FilterProp::ControlledContinuouslySinceTurnBegan),
        "continuity clause must carry ControlledContinuouslySinceTurnBegan, got {:?}",
        typed.properties
    );

    // --- Three fixtures; only one is a legal target. ---
    let mut state = GameState::new_two_player(42);
    // CR 102.1: the active player is PlayerId(0) at game start.
    assert_eq!(state.active_player, PlayerId(0));

    // Legal: active player's control, no summoning sickness (create_object does
    // not set the flag — a "pre-existing" battlefield creature).
    let continuous_active = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Continuous Active".to_string(),
        Zone::Battlefield,
    );
    // Illegal via continuity: active player's control, but summoning-sick.
    let fresh_active = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Fresh Active".to_string(),
        Zone::Battlefield,
    );
    // Illegal via controller: continuous control, but by the non-active player.
    let continuous_nonactive = create_object(
        &mut state,
        CardId(3),
        PlayerId(1),
        "Continuous Nonactive".to_string(),
        Zone::Battlefield,
    );
    for creature in [continuous_active, fresh_active, continuous_nonactive] {
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
    }
    state.objects.get_mut(&fresh_active).unwrap().summoning_sick = true;

    // The Nettling Imp source itself, controlled by the active player.
    let imp = create_object(
        &mut state,
        CardId(4),
        PlayerId(0),
        "Nettling Imp".to_string(),
        Zone::Battlefield,
    );

    // --- Drive the PARSED ability through the production target-slot builder. ---
    let resolved = build_resolved_from_def(ability_def, imp, PlayerId(0));
    let slots = build_target_slots(&state, &resolved).expect("target slots should build");
    assert_eq!(slots.len(), 1, "single target selection slot");
    // Revert-failing assertion: with the continuity arm reverted the parsed
    // filter is non-Wall-creature-only, so this set would also contain
    // `fresh_active` and `continuous_nonactive`.
    assert_eq!(
        slots[0].legal_targets,
        vec![TargetRef::Object(continuous_active)],
        "only the continuously-active-player-controlled creature is a legal target"
    );
}
