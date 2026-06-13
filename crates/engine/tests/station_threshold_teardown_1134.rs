//! Discriminating regression test for **issue #1134**: the Station mechanic
//! (Edge of Eternities Spacecraft, CR 721 / CR 702.184) gates its threshold
//! striations on charge counters, and those grants MUST tear down the instant
//! the counter total drops below the printed `{N+}` threshold.
//!
//! Inspirit, Flagship Vessel — a Legendary Artifact Spacecraft, printed P/T 5/5
//! (CR 721.2b box), with the printed striations:
//!
//! > 1+ | At the beginning of combat on your turn, put your choice of a +1/+1
//! >      counter or two charge counters on up to one other target artifact.
//! > 8+ | Flying
//! > Other artifacts you control have hexproof and indestructible.
//!
//! The `8+` striation contributes three continuous effects, all gated on
//! `StaticCondition::HasCounters { OfType(charge), minimum: 8 }`:
//!   1. Spacecraft becomes an artifact *creature* with base P/T 5/5 (CR 721.2b,
//!      synthesized by `database::synthesis::synthesize_station`).
//!   2. The Spacecraft itself gains Flying (SelfRef `AddKeyword`).
//!   3. *Other* artifacts the controller controls gain Hexproof and
//!      Indestructible (a non-SelfRef static, a continuation line under the
//!      `8+` striation per CR 721.2).
//!
//! THE DISCRIMINATOR (the behavior that would regress pre-67bf02ec7): when the
//! charge total falls from 8 to 7, the layer system must re-evaluate the
//! `HasCounters` gate and *remove* all three grants. CR 611.3a: a continuous
//! effect from a static ability "isn't locked in; it applies at any given
//! moment to whatever its text indicates." CR 721.2a/b phrases the striation as
//! "As long as this permanent has N or more charge counters on it…". A fix that
//! only added the grant at threshold but failed to tear it down on counter loss
//! would leave Flying/Hexproof/Indestructible/Creature-type present at 7
//! counters — every "torn down" assertion below would then fail.
//!
//! This test drives the real pipeline: it builds the Spacecraft from its
//! verbatim Oracle text through the scenario harness (the same `synthesize_all`
//! production runs), manipulates `obj.counters` directly (mirroring the runtime
//! effect of `Effect::PutCounter` / removal), and reads the *effective*
//! post-layer characteristics via `evaluate_layers`. It is a runtime regression
//! test, not an AST shape test.
//!
//! CR 122.1: a counter is a marker placed on an object that interacts with
//! rules, abilities, or effects (here, the charge-counter gate).
//! CR 611.3a: continuous effects from static abilities are not locked in.
//! CR 721.2a/b: a station symbol `{N+}` static applies only while the permanent
//! has N or more charge counters.

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

/// Inspirit, Flagship Vessel — verbatim Oracle text (from card-data.json).
const INSPIRIT_ORACLE: &str = "Station (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery. It's an artifact creature at 8+.)\n\
1+ | At the beginning of combat on your turn, put your choice of a +1/+1 counter or two charge counters on up to one other target artifact.\n\
8+ | Flying\n\
Other artifacts you control have hexproof and indestructible.";

/// Recompute every layer, then return the object after a fresh evaluation.
/// CR 611.3a: the `HasCounters` gate is re-tested here every pass, so the
/// effective characteristics always reflect the current counter total.
fn recompute(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

/// Set the charge-counter total on `id` to exactly `n` (CR 122.1). `n == 0`
/// leaves a zeroed entry, which `counter_condition_matches` treats as below any
/// positive minimum — the same result the runtime sees when counters are
/// removed.
fn set_charge(runner: &mut GameRunner, id: ObjectId, n: u32) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.counters
        .insert(CounterType::Generic("charge".to_string()), n);
}

fn is_creature(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .card_types
        .core_types
        .contains(&CoreType::Creature)
}

fn has_kw(runner: &GameRunner, id: ObjectId, kw: &Keyword) -> bool {
    has_keyword(&runner.state().objects[&id], kw)
}

#[test]
fn station_grant_tears_down_when_charge_drops_below_threshold() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Build Inspirit, Flagship Vessel inline as a Legendary Artifact Spacecraft
    // with printed P/T 5/5. The type line must be set BEFORE `from_oracle_text`
    // so the synthesis pipeline (`synthesize_station`) sees the Spacecraft
    // subtype + printed P/T and emits the 8+ creature-shift static (CR 721.2b).
    let inspirit = scenario
        .add_creature(P0, "Inspirit, Flagship Vessel", 5, 5)
        .as_artifact()
        .as_legendary()
        .with_subtypes(vec!["Spacecraft"])
        .from_oracle_text(INSPIRIT_ORACLE)
        .id();

    // A second artifact P0 controls — the recipient of the gated "Other
    // artifacts you control have hexproof and indestructible" grant.
    let recipient = scenario
        .add_creature(P0, "Recipient Artifact", 0, 0)
        .as_artifact()
        .id();

    let mut runner = scenario.build();

    // --- Below threshold (0 charge counters): no grants ---
    recompute(&mut runner);
    assert!(
        !is_creature(&runner, inspirit),
        "CR 721.2b: at 0 charge counters the Spacecraft is a noncreature artifact"
    );
    assert!(
        !has_kw(&runner, inspirit, &Keyword::Flying),
        "CR 721.2a: Flying (8+ striation) absent below threshold"
    );
    assert!(
        !has_kw(&runner, recipient, &Keyword::Hexproof),
        "CR 611.3a: other-artifact Hexproof grant absent below threshold"
    );
    assert!(
        !has_kw(&runner, recipient, &Keyword::Indestructible),
        "CR 611.3a: other-artifact Indestructible grant absent below threshold"
    );

    // --- At threshold (8 charge counters): all grants active ---
    set_charge(&mut runner, inspirit, 8);
    recompute(&mut runner);
    assert!(
        is_creature(&runner, inspirit),
        "CR 721.2b: at 8 charge counters the Spacecraft becomes an artifact creature"
    );
    {
        let obj = &runner.state().objects[&inspirit];
        assert_eq!(
            (obj.power, obj.toughness),
            (Some(5), Some(5)),
            "CR 721.2b: artifact creature uses its printed 5/5 box at threshold"
        );
    }
    assert!(
        has_kw(&runner, inspirit, &Keyword::Flying),
        "CR 721.2a: Flying granted at 8 charge counters"
    );
    assert!(
        has_kw(&runner, recipient, &Keyword::Hexproof),
        "CR 611.3a: other artifacts gain Hexproof at threshold"
    );
    assert!(
        has_kw(&runner, recipient, &Keyword::Indestructible),
        "CR 611.3a: other artifacts gain Indestructible at threshold"
    );

    // --- TEARDOWN (the discriminator): drop to 7, just below threshold ---
    // Pre-67bf02ec7 (grant added but never torn down) these four assertions
    // fail: the keywords / creature type survive at 7 counters.
    set_charge(&mut runner, inspirit, 7);
    recompute(&mut runner);
    assert!(
        !is_creature(&runner, inspirit),
        "CR 611.3a / CR 721.2b: creature-shift torn down at 7 (< 8) charge counters"
    );
    assert!(
        !has_kw(&runner, inspirit, &Keyword::Flying),
        "CR 611.3a: Flying torn down at 7 (< 8) charge counters"
    );
    assert!(
        !has_kw(&runner, recipient, &Keyword::Hexproof),
        "CR 611.3a: other-artifact Hexproof torn down at 7 (< 8) charge counters"
    );
    assert!(
        !has_kw(&runner, recipient, &Keyword::Indestructible),
        "CR 611.3a: other-artifact Indestructible torn down at 7 (< 8) charge counters"
    );

    // --- Drop all the way to 0: re-assert the all-clear ---
    set_charge(&mut runner, inspirit, 0);
    recompute(&mut runner);
    assert!(!is_creature(&runner, inspirit), "all-clear at 0 counters");
    assert!(
        !has_kw(&runner, inspirit, &Keyword::Flying),
        "all-clear at 0 counters: Flying"
    );
    assert!(
        !has_kw(&runner, recipient, &Keyword::Hexproof),
        "all-clear at 0 counters: Hexproof"
    );
    assert!(
        !has_kw(&runner, recipient, &Keyword::Indestructible),
        "all-clear at 0 counters: Indestructible"
    );
}
