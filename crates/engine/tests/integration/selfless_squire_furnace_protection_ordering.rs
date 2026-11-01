//! Matt R14 MED: one applicable protection quality must still flow through
//! the full replacement-candidate authority when it competes with an
//! amount-changing replacement (CR 616.1 + CR 616.1f).
//!
//! Board: P1 controls an established Selfless Squire (ETB shield long gone),
//! Serra's Emissary naming Creature, and P0 controls Furnace of Rath. One
//! incoming 1-damage creature event then has TWO applicable replacements — the
//! Furnace doubling and the Emissary protection — so the affected player (P1)
//! must be offered the order:
//!
//!   * Furnace first: 1 → 2, then protection prevents 2 → Squire gains 2;
//!   * protection first: prevents 1 → Squire gains 1, Furnace inapplicable.
//!
//! The eager single-quality protection gate used to prevent the original 1
//! before offering the legal order, so the Furnace-first row was unreachable.
//! Each test pins `ReplacementChoice { candidate_count: 2 }` (the gate never
//! fires — pre-fix there is no prompt at all), the exact `DamagePrevented`
//! amount, P1's unchanged life, and the exact counters the Squire consumes.
//!
//! Both the combat path (1/1 attacker, unblocked) and the non-combat path (a
//! `{T}` "deals 1 damage to each opponent" ping — untargeted, because
//! protection from creatures makes P1 an illegal target for a creature-source
//! targeted ability) are covered, each in both material orders.
//!
//! The Emissary's as-enters choice is established through the production
//! `ChooseOption` handler exactly as in
//! `serras_emissary_chosen_card_type_protection` (direct placement never runs
//! as-enters replacements, so the prompt is keyed to the permanent and
//! answered Creature); the `chosen_card_type` read below guards the choice.
//! The Squire is likewise placed directly, so its ETB shield never exists —
//! the two-candidate prompt itself proves no third shield competes.

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityKind, ChoiceType, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::exact_named_choice_source;
use crate::support::shared_card_db as load_db;

/// Verbatim Selfless Squire (Scryfall; shared with
/// `selfless_squire_damage_prevented_trigger`).
const SELFLESS_SQUIRE_TEXT: &str = "Flash\nWhen this creature enters, prevent all damage that would be dealt to you this turn.\nWhenever damage that would be dealt to you is prevented, put that many +1/+1 counters on this creature.";

/// Verbatim Furnace of Rath (MTGJSON Oracle, pinned in
/// `fixed_damage_amount_thresholds`).
const FURNACE: &str = "If a source would deal damage to a permanent or player, it deals double that damage to that permanent or player instead.";

/// Custom ping: untargeted so protection from creatures cannot fend off the
/// activation itself via targeting illegality — only the damage event.
const PINGER_TEXT: &str = "{T}: Ember Brute deals 1 damage to each opponent.";

fn stock_libraries(scenario: &mut GameScenario) {
    scenario.with_library_top(
        P0,
        &["F0a", "F0b", "F0c", "F0d", "F0e", "F0f", "F0g", "F0h"],
    );
    scenario.with_library_top(
        P1,
        &["F1a", "F1b", "F1c", "F1d", "F1e", "F1f", "F1g", "F1h"],
    );
}

struct Fixture {
    runner: GameRunner,
    squire: ObjectId,
    furnace: ObjectId,
    /// Non-combat damage source (the pinger) or combat attacker.
    source: ObjectId,
}

/// P1: established Squire + Emissary naming Creature. P0: Furnace plus the
/// damage source (`PINGER_TEXT` pinger for the non-combat rows, a 1/1 attacker
/// for the combat rows).
fn setup(db: &CardDatabase, combat: bool) -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let squire = scenario
        .add_creature_from_oracle(P1, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .id();
    let emissary = scenario.add_real_card(P1, "Serra's Emissary", Zone::Battlefield, db);
    let furnace = scenario
        .add_enchantment_from_oracle(P0, "Furnace of Rath", FURNACE)
        .id();
    let source = if combat {
        scenario.add_creature(P0, "Ember Mite", 1, 1).id()
    } else {
        scenario
            .add_creature_from_oracle(P0, "Ember Brute", 1, 1, PINGER_TEXT)
            .id()
    };
    let mut runner = scenario.build();

    // The ETB Choose effect (a replacement) yields `WaitingFor::NamedChoice`
    // keyed to the entering permanent; answer Creature through production.
    let named_source = exact_named_choice_source(runner.state(), emissary);
    runner.state_mut().waiting_for = WaitingFor::NamedChoice {
        free_entry: None,
        player: P1,
        choice_type: ChoiceType::card_type(),
        options: vec![
            "Artifact".to_string(),
            "Creature".to_string(),
            "Enchantment".to_string(),
            "Instant".to_string(),
            "Land".to_string(),
            "Planeswalker".to_string(),
            "Sorcery".to_string(),
        ],
        source: Some(named_source),
        persist_player: None,
    };
    runner
        .act(GameAction::ChooseOption {
            choice: "Creature".to_string(),
        })
        .expect("choosing Creature for the Emissary must resolve");
    assert_eq!(
        runner.state().objects[&emissary].chosen_card_type(),
        Some(CoreType::Creature),
        "reach guard: the Emissary must name Creature"
    );
    assert_eq!(
        counters_on(&runner, squire),
        0,
        "reach guard: the established Squire starts with no counters"
    );
    Fixture {
        runner,
        squire,
        furnace,
        source,
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

fn life_of(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.life)
        .unwrap_or(-999)
}

/// The live ordering prompt: exactly the Furnace doubling and the Emissary
/// protection. Pre-fix the eager gate prevented without prompting, so this
/// read is the MED discriminator.
///
/// Returns `(furnace_index, protection_index)`. The protection candidate is a
/// virtual quality candidate (sentinel source), so it is identified by its
/// "Prevent damage with protection" description, pinned verbatim.
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
            assert_eq!(
                candidates.len(),
                2,
                "the prompt must carry both candidates, got {candidates:?}"
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
                    panic!("the Emissary protection must be offered, got {candidates:?}")
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

/// Answer the live prompt with `first_index`, then drain any single-candidate
/// CR 616.1f repeat (the doubled event's lone protection may auto-apply or
/// re-prompt; either way it must be the protection, never the Furnace twice).
/// Returns every event the answers produced (the prevention application fires
/// here, not during the later stack drain).
fn answer_then_drain(
    runner: &mut GameRunner,
    first_index: usize,
    furnace: ObjectId,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let result = runner
        .act(GameAction::ChooseReplacement { index: first_index })
        .expect("the ordering choice must be answerable");
    events.extend(result.events);
    if let WaitingFor::ReplacementChoice {
        candidate_count,
        candidates,
        ..
    } = runner.state().waiting_for.clone()
    {
        assert_eq!(
            candidate_count, 1,
            "a CR 616.1f repeat must carry only the lone remainder, got {candidates:?}"
        );
        assert_ne!(
            candidates[0].source_id, furnace,
            "the Furnace must never apply twice to the same event"
        );
        let result = runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("the lone remainder must be answerable");
        events.extend(result.events);
    }
    events
}

fn prevented_amounts(events: &[GameEvent], target: TargetRef) -> Vec<u32> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamagePrevented {
                target: t, amount, ..
            } if *t == target => Some(*amount),
            _ => None,
        })
        .collect()
}

/// Shared tail: all damage prevented, P1's life untouched, and the Squire
/// consumed exactly the prevented amount.
fn assert_prevented_tail(
    fixture: &Fixture,
    events: &[GameEvent],
    p1_life_before: i32,
    expected_prevented: u32,
) {
    assert_eq!(
        prevented_amounts(events, TargetRef::Player(P1)),
        vec![expected_prevented],
        "exactly one prevention application must name P1"
    );
    assert_eq!(
        life_of(&fixture.runner, P1),
        p1_life_before,
        "P1's life must be unchanged — every point was prevented"
    );
    assert!(
        fixture.runner.state().stack.is_empty(),
        "the Squire trigger must have resolved before the counters are read"
    );
    assert_eq!(
        counters_on(&fixture.runner, fixture.squire),
        expected_prevented,
        "the Squire must consume exactly the prevented amount"
    );
}

fn pinger_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| matches!(a.kind, AbilityKind::Activated))
        .expect("the pinger must expose its activated ability")
}

fn pass_priority_to(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..4 {
        if runner.state().priority_player == player {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority must succeed");
    }
    panic!("priority never reached {player:?}");
}

fn run_noncombat_row(furnace_first: bool, expected_prevented: u32) {
    let Some(db) = load_db() else {
        eprintln!("card-data.json missing — skipping");
        return;
    };
    let mut fixture = setup(db, false);
    let p1_life_before = life_of(&fixture.runner, P1);

    // The Emissary choice answer leaves priority elsewhere; the ping is P0's.
    pass_priority_to(&mut fixture.runner, P0);
    let idx = pinger_index(&fixture.runner, fixture.source);
    let outcome = fixture.runner.activate(fixture.source, idx).resolve();
    let mut events = outcome.events().to_vec();

    let (furnace_index, protection_index) =
        assert_two_candidate_prompt(&fixture.runner, fixture.furnace);
    let first = if furnace_first {
        furnace_index
    } else {
        protection_index
    };
    let furnace = fixture.furnace;
    events.extend(answer_then_drain(&mut fixture.runner, first, furnace));
    fixture.runner.advance_until_stack_empty();
    assert_prevented_tail(&fixture, &events, p1_life_before, expected_prevented);
}

/// Declare the 1/1 into P1 with no blockers and drive to the ordering prompt,
/// collecting every event along the way.
fn drive_combat_to_choice(fixture: &mut Fixture) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let mut attacked = false;
    for _ in 0..80 {
        match fixture.runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { player, .. } if player == P0 && !attacked => {
                attacked = true;
                let result = fixture
                    .runner
                    .declare_attackers(&[(fixture.source, AttackTarget::Player(P1))])
                    .expect("declaring the 1/1 attacker must succeed");
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

/// Finish combat after the ordering answers, collecting every event (the
/// Squire trigger resolves on the way out).
fn finish_combat(fixture: &mut Fixture, events: &mut Vec<GameEvent>) {
    for _ in 0..200 {
        if matches!(
            fixture.runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            return;
        }
        match fixture.runner.state().waiting_for.clone() {
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
            other => panic!("unexpected prompt while finishing combat: {other:?}"),
        }
    }
    panic!("combat never finished");
}

fn run_combat_row(furnace_first: bool, expected_prevented: u32) {
    let Some(db) = load_db() else {
        eprintln!("card-data.json missing — skipping");
        return;
    };
    let mut fixture = setup(db, true);
    let p1_life_before = life_of(&fixture.runner, P1);

    let mut events = drive_combat_to_choice(&mut fixture);
    let (furnace_index, protection_index) =
        assert_two_candidate_prompt(&fixture.runner, fixture.furnace);
    let first = if furnace_first {
        furnace_index
    } else {
        protection_index
    };
    let furnace = fixture.furnace;
    events.extend(answer_then_drain(&mut fixture.runner, first, furnace));
    finish_combat(&mut fixture, &mut events);
    assert_prevented_tail(&fixture, &events, p1_life_before, expected_prevented);
}

/// CR 616.1 + CR 616.1f (non-combat): Furnace first doubles 1 → 2, then the
/// lone protection prevents 2 and the Squire consumes exactly 2.
#[test]
fn noncombat_furnace_first_prevents_two_and_squire_gets_two() {
    run_noncombat_row(true, 2);
}

/// CR 616.1 (non-combat): protection first prevents the original 1 and the
/// Squire consumes exactly 1; the Furnace never applies to the spent event.
#[test]
fn noncombat_protection_first_prevents_one_and_squire_gets_one() {
    run_noncombat_row(false, 1);
}

/// CR 616.1 + CR 616.1f (combat): Furnace first doubles 1 → 2, then the lone
/// protection prevents 2 and the Squire consumes exactly 2.
#[test]
fn combat_furnace_first_prevents_two_and_squire_gets_two() {
    run_combat_row(true, 2);
}

/// CR 616.1 (combat): protection first prevents the original 1 and the Squire
/// consumes exactly 1; the Furnace never applies to the spent event.
#[test]
fn combat_protection_first_prevents_one_and_squire_gets_one() {
    run_combat_row(false, 1);
}
