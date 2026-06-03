//! Regression: Timely Ward — issue #329.
//!
//! CR 601.3d + CR 702.8a + CR 903.3: "You may cast this spell as though it had
//! flash if it targets a commander." The conditional flash permission must
//! gate the cast on the chosen target being a commander. Prior to this fix the
//! parser detected the `if`-clause via the `SwallowedClause / Condition_If`
//! detector but emitted the bare `AsThoughHadFlash` casting option without a
//! condition slot — letting the AI / player cast at instant speed against any
//! creature regardless of commander status.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn load_db() -> Option<&'static CardDatabase> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        return None;
    }
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    Some(DB.get_or_init(|| CardDatabase::from_export(&path).expect("export should load")))
}

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// Set up a baseline scenario at P1's end step (outside P0's sorcery-speed
/// window) with Timely Ward in P0's hand, a commander on the battlefield, and
/// a non-commander creature also on the battlefield. The phase choice (P1's
/// end step + P0 has priority) ensures sorcery-speed timing is unavailable;
/// only the flash permission can authorize the cast.
fn build_scenario(
    db: &CardDatabase,
    commander_controller: engine::types::player::PlayerId,
) -> (
    engine::game::scenario::GameRunner,
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let timely_id = scenario.add_real_card(P0, "Timely Ward", Zone::Hand, db);
    // Two creatures on the battlefield: one designated as commander, one not.
    let commander_id =
        scenario.add_real_card(commander_controller, "Grizzly Bears", Zone::Battlefield, db);
    let plain_id =
        scenario.add_real_card(commander_controller, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    // Active player is P1; P0 must have priority for an outside-sorcery cast.
    runner.state_mut().active_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    // Stamp commander designation.
    runner
        .state_mut()
        .objects
        .get_mut(&commander_id)
        .unwrap()
        .is_commander = true;
    // Mana: {2}{W} = three white, or {W} + 2 generic; provide pool.
    add_mana(
        &mut runner,
        &[ManaType::White, ManaType::Colorless, ManaType::Colorless],
    );
    (runner, timely_id, commander_id, plain_id)
}

#[test]
fn timely_ward_cast_targeting_commander_succeeds() {
    // CR 601.3d: With a commander as the chosen target, the conditional flash
    // permission authorizes the cast outside sorcery-speed timing.
    let Some(db) = load_db() else {
        return;
    };
    let (mut runner, timely_id, commander_id, _plain_id) = build_scenario(db, P1);

    // Targeting a commander satisfies the conditional flash permission, so the
    // instant-speed cast is accepted: the driver declares the commander target,
    // finalizes, and resolves to a clean priority window. The Aura attaches to
    // the targeted commander (CR 303.4f).
    let outcome = runner.cast(timely_id).target_object(commander_id).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "conditional-flash cast targeting a commander must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[timely_id], Zone::Battlefield);
}

#[test]
fn timely_ward_cast_targeting_noncommander_is_rejected_at_finalize() {
    // CR 601.3d: With a non-commander creature as the chosen target, the
    // target-dependent flash condition fails at the finalize-time validation
    // gate. The cast must be rejected (and the stack-entry placeholder
    // popped). Prior to this fix the parser emitted the flash permission
    // unconditionally and the cast went through against any target.
    let Some(db) = load_db() else {
        return;
    };
    let (mut runner, timely_id, _commander_id, plain_id) = build_scenario(db, P1);
    let card_id = runner.state().objects[&timely_id].card_id;

    let r1 = runner
        .act(GameAction::CastSpell {
            object_id: timely_id,
            card_id,
            targets: vec![],
        })
        .expect("cast announcement should be accepted at instant speed via conditional flash");
    assert!(
        matches!(r1.waiting_for, WaitingFor::TargetSelection { .. }),
        "Expected TargetSelection, got {:?}",
        r1.waiting_for
    );

    let result = runner.act(GameAction::SelectTargets {
        targets: vec![TargetRef::Object(plain_id)],
    });
    assert!(
        result.is_err(),
        "targeting a non-commander at instant speed must be rejected at finalize; got {result:?}"
    );

    // After rejection the stack entry for the cancelled cast must have been
    // popped — the stack is empty again.
    assert!(
        runner.state().stack.is_empty(),
        "stack must be empty after cancelled cast, got {:?}",
        runner.state().stack
    );
}
