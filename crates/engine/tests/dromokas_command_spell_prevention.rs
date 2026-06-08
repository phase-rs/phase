//! Pipeline regression for **Dromoka's Command** mode 1 + mode 3 — the
//! source-scoped prevention + independent modal `PutCounter` interaction that
//! produced an infinite loop (Shalai and Hallar's "+1/+1 counter → deal damage
//! to opponent" trigger looping when Dromoka's mode-1 prevention shield was
//! fused as a blanket prevent-all whose rider was mode 3's `PutCounter`).
//!
//! Defect A (parser): "Prevent all damage target instant or sorcery spell would
//! deal this turn" dropped the source scope, producing a blanket prevent-all
//! shield (`target: Any`, no `damage_source_filter`).
//!
//! Defect B (resolver/chaining): mode 3's `PutCounter` was fused as mode 1's
//! prevention-shield `runtime_execute`, so the shield intercepted every
//! `DamageDone` event and re-fired the +1/+1 counter — an infinite loop.
//!
//! This test drives the REAL cast pipeline: P0 (the active player) casts a
//! damage instant at itself, holds it on the stack, then casts Dromoka's
//! Command in response — choosing mode 1 (target the instant) and mode 3 (put a
//! +1/+1 counter on P0's creature). It asserts:
//!   * the shield's `damage_source_filter` is `And[SpecificObject, Typed]`
//!     (source-scoped, not blanket);
//!   * the +1/+1 counter lands EXACTLY ONCE on the chosen creature (no loop);
//!   * the prevented spell deals no damage to P0.
//!
//! The wrong-source discrimination (a non-chosen source's damage is NOT
//! prevented) is covered at the resolver level by
//! `source_scoped_shield_only_prevents_chosen_spell_not_other_sources` in
//! `prevent_damage.rs`, which can drive the in-crate damage primitive directly.
//!
//! CR 609.7 + CR 609.7a + CR 615.2 + CR 700.2d.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetFilter;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DROMOKAS_COMMAND: &str = "Choose two —\n\
    • Prevent all damage target instant or sorcery spell would deal this turn.\n\
    • Target player sacrifices an enchantment.\n\
    • Put a +1/+1 counter on target creature.\n\
    • Target creature you control fights target creature you don't control.";

/// A simple damage instant: deals 3 damage to a target player. Cast by P0 at
/// itself; it stays on the stack while Dromoka (cast in response) resolves on
/// top, so Dromoka's mode 1 can target it as the prevention source.
const DAMAGE_INSTANT: &str = "This spell deals 3 damage to target player.";

#[test]
fn dromokas_command_mode_one_source_scoped_prevent_puts_counter_once_no_loop() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0's creature that receives the +1/+1 counter (mode 3).
    let my_creature = scenario.add_creature(P0, "Shalai and Hallar", 3, 4).id();

    // P0's damage instant aimed at P0 — the prevention source for mode 1.
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Searing Spell", true, DAMAGE_INSTANT)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    // P0's Dromoka's Command.
    let command = scenario
        .add_spell_to_hand_from_oracle(P0, "Dromoka's Command", true, DROMOKAS_COMMAND)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();

    // P0 casts the damage instant at itself and holds it on the stack (P0 is the
    // active player and retains priority after casting — it then casts Dromoka
    // in response).
    let bolt_card = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id: bolt_card,
            targets: vec![],
        })
        .expect("casting the damage instant must succeed");
    // Answer the bolt's single player-target slot, then leave it on the stack.
    drive_target_then_stop(&mut runner, &[], &[P0]);
    assert!(
        runner.state().stack.iter().any(|e| e.id == bolt),
        "the damage instant must be on the stack as Dromoka's prevention source"
    );

    // P0 casts Dromoka's Command in response, choosing mode 1 (target the
    // instant) and mode 3 (put a +1/+1 counter on P0's creature). The
    // SpellCast driver walks the modal slots in written order: mode-1 source
    // slot first (the stack spell), then mode-3 creature slot.
    let outcome = runner
        .cast(command)
        .modes(&[0, 2])
        .target_objects(&[bolt, my_creature])
        .resolve();

    // The +1/+1 counter must land EXACTLY ONCE — no loop.
    assert_eq!(
        outcome.state().objects[&my_creature]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1),
        "mode 3 must place exactly one +1/+1 counter (no infinite loop); counters: {:?}",
        outcome.state().objects[&my_creature].counters
    );

    // The shield must be source-scoped: `And[SpecificObject, Typed]`, NOT a
    // blanket prevent-all.
    let source_scoped = outcome
        .state()
        .pending_damage_replacements
        .iter()
        .chain(
            outcome.state().objects[&my_creature]
                .replacement_definitions
                .as_slice()
                .iter(),
        )
        .filter_map(|r| r.damage_source_filter.as_ref())
        .any(|f| {
            matches!(
                f,
                TargetFilter::And { filters }
                    if filters.iter().any(|x| matches!(x, TargetFilter::SpecificObject { .. }))
            )
        });
    assert!(
        source_scoped,
        "the prevention shield must carry an And[SpecificObject, Typed] source filter; \
         pending: {:?}",
        outcome.state().pending_damage_replacements
    );

    // The prevented spell dealt no damage to P0.
    assert_eq!(
        outcome.life_delta(P0),
        0,
        "the chosen spell's damage to P0 must be fully prevented"
    );

    // Sanity: the prevented spell left the stack to the graveyard.
    assert_eq!(outcome.zone_of(bolt), Zone::Graveyard);
}

/// Drive a just-cast spell through its target-selection slots (answering with
/// the declared object/player intent), then STOP at the post-cast priority
/// window, leaving the spell on the stack.
fn drive_target_then_stop(
    runner: &mut engine::game::scenario::GameRunner,
    objects: &[engine::types::identifiers::ObjectId],
    players: &[engine::types::player::PlayerId],
) {
    let mut remaining: Vec<engine::types::identifiers::ObjectId> = objects.to_vec();
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let choice = remaining
                    .iter()
                    .position(|&o| {
                        slot.legal_targets
                            .contains(&engine::types::ability::TargetRef::Object(o))
                    })
                    .map(|pos| engine::types::ability::TargetRef::Object(remaining.remove(pos)))
                    .or_else(|| {
                        players
                            .iter()
                            .find(|&&p| {
                                slot.legal_targets
                                    .contains(&engine::types::ability::TargetRef::Player(p))
                            })
                            .map(|&p| engine::types::ability::TargetRef::Player(p))
                    });
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("ChooseTarget must be accepted");
            }
            WaitingFor::Priority { .. } => return,
            other => panic!("unexpected waiting state while committing spell: {other:?}"),
        }
    }
    panic!("spell did not commit to the stack after 32 iterations");
}
