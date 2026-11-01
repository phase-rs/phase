//! Regression for GitHub issue #4359 — Sephiroth, Fabled SOLDIER // Sephiroth,
//! One-Winged Angel.
//!
//! Front face (`Sephiroth, Fabled SOLDIER`):
//!   "Whenever Sephiroth enters or attacks, you may sacrifice another
//!    creature. If you do, draw a card.
//!    Whenever another creature dies, target opponent loses 1 life and you
//!    gain 1 life. If this is the fourth time this ability has resolved this
//!    turn, transform Sephiroth."
//!
//! Back face (`Sephiroth, One-Winged Angel`), the Super Nova line:
//!   "Super Nova — As this creature transforms into Sephiroth, One-Winged
//!    Angel, you get an emblem with \"Whenever a creature dies, target
//!    opponent loses 1 life and you gain 1 life.\""
//!
//! Issue #4359 splits into two halves. The "does not transform" half was
//! already fixed by `8a05aa5af` (#8025) before this change; Test 3 is a
//! permanent regression guard for that half, not a discriminator. The "nor
//! give a player the emblem" half is the live defect this change fixes: the
//! Super Nova line was parsed as a battlefield-scoped `StaticDefinition`
//! (`GrantTrigger`) instead of a CR 701.27e `Transformed` trigger producing a
//! CR 114 emblem — so the command-zone emblem never existed, and any drain
//! from a creature death after Sephiroth left the battlefield was silently
//! lost.
//!
//! CR references (verified against `docs/MagicCompRules.txt`):
//!   - CR 701.27e: an `As … transforms into …` line is a triggered ability.
//!   - CR 114.1 + CR 114.2 + CR 114.4: emblems live in the command zone,
//!     owned and controlled by the trigger's controller, and their abilities
//!     function from the command zone.
//!   - CR 114.5: an emblem is neither a card nor a permanent — it persists
//!     for the rest of the game, independent of its source's zone.
//!   - CR 603.3a: a triggered ability is controlled by whoever controlled its
//!     source when it triggered.
//!   - CR 603.3d: "target opponent" is a fresh choice each time the granted
//!     trigger fires, not a value latched once at emblem creation.
//!   - CR 701.27f: an ability of a permanent transforms it only once per
//!     activation/triggering — no second emblem is created.
//!
//! WHAT THESE TESTS GUARD, AND WHAT THEY DO NOT. Sephiroth is loaded through
//! `add_real_card` / `shared_card_db`, i.e. from the PRE-PARSED committed
//! fixture (`tests/fixtures/integration_cards.json.gz`) — not by re-running the
//! parser over the Oracle text. So these tests guard the FIXTURE plus the
//! runtime path (emblem creation, CR 114 command-zone function, CR 603.3d
//! per-fire targeting); they cannot fail on a source-only revert of
//! `parse_as_transforms_into_keyword`. The source-level regression guard for
//! that parser change is the parser unit tests in
//! `parser/oracle_trigger_tests.rs`. To keep the fixture half honest, `setup`
//! asserts the back face still carries a `TriggerMode::Transformed` trigger, so
//! a stale or re-generated-from-a-regressed-parser fixture fails loudly here
//! instead of passing vacuously.

use engine::database::card_db::CardDatabase;
use engine::game::rehydrate_game_from_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);
const DESTROY: &str = "Destroy target creature.";

/// Shared 3-player setup: Sephiroth on P0's battlefield (pre-existing
/// permanent, per `add_real_card`), five 1/1 vanilla fodder creatures for P0,
/// and six free "Destroy target creature." instants in P0's hand — one per
/// kill this suite drives. 3 players so "target opponent" is a real CR
/// 603.3d choice and the unchosen third player is an observable control.
fn setup(db: &'static CardDatabase) -> (GameRunner, ObjectId, Vec<ObjectId>, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let seph = scenario.add_real_card(P0, "Sephiroth, Fabled SOLDIER", Zone::Battlefield, db);

    let fodder: Vec<ObjectId> = (0..5).map(|_| scenario.add_vanilla(P0, 1, 1)).collect();

    let destroys: Vec<ObjectId> = (0..6)
        .map(|i| {
            scenario
                .add_spell_to_hand_from_oracle(P0, &format!("Destroy Spell {i}"), true, DESTROY)
                .id()
        })
        .collect();

    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);

    // Precondition guard: without a back face, the whole test can pass
    // vacuously on a card that lost its transform target in the fixture.
    let back_face = runner.state().objects[&seph]
        .back_face
        .as_ref()
        .expect("precondition: Sephiroth must carry a back face");

    // FIXTURE-FRESHNESS GUARD. These tests read the pre-parsed committed
    // fixture, so a stale fixture (or one regenerated from a regressed parser)
    // would silently take the back face's Super Nova line back to its BASE
    // shape — a battlefield-scoped `StaticDefinition` — and this suite would
    // then be testing a card that no longer has the ability under test.
    // CR 701.27e: `As this creature transforms into …` is a TRIGGERED ability,
    // so the back face must carry a `TriggerMode::Transformed` trigger.
    assert!(
        back_face
            .trigger_definitions
            .iter_unchecked()
            .any(|trigger| matches!(trigger.mode, TriggerMode::Transformed)),
        "stale fixture: Sephiroth's back face must carry a CR 701.27e \
         TriggerMode::Transformed trigger for the Super Nova line; \
         regenerate crates/engine/tests/fixtures/integration_cards.json.gz"
    );

    (runner, seph, fodder, destroys)
}

/// Every object currently in `Zone::Command` flagged as an emblem.
fn command_zone_emblems(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .iter()
        .filter(|(_, obj)| obj.is_emblem && obj.zone == Zone::Command)
        .map(|(id, _)| *id)
        .collect()
}

/// CR 701.27e + CR 114.1/.2/.4/.5: the 4th resolution of Sephiroth's dies
/// trigger transforms him (already fixed — reach guard, not a discriminator)
/// AND creates a functioning Super Nova emblem in the command zone. The
/// PRIMARY discriminator is the emblem's existence after death 4; the
/// FUNCTIONAL discriminator is a fodder death AFTER Sephiroth has left the
/// battlefield, which only drains under the emblem (CR 114.4 + CR 114.5) —
/// at base the misparsed `GrantTrigger` static is battlefield-scoped and
/// leaves with Sephiroth, so that death drains 0/0/0.
#[test]
fn sephiroth_fourth_resolution_transform_grants_functioning_super_nova_emblem() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, seph, fodder, destroys) = setup(db);

    // Death 1 — REACH GUARD (passes at base): the front-face dies trigger and
    // its CR 603.3d "target opponent" choice are live.
    let outcome = runner
        .cast(destroys[0])
        .target_object(fodder[0])
        .target_player(P1)
        .resolve();
    outcome.assert_life_delta(P0, 1);
    outcome.assert_life_delta(P1, -1);
    outcome.assert_life_delta(P2, 0);

    // Deaths 2-3.
    for i in 1..3 {
        runner
            .cast(destroys[i])
            .target_object(fodder[i])
            .target_player(P1)
            .resolve();
    }

    // REACH GUARDS (pass at base): ledger at 3, not yet transformed, no
    // emblem exists yet. The "no emblem" check is a negative whose paired
    // positive is the ledger assertion on the same line.
    assert_eq!(
        runner
            .state()
            .ability_resolutions_this_turn
            .get(&(seph, 1))
            .copied(),
        Some(3),
        "reach guard: ledger must read 3 after three resolutions"
    );
    assert!(
        !runner.state().objects[&seph].transformed,
        "reach guard: must not be transformed after only 3 resolutions"
    );
    assert!(
        command_zone_emblems(&runner).is_empty(),
        "no emblem may exist before the 4th resolution"
    );

    // Death 4 — the transform-triggering resolution.
    runner
        .cast(destroys[3])
        .target_object(fodder[3])
        .target_player(P1)
        .resolve();

    // REACH GUARD (passes at base — fixed upstream by `8a05aa5af`): the
    // already-fixed transform half must still flip on the 4th resolution.
    assert!(
        runner.state().objects[&seph].transformed,
        "reach guard: the 4th resolution must transform Sephiroth"
    );

    // PRIMARY DISCRIMINATOR (fails on revert): at base the command zone is
    // empty after the 4th resolution (measured in five shapes). CR 114.1 +
    // CR 114.2 + CR 114.4: exactly one emblem object, owned and controlled by
    // P0 (Sephiroth's controller when the Transformed trigger fired), with
    // exactly one granted trigger definition.
    let emblems = command_zone_emblems(&runner);
    assert_eq!(
        emblems.len(),
        1,
        "PRIMARY DISCRIMINATOR: exactly one command-zone emblem must exist after \
         the 4th resolution, got {emblems:?}"
    );
    let emblem_obj = &runner.state().objects[&emblems[0]];
    assert_eq!(emblem_obj.owner, P0, "CR 114.2: emblem must be owned by P0");
    assert_eq!(
        emblem_obj.controller, P0,
        "CR 114.2: emblem must be controlled by P0"
    );
    assert_eq!(
        emblem_obj.trigger_definitions.len(),
        1,
        "the granted Super Nova emblem must carry exactly one trigger definition"
    );

    // Kill Sephiroth himself — a STEP, not a discriminator: both worlds drain
    // P0 +1 / P1 -1 here (at base from the misparsed battlefield-scoped
    // GrantTrigger static, which is still attached while Sephiroth is alive;
    // post-fix from the emblem). Asserted only as a reach guard that the kill
    // landed.
    runner
        .cast(destroys[4])
        .target_object(seph)
        .target_player(P1)
        .resolve();
    assert_eq!(
        runner.state().objects[&seph].zone,
        Zone::Graveyard,
        "reach guard: Sephiroth must be dead before the functional discriminator"
    );

    // FUNCTIONAL DISCRIMINATOR (fails on revert): a fodder death AFTER
    // Sephiroth has left the battlefield. CR 114.4 + CR 114.5: the emblem's
    // granted dies trigger functions from the command zone and persists for
    // the rest of the game, independent of Sephiroth's own zone. At base the
    // misparsed static was battlefield-scoped and left with Sephiroth, so
    // this death measured 0/0/0 at base.
    let outcome = runner
        .cast(destroys[5])
        .target_object(fodder[4])
        .target_player(P1)
        .resolve();
    outcome.assert_life_delta(P0, 1);
    outcome.assert_life_delta(P1, -1);
    outcome.assert_life_delta(P2, 0);

    // CR 701.27f: no second emblem was created. Paired positive is the life
    // delta assertion immediately above.
    let emblems = command_zone_emblems(&runner);
    assert_eq!(
        emblems.len(),
        1,
        "CR 701.27f: no second emblem may be created, got {emblems:?}"
    );
}

/// CR 114.2 + CR 603.3a: the emblem is owned and controlled by whoever
/// controlled Sephiroth when the Transformed trigger fired — the
/// CONTROLLER, not the owner. Multi-authority hostile fixture: P0 owns
/// Sephiroth but P1 controls him at the moment of the transform.
#[test]
fn sephiroth_super_nova_emblem_goes_to_the_controller_not_the_owner() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, seph, fodder, destroys) = setup(db);

    // Three deaths while P0 still controls Sephiroth.
    for i in 0..3 {
        runner
            .cast(destroys[i])
            .target_object(fodder[i])
            .target_player(P1)
            .resolve();
    }
    assert_eq!(
        runner
            .state()
            .ability_resolutions_this_turn
            .get(&(seph, 1))
            .copied(),
        Some(3),
        "reach guard: ledger must read 3 before the control change"
    );

    // CR 613.1b: control is a Layer-2 characteristic. `base_controller` is the
    // authority the layer pass recomputes `controller` from — writing only
    // `controller` is discarded on the next pass. Same pair
    // `CardBuilder::controlled_by` writes (game/scenario.rs).
    {
        let seph_obj = runner.state_mut().objects.get_mut(&seph).unwrap();
        seph_obj.base_controller = Some(P1);
        seph_obj.controller = P1;
    }

    // 4th death: P1 now controls Sephiroth, so P1 is not a legal "target
    // opponent" for its own trigger — target P2 instead.
    runner
        .cast(destroys[3])
        .target_object(fodder[3])
        .target_player(P2)
        .resolve();

    // Paired positive: the emblem exists at all, so the identity assertions
    // below cannot pass vacuously on an absent object.
    let emblems = runner
        .state()
        .objects
        .iter()
        .filter(|(_, obj)| obj.is_emblem && obj.zone == Zone::Command)
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    assert_eq!(emblems.len(), 1, "got {emblems:?}");
    let emblem_obj = &runner.state().objects[&emblems[0]];
    assert_eq!(
        emblem_obj.owner, P1,
        "CR 114.2 + CR 603.3a: emblem must be owned by the CONTROLLER (P1), not the owner (P0)"
    );
    assert_eq!(
        emblem_obj.controller, P1,
        "CR 114.2 + CR 603.3a: emblem must be controlled by the CONTROLLER (P1)"
    );
}

/// Regression guard for the already-fixed transform half of issue #4359 —
/// PASSES AT BASE BY DESIGN. This is the permanent Sephiroth-shaped witness
/// for the ordinal-sibling class fixed by `8a05aa5af`: the transform must
/// happen automatically, with no extra prompt surfaced between deaths.
#[test]
fn sephiroth_fourth_resolution_transform_is_automatic() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, seph, fodder, destroys) = setup(db);

    for i in 0..4 {
        let outcome = runner
            .cast(destroys[i])
            .target_object(fodder[i])
            .target_player(P1)
            .resolve();
        assert!(
            matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
            "no prompt other than the CR 603.3d TriggerTargetSelection may be surfaced \
             between deaths, got {:?}",
            outcome.final_waiting_for()
        );
    }

    assert!(
        runner.state().objects[&seph].transformed,
        "the user's \"it should happen automatically\": the 4th resolution must \
         transform Sephiroth with no extra prompt"
    );
}
