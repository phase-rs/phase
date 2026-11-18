//! F1: a Ward sacrifice shortfall settles the Ward unpaid (CR 702.21a +
//! CR 118.3 + CR 800.4a).
//!
//! Board (3 players): P0 controls Agent of Treachery and a P2-owned Mountain
//! the Agent stole — the post-steal board ("When this creature enters, gain
//! control of target permanent.") built through the sanctioned
//! `CardBuilder::controlled_by` fixture path (the same post-transfer split
//! `conceded_halo_combat_ordering_reproposal.rs` uses for Harmless Offering:
//! owner and controller stay split exactly as a resolved steal would leave
//! them). P1 controls Ulamog, the Defiler ("Ward—Sacrifice two permanents").
//!
//! P0 casts Lightning Bolt targeting Ulamog and accepts the Ward cost. The
//! sacrifice prompt parks offering [Agent, Mountain]:
//!
//! * Concession row: P2 concedes before P0 selects. CR 800.4a removes the
//!   P2-owned Mountain, so the pool no longer covers the two owed sacrifices.
//!   Selecting the Agent settles the Ward unpaid: Lightning Bolt is countered
//!   (Ulamog takes nothing), the Agent survives (no surviving permanent is
//!   sacrificed), and the game reaches an answerable Priority. Pre-fix the
//!   shortfall guard returned `ActionNotAllowed`, the public boundary
//!   restored the same unpayable choice, and the payer had no way out — the
//!   revert-flip is the answer erroring instead of settling.
//! * Payable control: no concession. Selecting the Agent re-prompts with the
//!   Mountain alone (`remaining: 1`); selecting it pays the Ward in full and
//!   Lightning Bolt resolves for 3 to Ulamog — the payment path the
//!   concession row retires was live.
//!
//! Verbatim Oracle texts (MTGJSON):
//! Agent of Treachery: "When this creature enters, gain control of target
//! permanent.\nAt the beginning of your end step, if you control three or
//! more permanents you don't own, draw three cards."
//! Ulamog, the Defiler: "When you cast this spell, target opponent exiles the
//! top half of their library, rounded up.\nWard—Sacrifice two
//! permanents.\nUlamog enters with a number of +1/+1 counters on it equal to
//! the greatest mana value among cards in exile.\nUlamog has annihilator X,
//! where X is the number of +1/+1 counters on it."
//! Lightning Bolt: "Lightning Bolt deals 3 damage to any target."

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, SacrificeRequirement};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const AGENT_ORACLE: &str = "When this creature enters, gain control of target permanent.\nAt the beginning of your end step, if you control three or more permanents you don't own, draw three cards.";
const ULAMOG_ORACLE: &str = "When you cast this spell, target opponent exiles the top half of their library, rounded up.\nWard—Sacrifice two permanents.\nUlamog enters with a number of +1/+1 counters on it equal to the greatest mana value among cards in exile.\nUlamog has annihilator X, where X is the number of +1/+1 counters on it.";
const BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";
const MOUNTAIN_ORACLE: &str = "({T}: Add {R}.)";

struct Fixture {
    runner: GameRunner,
    agent: ObjectId,
    mountain: ObjectId,
    ulamog: ObjectId,
    bolt: ObjectId,
}

fn stock_libraries(scenario: &mut GameScenario) {
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Fa", "Fb", "Fc", "Fd"]);
    }
}

/// P0: Agent of Treachery + the stolen P2-owned Mountain. P1: Ulamog, the
/// Defiler. P0's hand holds Lightning Bolt with {R} seeded to cast it.
fn setup() -> Fixture {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let agent = scenario
        .add_creature_from_oracle(P0, "Agent of Treachery", 2, 3, AGENT_ORACLE)
        .id();
    // The post-steal split: P2 owns the Mountain, P0 controls it — exactly
    // as a resolved Agent ETB would leave them (cf. the post-Offering split).
    let mountain = scenario
        .add_land_from_oracle(P2, "Mountain", MOUNTAIN_ORACLE)
        .controlled_by(P0)
        .id();
    let ulamog = scenario
        .add_creature_from_oracle(P1, "Ulamog, the Defiler", 7, 7, ULAMOG_ORACLE)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT_ORACLE)
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    Fixture {
        runner,
        agent,
        mountain,
        ulamog,
        bolt,
    }
}

/// The post-steal split the fixture sets up: P2 owns the Mountain, P0
/// controls it.
fn assert_post_steal_split(fixture: &Fixture) {
    let mountain = &fixture.runner.state().objects[&fixture.mountain];
    assert_eq!(
        mountain.owner, P2,
        "reach-guard: P2 must own the Mountain so the concession removes it"
    );
    assert_eq!(
        mountain.controller, P0,
        "reach-guard: P0 must control the Mountain so it is a Ward candidate"
    );
}

/// Cast Bolt at Ulamog and drive the Ward trigger to the sacrifice prompt.
/// Returns when `WardSacrificeChoice { remaining: 2 }` stands.
fn drive_bolt_to_ward_prompt(fixture: &mut Fixture) {
    fixture
        .runner
        .cast(fixture.bolt)
        .target_objects(&[fixture.ulamog])
        .commit();
    fixture.runner.advance_until_stack_empty();
    let WaitingFor::UnlessPayment { player, cost, .. } = fixture.runner.state().waiting_for.clone()
    else {
        panic!(
            "Ward must prompt the targeting player to pay, got {:?}",
            fixture.runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "the Bolt controller pays Ulamog's Ward");
    assert!(
        matches!(
            cost,
            AbilityCost::Sacrifice(ref c) if matches!(
                c.requirement,
                SacrificeRequirement::Count { count: 2 }
            )
        ),
        "Ulamog's Ward must be sacrifice-two, got {cost:?}"
    );
    fixture
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P0 accepts the Ward cost");
    match &fixture.runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            player,
            permanents,
            remaining,
            ..
        } => {
            assert_eq!(*player, P0, "P0 answers the sacrifice prompt");
            assert_eq!(*remaining, 2, "two sacrifices are owed");
            assert!(
                permanents.contains(&fixture.agent) && permanents.contains(&fixture.mountain),
                "the prompt must offer the Agent and the stolen Mountain, got {permanents:?}"
            );
        }
        other => panic!("expected the Ward sacrifice prompt, got {other:?}"),
    }
}

/// Drive to quiescence (empty stack, Priority), answering stray prompts.
/// Returns every event observed along the way.
fn drive_to_quiet(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..300 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return events;
                }
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
                events.extend(result.events);
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                let result = runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering triggers must succeed");
                events.extend(result.events);
            }
            other => panic!("unexpected prompt while draining: {other:?}"),
        }
    }
    panic!("never reached quiescence");
}

/// Concession row: P2 concedes while the Ward sacrifice prompt is parked.
/// The stolen Mountain leaves under CR 800.4a, so selecting the Agent — the
/// only surviving candidate — settles the Ward unpaid: Bolt is countered,
/// the Agent survives, and Priority is answerable.
#[test]
fn conceded_mountain_shortfall_settles_ward_unpaid() {
    let mut fixture = setup();
    assert_post_steal_split(&fixture);
    drive_bolt_to_ward_prompt(&mut fixture);

    fixture
        .runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert!(
        !fixture
            .runner
            .state()
            .battlefield
            .contains(&fixture.mountain),
        "reach-guard: the P2-owned Mountain must leave with P2 (CR 800.4a)"
    );
    assert!(
        matches!(
            fixture.runner.state().waiting_for,
            WaitingFor::WardSacrificeChoice { .. }
        ),
        "the sacrifice prompt must survive for its answerer P0, got {:?}",
        fixture.runner.state().waiting_for
    );

    // Pre-fix this answer errored (`ActionNotAllowed`) and the boundary
    // restored the same unpayable choice — the revert-flip.
    let result = fixture
        .runner
        .act(GameAction::SelectCards {
            cards: vec![fixture.agent],
        })
        .expect("the shortfall answer must settle, not strand");
    let mut events = result.events;
    events.extend(drive_to_quiet(&mut fixture.runner));

    assert_eq!(
        fixture.runner.state().objects[&fixture.bolt].zone,
        Zone::Graveyard,
        "Lightning Bolt must be countered to the graveyard without resolving"
    );
    assert_eq!(
        fixture.runner.state().objects[&fixture.agent].zone,
        Zone::Battlefield,
        "the Agent must survive: no surviving permanent is sacrificed"
    );
    assert!(
        !fixture
            .runner
            .state()
            .battlefield
            .contains(&fixture.mountain),
        "the Mountain stays gone (concession, not sacrifice)"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentSacrificed { .. })),
        "no sacrifice may be recorded when the Ward goes unpaid, got {events:?}"
    );
    assert_eq!(
        fixture.runner.state().objects[&fixture.ulamog].damage_marked,
        0,
        "Ulamog must take no damage from the countered Bolt"
    );
    assert!(
        fixture.runner.state().battlefield.contains(&fixture.ulamog),
        "Ulamog must still be on the battlefield"
    );
    assert_eq!(fixture.runner.life(P1), 20, "P1's life must be untouched");
    assert!(
        fixture.runner.state().stack.is_empty(),
        "stack is empty at the end"
    );
    assert!(
        matches!(
            fixture.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "Priority boundary, got {:?}",
        fixture.runner.state().waiting_for
    );
    fixture
        .runner
        .act(GameAction::PassPriority)
        .expect("the next state must be answerable");
}

/// Payable control: no concession. The Agent is sacrificed first, the prompt
/// re-offers the Mountain alone, paying it completes the Ward, and Bolt
/// resolves for 3 to Ulamog — the payment path the concession row retires.
#[test]
fn payable_ward_sacrifices_both_and_bolt_resolves() {
    let mut fixture = setup();
    assert_post_steal_split(&fixture);
    drive_bolt_to_ward_prompt(&mut fixture);

    fixture
        .runner
        .act(GameAction::SelectCards {
            cards: vec![fixture.agent],
        })
        .expect("the first sacrifice must succeed");
    match &fixture.runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            permanents,
            remaining: 1,
            ..
        } => assert_eq!(
            permanents,
            &vec![fixture.mountain],
            "the re-prompt must offer only the Mountain, got {permanents:?}"
        ),
        other => panic!("expected the second sacrifice prompt, got {other:?}"),
    }
    fixture
        .runner
        .act(GameAction::SelectCards {
            cards: vec![fixture.mountain],
        })
        .expect("the second sacrifice must complete the Ward");
    drive_to_quiet(&mut fixture.runner);

    assert_eq!(
        fixture.runner.state().objects[&fixture.agent].zone,
        Zone::Graveyard,
        "the Agent must be sacrificed to the Ward"
    );
    assert_eq!(
        fixture.runner.state().objects[&fixture.mountain].zone,
        Zone::Graveyard,
        "the Mountain must be sacrificed to the Ward"
    );
    assert_eq!(
        fixture.runner.state().objects[&fixture.ulamog].damage_marked,
        3,
        "Bolt must resolve for 3 to Ulamog once the Ward is paid"
    );
    assert!(
        fixture.runner.state().stack.is_empty(),
        "stack is empty at the end"
    );
}

/// Post-spend sequel: P0 sacrifices the Agent (committed), the prompt
/// re-offers the Mountain alone (`remaining: 1`), then P2 concedes. The
/// departed selection settles the Ward unpaid — the committed Agent stays
/// sacrificed (paper cannot un-sacrifice), Bolt is countered, and Priority
/// is answerable.
#[test]
fn conceded_mountain_after_first_sacrifice_settles_with_spend_standing() {
    let mut fixture = setup();
    assert_post_steal_split(&fixture);
    drive_bolt_to_ward_prompt(&mut fixture);

    fixture
        .runner
        .act(GameAction::SelectCards {
            cards: vec![fixture.agent],
        })
        .expect("the first sacrifice must succeed");
    assert!(
        matches!(
            fixture.runner.state().waiting_for,
            WaitingFor::WardSacrificeChoice { remaining: 1, .. }
        ),
        "one sacrifice must be owed after the first, got {:?}",
        fixture.runner.state().waiting_for
    );
    fixture
        .runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert!(
        !fixture
            .runner
            .state()
            .battlefield
            .contains(&fixture.mountain),
        "reach-guard: the P2-owned Mountain must leave with P2 (CR 800.4a)"
    );

    fixture
        .runner
        .act(GameAction::SelectCards {
            cards: vec![fixture.mountain],
        })
        .expect("the departed selection must settle, not strand");
    drive_to_quiet(&mut fixture.runner);

    assert_eq!(
        fixture.runner.state().objects[&fixture.agent].zone,
        Zone::Graveyard,
        "the pre-concession sacrifice stands: paper cannot un-sacrifice"
    );
    assert_eq!(
        fixture.runner.state().objects[&fixture.bolt].zone,
        Zone::Graveyard,
        "Lightning Bolt must be countered to the graveyard without resolving"
    );
    assert_eq!(
        fixture.runner.state().objects[&fixture.ulamog].damage_marked,
        0,
        "Ulamog must take no damage from the countered Bolt"
    );
    assert!(
        fixture.runner.state().stack.is_empty(),
        "stack is empty at the end"
    );
    assert!(
        matches!(
            fixture.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "Priority boundary, got {:?}",
        fixture.runner.state().waiting_for
    );
    fixture
        .runner
        .act(GameAction::PassPriority)
        .expect("the next state must be answerable");
}
