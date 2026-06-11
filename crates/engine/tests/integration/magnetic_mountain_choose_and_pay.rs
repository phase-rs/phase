//! Discriminating runtime test for Unit 07d — Magnetic Mountain's
//! "choose any number of tapped blue creatures ... and pay {4} for each
//! creature chosen this way. If the player does, untap those creatures."
//! upkeep trigger.
//!
//! Pre-07d the parser swallowed BOTH dropped pieces:
//!  1. the interactive selection clause — no effect ever populated the tracked
//!     set the `IfYouDo`/`Untap` reads, so it untapped nothing;
//!  2. the per-creature cost multiplier — the cost was a fixed `{4}`, not
//!     `{4}` × (creatures chosen).
//!
//! This test drives the full pipeline through `apply`: the upkeep trigger
//! fires, the engine raises `WaitingFor::ChooseObjectsSelection`, the player
//! selects 2 tapped blue creatures, the `PayCost` scaled mana charges
//! {4}×2 = {8}, and the reused `IfYouDo`/`Untap{TrackedSet}` tail untaps
//! exactly those 2 creatures. Every assertion fails against pre-07d behavior.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// CR 603.7e + CR 118.1: Magnetic Mountain's "choose any number ... pay {4}
/// for each ... chosen this way" upkeep trigger surfaces an interactive
/// selection, scales the mana cost by the selection size, and untaps the
/// chosen creatures.
#[test]
fn magnetic_mountain_choose_and_pay_per_creature_untaps_selection() {
    let mut scenario = GameScenario::new();
    // Start at Untap so advancing to the main phase passes through Upkeep,
    // firing the "at the beginning of each player's upkeep" trigger naturally.
    scenario.at_phase(Phase::Untap);

    // Magnetic Mountain — both lines parsed from Oracle text. Line 1's static
    // ("blue creatures don't untap") keeps the pre-tapped bears tapped through
    // the untap step; line 2 is the Unit 07d trigger under test.
    scenario.add_creature_from_oracle(
        P0,
        "Magnetic Mountain",
        0,
        1,
        "Blue creatures don't untap during their controllers' untap steps.\n\
         At the beginning of each player's upkeep, that player may choose any \
         number of tapped blue creatures they control and pay {4} for each \
         creature chosen this way. If the player does, untap those creatures.",
    );

    let blue_a = scenario.add_creature(P0, "Blue Bear A", 2, 2).id();
    let blue_b = scenario.add_creature(P0, "Blue Bear B", 2, 2).id();

    // Exactly {8} of mana — {4} × 2 chosen creatures. Stocked AFTER the untap
    // step below, not here: CR 500.4 empties each player's mana pool at the end
    // of every step, so build-time mana would be gone before the upkeep trigger
    // resolves.
    let pool: Vec<ManaUnit> = (0..8)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect();

    // Stock the library so the Draw step does not deck P0 out.
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);

    let mut runner = scenario.build();

    // Colour the two bears blue so they match the trigger's "tapped blue
    // creatures" filter.
    for id in [blue_a, blue_b] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .expect("bear object exists")
            .color = vec![ManaColor::Blue];
    }

    // Advance through Untap into Upkeep — the trigger fires and goes on the
    // stack. The `may` trigger first prompts to accept the optional effect.
    runner.auto_advance_to_main_phase();

    // Tap the bears now — after the untap step, before the trigger resolves —
    // so the "tapped blue creatures they control" filter has live targets when
    // `ChooseObjectsIntoTrackedSet` evaluates it.
    for id in [blue_a, blue_b] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .expect("bear object exists")
            .tapped = true;
    }

    // Stock P0's mana pool now — after the untap step (which emptied any
    // build-time mana per CR 500.4), so the upkeep trigger's
    // `PayCost { ScaledMana }` sees the full {8} when it resolves.
    if let Some(p) = runner.state_mut().players.iter_mut().find(|p| p.id == P0) {
        p.mana_pool.mana = pool;
    }

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the \"may\" trigger prompts to accept the optional effect, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the optional upkeep trigger");

    // Pass priority so the accepted trigger resolves off the stack — its
    // `ChooseObjectsIntoTrackedSet` head raises the interactive prompt.
    runner.advance_until_stack_empty();

    // DISCRIMINATING ASSERTION 1 — the swallowed selection clause now surfaces
    // a real interactive prompt. Pre-07d no selection effect existed at all.
    let WaitingFor::ChooseObjectsSelection {
        player, eligible, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected WaitingFor::ChooseObjectsSelection, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "the upkeep player makes the selection");
    assert_eq!(
        eligible.len(),
        2,
        "both tapped blue creatures are eligible, got {eligible:?}"
    );

    // Select BOTH tapped blue creatures.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(blue_a), TargetRef::Object(blue_b)],
        })
        .expect("object selection accepted");

    // Resolve the rest of the chain: PayCost { ScaledMana } then IfYouDo/Untap.
    runner.advance_until_stack_empty();

    // DISCRIMINATING ASSERTION 2 — PayCost scaled mana charged {4} × 2 =
    // {8}, draining the whole pool. Pre-07d the fixed {4} would leave 4 behind.
    let mana_left = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .mana_pool
        .mana
        .len();
    assert_eq!(
        mana_left, 0,
        "ScaledMana must charge {{4}}×2 = {{8}}, draining the 8-mana pool"
    );

    // DISCRIMINATING ASSERTION 3 — the IfYouDo/Untap tail untapped exactly the
    // chosen creatures (reading the freshly-published tracked set). Pre-07d the
    // tracked set was always empty, so nothing untapped.
    assert!(
        !runner.state().objects.get(&blue_a).unwrap().tapped,
        "chosen creature A must be untapped"
    );
    assert!(
        !runner.state().objects.get(&blue_b).unwrap().tapped,
        "chosen creature B must be untapped"
    );
}
