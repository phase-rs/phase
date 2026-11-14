//! CR 500.8 + CR 500.10a: an added combat phase follows the phase its text
//! names ("after this phase" is the phase the effect resolves in), and only an
//! effect that says a player gets the phase ("you get") is limited to that
//! player's turn. "There is / are" names no player, so the phase is added to
//! the turn in progress.
//!
//! Oracle text and rulings verified on Scryfall (Moraug, Fury of Akoum;
//! All-Out Assault; Take the Bait; Full Throttle).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ExtraPhase, GameState, WaitingFor};
use engine::types::identifiers::{ExtraPhaseId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::{Phase, PhaseGroup, TurnSegment};

const MORAUG: &str = "Each creature you control gets +1/+0 for each time it has attacked this turn.
Landfall — Whenever a land you control enters, if it's your main phase, there's an additional combat phase after this phase. At the beginning of that combat, untap all creatures you control.";

const ALL_OUT_ASSAULT: &str = "Creatures you control get +1/+1 and have deathtouch.
When this enchantment enters, if it's your main phase, there is an additional combat phase after this phase followed by an additional main phase. When you next attack this turn, untap each creature you control.";

const TAKE_THE_BAIT: &str = "Cast this spell only during combat on an opponent's turn.
Prevent all combat damage that would be dealt to you and planeswalkers you control this turn. Untap all attacking creatures and goad them. After this phase, there is an additional combat phase.";

const FULL_THROTTLE: &str = "After this main phase, there are two additional combat phases.
At the beginning of each combat this turn, untap all creatures that attacked this turn.";

/// An unrestricted added `segment`, taken after the step `anchor` ends.
fn added(anchor: Phase, segment: TurnSegment) -> ExtraPhase {
    ExtraPhase {
        anchor,
        segment,
        attacker_restriction: None,
        attacker_restriction_source: None,
        id: ExtraPhaseId::default(),
    }
}

/// The scheduled entries with their minted `id` cleared: these tests assert
/// anchors and phases, not identities.
fn scheduled(state: &GameState) -> Vec<ExtraPhase> {
    state
        .extra_phases
        .iter()
        .map(|entry| ExtraPhase {
            id: ExtraPhaseId::default(),
            ..entry.clone()
        })
        .collect()
}

/// Passes through the turn until `done` holds, answering each declaration
/// with `attackers` (each attacking P1; empty = no attack) and no blockers.
/// Returns every step entered on the way, in order.
fn drive_until(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    done: impl Fn(&GameRunner) -> bool,
) -> Vec<Phase> {
    let mut entered = Vec::new();
    for _ in 0..300 {
        if done(runner) {
            return entered;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: attackers
                    .iter()
                    .map(|id| (*id, AttackTarget::Player(P1)))
                    .collect(),
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            _ => GameAction::PassPriority,
        };
        let result = runner.act(action).expect("the turn advances");
        entered.extend(result.events.iter().filter_map(|event| match event {
            GameEvent::PhaseChanged { phase } => Some(*phase),
            _ => None,
        }));
    }
    panic!("the stop condition was never reached; entered {entered:?}");
}

/// The steps that begin a combat phase, a postcombat main phase or the ending
/// phase, in the order entered.
fn milestones(entered: &[Phase]) -> Vec<Phase> {
    entered
        .iter()
        .copied()
        .filter(|phase| {
            matches!(
                phase,
                Phase::BeginCombat | Phase::PostCombatMain | Phase::End
            )
        })
        .collect()
}

fn resolved_additional_phase(events: &[GameEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::AdditionalPhase,
                ..
            }
        )
    })
}

/// Plays `land` from P0's hand and resolves Moraug's landfall trigger.
fn play_land_and_resolve(runner: &mut GameRunner, land: ObjectId) -> Vec<GameEvent> {
    let card_id = runner.state().objects[&land].card_id;
    let mut events = runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("the land is playable")
        .events;
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("the trigger resolves")
                .events,
        );
    }
    assert!(
        runner.state().stack.is_empty(),
        "the landfall trigger resolved"
    );
    events
}

/// CR 500.8: Moraug's landfall resolving in the postcombat main phase adds a
/// combat phase after that main phase. Ruling: "if the landfall ability
/// resolves twice during your postcombat main phase, you'll get two
/// consecutive additional combat phases after your main phase … followed by
/// your ending phase."
#[test]
fn moraug_landfall_in_the_postcombat_main_phase_adds_a_combat_after_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Moraug, Fury of Akoum", 6, 6, MORAUG);
    let land = scenario.add_land_to_hand(P0, "Wastes").id();
    let mut runner = scenario.build();

    drive_until(&mut runner, &[], |r| {
        r.state().phase == Phase::PostCombatMain && r.state().stack.is_empty()
    });
    let events = play_land_and_resolve(&mut runner, land);
    assert!(
        resolved_additional_phase(&events),
        "the landfall trigger resolved its additional phase"
    );

    assert_eq!(
        scheduled(runner.state()),
        vec![added(
            Phase::PostCombatMain,
            TurnSegment::Phase(PhaseGroup::Combat)
        )],
        "the added combat follows the postcombat main phase the trigger resolved in"
    );
    let entered = drive_until(&mut runner, &[], |r| r.state().phase == Phase::End);
    assert_eq!(
        milestones(&entered),
        vec![Phase::BeginCombat, Phase::End],
        "the added combat, then the ending phase; entered {entered:?}"
    );
    assert_eq!(
        runner
            .state()
            .steps_started_this_turn
            .count(Phase::BeginCombat),
        2
    );
    assert!(runner.state().extra_phases.is_empty());
    assert!(runner.state().extra_phase_resume.is_empty());
}

/// CR 500.8: resolving in the precombat main phase, Moraug's landfall adds a
/// combat phase directly after that main phase, before the regular combat.
/// Ruling: "If the landfall ability resolves during your precombat main
/// phase, the additional combat phase will happen before your regular combat
/// phase."
#[test]
fn moraug_landfall_in_the_precombat_main_phase_adds_a_combat_before_the_regular_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Moraug, Fury of Akoum", 6, 6, MORAUG);
    let land = scenario.add_land_to_hand(P0, "Wastes").id();
    let mut runner = scenario.build();

    let events = play_land_and_resolve(&mut runner, land);
    assert!(
        resolved_additional_phase(&events),
        "the landfall trigger resolved its additional phase"
    );
    assert_eq!(
        scheduled(runner.state()),
        vec![added(
            Phase::PreCombatMain,
            TurnSegment::Phase(PhaseGroup::Combat)
        )],
        "the added combat follows the precombat main phase, so it comes first"
    );

    let entered = drive_until(&mut runner, &[], |r| r.state().phase == Phase::End);
    assert_eq!(
        milestones(&entered),
        vec![
            Phase::BeginCombat,
            Phase::BeginCombat,
            Phase::PostCombatMain,
            Phase::End
        ],
        "the added combat, the regular combat, then the postcombat main phase; \
         entered {entered:?}"
    );
    assert!(runner.state().extra_phases.is_empty());
    assert!(runner.state().extra_phase_resume.is_empty());
}

/// CR 500.8: All-Out Assault's "after this phase followed by an additional
/// main phase" in the precombat main phase adds its combat and main phase
/// after that main phase, so they come before the regular combat.
#[test]
fn all_out_assault_in_the_precombat_main_phase_adds_its_phases_after_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let assault = scenario
        .add_spell_to_hand_from_oracle(P0, "All-Out Assault", false, ALL_OUT_ASSAULT)
        .as_enchantment()
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner.cast(assault).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        scheduled(runner.state()),
        vec![
            added(
                Phase::PreCombatMain,
                TurnSegment::Phase(PhaseGroup::PostcombatMain)
            ),
            added(Phase::PreCombatMain, TurnSegment::Phase(PhaseGroup::Combat)),
        ],
        "both added phases follow the precombat main phase"
    );

    let entered = drive_until(&mut runner, &[], |r| r.state().phase == Phase::End);
    assert_eq!(
        milestones(&entered),
        vec![
            Phase::BeginCombat,
            Phase::PostCombatMain,
            Phase::BeginCombat,
            Phase::PostCombatMain,
            Phase::End,
        ],
        "the added combat and main phase, then the regular combat and \
         postcombat main phase; entered {entered:?}"
    );
    assert!(runner.state().extra_phases.is_empty());
    assert!(runner.state().extra_phase_resume.is_empty());
}

/// CR 500.10a: Take the Bait says "there is", not "you get", so the combat is
/// added to the turn in progress: the opponent whose turn it is gets it.
/// Ruling: "Take the Bait doesn't give the opponent whose turn it is an
/// additional main phase. They will move directly from the end of combat step
/// of one combat phase to the beginning of combat step of the next one."
#[test]
fn take_the_bait_on_an_opponents_turn_adds_a_combat_to_that_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let bait = scenario
        .add_spell_to_hand_from_oracle(P1, "Take the Bait", true, TAKE_THE_BAIT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    drive_until(&mut runner, &[attacker], |r| {
        r.state().phase == Phase::DeclareBlockers
            && matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P0)
    });
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 has priority in P0's declare blockers step"
    );

    let outcome = runner.cast(bait).resolve();
    // Reach guards: the spell resolved through its untap and goad clauses to
    // its additional-phase clause.
    assert!(
        !runner.state().objects[&attacker].tapped,
        "Take the Bait untapped the attacker"
    );
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Goad,
                ..
            }
        )),
        "the goad clause resolved"
    );
    assert!(
        resolved_additional_phase(outcome.events()),
        "the additional-phase clause resolved"
    );

    assert_eq!(
        scheduled(runner.state()),
        vec![added(
            Phase::EndCombat,
            TurnSegment::Phase(PhaseGroup::Combat)
        )],
        "the combat is added to P0's turn, after this combat phase"
    );
    let entered = drive_until(&mut runner, &[attacker], |r| r.state().phase == Phase::End);
    assert_eq!(
        milestones(&entered),
        vec![Phase::BeginCombat, Phase::PostCombatMain, Phase::End],
        "P0 moves from end of combat to the added combat, with no main phase \
         between; entered {entered:?}"
    );
    assert_eq!(runner.state().active_player, P0);
    assert_eq!(
        runner
            .state()
            .steps_started_this_turn
            .count(Phase::BeginCombat),
        2
    );
}

/// CR 500.10a: Full Throttle says "there are", so cast (with flash) in an
/// opponent's precombat main phase it adds two combats to that opponent's
/// turn. Ruling: "If you cast it during an opponent's main phase, there are
/// two additional combat phases, but that opponent gets to attack during those
/// combat phases, not you."
#[test]
fn full_throttle_in_an_opponents_main_phase_adds_two_combats_to_that_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let throttle = scenario
        .add_spell_to_hand_from_oracle(P1, "Full Throttle", false, FULL_THROTTLE)
        .with_mana_cost(ManaCost::generic(0))
        .with_keyword(Keyword::Flash)
        .id();
    let mut runner = scenario.build();

    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 has priority in P0's precombat main phase"
    );
    let outcome = runner.cast(throttle).resolve();
    assert!(
        resolved_additional_phase(outcome.events()),
        "the additional-phase effect resolved"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        scheduled(runner.state()),
        vec![added(Phase::PreCombatMain, TurnSegment::Phase(PhaseGroup::Combat)); 2],
        "two combats are added to P0's turn, after this main phase"
    );
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    let entered = drive_until(&mut runner, &[], |r| r.state().phase == Phase::End);
    assert_eq!(
        milestones(&entered),
        vec![
            Phase::BeginCombat,
            Phase::BeginCombat,
            Phase::BeginCombat,
            Phase::PostCombatMain,
            Phase::End,
        ],
        "two added combats, then P0's regular combat; entered {entered:?}"
    );
    assert_eq!(runner.state().active_player, P0);
}
