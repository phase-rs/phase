//! Fight for the Throne — the delayed `BecomeMonarch` trigger must honour its
//! intervening-`if` "if you control your commander".
//!
//! Oracle (MSC, verbatim): "Put a +1/+1 counter on target creature you control.
//! Then it fights target creature an opponent controls. When the creature an
//! opponent controls dies this turn, if you control your commander, you become
//! the monarch."
//!
//! The bug: `parse_inner_condition` produced
//! `StaticCondition::ControlsCommander { ownership: Own }` correctly, but the
//! `StaticCondition` -> `AbilityCondition` bridge
//! (`parser::oracle_effect::conditions::static_condition_to_ability_condition`)
//! listed that variant among its "no effect-resolution equivalent -> None" arms,
//! because `AbilityCondition` was the only one of the four condition
//! vocabularies missing a `ControlsCommander` mirror. So
//! `strip_leading_general_conditional` silently discarded the gate and the
//! delayed trigger made you the monarch UNCONDITIONALLY when the fought creature
//! died.
//!
//! These tests parse the verbatim Oracle text through
//! `add_spell_to_hand_from_oracle` (the production parser path the fix modifies)
//! and drive the full cast pipeline, so they exercise parser + runtime
//! end-to-end. They need no `integration_cards.json` regeneration — the card is
//! not in that fixture.
//!
//! Every negative fixture is paired with a positive reach-guard: the fought
//! creature must actually be in the graveyard. Without it a fixture would pass
//! vacuously if the fight failed to kill, the spell fizzled on targeting, or the
//! delayed trigger never fired at all.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ORACLE: &str = "Put a +1/+1 counter on target creature you control. \
Then it fights target creature an opponent controls. When the creature an opponent \
controls dies this turn, if you control your commander, you become the monarch.";

/// The post-resolution facts each fixture asserts on.
///
/// The two commander predicates are non-vacuity guards, not incidental extras:
/// the stolen-commander and owned-but-not-controlled fixtures below are only
/// meaningful if the owner/controller divergence actually materialized. Without
/// them, a `controlled_by` that silently failed (Layer 2 recomputes `controller`
/// from `base_controller` on every pass) would leave P0 controlling no commander
/// at all, and both fixtures would pass for entirely the wrong reason.
struct FightResult {
    monarch: Option<PlayerId>,
    fought_creature_zone: Zone,
    /// CR 903.3d: does P0 control a commander on the battlefield, any owner?
    p0_controls_any_commander: bool,
    /// CR 903.3: does a commander P0 OWNS sit on the battlefield, any controller?
    p0_owns_battlefield_commander: bool,
}

/// Cast and fully resolve Fight for the Throne, with `stage_commander` given the
/// chance to place a commander object first.
fn resolve_fight_for_the_throne(stage_commander: impl FnOnce(&mut GameScenario)) -> FightResult {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Fight for the Throne", true, ORACLE)
        .id();
    // CR 613.4c + CR 701.14a: the +1/+1 counter makes the 5/5 a 6/6, then each
    // creature deals damage equal to its power to the other — 6 kills the 1/1,
    // and the 1 dealt back leaves the 6/6 alive. Only the opponent's creature
    // dies, which is exactly the event the delayed trigger watches.
    let mine = scenario.add_creature(P0, "Throne Claimant", 5, 5).id();
    let theirs = scenario.add_creature(P1, "Doomed Squire", 1, 1).id();

    // The commander is ALWAYS a distinct object from both fighters. The gate is
    // evaluated live as the delayed ability resolves (CR 603.4 + CR 608.2a), so a commander
    // that were itself a fight participant could change zones mid-resolution and
    // make the expected value depend on the fight outcome rather than on the gate.
    stage_commander(&mut scenario);

    let mut runner = scenario.build();
    // CR 601.2c: slots answered in written order — the PutCounter target (a
    // creature you control), then the Fight target (a creature an opponent
    // controls).
    let outcome = runner.cast(spell).target_objects(&[mine, theirs]).resolve();
    let state = outcome.state();
    FightResult {
        monarch: state.monarch,
        fought_creature_zone: outcome.zone_of(theirs),
        // The sibling authority the `Own` arm must NOT be widened to.
        p0_controls_any_commander: engine::game::commander::controls_any_commander(state, P0),
        p0_owns_battlefield_commander: state.battlefield.iter().any(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| obj.is_commander && obj.owner == P0)
        }),
    }
}

/// POSITIVE — CR 903.3 + CR 903.3d: P0 owns and controls a commander on the
/// battlefield, so the intervening-`if` holds and the delayed trigger grants the
/// monarch designation (CR 725.1).
///
/// This is the reachability proof for the whole file: it shows the delayed
/// trigger fires, its gate is evaluated, and a passing gate still produces the
/// monarch. Without it the three negative fixtures below could all be passing
/// because the trigger never runs.
#[test]
fn own_commander_on_battlefield_grants_the_monarch() {
    let result = resolve_fight_for_the_throne(|scenario| {
        scenario
            .add_creature(P0, "Your Commander", 2, 2)
            .commander();
    });

    assert_eq!(
        result.fought_creature_zone,
        Zone::Graveyard,
        "reach-guard: the fought creature must actually have died for the delayed \
         trigger to fire"
    );
    assert_eq!(
        result.monarch,
        Some(P0),
        "CR 903.3d + CR 725.1: with an owned-and-controlled commander on the \
         battlefield the intervening-if holds, so P0 becomes the monarch"
    );
}

/// NEGATIVE + reach-guard — CR 603.4 + CR 608.2a: with no commander anywhere,
/// the intervening-`if` fails as the delayed ability resolves and nobody becomes
/// the monarch.
///
/// THE REGRESSION ASSERTION. Before the fix the gate was dropped during lowering
/// and this read `Some(P0)` — an unconditional monarch grant. Reverting the
/// bridge arm in `static_condition_to_ability_condition` flips it back.
#[test]
fn no_commander_means_no_monarch() {
    let result = resolve_fight_for_the_throne(|_scenario| {});

    assert_eq!(
        result.fought_creature_zone,
        Zone::Graveyard,
        "reach-guard: the fought creature must actually have died, so the delayed \
         trigger genuinely fired and was GATED rather than skipped upstream"
    );
    assert_eq!(
        result.monarch, None,
        "CR 603.4 + CR 608.2a: the intervening-if \"if you control your commander\" fails, so \
         the delayed BecomeMonarch must do nothing. Some(P0) here is the shipped \
         misparse — a dropped gate granting the monarch unconditionally"
    );
}

/// NEGATIVE + reach-guard — the `Own`-vs-`Any` discriminator. P0 has gained
/// control of P1's commander and owns none of their own.
///
/// CR 903.3: the commander designation "is not a characteristic of the object
/// represented by the card; rather, it is an attribute of the card itself", so a
/// stolen commander remains its OWNER's commander. Combined with CR 109.5's
/// possessive "your", `controls_own_commander` is false here even though
/// `controls_any_commander` is true.
///
/// An implementation that delegated the `Own` arm to `controls_any_commander`
/// flips this to `Some(P0)`.
#[test]
fn stolen_opponent_commander_does_not_satisfy_your_commander() {
    let result = resolve_fight_for_the_throne(|scenario| {
        scenario
            .add_creature(P1, "Opposing Commander", 3, 3)
            .commander()
            .controlled_by(P0);
    });

    assert_eq!(
        result.fought_creature_zone,
        Zone::Graveyard,
        "reach-guard: the fought creature must actually have died for the gate to \
         be reached"
    );
    // NON-VACUITY GUARD: the divergence must really exist. If `controlled_by`
    // had silently failed, P0 would control no commander at all and the negative
    // below would pass without discriminating anything.
    assert!(
        result.p0_controls_any_commander,
        "CR 903.3d: the stolen commander must genuinely be under P0's control, so \
         the Any predicate is TRUE while the Own predicate must be FALSE — that \
         gap is the whole point of this fixture"
    );
    assert!(
        !result.p0_owns_battlefield_commander,
        "P0 must own no commander here, or the fixture would not isolate the \
         owner conjunct"
    );
    assert_eq!(
        result.monarch, None,
        "CR 903.3 + CR 109.5: a commander P0 merely CONTROLS is still its owner's \
         commander, so \"your commander\" is not satisfied. Some(P0) means the Own \
         arm was widened to controls_any_commander"
    );
}

/// NEGATIVE + reach-guard — the other conjunct. P0 OWNS a commander but P1
/// controls it, and P0 controls no other commander.
///
/// CR 903.3d resolves "controlling a commander" against a permanent on the
/// battlefield, and CR 613.1b (Layer 2) is what moved control. Ownership alone
/// must not satisfy the gate.
#[test]
fn owning_a_commander_you_do_not_control_does_not_satisfy_the_gate() {
    let result = resolve_fight_for_the_throne(|scenario| {
        scenario
            .add_creature(P0, "Your Commander", 3, 3)
            .commander()
            .controlled_by(P1);
    });

    assert_eq!(
        result.fought_creature_zone,
        Zone::Graveyard,
        "reach-guard: the fought creature must actually have died for the gate to \
         be reached"
    );
    // NON-VACUITY GUARD: P0's commander must really be on the battlefield (just
    // under P1's control). Otherwise this fixture degenerates into "no commander
    // exists", which the `no_commander_means_no_monarch` case already covers.
    assert!(
        result.p0_owns_battlefield_commander,
        "CR 903.3: a commander P0 OWNS must be on the battlefield, so only the \
         controller conjunct is doing the work here"
    );
    assert!(
        !result.p0_controls_any_commander,
        "CR 613.1b: control must genuinely have moved to P1, or the controller \
         conjunct is not being isolated"
    );
    assert_eq!(
        result.monarch, None,
        "CR 903.3d: P0 owns but does not CONTROL the commander, so the gate fails. \
         Some(P0) means the Own arm dropped its controller conjunct"
    );
}
