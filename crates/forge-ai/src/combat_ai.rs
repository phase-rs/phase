use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

/// Choose which creatures to attack with (implemented in Task 2).
pub fn choose_attackers(_state: &GameState, _player: PlayerId) -> Vec<ObjectId> {
    Vec::new()
}

/// Choose blocker assignments (implemented in Task 2).
pub fn choose_blockers(
    _state: &GameState,
    _player: PlayerId,
    _attacker_ids: &[ObjectId],
) -> Vec<(ObjectId, ObjectId)> {
    Vec::new()
}
