//! Regression for the production capture `b152fcbf-0976-408a-a501-346237e1f8cb`:
//! a Bloodspore Thrinax Devour entry completed with an empty post-replacement
//! parent below its Devour-only ChangeZone snapshot. The stale resolution
//! carrier then let a later priority pass enter `start_next_turn` and panic.

use engine::game::engine::apply;
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastingVariant, GameState, PostReplacementDrainStack, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// CR 117.3b + CR 117.4 + CR 608.2c + CR 614.12a + CR 614.13a: an old pass
/// submitted from the captured priority window repairs the completed Devour
/// entry, grants priority to the active player, and does not advance the turn.
#[test]
fn captured_devour_rest_shape_recovers_before_a_stale_pass_can_start_the_next_turn() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 4, 0xB152_FCBF);
    state.phase = Phase::Cleanup;
    state.active_player = P3;
    state.priority_player = P2;
    state.waiting_for = WaitingFor::Priority { player: P2 };
    state.priority_pass_count = 3;
    state.priority_passes.extend([P0, P1, P3]);
    state.resolving_stack_entry = Some(StackEntry {
        id: ObjectId(347),
        source_id: ObjectId(347),
        controller: P3,
        kind: StackEntryKind::Spell {
            card_id: CardId(347),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 4,
        },
    });
    state
        .resolution_stack
        .push_post_replacement(PostReplacementDrainStack::default());
    state.push_devour_change_zone_snapshot([ObjectId(27), ObjectId(31)].into_iter().collect());

    let result = apply(&mut state, P2, GameAction::PassPriority)
        .expect("the stale captured pass repairs the ownerless rest state");

    assert!(
        state.resolution_stack.is_empty(),
        "the empty replacement parent and its Devour-only snapshot must both retire"
    );
    assert!(
        state.resolving_stack_entry.is_none(),
        "the completed spell carrier must settle before another turn can begin"
    );
    assert_eq!(
        state.phase,
        Phase::Cleanup,
        "the stale pass must not advance phase"
    );
    assert_eq!(state.priority_player, P3);
    assert_eq!(
        state.waiting_for,
        WaitingFor::Priority { player: P3 },
        "CR 117.3b grants the active player the recovered priority window"
    );
    assert!(state.priority_passes.is_empty());
    assert_eq!(state.priority_pass_count, 0);
    assert_eq!(result.waiting_for, state.waiting_for);
}
