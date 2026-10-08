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

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetFilter;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::{cast_spell_action, drive_modal_with_response, PriorityResponse};

const DROMOKAS_COMMAND: &str = "Choose two —\n\
    • Prevent all damage target instant or sorcery spell would deal this turn.\n\
    • Target player sacrifices an enchantment.\n\
    • Put a +1/+1 counter on target creature.\n\
    • Target creature you control fights target creature you don't control.";

/// A simple damage instant: deals 3 damage to a target player. Cast by P0 at
/// itself; it stays on the stack while Dromoka (cast in response) resolves on
/// top, so Dromoka's mode 1 can target it as the prevention source.
const DAMAGE_INSTANT: &str = "This spell deals 3 damage to target player.";

/// Cancel's printed text. Named "Cancel" and not "Counter": a card's own name
/// is normalized to `~` in its Oracle text, so a counterspell named "Counter"
/// would parse its own verb away and resolve as an inert `Unimplemented`.
const COUNTER_TARGET_SPELL: &str = "Counter target spell.";

/// CR 608.2b: a `ParentTargetSlot` prevention SOURCE that was an illegal target
/// as the chain began to resolve is dropped, while the rest of the spell still
/// applies to the slots that stayed legal.
///
/// Dromoka's Command is the only printed card that can show this: the other
/// four cards whose `PreventDamage` reads a `ParentTargetSlot` source (Awe
/// Strike, Dazzling Reflection, Hallow, Shieldmage Elder) each declare exactly
/// ONE target, so making it illegal makes every target illegal and
/// `stack.rs`'s `check_fizzle` counters the spell before
/// `record_illegal_target_slots` ever runs. Two declared slots are what put the
/// spell on the stamping path at all.
///
/// This row drives the REAL pipeline — the stamp is COMPUTED by
/// `record_illegal_target_slots` from the re-validated chain, not written by
/// the test — which is what the in-crate helper test beside
/// `resolve_source_filter` cannot do.
///
/// HONEST LIMITATION, and why this asserts the shield's SHAPE rather than an
/// unprevented damage event: slot 0 is a spell, and the only way a spell stops
/// being a legal target is to leave the stack, after which it deals no damage
/// at all. "Its damage is not prevented" is therefore unobservable for this
/// consumer on any printed card. What IS observable, and what discriminates, is
/// that the dropped slot resolves to `TargetFilter::None` instead of binding
/// the stale `SpecificObject`.
#[test]
fn dromokas_command_drops_a_prevention_source_that_became_illegal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Mode 3's recipient. It stays legal throughout, so Dromoka does NOT
    // fizzle, and the counter it receives proves the spell resolved.
    let my_creature = scenario.add_creature(P0, "Shalai and Hallar", 3, 4).id();

    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Searing Spell", true, DAMAGE_INSTANT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let command = scenario
        .add_spell_to_hand_from_oracle(P0, "Dromoka's Command", true, DROMOKAS_COMMAND)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    // P1's answer, cast in response to Dromoka: countering the bolt is what
    // makes Dromoka's slot 0 illegal before Dromoka resolves.
    let cancel = scenario
        .add_spell_to_hand_from_oracle(P1, "Cancel", true, COUNTER_TARGET_SPELL)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();

    let bolt_card = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id: bolt_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the damage instant must succeed");
    drive_target_then_stop(&mut runner, &[], &[P0]);

    // Dromoka declares both slots (the bolt, then the creature); P1 then
    // counters the bolt above it, so the chain re-validates with slot 0 gone.
    let cast = cast_spell_action(&runner, command);
    let events = drive_modal_with_response(
        &mut runner,
        cast,
        &[0, 2],
        &[bolt, my_creature],
        Some(PriorityResponse {
            player: P1,
            instant: cancel,
            target: bolt,
        }),
    );

    // Reach guards. Without these the row passes on a countered Dromoka, or on
    // a bolt that was never answered — in either case testing nothing.
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Graveyard,
        "reach guard: the response must have countered the bolt, or slot 0 \
         stays legal and nothing is stamped"
    );
    assert_eq!(
        runner.state().objects[&my_creature]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1),
        "reach guard: mode 3 must still resolve — CR 608.2b keeps the parts of \
         the effect whose targets stayed legal. Counters: {:?}",
        runner.state().objects[&my_creature].counters
    );

    // The stamped slot is DROPPED, not bound to the stale referent: no
    // installed shield names the countered bolt.
    let binds_the_bolt = runner
        .state()
        .pending_damage_replacements
        .iter()
        .chain(
            runner.state().objects[&my_creature]
                .replacement_definitions
                .as_slice()
                .iter(),
        )
        .filter_map(|r| r.damage_source_filter.as_ref())
        .any(|f| filter_names(f, bolt));
    assert!(
        !binds_the_bolt,
        "a prevention source that was an illegal target at resolution must be \
         dropped, not bound as SpecificObject; pending: {:?}, events: {events:?}",
        runner.state().pending_damage_replacements
    );
}

/// Whether `filter` binds `object` anywhere in its tree, so the assertion reads
/// the whole `And`/`Or` shape rather than only its outermost node.
fn filter_names(filter: &TargetFilter, object: engine::types::identifiers::ObjectId) -> bool {
    match filter {
        TargetFilter::SpecificObject { id } => *id == object,
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(|f| filter_names(f, object))
        }
        _ => false,
    }
}

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

            payment_mode: CastPaymentMode::Auto,
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
