//! CR 500.8 + CR 505.1a + CR 505.1b: World at War's "after the second main
//! phase this turn" names the turn's first postcombat main phase. The added
//! combat and main phase follow that phase, and a World at War resolving after
//! it has ended adds nothing.
//!
//! Oracle text and rulings verified on Scryfall (World at War).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ExtraPhase, WaitingFor};
use engine::types::identifiers::ExtraPhaseId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::{Phase, PhaseGroup, TurnSegment};
use engine::types::zones::Zone;

const WORLD_AT_WAR: &str = "After the second main phase this turn, there's an additional combat phase followed by an additional main phase. At the beginning of that combat, untap all creatures that attacked this turn.
Rebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";
// The scenario builder parses abilities from Oracle text but does not infer
// keywords from it, so each World at War carries `Keyword::Rebound` explicitly.

/// Passes through the turn, declaring no attackers and no blockers, until
/// `done` holds. Returns every step entered on the way, in order.
fn drive_until(runner: &mut GameRunner, done: impl Fn(&GameRunner) -> bool) -> Vec<Phase> {
    let mut entered = Vec::new();
    for _ in 0..300 {
        if done(runner) {
            return entered;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
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

/// CR 500.8 + CR 505.1a: cast in the precombat main phase, World at War adds
/// its combat and main phase after the second main phase, which is the natural
/// postcombat main phase. Ruling: "beginning phase, precombat main phase,
/// combat phase, postcombat main phase, [new combat phase], [new postcombat
/// main phase], ending phase."
#[test]
fn world_at_war_in_precombat_main_adds_its_phases_after_the_second_main_phase() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let world_at_war = scenario
        .add_spell_to_hand_from_oracle(P0, "World at War", false, WORLD_AT_WAR)
        .with_mana_cost(ManaCost::generic(0))
        .with_keyword(Keyword::Rebound)
        .id();

    let mut runner = scenario.build();
    runner.cast(world_at_war).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().extra_phases,
        vec![
            ExtraPhase {
                anchor: Phase::PostCombatMain,
                segment: TurnSegment::Phase(PhaseGroup::PostcombatMain),
                attacker_restriction: None,
                attacker_restriction_source: None,
                id: ExtraPhaseId(1),
            },
            ExtraPhase {
                anchor: Phase::PostCombatMain,
                segment: TurnSegment::Phase(PhaseGroup::Combat),
                attacker_restriction: None,
                attacker_restriction_source: None,
                id: ExtraPhaseId(2),
            },
        ],
        "both added phases follow the second main phase (CR 505.1a: the first \
         postcombat main phase)"
    );

    let entered = drive_until(&mut runner, |r| r.state().phase == Phase::End);

    assert_eq!(
        milestones(&entered),
        vec![
            Phase::BeginCombat,
            Phase::PostCombatMain,
            Phase::BeginCombat,
            Phase::PostCombatMain,
            Phase::End,
        ],
        "natural combat, second main phase, World at War's combat and main \
         phase, then the ending phase; entered {entered:?}"
    );
    let tally = &runner.state().steps_started_this_turn;
    assert_eq!(tally.count(Phase::BeginCombat), 2);
    assert_eq!(tally.count(Phase::PostCombatMain), 2);
    assert!(runner.state().extra_phases.is_empty());
    assert!(runner.state().extra_phase_resume.is_empty());
}

/// CR 500.8 + CR 505.1b: cast in the second main phase (the natural
/// postcombat main phase, in progress), World at War adds its combat and main
/// phase after it. Ruling: "As long as World at War resolves before the first
/// postcombat main phase of a turn ends, it will have its full effect."
#[test]
fn world_at_war_in_the_second_main_phase_adds_its_phases_after_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let world_at_war = scenario
        .add_spell_to_hand_from_oracle(P0, "World at War", false, WORLD_AT_WAR)
        .with_mana_cost(ManaCost::generic(0))
        .with_keyword(Keyword::Rebound)
        .id();

    let mut runner = scenario.build();
    drive_until(&mut runner, |r| {
        r.state().phase == Phase::PostCombatMain && r.state().stack.is_empty()
    });
    assert_eq!(
        runner
            .state()
            .steps_started_this_turn
            .count(Phase::PostCombatMain),
        1,
        "the second main phase is in progress"
    );

    runner.cast(world_at_war).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().extra_phases,
        vec![
            ExtraPhase {
                anchor: Phase::PostCombatMain,
                segment: TurnSegment::Phase(PhaseGroup::PostcombatMain),
                attacker_restriction: None,
                attacker_restriction_source: None,
                id: ExtraPhaseId(1),
            },
            ExtraPhase {
                anchor: Phase::PostCombatMain,
                segment: TurnSegment::Phase(PhaseGroup::Combat),
                attacker_restriction: None,
                attacker_restriction_source: None,
                id: ExtraPhaseId(2),
            },
        ],
        "the second main phase has not ended, so both added phases follow it"
    );

    let entered = drive_until(&mut runner, |r| r.state().phase == Phase::End);
    assert_eq!(
        milestones(&entered),
        vec![Phase::BeginCombat, Phase::PostCombatMain, Phase::End],
        "World at War's combat and main phase, then the ending phase; entered \
         {entered:?}"
    );
    let tally = &runner.state().steps_started_this_turn;
    assert_eq!(tally.count(Phase::BeginCombat), 2);
    assert_eq!(tally.count(Phase::PostCombatMain), 2);
    assert!(runner.state().extra_phases.is_empty());
    assert!(runner.state().extra_phase_resume.is_empty());
}

/// CR 505.1b + CR 500.8: a World at War that resolves in the third main phase
/// (the main phase another World at War created) adds nothing, because the
/// second main phase has already ended. Ruling: "if one World at War creates a
/// second combat phase and a third main phase in a turn, then a second World
/// at War is cast during that third main phase, no additional phases are
/// created. The rebound effect still works, though."
#[test]
fn world_at_war_in_the_third_main_phase_adds_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "World at War", false, WORLD_AT_WAR)
        .with_mana_cost(ManaCost::generic(0))
        .with_keyword(Keyword::Rebound)
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "World at War", false, WORLD_AT_WAR)
        .with_mana_cost(ManaCost::generic(0))
        .with_keyword(Keyword::Rebound)
        .id();

    let mut runner = scenario.build();
    runner.cast(first).resolve();
    runner.advance_until_stack_empty();

    // Drive to the third main phase: the second postcombat main phase begun.
    drive_until(&mut runner, |r| {
        r.state().phase == Phase::PostCombatMain
            && r.state()
                .steps_started_this_turn
                .count(Phase::PostCombatMain)
                == 2
            && r.state().stack.is_empty()
    });
    assert!(runner.state().extra_phases.is_empty());

    let outcome = runner.cast(second).resolve();
    // Reach guards: the effect resolved, and rebound exiled the card.
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::AdditionalPhase,
                ..
            }
        )),
        "the second World at War's additional-phase effect resolved"
    );
    assert_eq!(outcome.zone_of(second), Zone::Exile, "rebound still works");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().extra_phases.is_empty(),
        "the second main phase has ended, so no phases are added; got {:?}",
        runner.state().extra_phases
    );

    let entered = drive_until(&mut runner, |r| r.state().phase == Phase::End);
    assert_eq!(
        milestones(&entered),
        vec![Phase::End],
        "the ending phase follows the third main phase; entered {entered:?}"
    );
    let tally = &runner.state().steps_started_this_turn;
    assert_eq!(tally.count(Phase::BeginCombat), 2);
    assert_eq!(tally.count(Phase::PostCombatMain), 2);
}
