//! Discriminating regression test for **issue #762**: Scarblade's Malice —
//!
//! > Target creature you control gains deathtouch and lifelink until end of
//! > turn. When that creature dies this turn, create a 2/2 black and green Elf
//! > creature token.
//!
//! Confirmed parse (`data/card-data.json` → "scarblade's malice"): the spell's
//! sub-ability is `CreateDelayedTrigger { condition: WhenDies { filter:
//! ParentTarget }, effect: Token(2/2 Elf), uses_tracked_set: true }`.
//!
//! The bug: the engine never bound the `ParentTarget` filter in the `WhenDies`
//! condition to the chosen victim. Because Scarblade's Malice registers no
//! tracked set, `latest_tracked_set_id` is `None`, the tracked-set condition
//! rewrite is skipped, and — before the fix — `bind_contextual_filter_to_condition`
//! did not cover `WhenDies`. So the stored condition kept `ParentTarget`,
//! `matches_target_filter(state, dying_id, ParentTarget)` returned false for the
//! actual death, and the delayed trigger never fired → 0 tokens.
//!
//! The fix (engine-only, `effects/delayed_trigger.rs`): (1) run the tracked-set
//! condition rewrite BEFORE the single-target contextual bind, and (2) extend
//! the contextual bind to cover the whole zone-change condition family
//! (`WhenDies` / `WhenLeavesPlayFiltered` / `WhenEntersBattlefield` /
//! `WhenDiesOrExiled`). For Scarblade's Malice the contextual bind now rewrites
//! `ParentTarget` → `SpecificObject { victim }`, so the death matches and the
//! token is created.
//!
//! CR references (verified against `docs/MagicCompRules.txt`):
//!   - CR 603.7c: a delayed triggered ability that refers to a particular object
//!     still affects it (binds the chosen object into the condition).
//!   - CR 608.2k: an effect that refers to a specific untargeted object
//!     previously referred to by its trigger condition affects that object.
//!   - CR 700.4: "dies" = put into a graveyard from the battlefield.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::move_to_zone;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SCARBLADE_MALICE_ORACLE: &str = "Target creature you control gains deathtouch and lifelink until end of turn. When that creature dies this turn, create a 2/2 black and green Elf creature token.";

/// Count 2/2 Elf tokens on the battlefield (the Scarblade's Malice output).
fn elf_token_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|obj| {
                obj.is_token
                    && obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case("Elf"))
            })
        })
        .count()
}

/// Drive any auto-ordering / priority passes until the stack drains, so the
/// fired delayed trigger's Token effect resolves onto the battlefield.
fn drain_stack(runner: &mut GameRunner) {
    for _ in 0..200 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

/// Set up P0 with Scarblade's Malice in hand, a legal target creature, and cast
/// the spell targeting that creature. Returns the built runner plus the victim's
/// id. The spell costs `{0}`, so there is no mana window.
fn cast_scarblade_at(target_name: &str) -> (GameRunner, engine::types::identifiers::ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scarblade's Malice", true, SCARBLADE_MALICE_ORACLE)
        .id();
    let victim = scenario.add_creature(P0, target_name, 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let outcome = runner.cast(spell).target_objects(&[victim]).resolve();
    // The spell resolves (grants deathtouch/lifelink and installs the delayed
    // trigger). Drop the outcome; we assert on the runner afterwards.
    let _ = outcome;

    (runner, victim)
}

/// Kill `dying` by moving it to the graveyard (CR 700.4), then check delayed
/// triggers against that zone-change event and drain the stack so any fired
/// token effect resolves.
fn kill_and_fire(runner: &mut GameRunner, dying: engine::types::identifiers::ObjectId) {
    let mut events: Vec<GameEvent> = Vec::new();
    move_to_zone(runner.state_mut(), dying, Zone::Graveyard, &mut events);
    engine::game::triggers::check_delayed_triggers(runner.state_mut(), &events);
    drain_stack(runner);
}

/// FIX (revert→fail): the targeted creature dies this turn, so the delayed
/// trigger fires and creates exactly one 2/2 Elf token. Reverting Change 2 (the
/// merged `WhenDies` arm in `bind_contextual_filter_to_condition`) leaves the
/// condition filter as unbound `ParentTarget`, so the death does not match and
/// this assertion sees 0 tokens.
#[test]
fn scarblade_malice_creates_token_when_targeted_creature_dies() {
    let (mut runner, victim) = cast_scarblade_at("Elvish Warrior");

    assert_eq!(
        elf_token_count(&runner),
        0,
        "precondition: no Elf tokens before the targeted creature dies"
    );

    kill_and_fire(&mut runner, victim);

    // CR 603.7c + CR 608.2k + CR 700.4: the targeted victim dying fires the
    // bound WhenDies delayed trigger, creating exactly one 2/2 Elf token.
    assert_eq!(
        elf_token_count(&runner),
        1,
        "the targeted creature dying must create exactly one 2/2 Elf token \
         (0 = the WhenDies ParentTarget filter was never bound to the victim)"
    );
}

/// NEGATIVE (proves `SpecificObject`, not `Any`): a DIFFERENT creature — never
/// targeted by the spell — dies. The delayed trigger's condition is bound to the
/// SPECIFIC victim, so an unrelated death must NOT fire it → 0 tokens. If the
/// bind produced `Any` (or stayed unbound and matched loosely) this would
/// wrongly create a token.
#[test]
fn scarblade_malice_no_token_when_other_creature_dies() {
    let (mut runner, _victim) = cast_scarblade_at("Chosen Elf");

    // A second creature P0 controls that was NOT the spell's target.
    let bystander = {
        let state = runner.state_mut();
        let card_id = engine::types::identifiers::CardId(state.next_object_id);
        engine::game::zones::create_object(
            state,
            card_id,
            P0,
            "Bystander".to_string(),
            Zone::Battlefield,
        )
    };
    runner
        .state_mut()
        .objects
        .get_mut(&bystander)
        .unwrap()
        .card_types
        .core_types
        .push(engine::types::card_type::CoreType::Creature);

    kill_and_fire(&mut runner, bystander);

    // CR 603.7c: the delayed trigger is bound to the specific targeted victim,
    // so a different creature's death must not fire it.
    assert_eq!(
        elf_token_count(&runner),
        0,
        "a non-targeted creature dying must NOT create a token (the WhenDies \
         filter must be SpecificObject{{victim}}, not Any)"
    );
}
