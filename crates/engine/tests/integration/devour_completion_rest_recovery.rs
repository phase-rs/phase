//! Regression for the production capture `b152fcbf-0976-408a-a501-346237e1f8cb`:
//! a Bloodspore Thrinax Devour entry completed with an empty post-replacement
//! parent below its Devour-only ChangeZone snapshot. The stale resolution
//! carrier then let a later priority pass enter `start_next_turn` and panic.

use engine::game::engine::{apply, EngineError};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastingVariant, GameState, PendingResolutionCompletion, PendingSpellResolution,
    PostReplacementDrainStack, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

fn bare_spell_resolution_rest_state() -> GameState {
    let mut state = GameState::new(FormatConfig::free_for_all(), 4, 0xB152_FCBF);
    state.phase = Phase::Cleanup;
    state.active_player = P3;
    state.priority_player = P2;
    state.waiting_for = WaitingFor::Priority { player: P2 };
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
    state.push_spell_resolution(PendingSpellResolution {
        object_id: ObjectId(347),
        controller: P3,
        casting_variant: CastingVariant::Normal,
        cast_from_zone: None,
        cast_controller: Some(P3),
        cast_timing_permission: None,
        spell_targets: vec![],
        actual_mana_spent: 4,
        kickers_paid: vec![],
        additional_cost_payment_count: 0,
        additional_cost_payments: vec![],
        convoked_creatures: vec![],
    });
    state
}

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

/// An actor unauthorized in the captured window cannot consume the recovery
/// no-op, even though the impossible persisted state is repaired.
#[test]
fn unauthorized_pass_repairs_but_cannot_spend_devour_recovery() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 4, 0xB152_FCBF);
    state.phase = Phase::Cleanup;
    state.active_player = P3;
    state.priority_player = P2;
    state.waiting_for = WaitingFor::Priority { player: P2 };
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

    assert!(matches!(
        apply(&mut state, P0, GameAction::PassPriority),
        Err(EngineError::WrongPlayer)
    ));
    assert!(state.resolution_stack.is_empty());
    assert!(state.resolving_stack_entry.is_none());
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P3 });
}

/// The Discord turn-26 capture has no Devour frame: only a completed
/// permanent-spell epilogue remains above the resolving carrier.
#[test]
fn bare_spell_resolution_rest_recovers_before_a_captured_pass() {
    let mut state = bare_spell_resolution_rest_state();

    let result = apply(&mut state, P2, GameAction::PassPriority)
        .expect("the captured priority holder may submit the recovery no-op");

    assert!(state.resolution_stack.is_empty());
    assert!(state.resolving_stack_entry.is_none());
    assert_eq!(state.phase, Phase::Cleanup);
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P3 });
    assert_eq!(result.waiting_for, state.waiting_for);
}

/// A terminal-looking spell frame with another completion hold is live work,
/// not persisted residue; recovery must leave it to the ordinary resumer.
#[test]
fn bare_spell_resolution_recovery_preserves_live_completion_work() {
    let mut state = bare_spell_resolution_rest_state();
    state.pending_resolution_completion = Some(PendingResolutionCompletion {
        player: P3,
        source_id: ObjectId(347),
        final_cast: None,
    });
    let before = state.resolution_stack.clone();

    apply(&mut state, P0, GameAction::SetPhaseStops { stops: vec![] })
        .expect("actor-scoped preferences are valid at every prompt");

    assert_eq!(state.resolution_stack, before);
    assert!(state.resolving_stack_entry.is_some());
}
