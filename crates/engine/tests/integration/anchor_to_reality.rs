//! Anchor to Reality — the tutored Equipment/Vehicle's mana value gates scry 2.
//!
//! Verbatim Oracle (NEO 45):
//!   As an additional cost to cast this spell, sacrifice an artifact or creature.
//!   Search your library for an Equipment or Vehicle card, put that card onto
//!   the battlefield, then shuffle. If it has mana value less than the
//!   sacrificed permanent's mana value, scry 2.
//!
//! These tests drive the REAL parser (`add_spell_to_hand_from_oracle`) and the
//! REAL cast→resolution pipeline (scenario runner). The composition under test
//! is `SearchLibrary { Equipment or Vehicle }` → `ChangeZone -> Battlefield` →
//! `Shuffle` with a `Scry 2` sub-ability gated by "tutored card MV < sacrificed
//! permanent MV" — the subject-verb ("it has mana value ...") dynamic-threshold
//! surface of the referent-predicate grammar, with the RHS reading the
//! stamped additional-cost referent (`ObjectScope::CostPaidObject`).
//!
//! Revert map: without the dynamic "it has mana value" surface, the leading
//! "If ..." guard fails to parse and the scry becomes an UNCONDITIONAL
//! sub-ability (plus a `Condition_If` swallow warning). Reverted, every case
//! below scries:
//!   * `scry_when_tutored_mv_less_than_sacrificed` — passes either way (the
//!     gate is open); pins the positive branch, not the gate.
//!   * `no_scry_when_tutored_mv_not_less_than_sacrificed` — REVERT-FAILING.
//!     Tutored MV 5 vs sacrificed MV 3 must not scry; reverted, a ScryChoice
//!     prompt appears.
//!   * `no_scry_when_mana_values_are_equal` — REVERT-FAILING boundary. "Less
//!     than" is strict (CR 107.1b): equal MVs (3 vs 3) must not scry.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ANCHOR_ORACLE: &str = "As an additional cost to cast this spell, sacrifice an artifact or creature.\nSearch your library for an Equipment or Vehicle card, put that card onto the battlefield, then shuffle. If it has mana value less than the sacrificed permanent's mana value, scry 2.";

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana_type: ManaType, count: u32) {
    for _ in 0..count {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            mana_type,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

fn put_on_library_top(runner: &mut engine::game::scenario::GameRunner, obj_id: ObjectId) {
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), obj_id, Zone::Library, &mut events);
    let player = runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap();
    player.library.retain(|id| *id != obj_id);
    player.library.insert(0, obj_id);
}

/// Stage Anchor in hand, a MV-3 sacrifice victim on the battlefield, and one
/// Equipment with the given mana value in the library (plus padding so scry 2
/// always has cards to look at). Returns the runner, the spell, the victim,
/// and the Equipment.
fn setup(
    equipment_mv: u32,
) -> (
    engine::game::scenario::GameRunner,
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario
        .add_creature(P0, "Sacrificial Fodder", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();

    let anchor = scenario
        .add_spell_to_hand_from_oracle(P0, "Anchor to Reality", false, ANCHOR_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 2,
        })
        .id();

    let equipment = scenario
        .add_artifact_to_hand_from_oracle(P0, "Test Equipment", "")
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(ManaCost::generic(equipment_mv))
        .id();

    for i in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Library Padding {i}"));
    }

    let mut runner = scenario.build();
    // Anchor costs {2}{U}{U}.
    add_mana(&mut runner, ManaType::Blue, 2);
    add_mana(&mut runner, ManaType::Colorless, 2);
    put_on_library_top(&mut runner, equipment);
    (runner, anchor, victim, equipment)
}

/// Cast Anchor (sacrificing `victim`), resolve through the search by taking
/// `equipment`, and return with resolution parked at whatever prompt follows
/// the shuffle — `ScryChoice` when the gate passes, priority otherwise.
fn cast_and_tutor(
    runner: &mut engine::game::scenario::GameRunner,
    anchor: ObjectId,
    victim: ObjectId,
    equipment: ObjectId,
) {
    let outcome = runner.cast(anchor).pay_cost_with(&[victim]).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::SearchChoice { cards, .. } => {
            assert!(
                cards.contains(&equipment),
                "the Equipment must be a legal Anchor search choice"
            );
        }
        other => panic!("expected SearchChoice after Anchor resolves, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![equipment],
        })
        .expect("selecting the Equipment should resolve the tutor continuation");
}

fn assert_tutor_happened(
    runner: &engine::game::scenario::GameRunner,
    victim: ObjectId,
    equipment: ObjectId,
) {
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "the additional-cost victim must be sacrificed"
    );
    assert_eq!(
        runner.state().objects[&equipment].zone,
        Zone::Battlefield,
        "the tutored Equipment must be put onto the battlefield"
    );
}

/// MV 2 < MV 3: the gate passes, so scry 2 happens.
#[test]
fn scry_when_tutored_mv_less_than_sacrificed() {
    let (mut runner, anchor, victim, equipment) = setup(2);
    cast_and_tutor(&mut runner, anchor, victim, equipment);
    assert_tutor_happened(&runner, victim, equipment);

    match runner.state().waiting_for.clone() {
        WaitingFor::ScryChoice { cards, .. } => {
            assert_eq!(cards.len(), 2, "scry 2 looks at two cards");
            runner
                .act(GameAction::SelectCards { cards })
                .expect("keeping the scried cards on top must succeed");
        }
        other => panic!("expected ScryChoice when 2 < 3, got {other:?}"),
    }
    assert!(
        runner.state().stack.is_empty(),
        "Anchor must be fully resolved after the scry"
    );
}

/// MV 5 vs MV 3: the gate fails, so no scry prompt may appear.
#[test]
fn no_scry_when_tutored_mv_not_less_than_sacrificed() {
    let (mut runner, anchor, victim, equipment) = setup(5);
    cast_and_tutor(&mut runner, anchor, victim, equipment);
    assert_tutor_happened(&runner, victim, equipment);

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::ScryChoice { .. }),
        "no scry may be offered when tutored MV 5 is not less than sacrificed MV 3; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "Anchor must be fully resolved with no scry pending"
    );
}

/// MV 3 vs MV 3: "less than" is strict — equal mana values must not scry.
#[test]
fn no_scry_when_mana_values_are_equal() {
    let (mut runner, anchor, victim, equipment) = setup(3);
    cast_and_tutor(&mut runner, anchor, victim, equipment);
    assert_tutor_happened(&runner, victim, equipment);

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::ScryChoice { .. }),
        "no scry may be offered when tutored MV equals sacrificed MV 3; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "Anchor must be fully resolved with no scry pending"
    );
}
