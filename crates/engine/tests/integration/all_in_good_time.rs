//! All in Good Time: the extra turn comes without a scheme chain.
//!
//! Oracle: "When you set this scheme in motion, take an extra turn after this
//! one. Schemes can't be set in motion that turn."
//!
//! CR 701.32a + CR 904.9: "that turn" is the extra turn granted by the first
//! sentence (the Final Fortune anaphora). CR 101.2: the prohibition skips the
//! precombat-main turn-based action entirely — the next scheme stays on top,
//! face down, and no trigger fires. CR 904.10 / CR 314.6: each resolved
//! non-ongoing scheme still reaches the bottom of the scheme deck via SBA.
//!
//! Revert-to-red: drop the `CantSetSchemeInMotion` gate — the extra turn sets
//! the second scheme in motion (skip assertions fail). Resolve "that turn" as
//! `UntilEndOfTurn` — the prohibition expires before the extra turn (same).
//! Break the duration arming — the second scheme never fires again (expiry
//! assertions fail).

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    ContinuousModification, Duration, Effect, QuantityExpr, ResolvedAbility, StaticDefinition,
    TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{ExtraTurnId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const ALL_IN_GOOD_TIME: &str = "When you set this scheme in motion, take an extra turn after this one. Schemes can't be set in motion that turn.";
const MY_LAUGHTER_ECHOES: &str = "(An ongoing scheme remains face up until it's abandoned.)\nWhenever you set a non-ongoing scheme in motion, you may abandon this scheme. If you do, set that scheme in motion again.";
const ROOTS_OF_ALL_EVIL: &str =
    "When you set this scheme in motion, create five 1/1 green Saproling creature tokens.";
const UGINS_NEXUS: &str = "If a player would begin an extra turn, that player skips that turn instead.\nIf Ugin's Nexus would be put into a graveyard from the battlefield, instead exile it and take an extra turn after this one.";

// No `P2` constant exists in `scenario.rs`; the third seat is `PlayerId(2)`.
const P2: PlayerId = PlayerId(2);

fn scheme_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Scheme".to_string()], &[]);
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "{name}: document is gapped"
    );
    let mut face = CardFace {
        name: name.to_string(),
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Scheme);
    synthesize_archenemy(&mut face);
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(gaps.is_empty(), "{name}: gaps: {gaps:?}");
    face
}

/// Resolve every pending trigger with no choices (extra turn + prohibition
/// install are both prompt-free).
fn drain_triggers(runner: &mut GameRunner) {
    for _ in 0..64 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner
                    .pass_holding_team()
                    .expect("holding team passes together");
            }
            other => panic!("unexpected prompt while draining triggers: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
}

/// Drive to `target`, declaring no attackers/blockers through combat and
/// passing priority. Panics on any other prompt.
fn drive_to_phase(runner: &mut GameRunner, target: Phase) {
    for _ in 0..64 {
        runner.advance_to_phase(target);
        if runner.state().phase == target {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("declare no blockers");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .pass_holding_team()
                    .expect("holding team passes together");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected prompt driving to {target:?}: {other:?}"),
        }
    }
    panic!("never reached {target:?}");
}

fn is_face_up_in_command(runner: &GameRunner, scheme: ObjectId) -> bool {
    runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down
}

#[test]
fn all_in_good_time_skips_the_extra_turn_scheme_and_refires_after() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    let mut runner = scenario.build();

    let face = scheme_face("All in Good Time", ALL_IN_GOOD_TIME);
    let first = create_scheme_deck_card(runner.state_mut(), &face, P0);
    let second = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    // Turn 1: the turn-based action sets the first copy in motion; its
    // trigger queues the extra turn and installs the prohibition.
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        is_face_up_in_command(&runner, first),
        "reach guard: turn 1 must set the first scheme in motion"
    );
    drain_triggers(&mut runner);
    assert_eq!(
        runner.state().extra_turns.len(),
        1,
        "the trigger must queue one extra turn"
    );
    assert_eq!(
        runner.state().extra_turns[0].player,
        P0,
        "the extra turn must belong to the archenemy"
    );
    // The resolved non-ongoing scheme reaches the bottom via SBA.
    assert_eq!(
        runner.state().scheme_deck.back(),
        Some(&first),
        "the resolved scheme must be on the bottom of the scheme deck"
    );

    // Extra turn: the prohibition skips the turn-based action — the second
    // copy stays on top, face down, and no trigger fires.
    drive_to_phase(&mut runner, Phase::End);
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert_eq!(
        runner.state().active_player,
        P0,
        "reach guard: the extra turn must belong to the archenemy"
    );
    assert_eq!(
        runner.state().scheme_deck.front(),
        Some(&second),
        "the prohibited turn must not move the next scheme off the top"
    );
    assert!(
        runner.state().objects[&second].face_down,
        "the prohibited turn must not turn the next scheme face up"
    );
    assert!(
        !runner.state().command_zone.contains(&second),
        "the prohibited turn must not promote the next scheme"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the prohibited turn must fire no scheme trigger"
    );

    // Turn 2 (after the intervening opponent turn): the window has expired,
    // so the second copy fires normally — proving both the skip above (same
    // machinery, unblocked) and the expiry.
    drive_to_phase(&mut runner, Phase::End);
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert_eq!(
        runner.state().active_player,
        P1,
        "reach guard: the opponent's turn must intervene"
    );
    drive_to_phase(&mut runner, Phase::End);
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert_eq!(
        runner.state().active_player,
        P0,
        "reach guard: back to the archenemy's turn"
    );
    assert!(
        is_face_up_in_command(&runner, second),
        "after expiry the turn-based action must fire again"
    );
    drain_triggers(&mut runner);
    assert_eq!(
        runner.state().extra_turns.len(),
        1,
        "the second copy must queue its own extra turn"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

fn ongoing_scheme_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Scheme".to_string()], &[]);
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "{name}: document is gapped"
    );
    let mut face = CardFace {
        name: name.to_string(),
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Scheme);
    face.card_type.supertypes.push(Supertype::Ongoing);
    synthesize_archenemy(&mut face);
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(gaps.is_empty(), "{name}: gaps: {gaps:?}");
    face
}

fn saprolings_controlled_by(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield
                && obj.controller == player
                && obj.name.contains("Saproling")
        })
        .count()
}

/// The bound turn of every installed granted-turn prohibition window.
fn granted_turn_bound_ids(runner: &GameRunner) -> Vec<ExtraTurnId> {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter_map(|tce| match tce.duration {
            Duration::UntilEndOfGrantedTurn { turn: Some(bound) } => Some(bound),
            _ => None,
        })
        .collect()
}

/// Drive to P0's next precombat main, across any number of intervening turns.
fn drive_to_next_p0_main(runner: &mut GameRunner) {
    for _ in 0..12 {
        drive_to_phase(runner, Phase::End);
        drive_to_phase(runner, Phase::PreCombatMain);
        if runner.state().phase == Phase::PreCombatMain && runner.state().active_player == P0 {
            return;
        }
    }
    panic!("never reached P0's next precombat main");
}

/// Drain scheme triggers, accepting Laughter's "you may abandon" every time it
/// is offered. Returns whether the decision was offered.
///
/// The resolution order is load-bearing: AIGT's trigger must resolve BEFORE
/// Laughter's accept re-fires it, so the re-fire is attempted with the first
/// prohibition already installed. The command-zone scan order
/// [Laughter, AIGT] plus the stable timestamp sort lists Laughter at index 0
/// of the 2-trigger prompt, and the identity drain then puts AIGT's trigger on
/// top — guarded below, so a scan-order change fails loudly instead of
/// silently un-discriminating the test.
fn drain_with_laughter_accept(runner: &mut GameRunner, laughter: ObjectId, aigt: ObjectId) -> bool {
    let mut saw_may = false;
    let mut guarded = false;
    for _ in 0..128 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                if !guarded && triggers.len() == 2 {
                    guarded = true;
                    assert_eq!(
                        triggers[0].source_id, laughter,
                        "the order prompt must list Laughter at index 0"
                    );
                    assert_eq!(
                        triggers[1].source_id, aigt,
                        "the order prompt must list AIGT at index 1 (on top after identity drain)"
                    );
                }
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(player, P0, "the archenemy decides the 'you may'");
                saw_may = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting the abandon must be accepted");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .pass_holding_team()
                    .expect("holding team passes together");
            }
            other => panic!("unexpected prompt while re-firing: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all scheme triggers must have resolved"
    );
    assert!(
        guarded,
        "the 2-trigger order prompt must have been observed and guarded"
    );
    saw_may
}

/// 3-seat archenemy game (P0 archenemy) with scheme deck
/// [Laughter, All in Good Time, Roots of All Evil] (front = top).
fn three_seat_archenemy_with_schemes() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Library {i}"));
    }
    let mut runner = scenario.build();
    let laughter_face = ongoing_scheme_face("My Laughter Echoes", MY_LAUGHTER_ECHOES);
    let aigt_face = scheme_face("All in Good Time", ALL_IN_GOOD_TIME);
    let roots_face = scheme_face("Roots of All Evil", ROOTS_OF_ALL_EVIL);
    let laughter = create_scheme_deck_card(runner.state_mut(), &laughter_face, P0);
    let aigt = create_scheme_deck_card(runner.state_mut(), &aigt_face, P0);
    let roots = create_scheme_deck_card(runner.state_mut(), &roots_face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;
    (runner, laughter, aigt, roots)
}

fn assert_granted_turn_scheme_skipped(runner: &GameRunner, roots: ObjectId) {
    assert_eq!(
        runner.state().active_player,
        P0,
        "reach guard: a P0 granted turn must be running"
    );
    assert_eq!(
        runner.state().scheme_deck.front(),
        Some(&roots),
        "the prohibited turn must not move the next scheme off the top"
    );
    assert!(
        runner.state().objects[&roots].face_down,
        "the prohibited turn must not turn the next scheme face up"
    );
    assert!(
        !runner.state().command_zone.contains(&roots),
        "the prohibited turn must not promote the next scheme"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the prohibited turn must fire no scheme trigger"
    );
}

/// F1-wit: All in Good Time's prohibition binds the granted turn only, so My
/// Laughter Echoes re-fires it during the ORIGINAL turn.
///
/// CR 611.2a: the "that turn" window is the granted turn — dormant while the
/// original turn is still running. CR 500.7: extra turns are LIFO, so the
/// re-fire's turn is taken first.
///
/// Revert-to-red: resolve "that turn" as the old player-untap window — the
/// prohibition is live on the original turn, the re-fire is suppressed, and
/// only one extra turn is queued.
#[test]
fn laughter_refires_during_original_turn_three_player() {
    let (mut runner, laughter, aigt, roots) = three_seat_archenemy_with_schemes();

    // Turn 1 (P0): Laughter is set in motion — ongoing, stays face up, and its
    // watch ignores its own ongoing self.
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        is_face_up_in_command(&runner, laughter),
        "reach guard: turn 1 must set Laughter in motion"
    );
    drain_triggers(&mut runner);

    // Turn 2 (P0): AIGT is set in motion with Laughter watching. AIGT's
    // trigger resolves first (guarded order), then the "you may" re-fires
    // AIGT during the ORIGINAL turn.
    drive_to_next_p0_main(&mut runner);
    assert!(
        is_face_up_in_command(&runner, aigt),
        "reach guard: turn 2 must set AIGT in motion"
    );
    assert!(
        drain_with_laughter_accept(&mut runner, laughter, aigt),
        "the 'you may abandon' decision must be offered"
    );
    assert_eq!(
        runner.state().extra_turns.len(),
        2,
        "the re-fire must queue a second extra turn"
    );
    let g_orig = runner.state().extra_turns[0].id;
    let g_refire = runner.state().extra_turns[1].id;
    assert_ne!(g_orig, g_refire, "each grant mints its own turn identity");
    let mut bound = granted_turn_bound_ids(&runner);
    bound.sort();
    let mut expected = vec![g_orig, g_refire];
    expected.sort();
    assert_eq!(
        bound, expected,
        "the original + re-fire prohibitions must bind g_orig + g_refire"
    );
    assert!(
        runner.state().objects[&laughter].face_down,
        "reach guard: accepting the 'you may' must abandon Laughter"
    );

    // Granted turn 1 is the RE-FIRE's (LIFO): g_orig's turn is still queued.
    drive_to_next_p0_main(&mut runner);
    assert_eq!(
        runner.state().extra_turns.len(),
        1,
        "LIFO: the re-fire's turn is taken first"
    );
    assert_eq!(
        runner.state().extra_turns[0].id,
        g_orig,
        "LIFO: g_orig's turn waits while g_refire's runs"
    );
    assert_granted_turn_scheme_skipped(&runner, roots);
    assert!(
        granted_turn_bound_ids(&runner).contains(&g_refire),
        "g_refire's window is live during its turn"
    );
    assert!(
        granted_turn_bound_ids(&runner).contains(&g_orig),
        "g_orig's window stays installed dormant during the other granted turn"
    );

    // Granted turn 2 is the ORIGINAL's: g_refire's window retired at its end.
    drive_to_next_p0_main(&mut runner);
    assert!(
        runner.state().extra_turns.is_empty(),
        "both granted turns are consumed"
    );
    assert!(
        !granted_turn_bound_ids(&runner).contains(&g_refire),
        "g_refire's window retires when its turn ends"
    );
    assert!(
        granted_turn_bound_ids(&runner).contains(&g_orig),
        "g_orig's window stays until its own turn ends"
    );
    assert_granted_turn_scheme_skipped(&runner, roots);

    // Natural turn: g_orig's window retired, so Roots fires (5 Saprolings).
    drive_to_next_p0_main(&mut runner);
    assert!(
        !granted_turn_bound_ids(&runner).contains(&g_orig),
        "g_orig's window retires when its turn ends"
    );
    assert!(
        is_face_up_in_command(&runner, roots),
        "after expiry the turn-based action must fire again"
    );
    drain_triggers(&mut runner);
    assert_eq!(
        saprolings_controlled_by(&runner, P0),
        5,
        "Roots must have fired exactly once on the natural turn"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

/// F1-skipF: a counter-skipped granted turn retires its prohibition without
/// ever activating it; the co-pending grant's window is unaffected.
///
/// CR 614.10a: a skipped turn is never begun. Final Fortune ruling #8: the
/// bound window of a skipped granted turn never activates.
///
/// Revert-to-red: the old player-untap window arms on the next P0 untap — the
/// natural turn's scheme action is skipped instead of firing.
#[test]
fn skipped_extra_turn_retires_prohibition() {
    let (mut runner, laughter, aigt, roots) = three_seat_archenemy_with_schemes();

    // Turn 1 + turn 2 through the Laughter re-fire: both grants co-pending.
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        is_face_up_in_command(&runner, laughter),
        "reach guard: turn 1 must set Laughter in motion"
    );
    drain_triggers(&mut runner);
    drive_to_next_p0_main(&mut runner);
    assert!(
        is_face_up_in_command(&runner, aigt),
        "reach guard: turn 2 must set AIGT in motion"
    );
    assert!(
        drain_with_laughter_accept(&mut runner, laughter, aigt),
        "the 'you may abandon' decision must be offered"
    );
    assert_eq!(
        runner.state().extra_turns.len(),
        2,
        "reach guard: both grants co-pending"
    );
    let g_orig = runner.state().extra_turns[0].id;
    let g_refire = runner.state().extra_turns[1].id;

    // Stage the counter skip — exactly what "skip your next turn" writes.
    runner.state_mut().turns_to_skip[P0.0 as usize] = 1;

    // The LIFO-first (re-fire's) turn is skipped: the next P0 main already
    // runs g_orig's turn, with g_refire's window retired and g_orig's live.
    drive_to_next_p0_main(&mut runner);
    assert!(
        runner.state().extra_turns.is_empty(),
        "the skipped turn never runs: no granted turns remain queued"
    );
    assert!(
        !granted_turn_bound_ids(&runner).contains(&g_refire),
        "the skipped turn's window retires without activating"
    );
    assert!(
        granted_turn_bound_ids(&runner).contains(&g_orig),
        "the unskipped grant's prohibition stays installed"
    );
    assert_granted_turn_scheme_skipped(&runner, roots);

    // Natural turn: g_orig's window retired, so Roots fires (5 Saprolings).
    drive_to_next_p0_main(&mut runner);
    assert!(
        !granted_turn_bound_ids(&runner).contains(&g_orig),
        "g_orig's window retires when its turn ends"
    );
    assert!(
        is_face_up_in_command(&runner, roots),
        "after expiry the turn-based action must fire again"
    );
    drain_triggers(&mut runner);
    assert_eq!(
        saprolings_controlled_by(&runner, P0),
        5,
        "Roots must have fired exactly once on the natural turn"
    );
}

/// F1-skipP: a pipeline-skipped granted turn (Ugin's Nexus) retires its
/// prohibition; the next natural turn fires normally.
///
/// The Nexus is driven through production — parsed Oracle on a battlefield
/// artifact, whose BeginTurn+OnlyExtraTurn replacement takes the `Prevented`
/// arm. CR 614.10a + Final Fortune ruling #8 as in F1-skipF.
///
/// Revert-to-red: the old player-untap window arms on the next P0 untap — the
/// natural turn's scheme action is skipped instead of firing.
#[test]
fn prevented_extra_turn_retires_prohibition() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    let nexus = scenario
        .add_artifact_from_oracle(P0, "Ugin's Nexus", UGINS_NEXUS)
        .id();
    let mut runner = scenario.build();

    let aigt_face = scheme_face("All in Good Time", ALL_IN_GOOD_TIME);
    let roots_face = scheme_face("Roots of All Evil", ROOTS_OF_ALL_EVIL);
    let aigt = create_scheme_deck_card(runner.state_mut(), &aigt_face, P0);
    let roots = create_scheme_deck_card(runner.state_mut(), &roots_face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    assert!(
        runner.state().objects[&nexus]
            .replacement_definitions
            .iter_all()
            .any(|def| def.event == ReplacementEvent::BeginTurn),
        "reach guard: the Nexus must carry the BeginTurn skip replacement"
    );

    // Turn 1: AIGT queues one extra turn + one prohibition.
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        is_face_up_in_command(&runner, aigt),
        "reach guard: turn 1 must set AIGT in motion"
    );
    drain_triggers(&mut runner);
    assert_eq!(
        runner.state().extra_turns.len(),
        1,
        "reach guard: one extra turn queued"
    );
    assert_eq!(
        granted_turn_bound_ids(&runner).len(),
        1,
        "reach guard: one prohibition installed"
    );

    // The granted turn is pipeline-skipped: the next P0 main is natural — the
    // prohibition retired and Roots fires (5 Saprolings).
    drive_to_next_p0_main(&mut runner);
    assert!(
        runner.state().extra_turns.is_empty(),
        "reach guard: the skipped extra turn never ran"
    );
    assert!(
        runner.state().transient_continuous_effects.is_empty(),
        "the skipped turn's prohibition retires"
    );
    assert!(
        is_face_up_in_command(&runner, roots),
        "after the skip the turn-based action must fire again"
    );
    drain_triggers(&mut runner);
    assert_eq!(
        saprolings_controlled_by(&runner, P0),
        5,
        "Roots must have fired exactly once on the natural turn"
    );
}

/// F1-bind0: the unbound "that turn" anaphor installs nothing without a
/// singular grant (fail-closed), and a singular grant binds exactly one.
///
/// CR 608.2c + CR 500.7: "that turn" names the extra turn this resolution
/// granted; with none (or several) granted, no such turn exists. Resolved via
/// `resolve_ability_chain` so the bind prelude runs on the production path.
#[test]
fn unbound_window_installs_nothing() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Silvercoat Lion", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;

    let prohibition = || {
        ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::CantSetSchemeInMotion)
                    .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Scheme)))
                    .modifications(vec![ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantSetSchemeInMotion,
                    }])],
                duration: Some(Duration::UntilEndOfGrantedTurn { turn: None }),
                target: None,
                end_cost: None,
            },
            vec![],
            source,
            P0,
        )
    };
    let grant = |count: i32| {
        ResolvedAbility::new(
            Effect::ExtraTurn {
                target: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: count },
            },
            vec![],
            source,
            P0,
        )
    };

    // Empty ledger: no grant → no prohibition installed.
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &prohibition(), &mut events, 0)
        .expect("the unbound chain must complete");
    assert!(
        runner.state().transient_continuous_effects.is_empty(),
        "no grant → no prohibition installed"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach guard: no prompt (a no-op, not an error)"
    );

    // Ambiguous ledger: two grants → nothing installed.
    let mut events = Vec::new();
    let ambiguous = grant(2).sub_ability(prohibition());
    resolve_ability_chain(runner.state_mut(), &ambiguous, &mut events, 0)
        .expect("the ambiguous chain must complete");
    assert_eq!(
        runner.state().extra_turns.len(),
        2,
        "reach guard: two turns granted"
    );
    assert!(
        runner.state().transient_continuous_effects.is_empty(),
        "ambiguous grant → no prohibition installed"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach guard: no prompt (a no-op, not an error)"
    );

    // Positive control: a singular grant binds and installs exactly one window.
    let mut events = Vec::new();
    let singular = grant(1).sub_ability(prohibition());
    resolve_ability_chain(runner.state_mut(), &singular, &mut events, 0)
        .expect("the singular chain must complete");
    assert_eq!(
        runner.state().extra_turns.len(),
        3,
        "reach guard: the singular grant queued"
    );
    let bound = granted_turn_bound_ids(&runner);
    assert_eq!(
        bound.len(),
        1,
        "a singular grant installs exactly one window"
    );
    assert_eq!(
        bound[0],
        runner.state().extra_turns[2].id,
        "the window binds the granted turn's identity"
    );
}
