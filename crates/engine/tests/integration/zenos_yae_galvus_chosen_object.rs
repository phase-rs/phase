//! Zenos yae Galvus — the CR 607.2d chosen-object linkage end-to-end through the
//! REAL parser + trigger pipeline.
//!
//! Verbatim Oracle text (Scryfall FIN 127, transform DFC, front face):
//!
//! ```text
//! My First Friend — When Zenos yae Galvus enters, choose a creature an opponent
//! controls. Until end of turn, creatures other than Zenos yae Galvus and the
//! chosen creature get -2/-2.
//! When the chosen creature leaves the battlefield, transform Zenos yae Galvus.
//! ```
//!
//! The choice is NOT a target (CR 115.1): it is made while the trigger resolves
//! (CR 608.2d) and is recorded on the source as `ChosenAttribute::Card` by the
//! `RememberCard` writer the parser splices into the chain (CR 608.2c). The
//! linked "the chosen creature" readers (CR 607.2d) are
//!   * the population pump's `Not { ChosenCard }` exclusion (CR 611.2a/c), and
//!   * the leaves-the-battlefield trigger's `valid_card` (CR 603.6c + CR 603.10a).
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 115.1: "target" is what declares a target; this choice declares none.
//!   - CR 607.2d: "the chosen [value]" refers only to the linked choice.
//!   - CR 608.2c: the controller follows the instructions in order; the pick is
//!     recorded on the source.
//!   - CR 608.2d: a choice offered by a resolving effect is announced then.
//!   - CR 609.3: an impossible choice does only as much as possible.
//!   - CR 611.2a: the -2/-2 lasts until end of turn.
//!   - CR 611.2c: the pump's affected set is locked when the effect begins.
//!   - CR 603.6c: a leaves-the-battlefield trigger watches the zone change.
//!   - CR 603.10a: leaves-the-battlefield abilities look back in time.
//!   - CR 400.7: a zone change creates a new object, so the reader pins both
//!     the stable `ObjectId` and `incarnation` and matches only that occurrence.
//!
//! Runtime matrix (V1–V4 from the phase-2 plan §4.10). Each row's doc names the
//! engine-side edit whose removal flips it; rows with no engine discriminator
//! (V1/V3, prompt-shape) rely on their positive reach-guards.
//!
//! Fixture boundary: these rows build through `board()`, which uses
//! `add_real_card` — the card is hydrated from the pre-parsed committed fixture,
//! i.e. the parser output is cached at fixture-generation time. A bare
//! parser-arm regression is therefore discriminated directly by the parser unit
//! tests `zenos_trigger_one_pump_lowers_to_the_choice_chain`,
//! `chain_reader_walk_finds_population_family_readers`, and
//! `zenos_leaves_trigger_targets_the_chosen_creature` (engine lib test modules),
//! not by these runtime rows. Observing a parser revert end-to-end through these
//! rows requires regenerating the card export and the fixture first.

use super::rules::{
    GameAction, GameRunner, GameScenario, ObjectId, Phase, WaitingFor, Zone, P0, P1,
};
use engine::game::layers::evaluate_layers;
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{ChosenAttribute, TargetRef};

use crate::support::shared_card_db as load_db;

/// The front-face Oracle text, verbatim (the transform DFC's back face is
/// hydrated by `add_real_card` + `rehydrate_game_from_card_db`).
#[allow(dead_code)]
const ZENOS_ORACLE_TEXT: &str = "My First Friend — When Zenos yae Galvus enters, choose a creature an opponent controls. Until end of turn, creatures other than Zenos yae Galvus and the chosen creature get -2/-2.\nWhen the chosen creature leaves the battlefield, transform Zenos yae Galvus.";

/// Move `id` between zones through the real zone-change pipeline and run the
/// real trigger scan (the phase-1 Probe D recipe). Does NOT resolve the stack.
fn move_and_scan(runner: &mut GameRunner, id: ObjectId, to: Zone) {
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), id, to, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
}

/// Enter `zenos` from hand through the real pipeline and resolve its ETB
/// trigger up to the choice prompt.
fn enter_zenos(runner: &mut GameRunner, zenos: ObjectId) {
    move_and_scan(runner, zenos, Zone::Battlefield);
    runner.advance_until_stack_empty();
}

/// Assert the parked prompt is the choice with the expected cardinality and
/// exact eligible set, answer it with `pick` (or an empty selection), and
/// resolve.
fn answer_choice(
    runner: &mut GameRunner,
    expected: (u32, Option<u32>),
    expected_eligible: &[ObjectId],
    pick: Option<ObjectId>,
) {
    let WaitingFor::ChooseObjectsSelection {
        min, max, eligible, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Zenos's ETB must park on ChooseObjectsSelection, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        (*min, *max),
        expected,
        "the printed quantifier must publish the exact cardinality range"
    );
    let mut actual = eligible.clone();
    actual.sort();
    let mut expected_refs: Vec<TargetRef> = expected_eligible
        .iter()
        .map(|id| TargetRef::Object(*id))
        .collect();
    expected_refs.sort();
    assert_eq!(
        actual, expected_refs,
        "the eligible pool must be exactly the battlefield creatures matching the \
         chosen filter, got {eligible:?}"
    );
    let targets = pick
        .map(|id| vec![TargetRef::Object(id)])
        .unwrap_or_default();
    runner
        .act(GameAction::SelectTargets { targets })
        .expect("the choice answer must be accepted");
    runner.advance_until_stack_empty();
}

/// The remembered ids recorded on `host` via `ChosenAttribute::Card`.
fn remembered_cards(runner: &GameRunner, host: ObjectId) -> Vec<ObjectId> {
    runner.state().objects[&host]
        .chosen_attributes
        .iter()
        .filter_map(|attribute| match attribute {
            ChosenAttribute::Card(pin) => Some(pin.object_id),
            _ => None,
        })
        .collect()
}

/// `(power, toughness)` after the current layer state.
fn pt(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let object = &runner.state().objects[&id];
    (object.power, object.toughness)
}

/// Zenos in hand plus a 3/3 board: one P0 creature (`own_other`), two P1
/// creatures (`chosen` / `other_opponent`), and a 2/2 late entrant in hand.
struct Board {
    runner: GameRunner,
    zenos: ObjectId,
    chosen: ObjectId,
    other_opponent: ObjectId,
    own_other: ObjectId,
    late: ObjectId,
}

fn board() -> Board {
    let Some(db) = load_db() else {
        panic!("the curated card fixture must be present for this test");
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let zenos = scenario.add_real_card(P0, "Zenos yae Galvus", Zone::Hand, db);
    let chosen = scenario.add_real_card(P1, "Centaur Courser", Zone::Battlefield, db);
    let other_opponent = scenario.add_real_card(P1, "Watchwolf", Zone::Battlefield, db);
    let own_other = scenario.add_real_card(P0, "Centaur Courser", Zone::Battlefield, db);
    let late = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    Board {
        runner,
        zenos,
        chosen,
        other_opponent,
        own_other,
        late,
    }
}

// ─── V1: the ETB choice is non-target and remembers the pick ─────────────────

/// V1 (CR 115.1 + CR 607.2d + CR 608.2c/d). Prompt-shape row: no engine-side
/// revert flips it. The parser arm it exercises is discriminated directly by
/// `zenos_trigger_one_pump_lowers_to_the_choice_chain` (a Gate A revert leaves
/// the head `TargetOnly`, so no prompt would be raised — a fixture-cached
/// parser revert is not observable here without export + fixture regeneration).
#[test]
fn zenos_etb_remembers_only_the_chosen_opponent_creature() {
    let mut board = board();
    let Board {
        runner,
        zenos,
        chosen,
        other_opponent,
        own_other,
        ..
    } = &mut board;

    enter_zenos(runner, *zenos);

    // Eligibility is the class's whole point: the two opponent creatures are
    // offered; Zenos itself and P0's own creature are not (the choice is not a
    // target — CR 115.1 — and its filter is controller-relative).
    answer_choice(
        runner,
        (1, Some(1)),
        &[*chosen, *other_opponent],
        Some(*chosen),
    );
    assert_eq!(
        remembered_cards(runner, *zenos),
        vec![*chosen],
        "the chosen opponent creature must be the sole remembered card (CR 608.2c)"
    );
    // The chosen id is the SECOND-listed P1 creature, and neither P0 permanent
    // was selected — positive reach-guards that the pool offered what it claims.
    assert_ne!(*chosen, *own_other);
    assert_ne!(*chosen, *zenos);

    // Blink: leave and re-enter, then pick the OTHER opponent creature. The
    // writer is replace-on-rechoose, so exactly one id (the newest) remains.
    move_and_scan(runner, *zenos, Zone::Exile);
    move_and_scan(runner, *zenos, Zone::Battlefield);
    runner.advance_until_stack_empty();
    answer_choice(
        runner,
        (1, Some(1)),
        &[*chosen, *other_opponent],
        Some(*other_opponent),
    );
    assert_eq!(
        remembered_cards(runner, *zenos),
        vec![*other_opponent],
        "re-choosing on re-entry must REPLACE the remembered card, not accumulate"
    );
}

// ─── V2: the pump excludes exactly the source and the remembered object ──────

/// V2 (CR 607.2d + CR 611.2a/c). Engine-side discriminators: reverting the
/// `ExactLive` live-`Card` overlay in `source_context_from_filter`
/// (game/filter.rs) or the widened `ChosenCard` matcher makes the chosen
/// creature shrink with the rest, flipping the P/T assertions below. Parser-arm
/// reverts are discriminated directly by
/// `chain_reader_walk_finds_population_family_readers` and
/// `exclusion_list_subject_composes_only_the_new_class`.
#[test]
fn zenos_pump_affects_every_creature_except_source_and_chosen() {
    let mut board = board();
    let Board {
        runner,
        zenos,
        chosen,
        other_opponent,
        own_other,
        late,
    } = &mut board;

    enter_zenos(runner, *zenos);
    answer_choice(
        runner,
        (1, Some(1)),
        &[*chosen, *other_opponent],
        Some(*chosen),
    );

    evaluate_layers(runner.state_mut());
    assert_eq!(
        pt(runner, *zenos),
        (Some(4), Some(4)),
        "the source is excluded via FilterProp::Another (it must not shrink itself)"
    );
    assert_eq!(
        pt(runner, *chosen),
        (Some(3), Some(3)),
        "the remembered object is excluded via Not{{ChosenCard}}"
    );
    assert_eq!(
        pt(runner, *other_opponent),
        (Some(1), Some(1)),
        "a non-chosen opponent creature shrinks by -2/-2"
    );
    assert_eq!(
        pt(runner, *own_other),
        (Some(1), Some(1)),
        "a non-chosen creature of ANY controller shrinks by -2/-2 (CR 611.2a)"
    );

    // CR 611.2c: the population was locked when the pump began — a 2/2 entering
    // later this turn must NOT shrink.
    move_and_scan(runner, *late, Zone::Battlefield);
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());
    assert_eq!(
        pt(runner, *late),
        (Some(2), Some(2)),
        "CR 611.2c: a creature that enters after the pump begins stays at base P/T"
    );
}

// ─── V3: no legal choice → (0, Some(0)), nothing remembered, pump still runs ─

/// V3 (CR 609.3 + CR 607.2d). Prompt-shape row: no engine-side revert flips it.
/// The positive reach-guard is the exact infeasible tuple plus the pump still
/// applying to the only creature present; the Gate A arm is discriminated
/// directly by `zenos_trigger_one_pump_lowers_to_the_choice_chain`.
#[test]
fn zenos_no_legal_choice_still_pumps_and_remembers_nothing() {
    let Some(db) = load_db() else {
        panic!("the curated card fixture must be present for this test");
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let zenos = scenario.add_real_card(P0, "Zenos yae Galvus", Zone::Hand, db);
    let own_other = scenario.add_real_card(P0, "Centaur Courser", Zone::Battlefield, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    enter_zenos(&mut runner, zenos);

    // CR 609.3: an impossible exact choice clamps to the achievable (0, Some(0))
    // and still raises the prompt with an empty eligible set.
    answer_choice(&mut runner, (0, Some(0)), &[], None);

    assert!(
        remembered_cards(&runner, zenos).is_empty(),
        "an empty choice must record no ChosenAttribute::Card"
    );

    evaluate_layers(runner.state_mut());
    assert_eq!(
        pt(&runner, zenos),
        (Some(4), Some(4)),
        "the source stays excluded from the pump"
    );
    assert_eq!(
        pt(&runner, own_other),
        (Some(1), Some(1)),
        "CR 609.3: the pump still applies to every creature it can affect"
    );
}

// ─── V4: transform fires once, only for the chosen creature's departure ──────

/// V4 (CR 603.6c + CR 603.10a + CR 607.2d). Engine-side discriminator:
/// reverting the `ChosenCard` arm in `zone_change_filter_inner` (game/filter.rs)
/// leaves the chosen departure queuing nothing, so `transformed` stays false.
/// The parser arm is discriminated directly by
/// `zenos_leaves_trigger_targets_the_chosen_creature`.
#[test]
fn zenos_transforms_once_only_when_the_chosen_creature_leaves() {
    let mut board = board();
    let Board {
        runner,
        zenos,
        chosen,
        other_opponent,
        ..
    } = &mut board;

    enter_zenos(runner, *zenos);
    answer_choice(
        runner,
        (1, Some(1)),
        &[*chosen, *other_opponent],
        Some(*chosen),
    );
    assert!(!runner.state().objects[zenos].transformed);

    // Negative: a NON-chosen opponent creature departs — the look-back reader
    // must not re-identify it, so no trigger queues and Zenos stays front-face.
    move_and_scan(runner, *other_opponent, Zone::Graveyard);
    assert_eq!(
        runner.state().stack.len(),
        0,
        "a non-chosen departure must not queue the ChosenCard trigger"
    );
    runner.advance_until_stack_empty();
    assert!(
        !runner.state().objects[zenos].transformed,
        "a non-chosen departure must not transform Zenos"
    );

    // Positive: the REMEMBERED object departs — exactly one LTB trigger queues
    // and resolves to transform Zenos to the hydrated back face.
    move_and_scan(runner, *chosen, Zone::Graveyard);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the remembered creature's departure must queue exactly one trigger (CR 603.10a)"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[zenos].transformed,
        "the resolved trigger must transform Zenos (CR 603.6c)"
    );
    assert!(
        runner.state().objects[zenos].back_face.is_some(),
        "the DFC back face must be hydrated for the transform to be meaningful"
    );
}

/// V4, separate case: Zenos itself leaving the battlefield is NOT the chosen
/// creature's departure, so the linked reader must not fire.
#[test]
fn zenos_does_not_transform_when_it_itself_leaves() {
    let mut board = board();
    let Board {
        runner,
        zenos,
        chosen,
        other_opponent,
        ..
    } = &mut board;

    enter_zenos(runner, *zenos);
    answer_choice(
        runner,
        (1, Some(1)),
        &[*chosen, *other_opponent],
        Some(*chosen),
    );

    move_and_scan(runner, *zenos, Zone::Graveyard);
    assert_eq!(
        runner.state().stack.len(),
        0,
        "Zenos leaving must not queue the chosen-creature departure trigger"
    );
    runner.advance_until_stack_empty();
    assert!(
        !runner.state().objects[zenos].transformed,
        "Zenos leaving the battlefield must not transform it"
    );
}
