//! Tests relocated from `src/parser/oracle_vote.rs`'s test module.

use engine::parser::oracle_effect::parse_effect_chain_with_context;
use engine::parser::oracle_ir::context::ParseContext;
use engine::types::ability::AbilityKind;

/// Issue #3667 — the opponent half must surface a real player target slot.
#[test]
fn you_and_target_opponent_each_draw_surfaces_opponent_target_slot() {
    use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
    use engine::types::ability::TargetRef;
    use engine::types::game_state::GameState;
    use engine::types::identifiers::ObjectId;
    use engine::types::player::PlayerId;

    let parsed = parse_effect_chain_with_context(
        "you and target opponent each draw two cards",
        AbilityKind::Spell,
        &mut ParseContext::default(),
    );
    let state = GameState::new_two_player(42);
    let resolved = build_resolved_from_def(&parsed, ObjectId(1), PlayerId(0));
    let slots = build_target_slots(&state, &resolved).expect("target opponent draw needs a slot");
    assert_eq!(
        slots.len(),
        1,
        "opponent half must declare one player target"
    );
    assert!(
        slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(1))),
        "target slot must offer the opponent"
    );
    assert!(
        !slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(0))),
        "target opponent draw must not allow targeting yourself"
    );
}
