//! Runtime regression for GitHub issue #735 — Amalia Benavides Aguirre's
//! "destroy all other creatures if its power is exactly 20" must be a
//! CONDITIONAL board wipe gated on Amalia's own power, not an unconditional
//! wipe on every lifegain.
//!
//! https://github.com/phase-rs/phase/issues/735
//!
//! Amalia: "Whenever you gain life, ~ explores. Then destroy all other
//! creatures if its power is exactly 20."
//!
//! Two production edits make the sub-ability condition load-bearing at runtime:
//!   - Edit A (`oracle_util.rs`): "exactly N" → `Comparator::EQ`, so the
//!     "if its power is exactly 20" clause parses at all. Without it the
//!     DestroyAll clause fails to parse and the wipe is unconditional.
//!   - Edit B (`oracle_effect/conditions.rs`): the player-subject lifegain
//!     trigger binds "its power" to Amalia (the ability SOURCE), so at
//!     resolution the check reads Amalia's power. With the scope forced to
//!     `CostPaidObject` there is no cost-paid object for a lifegain trigger, so
//!     the check resolves 0 ≠ 20 and the wipe NEVER fires — the
//!     `amalia_power_20_wipes_others` test below is the load-bearing proof.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 608.2h: an effect that requires information from a specific object
//!     (here Amalia's power) determines it once, when the effect is applied.
//!   - CR 208.1: a creature's power can be modified by effects — Amalia's power
//!     is raised to exactly 20 here by stacking +1/+1 counters.
//!   - CR 701.44a: to explore, reveal the top library card; a land goes to hand
//!     (no +1/+1 counter, no choice) — a land-on-top makes explore resolve with
//!     no pause, so Amalia's staged power is unchanged by the explore.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Derived power of an object (counters/layers applied), read off live state.
fn power_of(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id].power.unwrap_or(0)
}

/// Current zone of an object.
fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

const AMALIA_ORACLE: &str = "Ward—Pay 3 life.\nWhenever you gain life, Amalia Benavides Aguirre explores. Then destroy all other creatures if its power is exactly 20. (To have this creature explore, reveal the top card of your library. Put that card into your hand if it's a land. Otherwise, put a +1/+1 counter on this creature, then put the card back or put it into your graveyard.)";

/// Stamp the top card of P0's library as a Land so Amalia's explore reveals a
/// land — CR 701.44a routes it to hand with no +1/+1 counter and no choice, so
/// the trigger resolves without pausing and Amalia's power is untouched.
fn stamp_library_top_land(runner: &mut GameRunner) {
    let top = runner.state().players[P0.0 as usize].library[0];
    let obj = runner
        .state_mut()
        .objects
        .get_mut(&top)
        .expect("library top");
    obj.card_types.core_types.push(CoreType::Land);
    obj.base_card_types = obj.card_types.clone();
}

/// Drive priority passes until the stack is empty (the lifegain trigger — and
/// its explore + conditional DestroyAll — has fully resolved).
fn drain_to_priority(runner: &mut GameRunner) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 256,
            "drain exceeded bound; waiting_for = {:?}",
            runner.state().waiting_for
        );
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

/// Fire a "you gain life" event for P0 and let the resulting Amalia trigger
/// resolve. Uses `LifeChanged { amount: +N }`, which the `LifeGained` trigger
/// matcher keys off (CR 119.3 lifegain event).
fn gain_life_and_resolve(runner: &mut GameRunner) {
    let events = vec![GameEvent::LifeChanged {
        player_id: P0,
        amount: 1,
    }];
    process_triggers(runner.state_mut(), &events);
    drain_to_priority(runner);
}

/// Build Amalia (base 2/2) plus a supporting cast and a land on top of the
/// library. `amalia_counters` +1/+1 counters raise Amalia's power to
/// `2 + amalia_counters`. Returns (runner, amalia_id, other_creature_id).
fn setup(amalia_counters: u32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Library Bottom"]);

    let mut builder =
        scenario.add_creature_from_oracle(P0, "Amalia Benavides Aguirre", 2, 2, AMALIA_ORACLE);
    if amalia_counters > 0 {
        builder.with_plus_counters(amalia_counters);
    }
    let amalia = builder.id();

    // A vanilla bystander that the wipe would destroy (Amalia survives via the
    // "all OTHER creatures" Another filter).
    let bystander = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    stamp_library_top_land(&mut runner);
    // CR 613.4c: settle layer 7c so Amalia's +1/+1 counters are reflected in her
    // current power before the trigger resolves (the live resolution pipeline
    // re-evaluates layers, but this makes the pre-resolution precondition read
    // the derived value rather than the printed base).
    engine::game::layers::evaluate_layers(runner.state_mut());
    (runner, amalia, bystander)
}

/// Amalia with power < 20: a lifegain must NOT wipe the board (0 ≠ 20).
/// Amalia is a base 2/2 with no counters (power 2), the bystander survives.
#[test]
fn amalia_power_below_20_other_creatures_survive() {
    let (mut runner, amalia, bystander) = setup(0);

    assert_eq!(
        power_of(&runner, amalia),
        2,
        "precondition: Amalia's power is 2 (< 20)"
    );

    gain_life_and_resolve(&mut runner);

    assert_eq!(
        zone_of(&runner, bystander),
        Zone::Battlefield,
        "with Amalia at power 2, the lifegain must NOT destroy other creatures"
    );
    assert_eq!(
        zone_of(&runner, amalia),
        Zone::Battlefield,
        "Amalia stays on the battlefield"
    );
}

/// THE load-bearing Source test (issue #735 core fix): Amalia at EXACTLY power
/// 20 → the lifegain fires the conditional DestroyAll → other creatures go to
/// the graveyard while Amalia survives (the "all OTHER creatures" Another
/// filter).
///
/// CR 608.2h: the power check is evaluated once when the DestroyAll is applied,
/// against Amalia (the SOURCE). CR 208.1: Amalia's power is set to exactly 20
/// by 18 +1/+1 counters on a base 2/2.
///
/// Revert-fail (Edit B): forcing `strip_property_conditional`'s scope to
/// `ObjectScope::CostPaidObject` makes the runtime check read a nonexistent
/// cost-paid object (a lifegain trigger has none) → resolves 0 ≠ 20 → the wipe
/// never fires → the bystander stays on the battlefield → this assert fails.
#[test]
fn amalia_power_20_wipes_others() {
    // 18 counters on a base 2/2 → power exactly 20.
    let (mut runner, amalia, bystander) = setup(18);

    assert_eq!(
        power_of(&runner, amalia),
        20,
        "precondition: Amalia's power is exactly 20"
    );

    gain_life_and_resolve(&mut runner);

    assert_eq!(
        zone_of(&runner, bystander),
        Zone::Graveyard,
        "with Amalia at power exactly 20, the lifegain must destroy all OTHER creatures"
    );
    assert_eq!(
        zone_of(&runner, amalia),
        Zone::Battlefield,
        "Amalia is NOT an 'other' creature and must survive its own board wipe"
    );
}
