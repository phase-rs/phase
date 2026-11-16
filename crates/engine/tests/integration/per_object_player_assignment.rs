//! CR 608.2c + CR 608.2d: a per-object player choice — "For each of those
//! creatures, choose a different opponent." over a population a reveal-until
//! producer published — records each answer for its own repetition's member
//! in the resolving ability's object→player assignment
//! (`SpellContext::object_player_assignment`), the record a later "the
//! permanent for which they were chosen" reads. The record travels with the
//! resolution across every pause and through a serde round trip, is fed only
//! by this ability's own per-object answers (never by a plain choice of the
//! same ability, nor by an entering permanent's "As ~ enters, choose a player"
//! answer — CR 614.12a + CR 607.2d), and an instruction following the
//! repetition runs exactly once, after the last answer (CR 608.2c).
//!
//! The instruments are SYNTHETIC texts built on Phase 2's reveal-until
//! instrument (`for_each_of_those_population.rs`), keeping three creature
//! cards for four players so each opponent can be chosen once. No printed
//! card carries the shape yet (measured census: zero `repeat_for`
//! TrackedSetSize nodes with a `Choose(Player | Opponent)` head); nothing
//! consumes the assignment yet.
//!
//! Derived reading (CR 608.2c, CR 608.2d, CR 609.3): for each kept creature,
//! in turn, the caster chooses an opponent not chosen for an earlier one; each
//! choice belongs to that creature; the following instructions run once,
//! after the last choice. With more creatures than opponents, a repetition
//! with no eligible opponent does nothing.

use crate::for_each_of_those_population::{
    assert_reveal_published_misses, iterated_members, muster_board, population_form,
    reveal_published_set, MusterBoard, MusterCard,
};
use engine::game::scenario::{GameRunner, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ChoiceType, ChosenAttribute, Effect, ObjectPlayerAssignment,
    PlayerChoiceDistinctness, QuantityExpr, QuantityRef, SubAbilityLink,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::resolution::{
    ResolutionFrame, ResolutionStateWire, RESOLUTION_STATE_WIRE_VERSION,
};
use engine::types::zones::Zone;

const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// True-Name Nemesis, verbatim Oracle text.
const TRUE_NAME_NEMESIS: &str =
    "As this creature enters, choose a player.\nThis creature has protection from the chosen player.";

/// The per-object sentence every instrument repeats over the kept creatures.
const PER_OBJECT: &str = "For each of those creatures, choose a different opponent.";

/// Phase 2's reveal-until producer wording (Dack Fayden, Helping Hand's own)
/// revealing until `count`, then the anaphoric haste grant.
fn producer(count: &str) -> String {
    format!(
        "Reveal cards from the top of your library until you reveal {count}. Put those \
         creature cards onto the battlefield, then shuffle. They gain haste until end of turn."
    )
}

/// I-A: the per-object choice followed by two plain opponent choices and a
/// life gain.
fn i_a_with(count: &str) -> String {
    format!(
        "{} {PER_OBJECT} Choose an opponent. Choose an opponent. You gain 1 life.",
        producer(count)
    )
}

fn i_a() -> String {
    i_a_with("three creature cards")
}

/// I-C: I-A over four kept creatures (one more than there are opponents).
fn i_c() -> String {
    i_a_with("four creature cards")
}

/// I-D: the per-object choice followed only by a life gain, whose single
/// application is observable on the board and depends on no trigger.
fn i_d() -> String {
    format!(
        "{} {PER_OBJECT} You gain 1 life.",
        producer("three creature cards")
    )
}

/// I-E: the per-object choice ends the text.
fn i_e() -> String {
    format!("{} {PER_OBJECT}", producer("three creature cards"))
}

/// Top to bottom: misses interleaved with Creatures A, B and C (kept by a
/// three-card reveal), Creature D (kept only by a four-card reveal), a miss.
const LIBRARY: &[MusterCard] = &[
    MusterCard::Sorcery("Miss One"),
    MusterCard::Creature("Creature A"),
    MusterCard::Land("Miss Two"),
    MusterCard::Creature("Creature B"),
    MusterCard::Creature("Creature C"),
    MusterCard::Creature("Creature D"),
    MusterCard::Sorcery("Miss Three"),
];

/// LIBRARY with True-Name Nemesis as the third creature card — revealed last
/// by a three-card reveal, so its as-enters choice is answered before the
/// first per-object prompt.
const LIBRARY_NEMESIS_LAST: &[MusterCard] = &[
    MusterCard::Sorcery("Miss One"),
    MusterCard::Creature("Creature A"),
    MusterCard::Land("Miss Two"),
    MusterCard::Creature("Creature B"),
    MusterCard::OracleCreature {
        name: "True-Name Nemesis",
        oracle: TRUE_NAME_NEMESIS,
        keywords: &[],
        power: 3,
        toughness: 1,
    },
    MusterCard::Creature("Creature D"),
    MusterCard::Sorcery("Miss Three"),
];

/// A library with no creature card at all.
const LIBRARY_NO_CREATURES: &[MusterCard] = &[
    MusterCard::Sorcery("Miss One"),
    MusterCard::Land("Miss Two"),
    MusterCard::Sorcery("Miss Three"),
];

fn lowered(oracle: &str) -> Vec<AbilityDefinition> {
    parse_oracle_text(oracle, "Reveal Muster", &[], &["Sorcery".to_string()], &[]).abilities
}

/// Parse reach-guard: the instrument's repeated node is
/// `Choose{Opponent{DistinctFromPriorChoices}}` repeated over the kept
/// permanents (the population form),
/// its sub (when `following`) a sequential sibling, and nothing is
/// unimplemented.
fn assert_per_object_shape(oracle: &str, following: bool) {
    let abilities = lowered(oracle);
    assert_eq!(abilities.len(), 1, "{oracle}");
    let mut cursor = Some(&abilities[0]);
    let mut repeated = None;
    while let Some(node) = cursor {
        assert!(
            !matches!(*node.effect, Effect::Unimplemented { .. }),
            "{oracle}: {:?}",
            node.effect
        );
        if node.repeat_for.is_some() {
            repeated = Some(node);
        }
        cursor = node.sub_ability.as_deref();
    }
    let node = repeated.unwrap_or_else(|| panic!("{oracle}: no repeated node"));
    assert_eq!(
        node.repeat_for,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: population_form(),
            }
        }),
        "{oracle}"
    );
    assert!(
        matches!(
            &*node.effect,
            Effect::Choose {
                choice_type: ChoiceType::Opponent {
                    restriction: None,
                    distinctness: PlayerChoiceDistinctness::DistinctFromPriorChoices,
                },
                ..
            }
        ),
        "{oracle}: {:?}",
        node.effect
    );
    assert_eq!(
        node.sub_ability
            .as_ref()
            .map(|sub| sub.sub_link == SubAbilityLink::SequentialSibling),
        following.then_some(true),
        "{oracle}: the following instruction is a sequential sibling"
    );
}

struct Prompt {
    choice_type: ChoiceType,
    options: Vec<String>,
}

fn prompt(runner: &GameRunner) -> Option<Prompt> {
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            ..
        } => {
            assert_eq!(*player, P0, "every prompt here is the caster's");
            Some(Prompt {
                choice_type: choice_type.clone(),
                options: options.clone(),
            })
        }
        _ => None,
    }
}

fn option_set(players: &[PlayerId]) -> Vec<String> {
    let mut set: Vec<String> = players.iter().map(|p| p.0.to_string()).collect();
    set.sort();
    set
}

fn sorted(mut options: Vec<String>) -> Vec<String> {
    options.sort();
    options
}

fn answer(runner: &mut GameRunner, player: PlayerId) {
    runner
        .act(GameAction::ChooseOption {
            choice: player.0.to_string(),
        })
        .expect("answering a legal player must succeed");
}

/// Reach-guard: the pending prompt is a per-object repetition's choice.
/// Returns its sorted options.
fn expect_per_object_prompt(runner: &GameRunner, ordinal: usize) -> Vec<String> {
    let p = prompt(runner).unwrap_or_else(|| {
        panic!(
            "per-object prompt {ordinal} must be pending, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        p.choice_type,
        ChoiceType::Opponent {
            restriction: None,
            distinctness: PlayerChoiceDistinctness::DistinctFromPriorChoices,
        },
        "prompt {ordinal} is the per-object choice"
    );
    sorted(p.options)
}

/// Reach-guard: the pending prompt is a plain "Choose an opponent." — never a
/// per-object prompt — offering every opponent.
fn expect_plain_prompt(runner: &GameRunner, ordinal: usize) {
    let p = prompt(runner).unwrap_or_else(|| {
        panic!(
            "plain prompt {ordinal} must be pending, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        p.choice_type,
        ChoiceType::Opponent {
            restriction: None,
            distinctness: PlayerChoiceDistinctness::Independent,
        },
        "plain prompt {ordinal}"
    );
    assert_eq!(sorted(p.options), option_set(&[P1, P2, P3]));
}

/// The population the repetition iterates, in iteration order (read once,
/// at the first per-object prompt): the repetition's member snapshot.
/// Reach-guard: the reveal's published set (located by content) holds every
/// member.
fn members(runner: &GameRunner) -> Vec<ObjectId> {
    let state = runner.state();
    let members = iterated_members(state);
    reveal_published_set(state, &members);
    members
}

/// CR 608.2c + CR 701.20a: the staged `misses` revealed before the last kept
/// creature card are in the reveal's published set, and not iterated.
fn assert_misses_published(board: &MusterBoard, population: &[ObjectId], misses: &[&str]) {
    let misses: Vec<ObjectId> = misses.iter().map(|name| board.card(name)).collect();
    assert_reveal_published_misses(board.runner.state(), population, &misses);
}

/// The assignment on the frame holding the pending answer: the parked repeat
/// template while repetitions remain, otherwise the parked continuation.
fn holder_record(state: &GameState) -> Vec<ObjectPlayerAssignment> {
    if let Some(repeat) = state.active_repeat_for() {
        return repeat.ability.context.object_player_assignment.clone();
    }
    state
        .active_ability_continuation()
        .expect("a holder frame is parked")
        .chain
        .context
        .object_player_assignment
        .clone()
}

/// The reference set on the parked following instruction.
fn continuation_reference_set(state: &GameState) -> Vec<PlayerId> {
    state
        .active_ability_continuation()
        .expect("the following instruction is parked")
        .chain
        .context
        .prior_player_choices
        .clone()
}

fn entry(object: ObjectId, player: PlayerId) -> ObjectPlayerAssignment {
    ObjectPlayerAssignment { object, player }
}

/// Reach-guard: the iterated population is exactly the `kept` cards, every
/// one on the battlefield.
fn assert_population(board: &MusterBoard, population: &[ObjectId], kept: &[&str]) {
    let mut expected: Vec<ObjectId> = kept.iter().map(|name| board.card(name)).collect();
    expected.sort();
    let mut actual = population.to_vec();
    actual.sort();
    assert_eq!(
        actual, expected,
        "the repetition iterates the kept creatures"
    );
    for id in population {
        assert_eq!(board.runner.state().objects[id].zone, Zone::Battlefield);
    }
}

/// V3a.1 (A3.6 assignment leg, interactive; E1′): each per-object answer is
/// recorded for its own repetition's member, and each later prompt excludes
/// the earlier picks (CR 608.2c + CR 608.2d).
#[test]
fn per_object_answers_are_recorded_for_their_own_members() {
    let oracle = i_a();
    assert_per_object_shape(&oracle, true);
    let mut board = muster_board(&oracle, LIBRARY);
    board.runner.cast(board.spell).resolve();

    assert_eq!(
        expect_per_object_prompt(&board.runner, 1),
        option_set(&[P1, P2, P3])
    );
    let m = members(&board.runner);
    assert_population(&board, &m, &["Creature A", "Creature B", "Creature C"]);
    assert_misses_published(&board, &m, &["Miss One", "Miss Two"]);
    for id in &m {
        assert!(
            board.runner.state().objects[id].has_keyword(&Keyword::Haste),
            "reach-guard: the grant before the repetition applied"
        );
    }
    answer(&mut board.runner, P2);

    assert_eq!(
        expect_per_object_prompt(&board.runner, 2),
        option_set(&[P1, P3]),
        "CR 608.2d: the second repetition excludes the first pick"
    );
    assert_eq!(holder_record(board.runner.state()), [entry(m[0], P2)]);
    assert!(
        board
            .runner
            .act(GameAction::ChooseOption {
                choice: P2.0.to_string(),
            })
            .is_err(),
        "CR 608.2d: re-submitting the earlier pick is an illegal option"
    );
    assert_eq!(
        expect_per_object_prompt(&board.runner, 2),
        option_set(&[P1, P3]),
        "the rejected answer leaves prompt 2 pending"
    );
    answer(&mut board.runner, P3);

    assert_eq!(
        expect_per_object_prompt(&board.runner, 3),
        option_set(&[P1])
    );
    assert_eq!(
        holder_record(board.runner.state()),
        [entry(m[0], P2), entry(m[1], P3)]
    );
    answer(&mut board.runner, P1);

    // Neither a miss nor the creature card beyond the count is ever keyed.
    expect_plain_prompt(&board.runner, 1);
    let record = holder_record(board.runner.state());
    for name in ["Miss One", "Miss Two", "Creature D", "Miss Three"] {
        let id = board.card(name);
        assert!(
            record.iter().all(|e| e.object != id),
            "{name} is not a member"
        );
    }
}

/// V3a.3 (C3.8 final repetition; the interface to the later handoff): the
/// last repetition's answer is recorded too, and the following instruction —
/// the record's later reader — holds every entry in member order when it
/// begins.
#[test]
fn following_instruction_receives_every_members_answer() {
    let oracle = i_a();
    assert_per_object_shape(&oracle, true);
    let mut board = muster_board(&oracle, LIBRARY);
    board.runner.cast(board.spell).resolve();
    expect_per_object_prompt(&board.runner, 1);
    let m = members(&board.runner);
    assert_misses_published(&board, &m, &["Miss One", "Miss Two"]);
    for (ordinal, pick) in [(1, P2), (2, P3), (3, P1)] {
        expect_per_object_prompt(&board.runner, ordinal);
        answer(&mut board.runner, pick);
    }

    // The first plain prompt was raised in the apply of the third answer.
    expect_plain_prompt(&board.runner, 1);
    assert!(
        board.runner.state().active_repeat_for().is_none(),
        "the repetition is complete"
    );
    assert_eq!(
        holder_record(board.runner.state()),
        [entry(m[0], P2), entry(m[1], P3), entry(m[2], P1)],
        "CR 608.2c: the following instruction reads every member's own answer"
    );
}

/// V3a.2 (C3.10 / A3.11, revert-failing — at PHASE_BASE the following life
/// gain ran after every per-object answer, +3 in total): an instruction
/// following a paused per-object repetition applies exactly once, after the
/// last answer, in the apply that records it (CR 608.2c).
#[test]
fn following_instruction_applies_once_after_the_last_answer() {
    let oracle = i_d();
    assert_per_object_shape(&oracle, true);
    let mut board = muster_board(&oracle, LIBRARY);
    let start = board.runner.life(P0);
    board.runner.cast(board.spell).resolve();
    for (ordinal, pick) in [(1, P1), (2, P2), (3, P3)] {
        expect_per_object_prompt(&board.runner, ordinal);
        assert_eq!(
            board.runner.life(P0),
            start,
            "CR 608.2c: the following instruction has not applied at prompt {ordinal}"
        );
        answer(&mut board.runner, pick);
    }
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "the third answer's apply ends the resolution, got {:?}",
        board.runner.state().waiting_for
    );
    assert_eq!(
        board.runner.life(P0),
        start + 1,
        "CR 608.2c: the following instruction applied exactly once"
    );
}

/// V3a.2b (CR 609.3; labelled regression guard): with no creature card
/// revealed there is no repetition, and the following instruction still
/// applies once.
#[test]
fn zero_members_run_the_following_instruction_once() {
    let oracle = i_d();
    let mut board = muster_board(&oracle, LIBRARY_NO_CREATURES);
    let start = board.runner.life(P0);
    board.runner.cast(board.spell).resolve();
    assert!(
        prompt(&board.runner).is_none(),
        "no member, no per-object prompt"
    );
    for name in ["Miss One", "Miss Two", "Miss Three"] {
        assert_eq!(
            board.runner.state().objects[&board.card(name)].zone,
            Zone::Library,
            "reach-guard: the reveal ran and left {name} in the library"
        );
    }
    assert_eq!(board.runner.life(P0), start + 1);
}

/// V3a.2c: a single member — the first pass's own final repetition pauses,
/// records its answer, and hands the following instructions the record; they
/// run once.
#[test]
fn single_member_repetition_records_and_runs_the_following_instructions_once() {
    let oracle = i_a_with("one creature card");
    assert_per_object_shape(&oracle, true);
    let mut board = muster_board(&oracle, LIBRARY);
    let start = board.runner.life(P0);
    board.runner.cast(board.spell).resolve();
    assert_eq!(
        expect_per_object_prompt(&board.runner, 1),
        option_set(&[P1, P2, P3])
    );
    let m = members(&board.runner);
    assert_population(&board, &m, &["Creature A"]);
    assert_misses_published(&board, &m, &["Miss One"]);
    answer(&mut board.runner, P3);

    expect_plain_prompt(&board.runner, 1);
    assert_eq!(holder_record(board.runner.state()), [entry(m[0], P3)]);
    answer(&mut board.runner, P1);
    expect_plain_prompt(&board.runner, 2);
    answer(&mut board.runner, P1);
    assert!(prompt(&board.runner).is_none(), "exactly two plain prompts");
    assert_eq!(board.runner.life(P0), start + 1);
}

/// V3a.4 (A3.6 hostile legs): an entering permanent's as-enters answer is not
/// recorded for any member (CR 614.12a + CR 607.2d), and a plain choice of
/// the same ability after the repetition is keyed to no member — the last
/// repetition's binding is consumed by its own answer.
#[test]
fn foreign_and_plain_answers_are_recorded_for_no_member() {
    let oracle = i_a();
    let mut board = muster_board(&oracle, LIBRARY_NEMESIS_LAST);
    let nemesis = board.card("True-Name Nemesis");
    board.runner.cast(board.spell).resolve();

    // The foreign prompt: Nemesis's "As this creature enters, choose a player".
    let foreign = prompt(&board.runner).expect("reach-guard: Nemesis's as-enters prompt");
    assert!(matches!(foreign.choice_type, ChoiceType::Player { .. }));
    answer(&mut board.runner, P1);
    assert!(
        board.runner.state().objects[&nemesis]
            .chosen_attributes
            .contains(&ChosenAttribute::Player(P1)),
        "reach-guard: Nemesis persisted its own chosen player"
    );

    assert_eq!(
        expect_per_object_prompt(&board.runner, 1),
        option_set(&[P1, P2, P3]),
        "CR 614.12a + CR 607.2d: Nemesis's P1 is not one of the spell's choices"
    );
    let m = members(&board.runner);
    assert_population(
        &board,
        &m,
        &["Creature A", "Creature B", "True-Name Nemesis"],
    );
    assert_misses_published(&board, &m, &["Miss One", "Miss Two"]);
    for (ordinal, pick) in [(1, P2), (2, P3), (3, P1)] {
        expect_per_object_prompt(&board.runner, ordinal);
        answer(&mut board.runner, pick);
    }

    expect_plain_prompt(&board.runner, 1);
    let per_object = [entry(m[0], P2), entry(m[1], P3), entry(m[2], P1)];
    assert_eq!(holder_record(board.runner.state()), per_object);
    answer(&mut board.runner, P3);

    expect_plain_prompt(&board.runner, 2);
    assert_eq!(
        holder_record(board.runner.state()),
        per_object,
        "the plain answer P3 is keyed to no member"
    );
    let mut reference = continuation_reference_set(board.runner.state());
    reference.sort();
    assert_eq!(
        reference,
        [P1, P2, P3],
        "the reference set holds only the spell's own picks"
    );
}

/// V3a.5 (CR 609.3 + consumption): with four members and three opponents the
/// fourth repetition has no eligible opponent and raises no prompt; the
/// following instructions run once, at once, and a later plain answer is
/// keyed to no member.
#[test]
fn impossible_final_repetition_records_nothing_and_runs_the_following_instructions() {
    let oracle = i_c();
    assert_per_object_shape(&oracle, true);
    let mut board = muster_board(&oracle, LIBRARY);
    board.runner.cast(board.spell).resolve();
    assert_eq!(
        expect_per_object_prompt(&board.runner, 1),
        option_set(&[P1, P2, P3])
    );
    let m = members(&board.runner);
    assert_population(
        &board,
        &m,
        &["Creature A", "Creature B", "Creature C", "Creature D"],
    );
    assert_misses_published(&board, &m, &["Miss One", "Miss Two"]);
    answer(&mut board.runner, P2);
    assert_eq!(
        expect_per_object_prompt(&board.runner, 2),
        option_set(&[P1, P3])
    );
    answer(&mut board.runner, P3);
    assert_eq!(
        expect_per_object_prompt(&board.runner, 3),
        option_set(&[P1])
    );
    answer(&mut board.runner, P1);

    // No fourth per-object prompt: the first plain prompt is pending.
    expect_plain_prompt(&board.runner, 1);
    let three = [entry(m[0], P2), entry(m[1], P3), entry(m[2], P1)];
    assert_eq!(holder_record(board.runner.state()), three);
    answer(&mut board.runner, P2);
    expect_plain_prompt(&board.runner, 2);
    assert_eq!(
        holder_record(board.runner.state()),
        three,
        "CR 609.3: the fourth member has no entry and the plain answer none"
    );
}

/// V3a.6 (A3.10 serialized-surface leg): a GameState serde round trip and a
/// resolution-state wire round trip, each taken while prompt 2 is pending
/// with one entry recorded, keep the record and the bound member; the answers
/// given afterwards are recorded for their own members beside it.
#[test]
fn assignment_survives_a_mid_resolution_serde_round_trip() {
    let oracle = i_a();
    for wire in [false, true] {
        let mut board = muster_board(&oracle, LIBRARY);
        board.runner.cast(board.spell).resolve();
        expect_per_object_prompt(&board.runner, 1);
        let m = members(&board.runner);
        assert_misses_published(&board, &m, &["Miss One", "Miss Two"]);
        answer(&mut board.runner, P2);
        expect_per_object_prompt(&board.runner, 2);

        let restored = if wire {
            let value = serde_json::to_value(ResolutionStateWire::from_game_state(
                board.runner.state().clone(),
            ))
            .expect("paused resolution serializes through the current wire");
            assert_eq!(
                value["resolution_state_version"],
                RESOLUTION_STATE_WIRE_VERSION
            );
            serde_json::from_value::<ResolutionStateWire>(value)
                .expect("paused resolution round-trips")
                .into_game_state()
        } else {
            let value = serde_json::to_value(board.runner.state()).expect("state serializes");
            serde_json::from_value::<GameState>(value).expect("state deserializes")
        };
        *board.runner.state_mut() = restored;

        let state = board.runner.state();
        assert!(
            matches!(
                state.resolution_stack.iter().last(),
                Some(ResolutionFrame::RepeatFor(_))
            ),
            "reach-guard: the repeat frame is still on top (wire: {wire})"
        );
        let template = &state.active_repeat_for().expect("repeat frame").ability;
        assert_eq!(
            template.context.object_player_assignment,
            [entry(m[0], P2)],
            "wire: {wire}"
        );
        assert_eq!(
            template.context.pending_choice_member,
            Some(m[1]),
            "wire: {wire}"
        );
        assert_eq!(
            expect_per_object_prompt(&board.runner, 2),
            option_set(&[P1, P3])
        );
        answer(&mut board.runner, P3);
        expect_per_object_prompt(&board.runner, 3);
        answer(&mut board.runner, P1);
        expect_plain_prompt(&board.runner, 1);
        assert_eq!(
            holder_record(board.runner.state()),
            [entry(m[0], P2), entry(m[1], P3), entry(m[2], P1)],
            "wire: {wire}"
        );
    }
}

/// V3a.7 integration sibling (labelled NON-discriminating: a new cast builds
/// a fresh resolving ability): a second resolution of the same spell starts
/// with an empty assignment.
#[test]
fn a_second_resolution_starts_with_an_empty_assignment() {
    let oracle = i_a();
    let mut board = muster_board(&oracle, LIBRARY);
    board.runner.cast(board.spell).resolve();
    for (ordinal, pick) in [(1, P2), (2, P3), (3, P1)] {
        expect_per_object_prompt(&board.runner, ordinal);
        answer(&mut board.runner, pick);
    }
    for ordinal in [1, 2] {
        expect_plain_prompt(&board.runner, ordinal);
        answer(&mut board.runner, P1);
    }
    assert!(prompt(&board.runner).is_none());

    // Return the resolved sorcery to hand and cast it again: Creature D is
    // the one creature card left, so the second repetition has one member.
    let spell = board.spell;
    {
        let state = board.runner.state_mut();
        let p0 = state.players.iter_mut().find(|p| p.id == P0).expect("P0");
        p0.graveyard.retain(|id| *id != spell);
        p0.hand.push_back(spell);
        state.objects.get_mut(&spell).expect("spell").zone = Zone::Hand;
    }
    board.runner.cast(spell).resolve();
    assert_eq!(
        expect_per_object_prompt(&board.runner, 1),
        option_set(&[P1, P2, P3]),
        "the first resolution's picks are not this resolution's"
    );
    let m = members(&board.runner);
    assert_eq!(m, [board.card("Creature D")]);
    answer(&mut board.runner, P3);
    expect_plain_prompt(&board.runner, 1);
    assert_eq!(
        holder_record(board.runner.state()),
        [entry(m[0], P3)],
        "the second resolution's record holds only its own answer"
    );
}

/// V3a.11 (revert-failing for the reference set — at PHASE_BASE a bare
/// repetition re-offered the first pick): a per-object repetition that ends
/// the chain records each answer on its parked template, so each prompt
/// excludes the earlier picks (CR 608.2d).
#[test]
fn bare_per_object_repetition_excludes_earlier_picks() {
    let oracle = i_e();
    assert_per_object_shape(&oracle, false);
    let mut board = muster_board(&oracle, LIBRARY);
    board.runner.cast(board.spell).resolve();
    assert_eq!(
        expect_per_object_prompt(&board.runner, 1),
        option_set(&[P1, P2, P3])
    );
    answer(&mut board.runner, P2);
    assert_eq!(
        expect_per_object_prompt(&board.runner, 2),
        option_set(&[P1, P3])
    );
    answer(&mut board.runner, P3);
    assert_eq!(
        expect_per_object_prompt(&board.runner, 3),
        option_set(&[P1])
    );
    answer(&mut board.runner, P1);
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "exactly three prompts"
    );
    assert!(board.runner.state().resolution_stack.is_empty());
}
