//! F3: a parked Damage ordering prompt is re-proposed when its candidates go
//! stale mid-park (CR 616.1 + CR 800.4a).
//!
//! Board (3 players): P0 controls Furnace of Rath and a Grizzly Bears (2/2).
//! P1 controls an established Selfless Squire (directly placed, so its ETB
//! shield never exists). P2 owns a Runed Halo naming "Grizzly Bears", which
//! P1 controls — the post-Harmless-Offering board ("Target opponent gains
//! control of target permanent you control.") built through the sanctioned
//! `CardBuilder::controlled_by` fixture path: a `GiveControl` recipient has
//! no player target slot in this engine (see
//! `do_this_only_once_each_turn.rs`), so resolving the Offering cannot pick
//! P1 out of P2's two opponents. Owner (P2) and controller (P1) stay split
//! exactly as a resolved Offering would leave them, and the concession,
//! ordering, and re-proposal legs under test all run through production.
//!
//! P0 attacks P1 with the Bears. The combat damage to P1 has two applicable
//! replacements — the Furnace doubling and the Halo protection — so the
//! ordering prompt parks with P1 to choose.
//!
//! * Concession row: P2 concedes while the prompt is parked. CR 800.4a removes
//!   the P2-owned Halo, so the retired protection candidate no longer applies.
//!   Answering re-proposes the Damage event through live re-discovery: only
//!   the Furnace doubling remains, dealing 4 to P1 with no prevention and no
//!   Squire trigger (0 counters). Pre-fix the retired Halo candidate was
//!   re-applied without revalidation (0 damage, Squire fires) — the
//!   revert-flip.
//! * Live-Halo control: no concession. Answering protection-first prevents
//!   the undoubled 2 (P1 untouched) and the Squire consumes exactly 2 —
//!   proving the ordering prompt and the prevention path were live.
//! * No-Furnace sanity: no Furnace in play. The lone Halo quality prevents
//!   the undoubled 2 and the Squire fires — the 0-doubling path.
//!
//! Verbatim Oracle texts (MTGJSON 5.3.0+20261114):
//! Runed Halo: "As this enchantment enters, choose a card name.\nYou have
//! protection from the chosen card name. (You can't be targeted, dealt damage,
//! or enchanted by anything with that name.)"
//! Selfless Squire: "Flash\nWhen this creature enters, prevent all damage
//! that would be dealt to you this turn.\nWhenever damage that would be dealt
//! to you is prevented, put that many +1/+1 counters on this creature."
//! Furnace of Rath: "If a source would deal damage to a permanent or player,
//! it deals double that damage to that permanent or player instead."

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ChosenAttribute;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);
const RUNED_HALO_ORACLE: &str = "As this enchantment enters, choose a card name.\nYou have protection from the chosen card name. (You can't be targeted, dealt damage, or enchanted by anything with that name.)";
const SELFLESS_SQUIRE_TEXT: &str = "Flash\nWhen this creature enters, prevent all damage that would be dealt to you this turn.\nWhenever damage that would be dealt to you is prevented, put that many +1/+1 counters on this creature.";
const FURNACE: &str = "If a source would deal damage to a permanent or player, it deals double that damage to that permanent or player instead.";

fn stock_libraries(scenario: &mut GameScenario) {
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Fa", "Fb", "Fc", "Fd"]);
    }
}

struct Fixture {
    runner: GameRunner,
    squire: ObjectId,
    bears: ObjectId,
    halo: ObjectId,
    furnace: Option<ObjectId>,
}

/// P0: Furnace (unless skipped) + Grizzly Bears. P1: established Squire.
/// P2-owned Halo naming "Grizzly Bears", controlled by P1 (the
/// post-Offering split; see the file header).
fn setup(with_furnace: bool) -> Fixture {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let furnace = if with_furnace {
        Some(
            scenario
                .add_enchantment_from_oracle(P0, "Furnace of Rath", FURNACE)
                .id(),
        )
    } else {
        None
    };
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let squire = scenario
        .add_creature_from_oracle(P1, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .id();
    let halo = scenario
        .add_enchantment_from_oracle(P2, "Runed Halo", RUNED_HALO_ORACLE)
        .controlled_by(P1)
        .id();
    let mut runner = scenario.build();

    // The Halo is directly placed, so name it through the production
    // attribute (the as-enters choice it would have made on cast).
    runner
        .state_mut()
        .objects
        .get_mut(&halo)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::CardName("Grizzly Bears".to_string()));
    assert_eq!(
        counters_on(&runner, squire),
        0,
        "reach-guard: the established Squire starts with no counters"
    );
    Fixture {
        runner,
        squire,
        bears,
        halo,
        furnace,
    }
}

fn counters_on(runner: &GameRunner, obj: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&obj)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

/// The post-Offering split the fixture sets up: P2 owns the Halo, P1
/// controls it (and chose the name through the production attribute).
fn assert_post_offering_split(fixture: &Fixture) {
    let halo = &fixture.runner.state().objects[&fixture.halo];
    assert_eq!(
        halo.owner, P2,
        "reach-guard: P2 must own the Halo so the concession removes it"
    );
    assert_eq!(
        halo.controller, P1,
        "reach-guard: P1 must control the Halo so P1 is protected"
    );
    assert_eq!(
        halo.chosen_card_name(),
        Some("Grizzly Bears"),
        "reach-guard: the Halo names Grizzly Bears"
    );
}

/// Declare the Bears into P1 with no blockers and drive to the parked Damage
/// ordering prompt, collecting events along the way.
fn drive_combat_to_choice(fixture: &mut Fixture) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let mut attacked = false;
    for _ in 0..80 {
        match fixture.runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { player, .. } if player == P0 && !attacked => {
                attacked = true;
                let result = fixture
                    .runner
                    .declare_attackers(&[(fixture.bears, AttackTarget::Player(P1))])
                    .expect("declaring the Bears attacker must succeed");
                events.extend(result.events);
            }
            WaitingFor::DeclareAttackers { .. } => {
                let result = fixture
                    .runner
                    .declare_attackers(&[])
                    .expect("declaring no attackers must succeed");
                events.extend(result.events);
            }
            WaitingFor::DeclareBlockers { .. } => {
                let result = fixture
                    .runner
                    .declare_blockers(&[])
                    .expect("declaring no blockers must succeed");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } => {
                let result = fixture
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
                events.extend(result.events);
            }
            WaitingFor::ReplacementChoice { .. } => {
                assert!(attacked, "combat must have started before the prompt");
                return events;
            }
            other => panic!("unexpected prompt while driving combat: {other:?}"),
        }
    }
    panic!("never reached the replacement-ordering prompt");
}

/// The live two-candidate ordering prompt: the Furnace doubling and the Halo
/// protection, chosen by the affected player P1. Returns
/// `(furnace_index, protection_index)`.
fn assert_two_candidate_prompt(runner: &GameRunner, furnace: ObjectId) -> (usize, usize) {
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player,
            candidate_count,
            candidates,
            ..
        } => {
            assert_eq!(
                *player, P1,
                "the affected player chooses the replacement order"
            );
            assert_eq!(
                *candidate_count, 2,
                "Furnace + one protection quality must publish two candidates, got {candidates:?}"
            );
            let furnace_index = candidates
                .iter()
                .position(|c| c.source_id == furnace)
                .unwrap_or_else(|| {
                    panic!("the Furnace doubling must be offered, got {candidates:?}")
                });
            let protection_index = candidates
                .iter()
                .position(|c| {
                    c.description
                        .starts_with("Prevent damage with protection from ")
                })
                .unwrap_or_else(|| {
                    panic!("the Halo protection must be offered, got {candidates:?}")
                });
            assert_ne!(
                furnace_index, protection_index,
                "the two candidates must be distinct doors, got {candidates:?}"
            );
            (furnace_index, protection_index)
        }
        other => panic!("expected a two-candidate replacement prompt, got {other:?}"),
    }
}

/// Drive to quiescence (empty stack, Priority), answering any stray prompts.
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
            WaitingFor::ReplacementChoice { .. } => {
                let result = runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("the ordering choice must be answerable");
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

/// Concession row: P2 concedes while the ordering prompt is parked. The
/// P2-owned Halo leaves, the retired protection candidate drops out of live
/// re-discovery, and the re-proposed event is doubled to 4 with no prevention
/// and no Squire trigger.
#[test]
fn conceded_halo_drops_out_of_parked_damage_ordering() {
    let mut fixture = setup(true);
    let p1_before = fixture.runner.life(P1);
    assert_post_offering_split(&fixture);
    drive_combat_to_choice(&mut fixture);
    let furnace = fixture.furnace.expect("this row needs the Furnace");
    let (_, _) = assert_two_candidate_prompt(&fixture.runner, furnace);

    fixture
        .runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert!(
        !fixture.runner.state().battlefield.contains(&fixture.halo),
        "reach-guard: the P2-owned Halo must leave with P2 (CR 800.4a)"
    );
    match &fixture.runner.state().waiting_for {
        WaitingFor::ReplacementChoice { player, .. } => {
            assert_eq!(*player, P1, "the parked prompt survives for its chooser P1")
        }
        other => panic!("the ordering prompt must survive the concession, got {other:?}"),
    }

    // Any answer re-proposes the now-stale event instead of applying a
    // retired candidate.
    let result = fixture
        .runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the stale ordering choice must be answerable");
    let mut events = result.events;
    events.extend(drive_to_quiet(&mut fixture.runner));

    assert_eq!(
        fixture.runner.life(P1),
        p1_before - 4,
        "the re-proposed event keeps only the Furnace doubling: 4 dealt to P1"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "no prevention may apply once the Halo is gone, got {events:?}"
    );
    assert_eq!(
        counters_on(&fixture.runner, fixture.squire),
        0,
        "the Squire must not trigger when nothing is prevented"
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
}

/// Live-Halo control: no concession. Answering protection-first prevents the
/// undoubled 2 and the Squire consumes exactly 2 — the ordering prompt and
/// the prevention path the concession row retires were both live.
#[test]
fn live_halo_control_prevents_and_squire_fires() {
    let mut fixture = setup(true);
    let p1_before = fixture.runner.life(P1);
    assert_post_offering_split(&fixture);
    drive_combat_to_choice(&mut fixture);
    let furnace = fixture.furnace.expect("this row needs the Furnace");
    let (_, protection_index) = assert_two_candidate_prompt(&fixture.runner, furnace);

    let result = fixture
        .runner
        .act(GameAction::ChooseReplacement {
            index: protection_index,
        })
        .expect("the ordering choice must be answerable");
    let mut events = result.events;
    events.extend(drive_to_quiet(&mut fixture.runner));

    assert_eq!(
        fixture.runner.life(P1),
        p1_before,
        "P1's life must be unchanged — every point was prevented"
    );
    assert_eq!(
        counters_on(&fixture.runner, fixture.squire),
        2,
        "the Squire must consume exactly the prevented 2"
    );
    assert!(
        fixture.runner.state().stack.is_empty(),
        "the Squire trigger must have resolved before the counters are read"
    );
}

/// No-Furnace sanity: with no doubling in the chain, the lone Halo quality
/// auto-applies (no ordering prompt parks), prevents the undoubled 2, and
/// the Squire fires.
#[test]
fn halo_without_furnace_prevents_and_squire_fires() {
    let mut fixture = setup(false);
    let p1_before = fixture.runner.life(P1);
    assert_post_offering_split(&fixture);

    let mut events = Vec::new();
    let mut attacked = false;
    for _ in 0..200 {
        if matches!(
            fixture.runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            break;
        }
        match fixture.runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { player, .. } if player == P0 && !attacked => {
                attacked = true;
                let result = fixture
                    .runner
                    .declare_attackers(&[(fixture.bears, AttackTarget::Player(P1))])
                    .expect("declaring the Bears attacker must succeed");
                events.extend(result.events);
            }
            WaitingFor::DeclareAttackers { .. } => {
                let result = fixture
                    .runner
                    .declare_attackers(&[])
                    .expect("declaring no attackers must succeed");
                events.extend(result.events);
            }
            WaitingFor::DeclareBlockers { .. } => {
                let result = fixture
                    .runner
                    .declare_blockers(&[])
                    .expect("declaring no blockers must succeed");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } => {
                let result = fixture
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
                events.extend(result.events);
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                let result = fixture
                    .runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering triggers must succeed");
                events.extend(result.events);
            }
            WaitingFor::ReplacementChoice { candidates, .. } => {
                panic!("no ordering prompt may park without the Furnace, got {candidates:?}")
            }
            other => panic!("unexpected prompt while draining combat: {other:?}"),
        }
    }
    assert!(attacked, "the Bears must have attacked");

    assert_eq!(
        fixture.runner.life(P1),
        p1_before,
        "P1's life must be unchanged — the Halo prevented the undoubled 2"
    );
    assert!(
        fixture.runner.state().stack.is_empty(),
        "the Squire trigger must have resolved before the counters are read"
    );
    assert_eq!(
        counters_on(&fixture.runner, fixture.squire),
        2,
        "the Squire must consume exactly the prevented 2"
    );
}
