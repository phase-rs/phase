//! Heart-Shaped Herb — the activated ability's trailing "and you become the
//! monarch" conjunct (CR 725.1) must actually grant the monarch at runtime.
//!
//! Oracle (verbatim, data/mtgjson/AtomicCards.json):
//!   If a source an opponent controls would deal damage to you, prevent 1 of
//!   that damage.
//!   {2}, {T}, Sacrifice this artifact: You may sacrifice a creature. If you
//!   do, return that card to the battlefield under its owner's control with
//!   three +1/+1 counters on it and you become the monarch.
//!
//! Before the `"you become "` bare-and splitter arm (parser/oracle_effect/
//! sequence.rs), the final conjunct was dropped SILENTLY: the return-destination
//! counter-suffix truncation in `strip_return_destination_ext_with_remainder`
//! (lower.rs) cut the remainder at the counter clause's START offset, discarding
//! the tail. The card reported as fully supported with zero coverage gaps while
//! never granting the monarch.
//!
//! Every test here drives the real `apply()` pipeline (GameScenario +
//! GameRunner::activate + CR 602 announce/pay/resolve) and asserts measured
//! state deltas, never AST shape.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const P1: PlayerId = PlayerId(1);

const ORACLE: &str = "If a source an opponent controls would deal damage to you, prevent 1 of that damage.\n{2}, {T}, Sacrifice this artifact: You may sacrifice a creature. If you do, return that card to the battlefield under its owner's control with three +1/+1 counters on it and you become the monarch.";

/// Build P0's turn at PreCombatMain with the Herb on the battlefield as an
/// artifact and EXACTLY ONE creature P0 controls.
///
/// The single-creature constraint is load-bearing, not incidental: `Effect::
/// Sacrifice` only raises `WaitingFor::EffectZoneChoice` when the eligible pool
/// EXCEEDS the count. With one eligible creature the CR 701.21a mandatory-all
/// fast path fires and no prompt opens — which matters because
/// `AbilityActivation` has no `.effect_zone(..)` setter (its `ResolutionPolicy`
/// hardcodes an empty `effect_zone_cards`), so a second eligible creature would
/// stall `drive_resolution` and surface as a confusing "monarch is None"
/// failure that mimics the bug under test. If a future variant needs a larger
/// pool, add `.effect_zone(&[ObjectId])` to `AbilityActivation` mirroring
/// `SpellCast::effect_zone` first.
fn setup() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // {2} generic — auto-tap is NOT modeled by the driver, so fund the pool.
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    let herb = {
        let mut b = scenario.add_creature(P0, "Heart-Shaped Herb", 0, 0);
        b.from_oracle_text(ORACLE).as_artifact();
        b.id()
    };
    let creature = scenario.add_vanilla(P0, 2, 2);
    let runner = scenario.build();
    (runner, herb, creature)
}

fn counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

/// CR 725.1 + CR 109.5: accepting the optional sacrifice must make the player
/// who ACTIVATED the ability the monarch.
///
/// Revert-failing assertion: `state().monarch == Some(P0)`. With the splitter
/// arm reverted the `BecomeMonarch` node does not exist in the chain at all, so
/// `monarch` stays `None`.
#[test]
fn accepting_the_sacrifice_makes_the_activating_player_the_monarch() {
    let (mut runner, herb, creature) = setup();
    assert_eq!(
        runner.state().monarch,
        None,
        "precondition: no monarch before activation (CR 725.1)"
    );

    let outcome = runner
        .activate(herb, 0)
        .pay_with(&[herb])
        .accept_optional()
        .resolve();

    // THE fix assertion.
    assert_eq!(
        outcome.state().monarch,
        Some(P0),
        "CR 725.1: the activating player must become the monarch"
    );

    // Paired positive reach-guards — prove the chain was actually REACHED and
    // the gated return ran, so the monarch assertion cannot be an artifact of
    // the ability short-circuiting somewhere else.
    assert_eq!(
        outcome.zone_of(creature),
        Zone::Battlefield,
        "the sacrificed creature must be returned to the battlefield"
    );
    assert_eq!(
        counters(&runner, creature),
        3,
        "CR 122.1: the returned creature must enter with three +1/+1 counters"
    );
}

/// CR 608.2c: the monarch grant is a ContinuationStep under the
/// `EffectOutcome`-gated return, so DECLINING "You may sacrifice a creature"
/// must skip it. A `SequentialSibling` placement would wrongly hand out the
/// monarch on decline.
///
/// Non-vacuous by construction: the negative monarch assertion is paired with
/// two positive guards proving the ability really was activated and its costs
/// really were paid — otherwise "monarch is None" would pass trivially on a
/// failed activation.
#[test]
fn declining_the_sacrifice_skips_the_monarch_grant() {
    let (mut runner, herb, creature) = setup();

    let outcome = runner
        .activate(herb, 0)
        .pay_with(&[herb])
        .decline_optional()
        .resolve();

    assert_eq!(
        outcome.state().monarch,
        None,
        "CR 608.2c: declining the optional sacrifice must skip the gated \
         continuation, so no monarch is designated"
    );

    // Reach-guard (a): the ability WAS activated and its sacrifice cost paid —
    // the Herb itself is gone from the battlefield (CR 701.21a).
    assert_ne!(
        outcome.zone_of(herb),
        Zone::Battlefield,
        "the Herb must have been sacrificed as an activation cost, proving the \
         ability was actually activated"
    );
    // Reach-guard (b): the gated ChangeZone did NOT run. A bare "creature is
    // not on the battlefield" check would be vacuous here (it never left), so
    // assert on the counters the return would have added.
    assert_eq!(
        counters(&runner, creature),
        0,
        "the gated return must not have run, so no +1/+1 counters were placed"
    );
}

/// Hostile multi-authority fixture. The sentence names TWO different players:
/// the returned card's OWNER (who gains control of the creature, CR 110.2) and
/// the ability's CONTROLLER (who becomes the monarch, CR 109.5). They are
/// normally the same player, which is exactly why a wrong binding would hide.
/// Here they are forced apart, and the opponent is already the monarch, so a
/// no-op would be indistinguishable from success without this fixture.
#[test]
fn monarch_binds_to_the_controller_while_the_creature_returns_to_its_owner() {
    let (mut runner, herb, creature) = setup();
    // The opponent is already the monarch — so "monarch == Some(P0)" can only
    // be produced by the grant actually running, never by the initial state.
    runner.state_mut().monarch = Some(P1);
    // P0 controls the creature (so P0 may sacrifice it, CR 701.21a) but P1
    // OWNS it (so "under its owner's control" returns it under P1's control).
    // `GameScenario` exposes no owner setter; `state_mut()` is the documented
    // escape hatch.
    runner.state_mut().objects.get_mut(&creature).unwrap().owner = P1;

    let outcome = runner
        .activate(herb, 0)
        .pay_with(&[herb])
        .accept_optional()
        .resolve();

    // CR 109.5: "you" on an activated ability is the player who ACTIVATED it —
    // not the sacrificed card's owner.
    assert_eq!(
        outcome.state().monarch,
        Some(P0),
        "CR 109.5: the monarch must be the ability's controller (P0), taking \
         the designation away from P1"
    );
    // CR 110.2 / CR 110.2a: "under its owner's control" is a separate
    // authority and must still bind to P1.
    assert_eq!(
        outcome.zone_of(creature),
        Zone::Battlefield,
        "the creature must be returned to the battlefield"
    );
    assert_eq!(
        outcome.state().objects[&creature].controller,
        P1,
        "CR 110.2: the returned creature enters under its OWNER's control (P1), \
         independently of who becomes the monarch"
    );
}
