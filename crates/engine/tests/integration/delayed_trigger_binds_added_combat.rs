//! CR 603.7a + CR 500.6 + CR 500.8: "At the beginning of that combat" names the
//! combat phase the preceding instruction added, and triggers when that combat
//! begins, not at the next beginning of combat. When no combat is added, no
//! delayed trigger is created.
//!
//! Oracle text and rulings verified on Scryfall (Moraug, Fury of Akoum; World
//! at War; Swinging Ship; Exploration).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    DelayedTriggerCondition, Effect, EffectKind, QuantityExpr, ResolvedAbility, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{DelayedTrigger, ExtraPhase, WaitingFor};
use engine::types::identifiers::{ExtraPhaseId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::{Phase, PhaseGroup, TurnSegment};
use engine::types::zones::Zone;

const MORAUG: &str = "Each creature you control gets +1/+0 for each time it has attacked this turn.
Landfall — Whenever a land you control enters, if it's your main phase, there's an additional combat phase after this phase. At the beginning of that combat, untap all creatures you control.";

const EXPLORATION: &str = "You may play an additional land on each of your turns.";

const WORLD_AT_WAR: &str = "After the second main phase this turn, there's an additional combat phase followed by an additional main phase. At the beginning of that combat, untap all creatures that attacked this turn.
Rebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";
// The scenario builder parses abilities from Oracle text but does not infer
// keywords from it, so each World at War carries `Keyword::Rebound` explicitly.

const SWINGING_SHIP: &str = "Visit — After the first combat phase this turn, there's an additional combat phase. At the beginning of that combat, untap all creatures that attacked this turn.";

/// Passes through the turn until `done` holds, answering each declaration with
/// `attackers` (each attacking P1; empty = no attack) and no blockers. Returns
/// every event on the way.
fn drive_until(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    done: impl Fn(&GameRunner) -> bool,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..300 {
        if done(runner) {
            return events;
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
        events.extend(runner.act(action).expect("the turn advances").events);
    }
    panic!("the stop condition was never reached");
}

/// Stop once the turn's `n`th combat phase has moved past its beginning of
/// combat step, so everything that triggered as it began has resolved.
fn past_beginning_of_combat(n: u32) -> impl Fn(&GameRunner) -> bool {
    move |r| {
        r.state().steps_started_this_turn.count(Phase::BeginCombat) == n
            && r.state().phase != Phase::BeginCombat
    }
}

/// Stop once `attacker` has attacked in the turn's `n`th combat phase.
fn attacked_in_combat(n: u32, attacker: ObjectId) -> impl Fn(&GameRunner) -> bool {
    move |r| {
        r.state().steps_started_this_turn.count(Phase::BeginCombat) == n
            && r.state().objects[&attacker].tapped
    }
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

fn tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
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

/// CR 603.7a + CR 500.8: two Moraug landfalls in the postcombat main phase add
/// two combats after it, and each untap belongs to its own combat. Ruling: "you'll
/// get two consecutive additional combat phases after your main phase
/// (untapping your creatures at the beginning of each)". The second landfall's
/// combat comes first (CR 500.8: the most recently created phase occurs first),
/// so a creature that attacks in it is untapped as the other added combat
/// begins.
#[test]
fn two_moraug_landfalls_untap_at_the_beginning_of_each_added_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Moraug, Fury of Akoum", 6, 6, MORAUG);
    scenario.add_enchantment_from_oracle(P0, "Exploration", EXPLORATION);
    let attacker = scenario.add_vanilla(P0, 2, 2);
    let first_land = scenario.add_land_to_hand(P0, "Wastes").id();
    let second_land = scenario.add_land_to_hand(P0, "Wastes").id();
    let mut runner = scenario.build();

    drive_until(&mut runner, &[], |r| {
        r.state().phase == Phase::PostCombatMain && r.state().stack.is_empty()
    });
    let first = play_land_and_resolve(&mut runner, first_land);
    let second = play_land_and_resolve(&mut runner, second_land);
    assert!(
        resolved_additional_phase(&first) && resolved_additional_phase(&second),
        "both landfall triggers resolved their additional phase"
    );
    assert_eq!(
        runner.state().extra_phases.len(),
        2,
        "two combats were added"
    );

    // The second landfall's combat begins first; the creature attacks in it.
    // Reach guard: the stop condition requires the attack to have tapped it.
    drive_until(&mut runner, &[attacker], attacked_in_combat(2, attacker));

    drive_until(&mut runner, &[], past_beginning_of_combat(3));
    assert!(
        !tapped(&runner, attacker),
        "the first landfall's untap fires as its own combat begins"
    );
}

/// CR 603.7a + CR 505.1a: World at War cast in the precombat main phase adds its
/// combat after the second main phase, and its untap waits for that combat.
/// Ruling: "those creatures untap when the new combat phase created by the
/// spell begins." A creature that attacks in the natural combat is untapped as
/// World at War's combat begins.
#[test]
fn world_at_war_in_precombat_main_untaps_at_the_combat_it_added() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_vanilla(P0, 2, 2);
    let world_at_war = scenario
        .add_spell_to_hand_from_oracle(P0, "World at War", false, WORLD_AT_WAR)
        .with_mana_cost(ManaCost::generic(0))
        .with_keyword(Keyword::Rebound)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(world_at_war).resolve();
    assert!(
        resolved_additional_phase(outcome.events()),
        "World at War resolved its additional phase"
    );
    runner.advance_until_stack_empty();

    // Reach guard: the creature attacks in the natural combat.
    drive_until(&mut runner, &[attacker], attacked_in_combat(1, attacker));

    drive_until(&mut runner, &[], past_beginning_of_combat(2));
    assert!(
        !tapped(&runner, attacker),
        "the creature that attacked in the natural combat untaps as World at \
         War's combat begins"
    );
}

/// CR 603.7a + CR 505.1b: a World at War resolving in the third main phase adds
/// no combat, so "that combat" names nothing and no delayed trigger is created.
/// Ruling: "if one World at War creates a second combat phase and a third main
/// phase in a turn, then a second World at War is cast during that third main
/// phase, no additional phases are created. The rebound effect still works,
/// though."
#[test]
fn late_world_at_war_creates_no_untap_trigger() {
    fn untap_triggers(runner: &GameRunner) -> usize {
        runner
            .state()
            .delayed_triggers
            .iter()
            .filter(|trigger| matches!(trigger.ability.effect, Effect::SetTapState { .. }))
            .count()
    }

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
    assert_eq!(
        untap_triggers(&runner),
        1,
        "positive control: the first World at War created its untap trigger"
    );

    // Drive to the third main phase: the second postcombat main phase begun.
    drive_until(&mut runner, &[], |r| {
        r.state().phase == Phase::PostCombatMain
            && r.state()
                .steps_started_this_turn
                .count(Phase::PostCombatMain)
                == 2
            && r.state().stack.is_empty()
    });
    assert_eq!(
        untap_triggers(&runner),
        0,
        "the first World at War's untap trigger fired at its combat"
    );

    let outcome = runner.cast(second).resolve();
    // Reach guards: the effect resolved, and rebound exiled the card.
    assert!(
        resolved_additional_phase(outcome.events()),
        "the second World at War's additional-phase effect resolved"
    );
    assert_eq!(outcome.zone_of(second), Zone::Exile, "rebound still works");
    runner.advance_until_stack_empty();
    assert!(runner.state().extra_phases.is_empty(), "no phase was added");
    assert_eq!(
        untap_triggers(&runner),
        0,
        "no combat was added, so no untap trigger was created"
    );
}

/// CR 717.4 + CR 505.5 + CR 603.7a: Swinging Ship visited as the precombat main
/// phase begins adds a combat after the first combat, and its untap waits for
/// that combat. Ruling: "At the beginning of each of those combats, any
/// creature that attacked during any combat that turn will untap."
#[test]
fn swinging_ship_visited_in_precombat_main_untaps_at_the_combat_it_added() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Draw);
    let attacker = scenario.add_vanilla(P0, 2, 2);
    let ship = scenario
        .add_artifact_from_oracle(P0, "Swinging Ship", SWINGING_SHIP)
        .with_subtypes(vec!["Attraction"])
        .from_oracle_text(SWINGING_SHIP)
        .id();
    let mut runner = scenario.build();
    // Every die result lights the Attraction, so the roll to visit visits it.
    runner
        .state_mut()
        .objects
        .get_mut(&ship)
        .unwrap()
        .attraction_lights = vec![1, 2, 3, 4, 5, 6];

    let visited = drive_until(&mut runner, &[], |r| {
        r.state().phase == Phase::PreCombatMain && r.state().stack.is_empty()
    });
    assert!(
        visited
            .iter()
            .any(|event| matches!(event, GameEvent::AttractionVisited { .. })),
        "reach guard: the roll to visit visited Swinging Ship"
    );
    assert!(
        resolved_additional_phase(&visited),
        "Swinging Ship's visit resolved its additional phase"
    );

    // Reach guard: the creature attacks in the natural combat.
    drive_until(&mut runner, &[attacker], attacked_in_combat(1, attacker));

    drive_until(&mut runner, &[], past_beginning_of_combat(2));
    assert!(
        !tapped(&runner, attacker),
        "the creature that attacked in the first combat untaps as Swinging \
         Ship's combat begins"
    );
}

/// T-U7a. CR 603.7a + CR 500.6 + CR 500.8: a trigger bound to one of two
/// combats added after the same phase fires as that combat begins, whatever
/// order the combats run in. The newer combat (B) runs first, then the older
/// (A), then the natural combat; the trigger is bound to A.
///
/// DISCRIMINATION: match on the step alone (drop the entry check in the
/// matcher) and the trigger fires as B, the turn's first combat, begins.
#[test]
fn a_trigger_bound_to_one_of_two_added_combats_fires_at_that_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_vanilla(P0, 1, 1);
    let mut runner = scenario.build();
    let [older, newer] = [ExtraPhaseId(1), ExtraPhaseId(2)];
    let state = runner.state_mut();
    for id in [older, newer] {
        state.extra_phases.push(ExtraPhase {
            anchor: Phase::PreCombatMain,
            segment: TurnSegment::Phase(PhaseGroup::Combat),
            attacker_restriction: None,
            attacker_restriction_source: None,
            id,
        });
    }
    state.delayed_triggers.push(DelayedTrigger::new(
        DelayedTriggerCondition::AtBeginningOfAddedPhase {
            phase: Phase::BeginCombat,
            entry: Some(older),
        },
        Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            P0,
        )),
        P0,
        source,
        true,
    ));
    let life = |runner: &GameRunner| runner.state().players[P0.0 as usize].life;
    let start = life(&runner);

    drive_until(&mut runner, &[], past_beginning_of_combat(1));
    assert_eq!(
        life(&runner),
        start,
        "the trigger does not fire as the other added combat begins"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach guard: B's combat began with the trigger still installed"
    );

    drive_until(&mut runner, &[], past_beginning_of_combat(2));
    assert_eq!(
        life(&runner),
        start + 3,
        "the trigger fires as the combat it names begins"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "CR 603.7b: the one-shot trigger is consumed"
    );

    drive_until(&mut runner, &[], past_beginning_of_combat(3));
    assert_eq!(life(&runner), start + 3, "CR 603.7b: it fires only once");
}
