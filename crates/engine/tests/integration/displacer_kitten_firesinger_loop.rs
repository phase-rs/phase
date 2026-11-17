//! CR 732.2a: Displacer Kitten + Blazing Firesinger is offered as an unbounded red-mana loop.
//!
//! Verbatim Oracle text, as the fixture stores it:
//!   Displacer Kitten: "Avoidance — Whenever you cast a noncreature spell, exile up to one target
//!     nonland permanent you control, then return that card to the battlefield under its owner's
//!     control."
//!   Blazing Firesinger // Seething Song (prepare): "This creature enters prepared." /
//!     Seething Song {2}{R}, Instant: "Add {R}{R}{R}{R}{R}."
//!
//! Each cycle, P0 casts the prepared Seething Song copy from exile (CR 722.3c: Firesinger stops
//! being prepared as it is cast, CR 601.2i). Kitten triggers on the noncreature cast (CR 603.2)
//! and resolves first: Firesinger is exiled and returns as a new object (CR 400.7), enters
//! prepared, and a new copy is created in exile. Seething Song then adds {R}{R}{R}{R}{R}, a net
//! {R}{R} over the copy's {2}{R}. The board is driven only through `apply()`.

use engine::analysis::resource::ResourceAxis;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{LoopDetectionMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const FIRESINGER: &str = "Blazing Firesinger";

/// Kitten on P0's battlefield, Firesinger cast from hand onto six Mountains and resolved, so it
/// entered prepared through its own replacement; the stack is empty and three Mountains are
/// untapped for the first copy. `mode` is the game's loop detection.
fn board(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let db = load_db().expect("the committed integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Displacer Kitten", Zone::Battlefield, db);
    let firesinger = scenario.add_real_card(P0, FIRESINGER, Zone::Hand, db);
    for _ in 0..6 {
        scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    let card_id = runner.state().objects[&firesinger].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: firesinger,
            card_id,
            targets: Vec::new(),
            payment_mode: Default::default(),
        })
        .expect("Firesinger is castable for {2}{R}");
    pass_until_stack_empty(&mut runner, Some(firesinger));
    assert!(
        runner.state().objects[&firesinger].prepared.is_some(),
        "CR 722.3a: Firesinger enters prepared"
    );
    (runner, firesinger)
}

fn red(runner: &GameRunner) -> usize {
    runner.state().players[0]
        .mana_pool
        .count_color(ManaType::Red)
}

/// Passes priority, answering Kitten's "up to one target" with `kitten_target` (`None` declines
/// it), until the stack is empty or the engine offers the loop.
fn pass_until_stack_empty(runner: &mut GameRunner, kitten_target: Option<ObjectId>) {
    for _ in 0..16 {
        let action = match &runner.state().waiting_for {
            WaitingFor::LoopShortcut { .. } => return,
            WaitingFor::TriggerTargetSelection { .. } => GameAction::ChooseTarget {
                target: kitten_target.map(TargetRef::Object),
            },
            _ if runner.state().stack.is_empty() => return,
            _ => GameAction::PassPriority,
        };
        runner.act(action).expect("the cycle's priority passes");
    }
    panic!("the stack never emptied");
}

/// One cycle: cast the prepared copy, blink Firesinger with Kitten, resolve the mana.
fn cycle(runner: &mut GameRunner, firesinger: ObjectId) {
    runner
        .act(GameAction::CastPreparedCopy { source: firesinger })
        .expect("the prepared Seething Song copy is castable");
    pass_until_stack_empty(runner, Some(firesinger));
}

#[test]
fn one_cycle_reprepares_firesinger_and_nets_two_red() {
    // Detection off, so no offer interrupts the second cycle whose yield this measures.
    let (mut runner, firesinger) = board(LoopDetectionMode::Off);
    let before = red(&runner);
    let incarnation = runner.state().objects[&firesinger].incarnation;
    runner
        .act(GameAction::CastPreparedCopy { source: firesinger })
        .expect("the prepared Seething Song copy is castable");
    assert!(
        runner.state().objects[&firesinger].prepared.is_none(),
        "CR 722.3c + CR 601.2i: Firesinger stops being prepared as the copy becomes cast"
    );
    pass_until_stack_empty(&mut runner, Some(firesinger));
    let object = &runner.state().objects[&firesinger];
    assert_eq!(object.zone, Zone::Battlefield, "Kitten returned Firesinger");
    assert_ne!(
        object.incarnation, incarnation,
        "CR 400.7: Firesinger returned as a new object"
    );
    assert!(
        object.prepared.is_some(),
        "CR 722.3a: the returned Firesinger enters prepared"
    );
    // The first copy is paid by three Mountains, so the whole {R}x5 floats.
    assert_eq!(red(&runner), before + 5, "Seething Song adds {{R}}x5");
    let after_first = red(&runner);
    cycle(&mut runner, firesinger);
    assert_eq!(
        red(&runner),
        after_first + 2,
        "a cycle paid from the pool nets {{R}}{{R}}"
    );
}

#[test]
fn interactive_mode_offers_the_red_mana_loop() {
    let (mut runner, firesinger) = board(LoopDetectionMode::Interactive);
    for _ in 0..6 {
        if let WaitingFor::LoopShortcut {
            proposer,
            certificate,
            ..
        } = &runner.state().waiting_for
        {
            assert_eq!(*proposer, P0);
            assert!(
                certificate
                    .unbounded
                    .contains(&ResourceAxis::Mana(ManaType::Red)),
                "red mana is an unbounded axis (got {:?})",
                certificate.unbounded
            );
            return;
        }
        cycle(&mut runner, firesinger);
    }
    panic!(
        "CR 732.2a: the loop was never offered; waiting on {:?}",
        runner.state().waiting_for
    );
}

/// Kitten's target declined: Firesinger stays unprepared after its copy is cast, so nothing can
/// be cast again and no loop is offered.
#[test]
fn declining_kittens_target_offers_no_loop() {
    let (mut runner, firesinger) = board(LoopDetectionMode::Interactive);
    for _ in 0..3 {
        if runner.state().objects[&firesinger].prepared.is_none() {
            break;
        }
        runner
            .act(GameAction::CastPreparedCopy { source: firesinger })
            .expect("the prepared Seething Song copy is castable");
        pass_until_stack_empty(&mut runner, None);
    }
    // Reach: the copy was cast and resolved.
    assert_eq!(red(&runner), 5, "Seething Song resolved once");
    assert!(
        runner.state().objects[&firesinger].prepared.is_none(),
        "CR 722.3c: Firesinger is unprepared once its copy is cast"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
        "no loop is offered"
    );
}
