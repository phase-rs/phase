//! CR 608.2h + CR 113.7a + CR 603.4 + CR 722.3a: effect-level "if this creature
//! is[n't] prepared" conditions must read the source subject through the same
//! exact-source authority as the trigger intervening-if recheck — the captured
//! incarnation's live-or-exit state — never a raw live lookup of
//! `ability.source_id` that a departed or blinked source can rebind (CR 400.7).
//!
//! Witnesses are synthetic admitted grammar parsed through the production
//! parser (`from_oracle_text`): an upkeep trigger whose single instruction is a
//! conditional draw ("…draw a card if this creature is[n't] prepared"), and —
//! for the activation epoch — a costed activated ability with the same
//! instruction ("{1}: Draw a card if this creature is[n't] prepared."), whose
//! resolving ability carries no trigger projection, only the source incarnation
//! captured when the activation reached the stack. Draw is the non-idempotent
//! conditional effect — every dispatch grows the hand by exactly one — so
//! hand-size deltas plus per-source `EffectResolved{Draw}` counts discriminate
//! a real resolution from a skipped dispatch. The live controls (prepared
//! dispatches, unprepared suppresses) keep the absence assertions in the
//! departure and blink twins non-vacuous.
//!
//! Each departure/blink pair stages a divergence between the exit state and the
//! live object at the same storage id: exit-prepared/returned-unprepared must
//! still see the exit designation, and exit-unprepared/returned-prepared must
//! still see the exit absence — for both polarities. A raw live read inverts at
//! least one verdict in every pair, so no twin can pass against the pre-fix
//! arm.
//!
//! The activation cohort mirrors the trigger matrix through the activated
//! consumer: the ability is activated (source incarnation captured at the
//! activation's announcement, before any cost is paid), then the source is
//! left alone (live), destroyed (departed), or blinked (a new incarnation at
//! the same storage id) before the ability resolves — including returns whose
//! prepared state diverges from the exit state in both directions. Completion
//! runs through the Priority system: the removal/blink is cast in response,
//! then priority is passed until the stack drains.
//!
//! The self-sacrifice cohort makes the cost itself move the source
//! ("Sacrifice this creature: Draw a card if this creature is[n't]
//! prepared."): the sacrifice advances the storage incarnation before stack
//! placement, so only the announcement-captured incarnation's exit last-known
//! information still records what the source was when the ability was
//! activated, and a placement-time stamp would invert the discriminating
//! verdicts (CR 602.2a + CR 113.7a).

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    GameState, ManaChoice, ManaChoicePrompt, PersistedGameState, PersistedRestoreFinalization,
    WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const PREPARED_DRAW_ORACLE: &str =
    "At the beginning of your upkeep, draw a card if this creature is prepared.";
const UNPREPARED_DRAW_ORACLE: &str =
    "At the beginning of your upkeep, draw a card if this creature isn't prepared.";
const PREPARED_ACTIVATION_ORACLE: &str = "{1}: Draw a card if this creature is prepared.";
const UNPREPARED_ACTIVATION_ORACLE: &str = "{1}: Draw a card if this creature isn't prepared.";
const SELF_SACRIFICE_PREPARED_ACTIVATION_ORACLE: &str =
    "Sacrifice this creature: Draw a card if this creature is prepared.";
const SELF_SACRIFICE_UNPREPARED_ACTIVATION_ORACLE: &str =
    "Sacrifice this creature: Draw a card if this creature isn't prepared.";
const DOOM_BLADE_ORACLE: &str = "Destroy target creature.";
const CLOUDSHIFT_ORACLE: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";

const WITNESS: &str = "Prepared Condition Witness";

fn stack_entries_for(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
}

fn state_prepared(state: &GameState, object: ObjectId) -> bool {
    state
        .objects
        .get(&object)
        .is_some_and(|object| object.prepared.is_some())
}

fn hand_size(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|seated| seated.id == player)
        .expect("the player is seated")
        .hand
        .len()
}

fn draw_dispatches(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Draw,
                    source_id,
                    ..
                } if *source_id == source
            )
        })
        .count()
}

fn cards_drawn_for(events: &[GameEvent], player: PlayerId) -> usize {
    events
        .iter()
        .filter(
            |event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == player),
        )
        .count()
}

fn setup(oracle: &str) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let witness = scenario
        .add_creature(P0, WITNESS, 2, 2)
        .from_oracle_text(oracle)
        .id();
    // Removal + blink for the departure/blink twins below (mirrors the
    // Woodwork Prodigy suite's default zero-cost spell builders).
    let doom_blade = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", true, DOOM_BLADE_ORACLE)
        .id();
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT_ORACLE)
        .id();
    // CR 704.5b decking guard: resolving draws must have cards available.
    for index in 0..20 {
        scenario.add_card_to_library_top(P0, &format!("Prepared Witness Library P0 {index}"));
        scenario.add_card_to_library_top(P1, &format!("Prepared Witness Library P1 {index}"));
    }
    let mut runner = scenario.build();

    // The Biblioplex gate (`has_prepare_face`, CR 722.3a reminder) no-ops
    // `prepare_object` on creatures without a Prepare back face, so stage one
    // exactly like the Woodwork Prodigy suite.
    runner
        .state_mut()
        .objects
        .get_mut(&witness)
        .unwrap()
        .back_face = Some(BackFaceData {
        layout_kind: Some(LayoutKind::Prepare),
        ..BackFaceData::default()
    });

    // Parsed-control gate: the trailing suffix condition must be admitted
    // grammar AND must have parsed to the Prepared predicate — not residue.
    let parsed_json = serde_json::to_string(&runner.state().objects[&witness].trigger_definitions)
        .expect("the witness's trigger definitions must serialize");
    assert!(
        parsed_json.contains("Prepared"),
        "the trailing condition must parse to the Prepared predicate: {parsed_json}"
    );
    assert!(
        !parsed_json.contains("\"Unimplemented\""),
        "the witness ability tree must contain no Effect::Unimplemented: {parsed_json}"
    );
    (runner, witness, doom_blade, cloudshift)
}

fn prepare_witness(runner: &mut GameRunner, object: ObjectId) {
    let mut events = Vec::new();
    engine::game::effects::prepare::prepare_object(runner.state_mut(), object, &mut events);
    assert!(
        state_prepared(runner.state(), object),
        "staging must leave the object prepared"
    );
}

fn drive_until_stack_empty(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        let result = runner
            .act(GameAction::PassPriority)
            .expect("passing priority must be accepted while the ability waits");
        events.extend(result.events);
    }
    assert!(
        runner.state().stack.is_empty(),
        "the pending ability must fully resolve"
    );
    events
}

fn single_battlefield_witness(state: &GameState) -> ObjectId {
    let matches: Vec<ObjectId> = state
        .objects
        .values()
        .filter(|object| object.name == WITNESS && object.zone == Zone::Battlefield)
        .map(|object| object.id)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "exactly one witness must be on the battlefield"
    );
    matches[0]
}

/// Build the activation witness: one costed activated ability whose single
/// instruction is the conditional draw, a funded `{1}`, and removal + blink in
/// hand — the activated cohort of [`setup`].
fn setup_activation(oracle: &str) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The synthetic activation costs {1}; the scenario driver never auto-taps,
    // so fund the pool with one colorless unit (CR 602.2b payment).
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let witness = scenario
        .add_creature(P0, WITNESS, 2, 2)
        .from_oracle_text(oracle)
        .id();
    let doom_blade = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", true, DOOM_BLADE_ORACLE)
        .id();
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT_ORACLE)
        .id();
    // CR 704.5b decking guard: resolving draws must have cards available.
    for index in 0..20 {
        scenario.add_card_to_library_top(P0, &format!("Prepared Witness Library P0 {index}"));
        scenario.add_card_to_library_top(P1, &format!("Prepared Witness Library P1 {index}"));
    }
    let mut runner = scenario.build();

    // The Biblioplex gate (`has_prepare_face`, CR 722.3a reminder) no-ops
    // `prepare_object` on creatures without a Prepare back face, so stage one
    // exactly like the trigger cohort.
    runner
        .state_mut()
        .objects
        .get_mut(&witness)
        .unwrap()
        .back_face = Some(BackFaceData {
        layout_kind: Some(LayoutKind::Prepare),
        ..BackFaceData::default()
    });

    // Parsed-control gate: the trailing suffix condition must be admitted
    // grammar AND must have parsed to the Prepared predicate — not residue.
    assert_activation_parses_to_prepared(runner.state(), witness);
    (runner, witness, doom_blade, cloudshift)
}

/// Parsed-control gate shared by the activation setups: the trailing suffix
/// condition must be admitted grammar AND must have parsed to the Prepared
/// predicate — not residue.
fn assert_activation_parses_to_prepared(state: &GameState, witness: ObjectId) {
    let parsed_json = serde_json::to_string(&state.objects[&witness].abilities)
        .expect("the witness's abilities must serialize");
    assert!(
        parsed_json.contains("Prepared"),
        "the trailing condition must parse to the Prepared predicate: {parsed_json}"
    );
    assert!(
        !parsed_json.contains("\"Unimplemented\""),
        "the witness ability tree must contain no Effect::Unimplemented: {parsed_json}"
    );
}

/// Build the self-sacrifice activation witness — the cost-moves-the-source
/// cohort: the ability's own `Sacrifice this creature` cost advances the
/// storage incarnation before stack placement, so the announcement-captured
/// incarnation's exit last-known information is the only surviving record of
/// what the source was when the ability was activated. The witness stages the
/// Prepare back face so `prepare_witness` can mark it.
fn setup_self_sacrifice_activation(oracle: &str) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let witness = scenario
        .add_creature(P0, WITNESS, 2, 2)
        .from_oracle_text(oracle)
        .id();
    // CR 704.5b decking guard: resolving draws must have cards available.
    for index in 0..20 {
        scenario.add_card_to_library_top(P0, &format!("Prepared Witness Library P0 {index}"));
        scenario.add_card_to_library_top(P1, &format!("Prepared Witness Library P1 {index}"));
    }
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&witness)
        .unwrap()
        .back_face = Some(BackFaceData {
        layout_kind: Some(LayoutKind::Prepare),
        ..BackFaceData::default()
    });

    // Admission positively checked: the self-sacrifice cost and the trailing
    // prepared condition must both be admitted grammar, not residue.
    assert_activation_parses_to_prepared(runner.state(), witness);
    assert!(
        runner.state().objects[&witness]
            .abilities
            .iter()
            .any(|ability| ability.cost.is_some()),
        "the self-sacrifice activation must carry its printed cost"
    );
    (runner, witness)
}

fn activation_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("the witness has a costed activated ability")
}

/// Put the witness's synthetic activation on the stack through the production
/// announcement + payment pipeline (CR 602.2a/b) and stop at the
/// post-announcement priority window, so a test can respond before it resolves.
fn activate_onto_stack(runner: &mut GameRunner, source: ObjectId) {
    let index = activation_index(runner, source);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activation is accepted");
    for _ in 0..16 {
        match &runner.state().waiting_for {
            // CR 602.2b: finalize the {1} from the funded pool (mirrors the
            // scenario driver's own `ManaPayment` arm).
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing the activation's mana payment must be accepted");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while activating: {other:?}"),
        }
    }
    assert_eq!(
        stack_entries_for(runner, source),
        1,
        "the ability must wait on the stack, not resolve"
    );
}

/// Positive reach/dispatch control: a live prepared source satisfies the
/// suffix condition at resolution, so the draw dispatches exactly once.
#[test]
fn prepared_source_condition_dispatches_for_live_prepared_source() {
    let (mut runner, witness, _, _) = setup(PREPARED_DRAW_ORACLE);
    prepare_witness(&mut runner, witness);

    runner.advance_to_upkeep();
    assert_eq!(
        stack_entries_for(&runner, witness),
        1,
        "the upkeep trigger must wait on the stack"
    );

    let baseline = hand_size(runner.state(), P0);
    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(
        hand_size(runner.state(), P0),
        baseline + 1,
        "the conditional draw must grow the hand by exactly one"
    );
    assert_eq!(
        draw_dispatches(&events, witness),
        1,
        "the live prepared source must dispatch its draw"
    );
    assert_eq!(cards_drawn_for(&events, P0), 1, "exactly one card is drawn");
}

/// Paired suppression control: the same witness unprepared dispatches nothing,
/// so the positive control above cannot be vacuous.
#[test]
fn prepared_source_condition_suppresses_for_live_unprepared_source() {
    let (mut runner, witness, _, _) = setup(PREPARED_DRAW_ORACLE);

    runner.advance_to_upkeep();
    assert_eq!(
        stack_entries_for(&runner, witness),
        1,
        "the upkeep trigger must still be put on the stack — the condition gates the effect, not the trigger"
    );

    let baseline = hand_size(runner.state(), P0);
    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(
        hand_size(runner.state(), P0),
        baseline,
        "the suppressed draw must not change the hand"
    );
    assert_eq!(
        draw_dispatches(&events, witness),
        0,
        "an unprepared live source must not dispatch a draw"
    );
    assert_eq!(cards_drawn_for(&events, P0), 0, "no card may be drawn");
}

/// Negated polarity, positive control: "isn't prepared" dispatches for an
/// unprepared live source.
#[test]
fn negated_prepared_source_condition_dispatches_for_live_unprepared_source() {
    let (mut runner, witness, _, _) = setup(UNPREPARED_DRAW_ORACLE);

    runner.advance_to_upkeep();
    assert_eq!(stack_entries_for(&runner, witness), 1);

    let baseline = hand_size(runner.state(), P0);
    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(hand_size(runner.state(), P0), baseline + 1);
    assert_eq!(draw_dispatches(&events, witness), 1);
    assert_eq!(cards_drawn_for(&events, P0), 1);
}

/// Negated polarity, suppression control: "isn't prepared" suppresses for a
/// prepared live source.
#[test]
fn negated_prepared_source_condition_suppresses_for_live_prepared_source() {
    let (mut runner, witness, _, _) = setup(UNPREPARED_DRAW_ORACLE);
    prepare_witness(&mut runner, witness);

    runner.advance_to_upkeep();
    assert_eq!(stack_entries_for(&runner, witness), 1);

    let baseline = hand_size(runner.state(), P0);
    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(hand_size(runner.state(), P0), baseline);
    assert_eq!(draw_dispatches(&events, witness), 0);
    assert_eq!(cards_drawn_for(&events, P0), 0);
}

/// CR 608.2h + CR 722.3a: a designation gained before departure survives the
/// source's death through the exit last-known information. The live graveyard
/// card is unprepared, so a raw live read would suppress — the draw proves the
/// exit read.
#[test]
fn prepared_source_condition_reads_exit_state_after_prepared_departure() {
    let (mut runner, witness, doom_blade, _) = setup(PREPARED_DRAW_ORACLE);
    prepare_witness(&mut runner, witness);

    runner.advance_to_upkeep();
    assert_eq!(stack_entries_for(&runner, witness), 1);
    let baseline = hand_size(runner.state(), P0);

    let outcome = runner.cast(doom_blade).target_object(witness).resolve();

    outcome.assert_zone(&[witness], Zone::Graveyard);
    assert!(
        outcome.state().stack.is_empty(),
        "the upkeep trigger must fully resolve"
    );
    assert!(
        outcome.state().objects[&witness].prepared.is_none(),
        "the departed graveyard card's live designation is cleared — the exit snapshot is the only surviving record"
    );
    assert_eq!(
        hand_size(outcome.state(), P0),
        baseline,
        "the spell left the hand and the exit-prepared draw replaced it"
    );
    assert_eq!(
        draw_dispatches(outcome.events(), witness),
        1,
        "the exit-prepared source must still dispatch its draw"
    );
    assert_eq!(cards_drawn_for(outcome.events(), P0), 1);
}

/// Departure with an unprepared exit: the condition suppresses (paired with
/// the exit-prepared twin above).
#[test]
fn prepared_source_condition_suppresses_after_unprepared_departure() {
    let (mut runner, witness, doom_blade, _) = setup(PREPARED_DRAW_ORACLE);

    runner.advance_to_upkeep();
    assert_eq!(stack_entries_for(&runner, witness), 1);
    let baseline = hand_size(runner.state(), P0);

    let outcome = runner.cast(doom_blade).target_object(witness).resolve();

    outcome.assert_zone(&[witness], Zone::Graveyard);
    assert_eq!(
        hand_size(outcome.state(), P0),
        baseline - 1,
        "only the spell leaves the hand — the suppressed draw adds nothing"
    );
    assert_eq!(draw_dispatches(outcome.events(), witness), 0);
    assert_eq!(cards_drawn_for(outcome.events(), P0), 0);
}

/// Negated polarity across departure: "isn't prepared" must read the
/// exit-prepared state and suppress, even though the live graveyard card reads
/// unprepared — a raw live read would invert this verdict.
#[test]
fn negated_prepared_source_condition_reads_exit_state_after_prepared_departure() {
    let (mut runner, witness, doom_blade, _) = setup(UNPREPARED_DRAW_ORACLE);
    prepare_witness(&mut runner, witness);

    runner.advance_to_upkeep();
    assert_eq!(stack_entries_for(&runner, witness), 1);
    let baseline = hand_size(runner.state(), P0);

    let outcome = runner.cast(doom_blade).target_object(witness).resolve();

    outcome.assert_zone(&[witness], Zone::Graveyard);
    assert!(
        outcome.state().objects[&witness].prepared.is_none(),
        "the departed graveyard card's live designation is cleared"
    );
    assert_eq!(
        hand_size(outcome.state(), P0),
        baseline - 1,
        "the exit was prepared — 'isn't prepared' must suppress"
    );
    assert_eq!(draw_dispatches(outcome.events(), witness), 0);
    assert_eq!(cards_drawn_for(outcome.events(), P0), 0);
}

/// Negated polarity, departure-positive twin: an unprepared exit satisfies
/// "isn't prepared" and dispatches.
#[test]
fn negated_prepared_source_condition_dispatches_after_unprepared_departure() {
    let (mut runner, witness, doom_blade, _) = setup(UNPREPARED_DRAW_ORACLE);

    runner.advance_to_upkeep();
    assert_eq!(stack_entries_for(&runner, witness), 1);
    let baseline = hand_size(runner.state(), P0);

    let outcome = runner.cast(doom_blade).target_object(witness).resolve();

    outcome.assert_zone(&[witness], Zone::Graveyard);
    assert_eq!(
        hand_size(outcome.state(), P0),
        baseline,
        "the spell left the hand and the exit-unprepared draw replaced it"
    );
    assert_eq!(draw_dispatches(outcome.events(), witness), 1);
    assert_eq!(cards_drawn_for(outcome.events(), P0), 1);
}

/// CR 400.7 + CR 608.2h: a blink return is a new incarnation at the same
/// storage id (the pre-blink incarnation is a different `incarnation`). The
/// exit snapshot held the prepared designation; the returned object starts
/// unprepared — the draw proves the condition read the captured incarnation,
/// not the live replacement.
#[test]
fn prepared_source_condition_reads_exit_state_after_prepared_blink() {
    let (mut runner, witness, _, cloudshift) = setup(PREPARED_DRAW_ORACLE);
    prepare_witness(&mut runner, witness);

    runner.advance_to_upkeep();
    assert_eq!(stack_entries_for(&runner, witness), 1);
    let baseline = hand_size(runner.state(), P0);
    let incarnation_before = runner.state().objects[&witness].incarnation;

    let outcome = runner.cast(cloudshift).target_object(witness).resolve();

    let returned = single_battlefield_witness(outcome.state());
    assert_ne!(
        outcome.state().objects[&returned].incarnation,
        incarnation_before,
        "the blink must return a new incarnation"
    );
    assert!(
        !state_prepared(outcome.state(), returned),
        "the return starts unprepared while the exit was prepared — the discriminating divergence"
    );
    assert_eq!(
        hand_size(outcome.state(), P0),
        baseline,
        "the spell left the hand and the exit-prepared draw replaced it"
    );
    assert_eq!(
        draw_dispatches(outcome.events(), witness),
        1,
        "the exit-prepared incarnation must still dispatch its draw"
    );
    assert_eq!(cards_drawn_for(outcome.events(), P0), 1);
}

/// CR 400.7 + CR 608.2h, inverted divergence: the source departs unprepared
/// and the RETURNED incarnation is prepared before resolution resumes. The
/// condition must still read the exit absence, so the prepared live return
/// must not satisfy it.
#[test]
fn prepared_source_condition_blink_returned_prepared_still_suppresses() {
    let (mut runner, witness, _, cloudshift) = setup(PREPARED_DRAW_ORACLE);

    runner.advance_to_upkeep();
    assert_eq!(stack_entries_for(&runner, witness), 1);
    let baseline = hand_size(runner.state(), P0);

    // Commit and resolve only the blink; the upkeep trigger stays beneath it.
    runner.cast(cloudshift).target_object(witness).commit();
    runner.resolve_top();
    assert_eq!(
        stack_entries_for(&runner, cloudshift),
        0,
        "the blink must resolve first"
    );
    assert_eq!(
        stack_entries_for(&runner, witness),
        1,
        "the upkeep trigger must still wait"
    );

    let returned = single_battlefield_witness(runner.state());
    prepare_witness(&mut runner, returned);

    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(
        hand_size(runner.state(), P0),
        baseline - 1,
        "the exit state was unprepared — the prepared live return must not satisfy the condition"
    );
    assert_eq!(
        draw_dispatches(&events, witness),
        0,
        "the prepared returned incarnation must not rebind the condition"
    );
    assert_eq!(cards_drawn_for(&events, P0), 0);
    assert!(
        state_prepared(runner.state(), returned),
        "the return stays prepared — the suppression is the condition read, not an unprepare"
    );
}

/// Negated polarity across blink: "isn't prepared" must read the exit-prepared
/// state and suppress, even though the live returned incarnation reads
/// unprepared — a raw live read would invert this verdict.
#[test]
fn negated_prepared_source_condition_reads_exit_state_after_prepared_blink() {
    let (mut runner, witness, _, cloudshift) = setup(UNPREPARED_DRAW_ORACLE);
    prepare_witness(&mut runner, witness);

    runner.advance_to_upkeep();
    assert_eq!(stack_entries_for(&runner, witness), 1);
    let baseline = hand_size(runner.state(), P0);

    let outcome = runner.cast(cloudshift).target_object(witness).resolve();

    let returned = single_battlefield_witness(outcome.state());
    assert!(
        !state_prepared(outcome.state(), returned),
        "the return starts unprepared while the exit was prepared"
    );
    assert_eq!(
        hand_size(outcome.state(), P0),
        baseline - 1,
        "the exit was prepared — 'isn't prepared' must suppress despite the unprepared live return"
    );
    assert_eq!(draw_dispatches(outcome.events(), witness), 0);
    assert_eq!(cards_drawn_for(outcome.events(), P0), 0);
}

/// Negated polarity, blink-positive twin: the source departs unprepared and
/// the returned incarnation is prepared before resolution resumes. The exit
/// absence satisfies "isn't prepared", so the draw dispatches despite the
/// prepared live return.
#[test]
fn negated_prepared_source_condition_blink_returned_prepared_still_dispatches() {
    let (mut runner, witness, _, cloudshift) = setup(UNPREPARED_DRAW_ORACLE);

    runner.advance_to_upkeep();
    assert_eq!(stack_entries_for(&runner, witness), 1);
    let baseline = hand_size(runner.state(), P0);

    runner.cast(cloudshift).target_object(witness).commit();
    runner.resolve_top();
    assert_eq!(stack_entries_for(&runner, cloudshift), 0);
    assert_eq!(stack_entries_for(&runner, witness), 1);

    let returned = single_battlefield_witness(runner.state());
    prepare_witness(&mut runner, returned);

    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(
        hand_size(runner.state(), P0),
        baseline,
        "the spell left the hand and the exit-unprepared draw replaced it"
    );
    assert_eq!(
        draw_dispatches(&events, witness),
        1,
        "the exit-unprepared incarnation must dispatch despite the prepared live return"
    );
    assert_eq!(cards_drawn_for(&events, P0), 1);
    assert!(
        state_prepared(runner.state(), returned),
        "the return stays prepared throughout"
    );
}

// ---------------------------------------------------------------------------
// Activation cohort: the same condition consumed by an activated ability
// (`trigger_source = None`), reading the incarnation captured at the
// activation's announcement — before any cost is paid.
// ---------------------------------------------------------------------------

/// Activated reach/dispatch control: a live prepared source satisfies the
/// activated suffix condition at resolution.
#[test]
fn prepared_activation_dispatches_for_live_prepared_source() {
    let (mut runner, witness, _, _) = setup_activation(PREPARED_ACTIVATION_ORACLE);
    prepare_witness(&mut runner, witness);

    activate_onto_stack(&mut runner, witness);
    let baseline = hand_size(runner.state(), P0);
    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(
        hand_size(runner.state(), P0),
        baseline + 1,
        "the conditional draw must grow the hand by exactly one"
    );
    assert_eq!(
        draw_dispatches(&events, witness),
        1,
        "the live prepared source must dispatch its draw"
    );
    assert_eq!(cards_drawn_for(&events, P0), 1, "exactly one card is drawn");
}

/// Activated suppression control: the same witness unprepared dispatches
/// nothing, so the positive control above cannot be vacuous.
#[test]
fn prepared_activation_suppresses_for_live_unprepared_source() {
    let (mut runner, witness, _, _) = setup_activation(PREPARED_ACTIVATION_ORACLE);

    activate_onto_stack(&mut runner, witness);
    let baseline = hand_size(runner.state(), P0);
    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(
        hand_size(runner.state(), P0),
        baseline,
        "the suppressed draw must not change the hand"
    );
    assert_eq!(
        draw_dispatches(&events, witness),
        0,
        "an unprepared live source must not dispatch a draw"
    );
    assert_eq!(cards_drawn_for(&events, P0), 0, "no card may be drawn");
}

/// Negated polarity, activated positive control: "isn't prepared" dispatches
/// for an unprepared live source.
#[test]
fn negated_prepared_activation_dispatches_for_live_unprepared_source() {
    let (mut runner, witness, _, _) = setup_activation(UNPREPARED_ACTIVATION_ORACLE);

    activate_onto_stack(&mut runner, witness);
    let baseline = hand_size(runner.state(), P0);
    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(hand_size(runner.state(), P0), baseline + 1);
    assert_eq!(draw_dispatches(&events, witness), 1);
    assert_eq!(cards_drawn_for(&events, P0), 1);
}

/// Negated polarity, activated suppression control: "isn't prepared"
/// suppresses for a prepared live source.
#[test]
fn negated_prepared_activation_suppresses_for_live_prepared_source() {
    let (mut runner, witness, _, _) = setup_activation(UNPREPARED_ACTIVATION_ORACLE);
    prepare_witness(&mut runner, witness);

    activate_onto_stack(&mut runner, witness);
    let baseline = hand_size(runner.state(), P0);
    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(hand_size(runner.state(), P0), baseline);
    assert_eq!(draw_dispatches(&events, witness), 0);
    assert_eq!(cards_drawn_for(&events, P0), 0);
}

/// CR 608.2h + CR 113.7a + CR 722.3a: the activation captured the source
/// incarnation when it reached the stack; destroying the source in response
/// must still read that incarnation's exit last-known information. The live
/// graveyard card is unprepared, so a raw live read of `ability.source_id`
/// would suppress — the draw proves the exit read.
#[test]
fn prepared_activation_reads_exit_state_after_prepared_departure() {
    let (mut runner, witness, doom_blade, _) = setup_activation(PREPARED_ACTIVATION_ORACLE);
    prepare_witness(&mut runner, witness);

    activate_onto_stack(&mut runner, witness);
    let baseline = hand_size(runner.state(), P0);

    let outcome = runner.cast(doom_blade).target_object(witness).resolve();

    outcome.assert_zone(&[witness], Zone::Graveyard);
    assert!(
        outcome.state().stack.is_empty(),
        "the activation must fully resolve"
    );
    assert!(
        outcome.state().objects[&witness].prepared.is_none(),
        "the departed graveyard card's live designation is cleared — the exit snapshot is the only surviving record"
    );
    assert_eq!(
        hand_size(outcome.state(), P0),
        baseline,
        "the spell left the hand and the exit-prepared draw replaced it"
    );
    assert_eq!(
        draw_dispatches(outcome.events(), witness),
        1,
        "the exit-prepared source must still dispatch its draw"
    );
    assert_eq!(cards_drawn_for(outcome.events(), P0), 1);
}

/// Activated departure with an unprepared exit: the condition suppresses
/// (paired with the exit-prepared twin above).
#[test]
fn prepared_activation_suppresses_after_unprepared_departure() {
    let (mut runner, witness, doom_blade, _) = setup_activation(PREPARED_ACTIVATION_ORACLE);

    activate_onto_stack(&mut runner, witness);
    let baseline = hand_size(runner.state(), P0);

    let outcome = runner.cast(doom_blade).target_object(witness).resolve();

    outcome.assert_zone(&[witness], Zone::Graveyard);
    assert_eq!(
        hand_size(outcome.state(), P0),
        baseline - 1,
        "only the spell leaves the hand — the suppressed draw adds nothing"
    );
    assert_eq!(draw_dispatches(outcome.events(), witness), 0);
    assert_eq!(cards_drawn_for(outcome.events(), P0), 0);
}

/// Negated polarity across activated departure: "isn't prepared" must read the
/// exit-prepared state and suppress, even though the live graveyard card reads
/// unprepared — a raw live read would invert this verdict.
#[test]
fn negated_prepared_activation_reads_exit_state_after_prepared_departure() {
    let (mut runner, witness, doom_blade, _) = setup_activation(UNPREPARED_ACTIVATION_ORACLE);
    prepare_witness(&mut runner, witness);

    activate_onto_stack(&mut runner, witness);
    let baseline = hand_size(runner.state(), P0);

    let outcome = runner.cast(doom_blade).target_object(witness).resolve();

    outcome.assert_zone(&[witness], Zone::Graveyard);
    assert!(
        outcome.state().objects[&witness].prepared.is_none(),
        "the departed graveyard card's live designation is cleared"
    );
    assert_eq!(
        hand_size(outcome.state(), P0),
        baseline - 1,
        "the exit was prepared — 'isn't prepared' must suppress"
    );
    assert_eq!(draw_dispatches(outcome.events(), witness), 0);
    assert_eq!(cards_drawn_for(outcome.events(), P0), 0);
}

/// Negated polarity, activated departure-positive twin: an unprepared exit
/// satisfies "isn't prepared" and dispatches.
#[test]
fn negated_prepared_activation_dispatches_after_unprepared_departure() {
    let (mut runner, witness, doom_blade, _) = setup_activation(UNPREPARED_ACTIVATION_ORACLE);

    activate_onto_stack(&mut runner, witness);
    let baseline = hand_size(runner.state(), P0);

    let outcome = runner.cast(doom_blade).target_object(witness).resolve();

    outcome.assert_zone(&[witness], Zone::Graveyard);
    assert_eq!(
        hand_size(outcome.state(), P0),
        baseline,
        "the spell left the hand and the exit-unprepared draw replaced it"
    );
    assert_eq!(draw_dispatches(outcome.events(), witness), 1);
    assert_eq!(cards_drawn_for(outcome.events(), P0), 1);
}

/// CR 400.7 + CR 608.2h: blinking the activated source returns a new
/// incarnation at the same storage id while the exit snapshot held the
/// prepared designation. The returned object starts unprepared — the draw
/// proves the condition read the activation-captured incarnation, not the
/// live replacement.
#[test]
fn prepared_activation_reads_exit_state_after_prepared_blink() {
    let (mut runner, witness, _, cloudshift) = setup_activation(PREPARED_ACTIVATION_ORACLE);
    prepare_witness(&mut runner, witness);

    activate_onto_stack(&mut runner, witness);
    let baseline = hand_size(runner.state(), P0);
    let incarnation_before = runner.state().objects[&witness].incarnation;

    let outcome = runner.cast(cloudshift).target_object(witness).resolve();

    let returned = single_battlefield_witness(outcome.state());
    assert_ne!(
        outcome.state().objects[&returned].incarnation,
        incarnation_before,
        "the blink must return a new incarnation"
    );
    assert!(
        !state_prepared(outcome.state(), returned),
        "the return starts unprepared while the exit was prepared — the discriminating divergence"
    );
    assert_eq!(
        hand_size(outcome.state(), P0),
        baseline,
        "the spell left the hand and the exit-prepared draw replaced it"
    );
    assert_eq!(
        draw_dispatches(outcome.events(), witness),
        1,
        "the exit-prepared incarnation must still dispatch its draw"
    );
    assert_eq!(cards_drawn_for(outcome.events(), P0), 1);
}

/// CR 400.7 + CR 608.2h, inverted divergence for the activated consumer: the
/// source departs unprepared and the RETURNED incarnation is prepared before
/// the ability resolves. The condition must still read the exit absence, so
/// the prepared live return must not satisfy it.
#[test]
fn prepared_activation_blink_returned_prepared_still_suppresses() {
    let (mut runner, witness, _, cloudshift) = setup_activation(PREPARED_ACTIVATION_ORACLE);

    activate_onto_stack(&mut runner, witness);
    let baseline = hand_size(runner.state(), P0);

    // Commit and resolve only the blink; the activation stays beneath it.
    runner.cast(cloudshift).target_object(witness).commit();
    runner.resolve_top();
    assert_eq!(
        stack_entries_for(&runner, cloudshift),
        0,
        "the blink must resolve first"
    );
    assert_eq!(
        stack_entries_for(&runner, witness),
        1,
        "the activation must still wait"
    );

    let returned = single_battlefield_witness(runner.state());
    prepare_witness(&mut runner, returned);

    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(
        hand_size(runner.state(), P0),
        baseline - 1,
        "the exit state was unprepared — the prepared live return must not satisfy the condition"
    );
    assert_eq!(
        draw_dispatches(&events, witness),
        0,
        "the prepared returned incarnation must not rebind the condition"
    );
    assert_eq!(cards_drawn_for(&events, P0), 0);
    assert!(
        state_prepared(runner.state(), returned),
        "the return stays prepared — the suppression is the condition read, not an unprepare"
    );
}

/// Negated polarity across activated blink: "isn't prepared" must read the
/// exit-prepared state and suppress, even though the live returned incarnation
/// reads unprepared — a raw live read would invert this verdict.
#[test]
fn negated_prepared_activation_reads_exit_state_after_prepared_blink() {
    let (mut runner, witness, _, cloudshift) = setup_activation(UNPREPARED_ACTIVATION_ORACLE);
    prepare_witness(&mut runner, witness);

    activate_onto_stack(&mut runner, witness);
    let baseline = hand_size(runner.state(), P0);

    let outcome = runner.cast(cloudshift).target_object(witness).resolve();

    let returned = single_battlefield_witness(outcome.state());
    assert!(
        !state_prepared(outcome.state(), returned),
        "the return starts unprepared while the exit was prepared"
    );
    assert_eq!(
        hand_size(outcome.state(), P0),
        baseline - 1,
        "the exit was prepared — 'isn't prepared' must suppress despite the unprepared live return"
    );
    assert_eq!(draw_dispatches(outcome.events(), witness), 0);
    assert_eq!(cards_drawn_for(outcome.events(), P0), 0);
}

/// Negated polarity, activated blink-positive twin: the source departs
/// unprepared and the returned incarnation is prepared before the ability
/// resolves. The exit absence satisfies "isn't prepared", so the draw
/// dispatches despite the prepared live return.
#[test]
fn negated_prepared_activation_blink_returned_prepared_still_dispatches() {
    let (mut runner, witness, _, cloudshift) = setup_activation(UNPREPARED_ACTIVATION_ORACLE);

    activate_onto_stack(&mut runner, witness);
    let baseline = hand_size(runner.state(), P0);

    runner.cast(cloudshift).target_object(witness).commit();
    runner.resolve_top();
    assert_eq!(stack_entries_for(&runner, cloudshift), 0);
    assert_eq!(stack_entries_for(&runner, witness), 1);

    let returned = single_battlefield_witness(runner.state());
    prepare_witness(&mut runner, returned);

    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(
        hand_size(runner.state(), P0),
        baseline,
        "the spell left the hand and the exit-unprepared draw replaced it"
    );
    assert_eq!(
        draw_dispatches(&events, witness),
        1,
        "the exit-unprepared incarnation must dispatch despite the prepared live return"
    );
    assert_eq!(cards_drawn_for(&events, P0), 1);
    assert!(
        state_prepared(runner.state(), returned),
        "the return stays prepared throughout"
    );
}

// ---------------------------------------------------------------------------
// Self-sacrifice cohort: the activation's own cost moves the source, so the
// announced incarnation (before payment) is the epoch the condition gate must
// read — the exit LKI of the sacrificed permanent, never the graveyard object
// the placement stamp names in `source_incarnation`.
// ---------------------------------------------------------------------------

/// CR 602.2a + CR 113.7a + CR 608.2h: the activation's own cost sacrifices the
/// source, so the storage incarnation advances before stack placement. The
/// announced incarnation's exit last-known information must answer the
/// condition: the exit was prepared, so the draw dispatches even though the
/// live graveyard card reads unprepared — reading the placement epoch instead
/// would name the graveyard incarnation and suppress.
#[test]
fn prepared_activation_self_sacrifice_dispatches_after_prepared_exit() {
    let (mut runner, witness) =
        setup_self_sacrifice_activation(SELF_SACRIFICE_PREPARED_ACTIVATION_ORACLE);
    prepare_witness(&mut runner, witness);
    let incarnation_before = runner.state().objects[&witness].incarnation;

    activate_onto_stack(&mut runner, witness);

    assert_eq!(
        runner.state().objects[&witness].zone,
        Zone::Graveyard,
        "the self-sacrifice cost must have moved the source before placement"
    );
    assert_ne!(
        runner.state().objects[&witness].incarnation,
        incarnation_before,
        "the sacrifice advances the storage incarnation — a placement-time stamp would name this object"
    );
    assert!(
        runner.state().objects[&witness].prepared.is_none(),
        "the departed graveyard card's live designation is cleared — the exit snapshot is the only surviving record"
    );

    // Field split contract: the announcement epoch names the pre-cost object,
    // while the placement stamp names the post-cost object the
    // source-reconstruction consumers (copy sources, host references, craft
    // returns) read — and it must NOT be the epoch the condition gate reads.
    let placed = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == witness)
        .and_then(|entry| entry.ability())
        .expect("the activated ability must wait on the stack");
    assert_eq!(
        placed.announced_source_incarnation,
        Some(incarnation_before),
        "the announcement epoch must name the pre-cost object"
    );
    assert_eq!(
        placed.source_incarnation,
        Some(runner.state().objects[&witness].incarnation),
        "the placement epoch must name the sacrificed object the reconstruction consumers read"
    );

    let baseline = hand_size(runner.state(), P0);
    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(
        hand_size(runner.state(), P0),
        baseline + 1,
        "the announced incarnation's exit-prepared state must dispatch the draw"
    );
    assert_eq!(
        draw_dispatches(&events, witness),
        1,
        "the draw must dispatch from the announced source epoch"
    );
    assert_eq!(cards_drawn_for(&events, P0), 1);
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete into a Priority window"
    );
}

/// Paired control: an unprepared self-sacrifice exit suppresses the positive
/// draw — the announced incarnation's exit absence survives the cost.
#[test]
fn prepared_activation_self_sacrifice_suppresses_after_unprepared_exit() {
    let (mut runner, witness) =
        setup_self_sacrifice_activation(SELF_SACRIFICE_PREPARED_ACTIVATION_ORACLE);

    activate_onto_stack(&mut runner, witness);
    assert_eq!(
        runner.state().objects[&witness].zone,
        Zone::Graveyard,
        "the cost must have sacrificed the source"
    );

    let baseline = hand_size(runner.state(), P0);
    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(
        hand_size(runner.state(), P0),
        baseline,
        "the exit was unprepared — the suppressed draw adds nothing"
    );
    assert_eq!(draw_dispatches(&events, witness), 0);
    assert_eq!(cards_drawn_for(&events, P0), 0);
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete into a Priority window"
    );
}

/// Negated polarity across the self-sacrifice cost: "isn't prepared" must read
/// the announced incarnation's exit state and suppress, even though the live
/// graveyard card reads unprepared — a placement-time stamp would invert this
/// verdict.
#[test]
fn negated_prepared_activation_self_sacrifice_suppresses_after_prepared_exit() {
    let (mut runner, witness) =
        setup_self_sacrifice_activation(SELF_SACRIFICE_UNPREPARED_ACTIVATION_ORACLE);
    prepare_witness(&mut runner, witness);

    activate_onto_stack(&mut runner, witness);
    assert_eq!(
        runner.state().objects[&witness].zone,
        Zone::Graveyard,
        "the cost must have sacrificed the source"
    );
    assert!(
        runner.state().objects[&witness].prepared.is_none(),
        "the departed graveyard card's live designation is cleared"
    );

    let baseline = hand_size(runner.state(), P0);
    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(
        hand_size(runner.state(), P0),
        baseline,
        "the exit was prepared — 'isn't prepared' must suppress"
    );
    assert_eq!(draw_dispatches(&events, witness), 0);
    assert_eq!(cards_drawn_for(&events, P0), 0);
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete into a Priority window"
    );
}

/// Negated polarity, self-sacrifice positive twin: an unprepared exit satisfies
/// "isn't prepared" and dispatches.
#[test]
fn negated_prepared_activation_self_sacrifice_dispatches_after_unprepared_exit() {
    let (mut runner, witness) =
        setup_self_sacrifice_activation(SELF_SACRIFICE_UNPREPARED_ACTIVATION_ORACLE);

    activate_onto_stack(&mut runner, witness);
    assert_eq!(
        runner.state().objects[&witness].zone,
        Zone::Graveyard,
        "the cost must have sacrificed the source"
    );

    let baseline = hand_size(runner.state(), P0);
    let events = drive_until_stack_empty(&mut runner);

    assert_eq!(
        hand_size(runner.state(), P0),
        baseline + 1,
        "the exit was unprepared — the draw must dispatch"
    );
    assert_eq!(draw_dispatches(&events, witness), 1);
    assert_eq!(cards_drawn_for(&events, P0), 1);
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete into a Priority window"
    );
}

// ---------------------------------------------------------------------------
// Mana-ability cohort: CR 605.3b resolves a mana ability inline, without the
// stack, but its cost is still paid after the ability is announced. A
// self-sacrifice cost therefore advances the storage incarnation before the
// inline sub-chain resolves, so the source-subject condition gate must read
// the announcement-captured incarnation's exit last-known information — the
// same authority the stack path reads — never the live moved object.
// ---------------------------------------------------------------------------

const SELF_SACRIFICE_MANA_PREPARED_ORACLE: &str =
    "Sacrifice this creature: Add {G}. You gain 1 life if this creature is prepared.";
const SELF_SACRIFICE_MANA_UNPREPARED_ORACLE: &str =
    "Sacrifice this creature: Add {G}. You gain 1 life if this creature isn't prepared.";

fn life_total(state: &GameState, player: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|seated| seated.id == player)
        .expect("the player is seated")
        .life
}

fn gain_life_dispatches(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::GainLife,
                    source_id,
                    ..
                } if *source_id == source
            )
        })
        .count()
}

/// Activate the witness's mana ability through the production action route
/// (CR 605.3b: it resolves inline, so there is no stack window). The sacrifice
/// cost is the only cost, so no mana-payment prompt is expected.
fn activate_mana_ability_publicly(runner: &mut GameRunner, source: ObjectId) -> Vec<GameEvent> {
    let index = activation_index(runner, source);
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the mana activation is accepted");
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the inline mana resolution must complete into a Priority window"
    );
    result.events
}

/// Parsed public-activation control: a prepared self-sacrificing mana ability
/// gains 1 life from the announced incarnation's exit snapshot. A live read of
/// the post-cost graveyard object would see an unprepared card and suppress.
#[test]
fn prepared_mana_ability_self_sacrifice_dispatches_life_gain_from_announced_exit() {
    let (mut runner, witness) =
        setup_self_sacrifice_activation(SELF_SACRIFICE_MANA_PREPARED_ORACLE);
    prepare_witness(&mut runner, witness);
    let incarnation_before = runner.state().objects[&witness].incarnation;
    let life_before = life_total(runner.state(), P0);

    let events = activate_mana_ability_publicly(&mut runner, witness);

    assert_eq!(
        runner.state().objects[&witness].zone,
        Zone::Graveyard,
        "the self-sacrifice cost must have moved the source before the sub-chain"
    );
    assert_ne!(
        runner.state().objects[&witness].incarnation,
        incarnation_before,
        "the sacrifice advances the storage incarnation — a live read names the moved object"
    );
    assert!(
        runner.state().objects[&witness].prepared.is_none(),
        "the departed graveyard card's live designation is cleared — the exit snapshot is the only surviving record"
    );
    assert_eq!(
        life_total(runner.state(), P0),
        life_before + 1,
        "the announced incarnation's exit-prepared state must gain 1 life"
    );
    assert_eq!(
        gain_life_dispatches(&events, witness),
        1,
        "the life gain must dispatch from the announced source epoch"
    );
}

/// Paired suppression control: an unprepared self-sacrifice exit gains nothing,
/// so the positive control above cannot be vacuous.
#[test]
fn prepared_mana_ability_self_sacrifice_suppresses_life_gain_from_unprepared_exit() {
    let (mut runner, witness) =
        setup_self_sacrifice_activation(SELF_SACRIFICE_MANA_PREPARED_ORACLE);
    let life_before = life_total(runner.state(), P0);

    let events = activate_mana_ability_publicly(&mut runner, witness);

    assert_eq!(
        runner.state().objects[&witness].zone,
        Zone::Graveyard,
        "the cost must have sacrificed the source"
    );
    assert_eq!(
        life_total(runner.state(), P0),
        life_before,
        "the exit was unprepared — the suppressed life gain adds nothing"
    );
    assert_eq!(gain_life_dispatches(&events, witness), 0);
}

/// Negated polarity across the mana route: "isn't prepared" must read the
/// announced incarnation's exit-prepared state and suppress, even though the
/// live graveyard card reads unprepared — a live read would invert this verdict.
#[test]
fn negated_prepared_mana_ability_self_sacrifice_suppresses_life_gain_from_prepared_exit() {
    let (mut runner, witness) =
        setup_self_sacrifice_activation(SELF_SACRIFICE_MANA_UNPREPARED_ORACLE);
    prepare_witness(&mut runner, witness);
    let life_before = life_total(runner.state(), P0);

    let events = activate_mana_ability_publicly(&mut runner, witness);

    assert_eq!(
        runner.state().objects[&witness].zone,
        Zone::Graveyard,
        "the cost must have sacrificed the source"
    );
    assert!(
        runner.state().objects[&witness].prepared.is_none(),
        "the departed graveyard card's live designation is cleared"
    );
    assert_eq!(
        life_total(runner.state(), P0),
        life_before,
        "the exit was prepared — 'isn't prepared' must suppress despite the unprepared live card"
    );
    assert_eq!(gain_life_dispatches(&events, witness), 0);
}

/// Negated polarity, mana-route positive twin: an unprepared exit satisfies
/// "isn't prepared" and gains 1 life.
#[test]
fn negated_prepared_mana_ability_self_sacrifice_dispatches_life_gain_from_unprepared_exit() {
    let (mut runner, witness) =
        setup_self_sacrifice_activation(SELF_SACRIFICE_MANA_UNPREPARED_ORACLE);
    let life_before = life_total(runner.state(), P0);

    let events = activate_mana_ability_publicly(&mut runner, witness);

    assert_eq!(
        runner.state().objects[&witness].zone,
        Zone::Graveyard,
        "the cost must have sacrificed the source"
    );
    assert_eq!(
        life_total(runner.state(), P0),
        life_before + 1,
        "the exit was unprepared — the life gain must dispatch"
    );
    assert_eq!(gain_life_dispatches(&events, witness), 1);
}

// ---------------------------------------------------------------------------
// Mana-ability colour-choice cohort: "Add one mana of any color" (the printed
// grammar of Wild Cantor's "Sacrifice this creature: Add one mana of any
// color.") cannot resolve without a choice, so the sacrifice is paid, the
// activation parks on `WaitingFor::ChooseManaColor`, and production plus the
// inline sub-chain run only when the colour is answered (CR 605.3b + CR
// 106.1a). The announcement-captured incarnation must survive that pending
// continuation, including a persisted round-trip of the parked prompt, so the
// resumed sub-chain's source-subject condition still reads the exit last-known
// information (CR 400.7 + CR 602.2a + CR 608.2h + CR 113.7a) instead of the
// live graveyard card at the same storage id.
// ---------------------------------------------------------------------------

const SELF_SACRIFICE_ANY_COLOR_PREPARED_ORACLE: &str =
    "Sacrifice this creature: Add one mana of any color. You gain 1 life if this creature is prepared.";
const SELF_SACRIFICE_ANY_COLOR_UNPREPARED_ORACLE: &str =
    "Sacrifice this creature: Add one mana of any color. You gain 1 life if this creature isn't prepared.";

/// The engine's own save/undo/P2P restore pipeline (`PersistedGameState`), so
/// the parked colour choice's pending mana ability is carried exactly as a
/// resumed session would carry it.
fn round_trip(runner: &GameRunner) -> GameRunner {
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("state serializes");
    let state = serde_json::from_str::<PersistedGameState>(&json)
        .expect("persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("persisted state is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("restored state is publishable");
    GameRunner::from_state(state)
}

/// Life gained and `GainLife` dispatches for this witness once its colour
/// choice is answered.
struct ColorChoiceOutcome {
    life_delta: i32,
    gain_life_dispatches: usize,
}

/// Activate the any-colour witness publicly, require the post-sacrifice
/// colour prompt, round-trip the parked state, then answer the prompt through
/// `GameAction::ChooseManaColor` and require completion into Priority.
fn answer_color_choice_after_self_sacrifice(
    runner: GameRunner,
    witness: ObjectId,
) -> ColorChoiceOutcome {
    let mut runner = runner;
    let incarnation_before = runner.state().objects[&witness].incarnation;
    let life_before = life_total(runner.state(), P0);
    let index = activation_index(&runner, witness);
    runner
        .act(GameAction::ActivateAbility {
            source_id: witness,
            ability_index: index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the mana activation is accepted");
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::SingleColor { options },
                ..
            } if options.contains(&ManaType::Blue)
        ),
        "an any-colour mana ability must park on its colour choice: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&witness].zone,
        Zone::Graveyard,
        "the sacrifice cost is paid before the colour is chosen"
    );
    assert_ne!(
        runner.state().objects[&witness].incarnation,
        incarnation_before,
        "the sacrifice advances the storage incarnation — a live read names the moved object"
    );
    assert!(
        runner.state().objects[&witness].prepared.is_none(),
        "the departed graveyard card's live designation is cleared"
    );
    assert_eq!(
        life_total(runner.state(), P0),
        life_before,
        "nothing resolves before the colour is chosen"
    );

    let mut runner = round_trip(&runner);
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "the persisted round-trip keeps the parked colour choice"
    );
    let events = runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("the colour choice resumes the mana ability")
        .events;
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the resumed mana ability must complete into a Priority window"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Blue),
        1,
        "the chosen colour is produced on resume"
    );
    ColorChoiceOutcome {
        life_delta: life_total(runner.state(), P0) - life_before,
        gain_life_dispatches: gain_life_dispatches(&events, witness),
    }
}

/// Colour-choice discriminator: a prepared exit gains 1 life after the colour
/// is answered. A live read of the graveyard card at resume would suppress.
#[test]
fn prepared_any_color_mana_ability_dispatches_life_gain_from_announced_exit_after_color_choice() {
    let (mut runner, witness) =
        setup_self_sacrifice_activation(SELF_SACRIFICE_ANY_COLOR_PREPARED_ORACLE);
    prepare_witness(&mut runner, witness);

    let outcome = answer_color_choice_after_self_sacrifice(runner, witness);

    assert_eq!(
        outcome.life_delta, 1,
        "the announced incarnation's exit-prepared state must gain 1 life on resume"
    );
    assert_eq!(outcome.gain_life_dispatches, 1);
}

/// Paired suppression control: an unprepared exit gains nothing on resume, so
/// the positive case above cannot be vacuous.
#[test]
fn prepared_any_color_mana_ability_suppresses_life_gain_from_unprepared_exit_after_color_choice() {
    let (runner, witness) =
        setup_self_sacrifice_activation(SELF_SACRIFICE_ANY_COLOR_PREPARED_ORACLE);

    let outcome = answer_color_choice_after_self_sacrifice(runner, witness);

    assert_eq!(outcome.life_delta, 0, "the exit was unprepared");
    assert_eq!(outcome.gain_life_dispatches, 0);
}

/// Negated colour-choice discriminator: "isn't prepared" reads the prepared
/// exit and suppresses on resume. A live read of the unprepared graveyard card
/// would gain 1 life.
#[test]
fn negated_prepared_any_color_mana_ability_suppresses_life_gain_from_prepared_exit_after_color_choice(
) {
    let (mut runner, witness) =
        setup_self_sacrifice_activation(SELF_SACRIFICE_ANY_COLOR_UNPREPARED_ORACLE);
    prepare_witness(&mut runner, witness);

    let outcome = answer_color_choice_after_self_sacrifice(runner, witness);

    assert_eq!(
        outcome.life_delta, 0,
        "the exit was prepared — 'isn't prepared' must suppress despite the unprepared live card"
    );
    assert_eq!(outcome.gain_life_dispatches, 0);
}

/// Negated positive twin: an unprepared exit satisfies "isn't prepared" and
/// gains 1 life on resume.
#[test]
fn negated_prepared_any_color_mana_ability_dispatches_life_gain_from_unprepared_exit_after_color_choice(
) {
    let (runner, witness) =
        setup_self_sacrifice_activation(SELF_SACRIFICE_ANY_COLOR_UNPREPARED_ORACLE);

    let outcome = answer_color_choice_after_self_sacrifice(runner, witness);

    assert_eq!(outcome.life_delta, 1, "the exit was unprepared");
    assert_eq!(outcome.gain_life_dispatches, 1);
}
