//! Discriminating integration test for **Kutzil's Flanker** mode 1.
//!
//! Kutzil's Flanker's first chosen mode is "Put a +1/+1 counter on this
//! creature for each creature that left the battlefield under your control
//! this turn." The count is a `QuantityRef::ZoneChangeCountThisTurn` with a
//! destination-agnostic ("left the battlefield" => to: None) filter scoped to
//! creatures you control. The parser previously had no arm for the
//! "left the battlefield under your control" subject (only the graveyard-only
//! "died" form), so this mode was swallowed (DynamicQty warning) and produced
//! no counters at runtime.
//!
//! This test drives the REAL Oracle text through cast -> ETB trigger ->
//! choose-one mode 1 -> resolve and asserts the concrete outcome: with two
//! creatures having left the battlefield this turn — one under the caster's
//! control, one under the opponent's — the Flanker enters with exactly ONE
//! +1/+1 counter (only the caster-controlled departure counts).
//!
//! Fail-first: with the parse arm regressed, mode 1 is Unimplemented and no
//! counter is placed (assertion `== 1` fails). The "under your control"
//! scoping is discriminated by the negative control (the opponent's departed
//! creature must NOT contribute), and the destination-agnostic matching is
//! discriminated by routing the caster's creature to exile (not the
//! graveyard), which a graveyard-only "died" count would miss.
//!
//! CR 400.7 + CR 603.10a: leaves-the-battlefield is a look-back zone-change
//! event counted over `zone_changes_this_turn` using last-known info.
//! CR 122.1: +1/+1 counters placed by the resolving ability.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FLANKER_TEXT: &str = "Flash\n\
    When this creature enters, choose one —\n\
    • Put a +1/+1 counter on this creature for each creature that left the battlefield under your control this turn.\n\
    • You gain 2 life and scry 2.\n\
    • Exile target player's graveyard.";

#[test]
fn kutzils_flanker_mode_one_counts_only_your_departed_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A creature under the CASTER's control that will leave the battlefield
    // (routed to exile to prove the count is destination-agnostic, not
    // graveyard-only).
    let my_leaver = scenario.add_creature(P0, "My Bear", 2, 2).id();
    // A creature under the OPPONENT's control that will also leave — the
    // negative control: it must NOT contribute to "under your control".
    let their_leaver = scenario.add_creature(P1, "Their Bear", 2, 2).id();

    // Kutzil's Flanker in the caster's hand, parsed from real Oracle text,
    // cost zeroed so the cast resolves without mana plumbing.
    let flanker = scenario
        .add_creature_to_hand_from_oracle(P0, "Kutzil's Flanker", 2, 2, FLANKER_TEXT)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();

    // Drive two REAL leaves-the-battlefield events through the production
    // zones pipeline so `zone_changes_this_turn` is populated exactly as it
    // would be in a game (CR 400.7 recording path).
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), my_leaver, Zone::Exile, &mut events);
    move_to_zone(
        runner.state_mut(),
        their_leaver,
        Zone::Graveyard,
        &mut events,
    );

    // Sanity: both left the battlefield.
    assert_eq!(runner.state().objects[&my_leaver].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&their_leaver].zone, Zone::Graveyard);

    // Cast the Flanker; it enters and its ETB choose-one trigger goes on the
    // stack.
    let flanker_card_id = runner.state().objects[&flanker].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: flanker,
            card_id: flanker_card_id,
            targets: vec![],
        })
        .expect("casting Kutzil's Flanker must succeed");

    drive_and_choose_mode_one(&mut runner);

    // DISCRIMINATOR: exactly one creature left the battlefield under the
    // caster's control this turn, so the Flanker has exactly one +1/+1
    // counter. The opponent's departed creature must not contribute, and the
    // exile (non-graveyard) destination of the caster's creature must still
    // be counted.
    assert_eq!(
        runner.state().objects[&flanker]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1),
        "mode 1 must place one +1/+1 counter (one creature left under your \
         control this turn); counters: {:?}",
        runner.state().objects[&flanker].counters
    );
}

/// Drive the cast to resolution, choosing mode 1 (`SelectModes { indices: [0] }`)
/// when the Flanker's ETB choose-one prompt appears. Bounded loop guards
/// against a stall.
fn drive_and_choose_mode_one(runner: &mut engine::game::scenario::GameRunner) {
    let mut chose_mode = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && chose_mode {
                    return;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            WaitingFor::AbilityModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes { indices: vec![0] })
                    .expect("choosing mode 1 must succeed");
                chose_mode = true;
            }
            other => panic!("unexpected waiting state during Flanker resolution: {other:?}"),
        }
    }
    panic!("resolution did not complete after 40 iterations — likely a stall");
}
