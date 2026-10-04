//! CR 608.2c + CR 608.2d: the simultaneous handoff — "Each opponent gains
//! control of the permanent for which they were chosen." — as
//! `Effect::GiveControl` over the resolving ability's own object→player
//! assignment (`TargetFilter::ChoiceAssignment`): its target is the
//! assignment's objects, its recipient the player the assignment records for
//! the object being handed.
//!
//! Derived reading (CR 608.2c, CR 608.2d, CR 608.2e, CR 608.2f, CR 611.2a,
//! CR 613.1b, CR 603.2, CR 603.3, CR 603.10d + CR 603.3a, CR 800.4b, CR 609.3):
//! after the last per-object choice, ONE application gives every assigned
//! object to exactly the player chosen for it. The control changes are
//! processed together, last indefinitely and apply in layer 2; every
//! control-change event comes from that one application, so a trigger on one
//! of them is put on the stack when the resolution ends, and a "When you lose
//! control of ~" trigger is controlled by the player who controlled its source.
//! Before the last choice no object changes control.
//!
//! The chain is HAND-BUILT per charter A3.13 — the lowering is Phase 3c's: Phase
//! 3a's per-object choice instrument (I-D) is parsed, and the instruction
//! following its repetition (a sequential sibling, the link Phase 3a
//! established) has its effect replaced by the handoff. No printed card emits
//! the reference yet. Bucknard's Everfull Purse (verbatim Oracle) pins that an
//! existing recipient form resolves as before.

use crate::for_each_of_those_population::{
    finish_muster_library, stage_muster_library, MusterCard,
};
use engine::game::players::neighbor;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ChoiceAssignmentSide, ChoiceType, ContinuousModification, Duration, Effect,
    EffectKind, PlayerChoiceDistinctness, QuantityExpr, QuantityRef, SeatDirection, SubAbilityLink,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ActionResult, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// The per-object sentence Phase 3a's instruments repeat over the kept
/// creatures.
const PER_OBJECT: &str = "For each of those creatures, choose a different opponent.";

/// Khârn the Betrayer, verbatim Oracle text.
const KHARN: &str = "Berzerker — Khârn the Betrayer attacks or blocks each combat if able.\n\
                     Sigil of Corruption — When you lose control of Khârn the Betrayer, draw two \
                     cards.\nThe Betrayer — If damage would be dealt to Khârn the Betrayer, \
                     prevent that damage and an opponent of your choice gains control of it.";

/// Bucknard's Everfull Purse, verbatim Oracle text.
const BUCKNARDS_ORACLE: &str = "{1}, {T}: Roll a d4 and create a number of Treasure tokens equal to the result. The player to your right gains control of this artifact.";

/// Phase 2's reveal-until producer wording (Dack Fayden, Helping Hand's own)
/// revealing until `count`, then the anaphoric haste grant.
fn producer(count: &str) -> String {
    format!(
        "Reveal cards from the top of your library until you reveal {count}. Put those \
         creature cards onto the battlefield, then shuffle. They gain haste until end of turn."
    )
}

/// Phase 3a's I-D: the per-object choice followed by one instruction (a life
/// gain), whose effect the handoff replaces.
fn i_d(count: &str) -> String {
    format!("{} {PER_OBJECT} You gain 1 life.", producer(count))
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

/// LIBRARY with Khârn the Betrayer as the first creature card revealed.
const LIBRARY_KHARN_FIRST: &[MusterCard] = &[
    MusterCard::Sorcery("Miss One"),
    MusterCard::OracleCreature {
        name: "Khârn the Betrayer",
        oracle: KHARN,
        keywords: &[],
        power: 5,
        toughness: 1,
    },
    MusterCard::Land("Miss Two"),
    MusterCard::Creature("Creature B"),
    MusterCard::Creature("Creature C"),
    MusterCard::Creature("Creature D"),
    MusterCard::Sorcery("Miss Three"),
];

struct HandoffBoard {
    runner: GameRunner,
    spell: ObjectId,
    library: Vec<(&'static str, ObjectId)>,
    bystander_p1: ObjectId,
}

impl HandoffBoard {
    fn card(&self, name: &str) -> ObjectId {
        self.library
            .iter()
            .find(|(card, _)| *card == name)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no staged library card named {name}"))
    }
}

/// The handoff Phase 3c lowers to.
fn handoff_effect() -> Effect {
    Effect::GiveControl {
        target: TargetFilter::ChoiceAssignment {
            side: ChoiceAssignmentSide::Objects,
        },
        recipient: TargetFilter::ChoiceAssignment {
            side: ChoiceAssignmentSide::PlayerForObject,
        },
    }
}

/// Parses I-D over `count` and replaces the effect of the instruction
/// following the per-object repetition with the handoff (hand-built per
/// charter A3.13 — the lowering is Phase 3c's). The link stays the parsed
/// sequential sibling Phase 3a established, never a `ContinuationStep`
/// re-link into the repetition. Reach-guard: that instruction is the parsed
/// `GainLife` sequential sibling of the `TrackedSetSize` repetition.
fn handoff_definition(count: &str) -> AbilityDefinition {
    let oracle = i_d(count);
    let mut abilities = parse_oracle_text(
        &oracle,
        "Handoff Muster",
        &[],
        &["Sorcery".to_string()],
        &[],
    )
    .abilities;
    assert_eq!(abilities.len(), 1, "{oracle}");
    let mut def = abilities.remove(0);
    let mut cursor = &mut def;
    loop {
        if cursor.repeat_for
            == Some(QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize,
            })
        {
            break;
        }
        cursor = cursor
            .sub_ability
            .as_deref_mut()
            .unwrap_or_else(|| panic!("{oracle}: no TrackedSetSize repetition"));
    }
    assert!(
        matches!(
            &*cursor.effect,
            Effect::Choose {
                choice_type: ChoiceType::Opponent {
                    restriction: None,
                    distinctness: PlayerChoiceDistinctness::DistinctFromPriorChoices,
                },
                ..
            }
        ),
        "{oracle}: the repetition is the per-object choice"
    );
    let following = cursor
        .sub_ability
        .as_deref_mut()
        .expect("the repetition has a following instruction");
    assert_eq!(following.sub_link, SubAbilityLink::SequentialSibling);
    assert!(
        matches!(&*following.effect, Effect::GainLife { .. }),
        "the following instruction is I-D's life gain: {:?}",
        following.effect
    );
    *following.effect = handoff_effect();
    def
}

/// Four players; P0 holds the {0} handoff sorcery and a library staged from
/// `library_top_first`; P1 controls one bystander creature.
fn handoff_board(count: &str, library_top_first: &[MusterCard]) -> HandoffBoard {
    let def = handoff_definition(count);
    let mut scenario = GameScenario::new_n_player(4, 2026);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Handoff Muster", false)
        .with_mana_cost(ManaCost::generic(0))
        .with_ability_definition(def)
        .id();
    let library = stage_muster_library(&mut scenario, P0, library_top_first);
    let bystander_p1 = scenario.add_creature(P1, "Bystander One", 1, 1).id();
    let mut runner = scenario.build();
    finish_muster_library(&mut runner, P0, library_top_first, &library);
    HandoffBoard {
        runner,
        spell,
        library,
        bystander_p1,
    }
}

/// Reach-guard: the pending prompt is the caster's per-object choice. Returns
/// its sorted options.
fn expect_per_object_prompt(runner: &GameRunner, ordinal: usize) -> Vec<String> {
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            ..
        } => {
            assert_eq!(*player, P0, "prompt {ordinal} is the caster's");
            assert_eq!(
                *choice_type,
                ChoiceType::Opponent {
                    restriction: None,
                    distinctness: PlayerChoiceDistinctness::DistinctFromPriorChoices,
                },
                "prompt {ordinal} is the per-object choice"
            );
            let mut options = options.clone();
            options.sort();
            options
        }
        other => panic!("per-object prompt {ordinal} must be pending, got {other:?}"),
    }
}

fn option_set(players: &[PlayerId]) -> Vec<String> {
    let mut set: Vec<String> = players.iter().map(|p| p.0.to_string()).collect();
    set.sort();
    set
}

fn answer(runner: &mut GameRunner, player: PlayerId) -> ActionResult {
    runner
        .act(GameAction::ChooseOption {
            choice: player.0.to_string(),
        })
        .expect("answering a legal player must succeed")
}

/// The population the repetition iterates, in iteration order (read once, at
/// the first per-object prompt).
fn members(runner: &GameRunner) -> Vec<ObjectId> {
    let state = runner.state();
    let id = state
        .chain_tracked_set_id
        .expect("the reveal published its kept set");
    state.tracked_object_sets[&id].clone()
}

/// Reach-guard: the iterated population is exactly the `kept` cards, every one
/// on the battlefield.
fn assert_population(board: &HandoffBoard, population: &[ObjectId], kept: &[&str]) {
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

/// `(object, old, new)` of every control-change event.
fn control_changes(events: &[GameEvent]) -> Vec<(ObjectId, PlayerId, PlayerId)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ControllerChanged {
                object_id,
                old_controller,
                new_controller,
            } => Some((*object_id, *old_controller, *new_controller)),
            _ => None,
        })
        .collect()
}

/// The `EffectResolved{GiveControl}` events' sources.
fn handoff_resolutions(events: &[GameEvent]) -> Vec<ObjectId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::EffectResolved {
                kind: EffectKind::GiveControl,
                source_id,
                ..
            } => Some(*source_id),
            _ => None,
        })
        .collect()
}

fn controller(state: &GameState, object: ObjectId) -> PlayerId {
    state.objects[&object].controller
}

/// The durations of the control effects affecting `object`.
fn control_durations(state: &GameState, object: ObjectId) -> Vec<Duration> {
    state
        .transient_continuous_effects
        .iter()
        .filter(|tce| {
            tce.affected == TargetFilter::SpecificObject { id: object }
                && tce
                    .modifications
                    .contains(&ContinuousModification::ChangeController)
        })
        .map(|tce| tce.duration.clone())
        .collect()
}

/// V3b.9 (A3.13; C3.4(a)/(b) and C3.2's run-once link at building-block
/// level): answered interactively across pauses, no member changes control and
/// no control-change event is emitted at any prompt; after the last answer each
/// member is under exactly its chosen player, from one application in the final
/// apply (CR 608.2c, CR 608.2e + CR 608.2f, CR 611.2a, CR 613.1b).
#[test]
fn handoff_applies_once_after_the_last_per_object_answer() {
    let mut board = handoff_board("three creature cards", LIBRARY);
    let bystander_controller = controller(board.runner.state(), board.bystander_p1);
    let cast = board.runner.cast(board.spell).resolve();
    assert!(control_changes(cast.events()).is_empty());
    assert!(handoff_resolutions(cast.events()).is_empty());

    // Reach-guard (P3b.5): the cast declared no target for the handoff and
    // reached the first per-object prompt.
    assert_eq!(
        expect_per_object_prompt(&board.runner, 1),
        option_set(&[P1, P2, P3])
    );
    let m = members(&board.runner);
    assert_population(&board, &m, &["Creature A", "Creature B", "Creature C"]);

    let answers = [P2, P3, P1];
    let mut last = None;
    for (index, pick) in answers.into_iter().enumerate() {
        let ordinal = index + 1;
        let expected: Vec<PlayerId> = answers[index..].to_vec();
        assert_eq!(
            expect_per_object_prompt(&board.runner, ordinal),
            option_set(&expected),
            "prompt {ordinal} excludes the earlier picks"
        );
        for id in &m {
            assert_eq!(
                controller(board.runner.state(), *id),
                P0,
                "CR 608.2c: no member changes control before the last answer (prompt {ordinal})"
            );
        }
        let result = answer(&mut board.runner, pick);
        if ordinal < answers.len() {
            assert!(
                control_changes(&result.events).is_empty(),
                "no control-change event in answer {ordinal}'s apply"
            );
            assert!(handoff_resolutions(&result.events).is_empty());
        } else {
            last = Some(result);
        }
    }

    let last = last.expect("the third answer's result");
    assert!(
        matches!(last.waiting_for, WaitingFor::Priority { .. }),
        "the third answer's apply ends the resolution, got {:?}",
        last.waiting_for
    );
    assert_eq!(
        control_changes(&last.events),
        [(m[0], P0, P2), (m[1], P0, P3), (m[2], P0, P1)],
        "every control-change event comes from the final apply"
    );
    assert_eq!(
        handoff_resolutions(&last.events),
        [board.spell],
        "one application"
    );
    let state = board.runner.state();
    for (id, pick) in m.iter().zip(answers) {
        assert_eq!(controller(state, *id), pick);
        assert_eq!(control_durations(state, *id), [Duration::Permanent]);
    }
    for name in ["Creature D", "Miss One", "Miss Two", "Miss Three"] {
        assert_eq!(
            state.objects[&board.card(name)].zone,
            Zone::Library,
            "{name} is never handed"
        );
    }
    assert_eq!(
        controller(state, board.bystander_p1),
        bystander_controller,
        "the bystander is untouched"
    );
    assert!(
        control_durations(state, board.bystander_p1).is_empty(),
        "no control effect on the bystander"
    );
}

/// V3b.10 (A3.13(b); building block of C3.4(b)): the handoff's control-change
/// events come from the final apply, so Khârn the Betrayer's "When you lose
/// control of ~" triggers once, is put on the stack when the resolution ends
/// (CR 603.2 + CR 603.3), and is controlled by the player who controlled
/// Khârn (CR 603.10d + CR 603.3a).
#[test]
fn lose_control_trigger_is_placed_after_the_handoff_for_its_old_controller() {
    let mut board = handoff_board("three creature cards", LIBRARY_KHARN_FIRST);
    let kharn = board.card("Khârn the Betrayer");
    board.runner.cast(board.spell).resolve();
    expect_per_object_prompt(&board.runner, 1);
    let m = members(&board.runner);
    assert_population(
        &board,
        &m,
        &["Khârn the Betrayer", "Creature B", "Creature C"],
    );
    assert_eq!(m[0], kharn, "Khârn is the first member, handed first");
    let hand_before = board.runner.state().players[0].hand.len();

    for (ordinal, pick) in [(1, P2), (2, P3), (3, P1)] {
        expect_per_object_prompt(&board.runner, ordinal);
        if ordinal > 1 {
            let state = board.runner.state();
            assert_eq!(
                controller(state, kharn),
                P0,
                "reach-guard: Khârn is still P0's at prompt {ordinal}"
            );
            assert!(
                state.stack.iter().all(|entry| entry.source_id != kharn),
                "no Khârn trigger is pending at prompt {ordinal}"
            );
            assert_eq!(state.players[0].hand.len(), hand_before);
        }
        answer(&mut board.runner, pick);
    }

    let state = board.runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(controller(state, kharn), P2);
    let top = state.stack.last().expect("Khârn's trigger is on the stack");
    assert_eq!(
        top.source_id, kharn,
        "the stack's top entry is Khârn's trigger"
    );
    assert_eq!(
        top.controller, P0,
        "CR 603.10d + CR 603.3a: the trigger is controlled by Khârn's old controller"
    );

    board.runner.resolve_top();
    assert_eq!(
        board.runner.state().players[0].hand.len(),
        hand_before + 2,
        "Sigil of Corruption: P0 draws two cards"
    );
}

/// V3b.11 (hostile — a member with no entry; CR 609.3): four members and three
/// opponents. The fourth repetition has no eligible opponent, so the fourth
/// member is not an object of the assignment and stays with its controller.
#[test]
fn a_member_with_no_chosen_player_is_not_handed() {
    let mut board = handoff_board("four creature cards", LIBRARY);
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
    let last = answer(&mut board.runner, P1);

    assert!(
        matches!(last.waiting_for, WaitingFor::Priority { .. }),
        "no fourth prompt, got {:?}",
        last.waiting_for
    );
    assert_eq!(
        control_changes(&last.events),
        [(m[0], P0, P2), (m[1], P0, P3), (m[2], P0, P1)]
    );
    assert_eq!(handoff_resolutions(&last.events), [board.spell]);
    let state = board.runner.state();
    assert_eq!(controller(state, m[0]), P2);
    assert_eq!(controller(state, m[1]), P3);
    assert_eq!(controller(state, m[2]), P1);
    assert_eq!(controller(state, m[3]), P0);
    assert!(
        control_durations(state, m[3]).is_empty(),
        "no control effect for the member with no entry"
    );
}

fn floating_colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn treasures_controlled_by(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| {
            o.card_types.subtypes.contains(&"Treasure".to_string()) && o.controller == player
        })
        .count()
}

/// V3b.12 (A3.15 preservation; verbatim Oracle): Bucknard's Everfull Purse's
/// existing recipient form (`Neighbor{Right}`, `SelfRef` target) resolves as
/// before. Derived from its text: no "target", so no target slot; P0 creates
/// as many Treasures as the roll's result (CR 706.2) and keeps them; the player
/// to P0's right — the previous player in clockwise turn order (CR 103.1 +
/// CR 101.4), P3 — alone gains control of the Purse, indefinitely (CR 611.2a),
/// in layer 2 (CR 613.1b), with exactly one control-change event (CR 110.2).
#[test]
fn bucknards_everfull_purse_passes_to_the_right_through_the_restructured_authority() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let purse = scenario
        .add_creature(P0, "Bucknard's Everfull Purse", 0, 0)
        .as_artifact()
        .from_oracle_text(BUCKNARDS_ORACLE)
        .id();
    let other = scenario.add_creature(P0, "Second Permanent", 1, 1).id();
    scenario.with_mana_pool(P0, floating_colorless(1));
    let mut runner = scenario.build();
    assert_eq!(runner.state().seat_order, [P0, P1, P2, P3]);
    assert_eq!(neighbor(runner.state(), P0, SeatDirection::Right), P3);
    assert_eq!(controller(runner.state(), purse), P0);

    // Announce step by step through `apply`, so a target slot would be seen.
    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: purse,
            ability_index: 0,
        })
        .expect("the Purse's ability activates")
        .events;
    loop {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => events.extend(
                runner
                    .act(GameAction::PassPriority)
                    .expect("the floating mana pays {1}")
                    .events,
            ),
            WaitingFor::Priority { .. } => break,
            other => panic!("no target slot or other prompt is raised, got {other:?}"),
        }
    }
    assert_eq!(runner.state().stack.len(), 1, "the ability is on the stack");
    while !runner.state().stack.is_empty() {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority resolves the ability")
                .events,
        );
    }

    let roll = events
        .iter()
        .find_map(|event| match event {
            GameEvent::DieRolled {
                sides: 4, result, ..
            } => result.map(usize::from),
            _ => None,
        })
        .expect("reach-guard: the ability rolled a d4");
    let state = runner.state();
    assert_eq!(controller(state, purse), P3, "the player to P0's right");
    assert_eq!(control_changes(&events), [(purse, P0, P3)]);
    assert_eq!(control_durations(state, purse), [Duration::Permanent]);
    assert_eq!(treasures_controlled_by(state, P0), roll);
    for player in [P1, P2, P3] {
        assert_eq!(treasures_controlled_by(state, player), 0);
    }
    assert_eq!(controller(state, other), P0, "only the Purse is handed");
    assert!(control_durations(state, other).is_empty());
}
