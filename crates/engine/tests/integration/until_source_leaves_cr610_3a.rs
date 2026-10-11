//! CR 610.3 + CR 610.3a + CR 610.3b regressions for an "until" exile whose
//! specified event occurs while the ability that creates the initial exile is
//! held on the stack.
//!
//! CR 610.3a: if a resolving activated ability creates the initial one-shot
//! effect, and the specified event occurred after that ability was put onto
//! the stack, the object doesn't move. CR 602.2a puts an activated ability on
//! the stack before its costs are paid, so any departure of its source while
//! it waits counts. CR 610.3b is the triggered-ability twin.

use std::collections::VecDeque;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Duration, EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ExileLinkKind, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ANGEL_OF_CONDEMNATION: &str = "Flying, vigilance\n{2}{W}, {T}: Exile another target creature. Return that card to the battlefield under its owner's control at the beginning of the next end step.\n{2}{W}, {T}, Exert this creature: Exile another target creature until this creature leaves the battlefield. (An exerted creature won't untap during your next untap step.)";
const BANISHER_PRIEST: &str = "When this creature enters, exile target creature an opponent controls until this creature leaves the battlefield.";
const GHOSTLY_FLICKER: &str = "Exile two target artifacts, creatures, and/or lands you control, then return those cards to the battlefield under your control.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";

/// Passes priority (answering each target prompt from `targets`, in order)
/// until `stop` holds, and returns every event emitted on the way.
fn drive(
    runner: &mut GameRunner,
    stop: impl Fn(&GameState) -> bool,
    mut targets: VecDeque<ObjectId>,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..64 {
        if stop(runner.state()) {
            return events;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                GameAction::PassPriority
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(
                        targets
                            .pop_front()
                            .expect("a declared target for this prompt"),
                    )),
                }
            }
            other => panic!("unexpected prompt: {other:?}"),
        };
        events.extend(
            runner
                .act(action)
                .expect("the engine accepts the action")
                .events,
        );
    }
    panic!(
        "stop condition never reached; waiting for {:?}",
        runner.state().waiting_for
    );
}

/// P0 holds priority with exactly `len` objects on the stack.
fn priority_with_stack(len: usize) -> impl Fn(&GameState) -> bool {
    move |state: &GameState| {
        matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.len() == len
    }
}

fn until_links(state: &GameState) -> usize {
    state
        .exile_links
        .iter()
        .filter(|link| matches!(link.kind, ExileLinkKind::UntilSourceLeaves { .. }))
        .count()
}

/// Whether the held ability's exile node resolved for `source` (it was not
/// countered for an illegal target).
fn exile_node_resolved(events: &[GameEvent], source: ObjectId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved { kind: EffectKind::ChangeZone, source_id, .. }
                if *source_id == source
        )
    })
}

struct AngelBoard {
    runner: GameRunner,
    angel: ObjectId,
    land: ObjectId,
    c1: ObjectId,
    flicker: ObjectId,
    unsummon: ObjectId,
    unsummon_two: ObjectId,
}

/// Angel of Condemnation with `{2}{W}` floating, a land to pair with it for
/// Ghostly Flicker, and an opponent's creature C1. The responses cost zero.
fn angel_board() -> AngelBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let angel = scenario
        .add_creature_from_oracle(P0, "Angel of Condemnation", 3, 3, ANGEL_OF_CONDEMNATION)
        .id();
    let land = scenario.add_land_from_oracle(P0, "Blank Land", "").id();
    let c1 = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let flicker = scenario
        .add_spell_to_hand_from_oracle(P0, "Ghostly Flicker", true, GHOSTLY_FLICKER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let unsummon_two = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_mana_pool(
        P0,
        [ManaType::White, ManaType::Colorless, ManaType::Colorless]
            .into_iter()
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    AngelBoard {
        runner: scenario.build(),
        angel,
        land,
        c1,
        flicker,
        unsummon,
        unsummon_two,
    }
}

/// Activates Angel's exert ability targeting C1 and leaves it waiting on the
/// stack with P0 holding priority.
fn activate_exert_exile(board: &mut AngelBoard) {
    let ability_index = board.runner.state().objects[&board.angel]
        .abilities
        .iter()
        .position(|ability| ability.duration == Some(Duration::UntilHostLeavesPlay))
        .expect("the exert ability's exile is bounded by Angel leaving");
    board
        .runner
        .act(GameAction::ActivateAbility {
            source_id: board.angel,
            ability_index,
        })
        .expect("Angel's exert ability can be activated");
    drive(
        &mut board.runner,
        priority_with_stack(1),
        VecDeque::from([board.c1]),
    );
    // Reach-guard: the held entry is the activated, duration-bounded exile.
    let held = board.runner.state().stack.back().expect("activation held");
    assert!(
        matches!(
            &held.kind,
            StackEntryKind::ActivatedAbility { ability, .. }
                if ability.duration == Some(Duration::UntilHostLeavesPlay)
                    && ability.targets == vec![TargetRef::Object(board.c1)]
        ),
        "the exert ability must wait on the stack targeting C1"
    );
}

/// Bounces `creature` with a zero-cost Unsummon and resolves the stack.
fn bounce(runner: &mut GameRunner, unsummon: ObjectId, creature: ObjectId) {
    runner.cast(unsummon).target_object(creature).commit();
    drive(runner, priority_with_stack(0), VecDeque::new());
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Hand,
        "reach-guard: the bounce returned {creature:?} to its owner's hand"
    );
}

/// CR 610.3a + CR 602.2a: Angel leaves (and returns) after its exert ability
/// was put onto the stack, so the ability's exile does not move C1, and the
/// new Angel later leaving returns nothing.
#[test]
fn activated_until_exile_does_nothing_when_its_source_was_blinked_in_response() {
    let mut board = angel_board();
    activate_exert_exile(&mut board);
    let incarnation = board.runner.state().objects[&board.angel].incarnation;

    board
        .runner
        .cast(board.flicker)
        .target_objects(&[board.angel, board.land])
        .commit();
    drive(&mut board.runner, priority_with_stack(1), VecDeque::new());
    assert_eq!(
        board.runner.state().objects[&board.angel].zone,
        Zone::Battlefield,
        "reach-guard: Ghostly Flicker returned Angel"
    );
    assert_ne!(
        board.runner.state().objects[&board.angel].incarnation,
        incarnation,
        "reach-guard: Angel is a new object before its ability resolves"
    );

    let events = drive(&mut board.runner, priority_with_stack(0), VecDeque::new());
    assert!(
        exile_node_resolved(&events, board.angel),
        "reach-guard: the exert ability resolved its exile node"
    );
    assert_eq!(
        board.runner.state().objects[&board.c1].zone,
        Zone::Battlefield,
        "CR 610.3a: Angel left after its ability was put onto the stack, so C1 doesn't move"
    );
    assert_eq!(
        until_links(board.runner.state()),
        0,
        "the refused exile installs no return link"
    );

    bounce(&mut board.runner, board.unsummon, board.angel);
    assert_eq!(
        board.runner.state().objects[&board.c1].zone,
        Zone::Battlefield,
        "C1 is untouched when the new Angel leaves"
    );
}

/// CR 610.3a: Angel is returned to its owner's hand while its exert ability
/// waits, so the ability's exile does not move C1.
#[test]
fn activated_until_exile_does_nothing_when_its_source_left_in_response() {
    let mut board = angel_board();
    activate_exert_exile(&mut board);

    board
        .runner
        .cast(board.unsummon)
        .target_object(board.angel)
        .commit();
    drive(&mut board.runner, priority_with_stack(1), VecDeque::new());
    assert_eq!(
        board.runner.state().objects[&board.angel].zone,
        Zone::Hand,
        "reach-guard: Angel left the battlefield before its ability resolves"
    );

    let events = drive(&mut board.runner, priority_with_stack(0), VecDeque::new());
    assert!(
        exile_node_resolved(&events, board.angel),
        "reach-guard: the exert ability resolved its exile node"
    );
    assert_eq!(
        board.runner.state().objects[&board.c1].zone,
        Zone::Battlefield,
        "CR 610.3a: Angel left after its ability was put onto the stack, so C1 doesn't move"
    );
    assert_eq!(
        until_links(board.runner.state()),
        0,
        "the refused exile installs no return link"
    );
}

/// CR 610.3: with no response, C1 is exiled until Angel leaves the
/// battlefield, and returns when it does.
#[test]
fn activated_until_exile_returns_when_its_source_later_leaves() {
    let mut board = angel_board();
    activate_exert_exile(&mut board);

    drive(&mut board.runner, priority_with_stack(0), VecDeque::new());
    assert_eq!(
        board.runner.state().objects[&board.c1].zone,
        Zone::Exile,
        "C1 is exiled when Angel stays"
    );
    assert_eq!(until_links(board.runner.state()), 1);

    bounce(&mut board.runner, board.unsummon_two, board.angel);
    assert_eq!(
        board.runner.state().objects[&board.c1].zone,
        Zone::Battlefield,
        "CR 610.3: C1 returns when Angel leaves the battlefield"
    );
}

/// CR 610.3b: Banisher Priest is flickered while its enters trigger (targeting
/// C1) waits. The old trigger's exile does not move C1; the re-entered
/// Priest's own trigger exiles C2 until that Priest leaves.
#[test]
fn triggered_until_exile_still_skips_after_its_source_was_blinked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let priest = scenario
        .add_creature_to_hand_from_oracle(P0, "Banisher Priest", 2, 2, BANISHER_PRIEST)
        .with_mana_cost(ManaCost::zero())
        .id();
    let land = scenario.add_land_from_oracle(P0, "Blank Land", "").id();
    let c1 = scenario.add_creature(P1, "Opponent Bear One", 2, 2).id();
    let c2 = scenario.add_creature(P1, "Opponent Bear Two", 2, 2).id();
    let flicker = scenario
        .add_spell_to_hand_from_oracle(P0, "Ghostly Flicker", true, GHOSTLY_FLICKER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(priest).commit();
    drive(
        &mut runner,
        |state: &GameState| {
            priority_with_stack(1)(state) && state.objects[&priest].zone == Zone::Battlefield
        },
        VecDeque::from([c1]),
    );
    let incarnation = runner.state().objects[&priest].incarnation;

    runner
        .cast(flicker)
        .target_objects(&[priest, land])
        .commit();
    // The re-entered Priest's trigger targets C2 and waits above the old one.
    drive(
        &mut runner,
        |state: &GameState| {
            priority_with_stack(2)(state) && state.objects[&flicker].zone == Zone::Graveyard
        },
        VecDeque::from([c2]),
    );
    assert_eq!(
        runner.state().objects[&priest].incarnation,
        incarnation + 2,
        "reach-guard: Priest left and re-entered while its first trigger waits"
    );

    drive(&mut runner, priority_with_stack(0), VecDeque::new());
    assert_eq!(
        runner.state().objects[&c1].zone,
        Zone::Battlefield,
        "CR 610.3b: Priest left after the first trigger triggered, so C1 doesn't move"
    );
    assert_eq!(
        runner.state().objects[&c2].zone,
        Zone::Exile,
        "the re-entered Priest's trigger exiles C2"
    );

    bounce(&mut runner, unsummon, priest);
    assert_eq!(
        runner.state().objects[&c2].zone,
        Zone::Battlefield,
        "CR 610.3: C2 returns when the re-entered Priest leaves"
    );
}
