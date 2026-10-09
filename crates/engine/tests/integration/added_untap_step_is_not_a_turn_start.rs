//! An untap step an effect adds is not the start of a turn. CR 500.10 creates a
//! phase to hold the step, and CR 500.8 adds a whole beginning phase; neither
//! creates a turn. So the added step performs the untap step's own actions
//! (CR 502.3 untapping; CR 500.4: a duration until the untap step ends), and
//! nothing whose deadline is "your next turn" ends or is armed in it
//! (CR 611.2a).
//!
//! Two shapes add an untap step: Untap, Upkeep, Draw's untap mode (a phase
//! created for the step) and Temple of Atropos's "additional beginning phase
//! after this phase", seeded here as the scheduled unit it installs.

use engine::ai_support::legal_actions;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Duration, PlayerScope};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{ExtraPhase, GameState, WaitingFor};
use engine::types::identifiers::{ExtraPhaseId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::{Phase, PhaseGroup, TurnSegment};
use engine::types::zones::Zone;

/// Verbatim Oracle text (pinned MTGJSON AtomicCards).
const UNTAP_UPKEEP_DRAW: &str = "Choose one \u{2014}\n\u{2022} After this phase, there is an additional untap step.\n\u{2022} After this phase, there is an additional upkeep step.\n\u{2022} After this phase, there is an additional draw step.\nEntwine {3} (Choose all of them if you pay the entwine cost.)";
/// Verbatim Oracle text (pinned MTGJSON AtomicCards).
const SYLVAN_AWAKENING: &str = "Until your next turn, all lands you control become 2/2 Elemental creatures with reach, indestructible, and haste. They're still lands.";
/// Verbatim Oracle text (pinned MTGJSON AtomicCards).
const TAUNT_FROM_THE_RAMPART: &str = "Goad all creatures your opponents control. Until your next turn, those creatures can't block. (Until your next turn, those creatures attack each combat if able and attack a player other than you if able.)";
/// Verbatim Oracle text (pinned MTGJSON AtomicCards).
const FROST_BREATH: &str = "Tap up to two target creatures. Those creatures don't untap during their controller's next untap step.";
/// Verbatim Oracle text (pinned MTGJSON AtomicCards).
const NIVIX: &str = "{T}: Add {C}.\n{2}{U}{R}, {T}: Exile the top card of your library. Until your next turn, you may cast it if it's an instant or sorcery spell.";
/// Verbatim Oracle text (pinned MTGJSON AtomicCards).
const LIGHT_UP_THE_STAGE: &str = "Spectacle {R} (You may cast this spell for its spectacle cost rather than its mana cost if an opponent lost life this turn.)\nExile the top two cards of your library. Until the end of your next turn, you may play those cards.";
/// No printed card grants a play permission until an untap step (census:
/// `p8b-logs/grant_census.log`), so the wording is assembled from an impulse
/// grant and the untap-step deadline the duration grammar reads for a
/// permission. The grammar refuses "your next untap step" by design
/// (`oracle_nom::duration::step_deadline_scope`); "the next untap step" names
/// the first untap step to occur, whoever's it is.
const UNTAP_STEP_GRANT: &str =
    "When ~ enters, exile the top card of your library. Until the next untap step, you may play that card.";

/// Adds Untap, Upkeep, Draw to P0's hand at {1}{U}, with two Islands to pay
/// for it. Returns the spell and the Islands; the Islands tapped for the spell
/// untap only if an untap step runs.
fn add_untap_upkeep_draw(scenario: &mut GameScenario) -> (ObjectId, [ObjectId; 2]) {
    let islands = [
        scenario.add_basic_land(P0, ManaColor::Blue),
        scenario.add_basic_land(P0, ManaColor::Blue),
    ];
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Untap, Upkeep, Draw", false, UNTAP_UPKEEP_DRAW)
        .from_oracle_text_with_keywords(&["Entwine"], UNTAP_UPKEEP_DRAW)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    (spell, islands)
}

/// A board in P0's precombat main phase with library cards for both players,
/// so every draw the tests pass through has a card to draw.
fn scenario_at_main() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..3 {
        scenario.add_card_to_library_top(P0, "Island");
        scenario.add_card_to_library_top(P1, "Island");
    }
    scenario
}

/// CR 500.8: the unit Temple of Atropos schedules — an additional beginning
/// phase after this postcombat main phase.
fn schedule_beginning_phase_after_postcombat_main(runner: &mut GameRunner) {
    runner.state_mut().extra_phases.push(ExtraPhase {
        anchor: Phase::PostCombatMain,
        segment: TurnSegment::Phase(PhaseGroup::Beginning),
        attacker_restriction: None,
        attacker_restriction_source: None,
        id: ExtraPhaseId::default(),
    });
}

/// Drive the game until `stop` holds, answering each prompt the board raises:
/// priority passes, P0 declares no attackers, P1 attacks P0 with every
/// creature it may, and no one blocks. Returns every step entered.
fn drive_until(runner: &mut GameRunner, stop: impl Fn(&GameState) -> bool) -> Vec<Phase> {
    let mut steps = Vec::new();
    for _ in 0..200 {
        if stop(runner.state()) {
            return steps;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers {
                player,
                valid_attacker_ids,
                ..
            } => GameAction::DeclareAttackers {
                attacks: if *player == P1 {
                    valid_attacker_ids
                        .iter()
                        .map(|id| (*id, AttackTarget::Player(P0)))
                        .collect()
                } else {
                    Vec::new()
                },
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: Vec::new(),
            },
            other => panic!("unexpected prompt while driving: {other:?}"),
        };
        let result = runner.act(action).expect("driver action accepted");
        steps.extend(result.events.iter().filter_map(|event| match event {
            GameEvent::PhaseChanged { phase } => Some(*phase),
            _ => None,
        }));
    }
    panic!("stop condition never held; steps entered: {steps:?}");
}

fn at_step_of(turn: u32, phase: Phase) -> impl Fn(&GameState) -> bool {
    move |state| state.turn_number == turn && state.phase == phase
}

fn tapped(runner: &GameRunner, ids: &[ObjectId]) -> Vec<bool> {
    ids.iter()
        .map(|id| runner.state().objects[id].tapped)
        .collect()
}

fn creatures(runner: &GameRunner, ids: &[ObjectId]) -> Vec<bool> {
    ids.iter()
        .map(|id| {
            runner.state().objects[id]
                .card_types
                .core_types
                .contains(&CoreType::Creature)
        })
        .collect()
}

fn goaded_by_p0(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].goaded_by.contains(&P0)
}

fn can_cast(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id))
}

/// The durations recorded on `id`'s casting permissions, in grant order.
fn permission_durations(runner: &GameRunner, id: ObjectId) -> Vec<Option<Duration>> {
    runner.state().objects[&id]
        .casting_permissions
        .iter()
        .map(|permission| permission.lifetime().duration.cloned())
        .collect()
}

/// An "until your next turn" board: Sylvan Awakening makes P0's Islands
/// creatures, and Taunt from the Rampart goads P1's creature (CR 701.15a).
struct Awakened {
    runner: GameRunner,
    spell: ObjectId,
    islands: [ObjectId; 2],
    foe: ObjectId,
}

fn awakened_board() -> Awakened {
    let mut scenario = scenario_at_main();
    let (spell, islands) = add_untap_upkeep_draw(&mut scenario);
    let foe = scenario.add_vanilla(P1, 2, 2);
    let awakening = scenario
        .add_spell_to_hand_from_oracle(P0, "Sylvan Awakening", false, SYLVAN_AWAKENING)
        .id();
    let taunt = scenario
        .add_spell_to_hand_from_oracle(P0, "Taunt from the Rampart", false, TAUNT_FROM_THE_RAMPART)
        .id();
    let mut runner = scenario.build();
    runner.cast(awakening).resolve();
    runner.cast(taunt).resolve();
    // Reach guards: both effects are in place before any added step.
    assert_eq!(
        creatures(&runner, &islands),
        vec![true, true],
        "reach guard: land creatures"
    );
    assert!(goaded_by_p0(&runner, foe), "reach guard: goad");
    Awakened {
        runner,
        spell,
        islands,
        foe,
    }
}

/// CR 611.2a + CR 500.10 (T-TSa): "until your next turn" lasts until the turn
/// that begins with P0's next natural untap step. The untap step Untap, Upkeep,
/// Draw adds untaps the Islands, and leaves them creatures and the foe goaded.
#[test]
fn created_untap_step_keeps_until_your_next_turn_effects() {
    let Awakened {
        mut runner,
        spell,
        islands,
        foe,
    } = awakened_board();
    runner.cast(spell).modes(&[0]).resolve();
    assert_eq!(
        tapped(&runner, &islands),
        vec![true, true],
        "reach guard: paid"
    );

    let turn = runner.state().turn_number;
    let steps = drive_until(&mut runner, at_step_of(turn, Phase::BeginCombat));

    assert_eq!(steps, vec![Phase::Untap, Phase::BeginCombat]);
    assert_eq!(
        tapped(&runner, &islands),
        vec![false, false],
        "reach guard: the added step ran"
    );
    assert_eq!(
        (creatures(&runner, &islands), goaded_by_p0(&runner, foe)),
        (vec![true, true], true),
        "(land creatures, goaded)"
    );
}

/// CR 611.2a + CR 500.8 (T-TSb): the same across the untap step of an added
/// beginning phase.
#[test]
fn added_beginning_phase_keeps_until_your_next_turn_effects() {
    let Awakened {
        mut runner,
        islands,
        foe,
        ..
    } = awakened_board();
    schedule_beginning_phase_after_postcombat_main(&mut runner);

    let turn = runner.state().turn_number;
    let steps = drive_until(&mut runner, at_step_of(turn, Phase::End));

    let beginning_phase = [
        Phase::PostCombatMain,
        Phase::Untap,
        Phase::Upkeep,
        Phase::Draw,
        Phase::End,
    ];
    assert!(
        steps.windows(5).any(|window| window == beginning_phase),
        "reach guard: the added beginning phase ran: {steps:?}"
    );
    assert_eq!(
        (creatures(&runner, &islands), goaded_by_p0(&runner, foe)),
        (vec![true, true], true),
        "(land creatures, goaded)"
    );
}

/// CR 611.2a (T-TSc), the paired positive: the untap step that begins P0's
/// next turn still ends both effects.
#[test]
fn the_next_turns_own_untap_step_ends_them() {
    let Awakened {
        mut runner,
        spell,
        islands,
        foe,
    } = awakened_board();
    runner.cast(spell).modes(&[0]).resolve();
    let turn = runner.state().turn_number;
    drive_until(&mut runner, at_step_of(turn, Phase::BeginCombat));
    assert_eq!(
        creatures(&runner, &islands),
        vec![true, true],
        "reach guard"
    );

    drive_until(&mut runner, at_step_of(turn + 1, Phase::Upkeep));
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(
        creatures(&runner, &islands),
        vec![true, true],
        "reach guard: the opponent's turn does not end P0's effects"
    );
    assert!(
        goaded_by_p0(&runner, foe),
        "reach guard: goad lasts through P1's turn"
    );

    drive_until(&mut runner, at_step_of(turn + 2, Phase::Upkeep));
    assert_eq!(runner.state().active_player, P0);
    assert_eq!(creatures(&runner, &islands), vec![false, false]);
    assert!(!goaded_by_p0(&runner, foe));
}

/// CR 502.3 + CR 500.4 (T-TSd): an added untap step performs the untap step's
/// own actions. Frost Breath's "don't untap during their controller's next
/// untap step" holds the creature tapped there and ends, and the Islands
/// untap.
#[test]
fn an_added_untap_step_performs_the_untap_steps_own_actions() {
    let mut scenario = scenario_at_main();
    let (spell, islands) = add_untap_upkeep_draw(&mut scenario);
    let bear = scenario.add_vanilla(P0, 2, 2);
    let frost_breath = scenario
        .add_spell_to_hand_from_oracle(P0, "Frost Breath", true, FROST_BREATH)
        .id();
    let mut runner = scenario.build();
    runner.cast(frost_breath).target_object(bear).resolve();
    let holds = |runner: &GameRunner| {
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .filter(|effect| {
                effect.duration
                    == Duration::UntilNextStepOf {
                        step: Phase::Untap,
                        player: PlayerScope::Controller,
                    }
            })
            .count()
    };
    assert!(
        tapped(&runner, &[bear])[0],
        "reach guard: Frost Breath tapped it"
    );
    assert_eq!(
        holds(&runner),
        1,
        "reach guard: the untap-step duration is in place"
    );
    runner.cast(spell).modes(&[0]).resolve();

    let turn = runner.state().turn_number;
    let steps = drive_until(&mut runner, at_step_of(turn, Phase::BeginCombat));

    assert_eq!(steps, vec![Phase::Untap, Phase::BeginCombat], "reach guard");
    assert_eq!(
        tapped(&runner, &islands),
        vec![false, false],
        "CR 502.3: the untap ran"
    );
    assert!(
        tapped(&runner, &[bear])[0],
        "CR 502.3: the held creature stays tapped"
    );
    assert_eq!(
        holds(&runner),
        0,
        "CR 500.4: the duration ends in the added untap step"
    );
}

/// CR 611.2a + CR 500.10 (T-TSe, first kind): Nivix's "until your next turn"
/// cast permission survives an added untap step.
#[test]
fn until_your_next_turn_grant_survives_an_added_untap_step() {
    let mut scenario = scenario_at_main();
    let (spell, _) = add_untap_upkeep_draw(&mut scenario);
    for color in [
        ManaColor::Blue,
        ManaColor::Red,
        ManaColor::Blue,
        ManaColor::Blue,
    ] {
        scenario.add_basic_land(P0, color);
    }
    let nivix = scenario
        .add_land_from_oracle(P0, "Nivix, Aerie of the Firemind", NIVIX)
        .id();
    let exiled = scenario
        .add_spell_to_library_top(P0, "Library Instant", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.activate(nivix, 1).resolve();
    let until_your_next_turn = vec![Some(Duration::UntilNextTurnOf {
        player: PlayerScope::Controller,
    })];
    assert_eq!(
        runner.state().objects[&exiled].zone,
        Zone::Exile,
        "reach guard"
    );
    assert_eq!(
        permission_durations(&runner, exiled),
        until_your_next_turn,
        "reach guard"
    );
    runner.cast(spell).modes(&[0]).resolve();

    let turn = runner.state().turn_number;
    let steps = drive_until(&mut runner, at_step_of(turn, Phase::BeginCombat));

    assert_eq!(steps, vec![Phase::Untap, Phase::BeginCombat], "reach guard");
    assert_eq!(permission_durations(&runner, exiled), until_your_next_turn);
    assert!(can_cast(&runner, exiled));
}

/// CR 611.2a + CR 500.8 (T-TSe, second kind): Light Up the Stage's "until the
/// end of your next turn" grant is not armed by the untap step of an added
/// beginning phase, so it outlives this turn's cleanup step.
#[test]
fn until_the_end_of_your_next_turn_grant_is_not_armed_by_an_added_untap_step() {
    let mut scenario = scenario_at_main();
    let exiled = [
        scenario
            .add_spell_to_library_top(P0, "Library Top A", false)
            .with_mana_cost(ManaCost::zero())
            .id(),
        scenario
            .add_spell_to_library_top(P0, "Library Top B", false)
            .with_mana_cost(ManaCost::zero())
            .id(),
    ];
    let stage = scenario
        .add_spell_to_hand_from_oracle(P0, "Light Up the Stage", false, LIGHT_UP_THE_STAGE)
        .id();
    let mut runner = scenario.build();
    runner.cast(stage).resolve();
    let until_end_of_your_next_turn = vec![Some(Duration::UntilEndOfNextTurnOf {
        player: PlayerScope::Controller,
    })];
    for id in exiled {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile, "reach guard");
        assert_eq!(
            permission_durations(&runner, id),
            until_end_of_your_next_turn,
            "reach guard"
        );
    }
    schedule_beginning_phase_after_postcombat_main(&mut runner);

    let turn = runner.state().turn_number;
    let steps = drive_until(&mut runner, at_step_of(turn + 1, Phase::Upkeep));

    assert!(
        steps
            .windows(2)
            .any(|pair| pair == [Phase::PostCombatMain, Phase::Untap]),
        "reach guard: the added beginning phase ran: {steps:?}"
    );
    assert!(
        steps.contains(&Phase::Cleanup),
        "reach guard: this turn's cleanup ran"
    );
    for id in exiled {
        assert_eq!(
            permission_durations(&runner, id),
            until_end_of_your_next_turn
        );
    }
}

/// CR 500.4 (T-TSe, third kind): a grant until the next untap step ends in an
/// added untap step.
#[test]
fn until_the_next_untap_step_grant_ends_in_an_added_untap_step() {
    let mut scenario = scenario_at_main();
    let (spell, _) = add_untap_upkeep_draw(&mut scenario);
    let exiled = scenario
        .add_spell_to_library_top(P0, "Library Instant", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let granter = scenario
        .add_creature_to_hand_from_oracle(P0, "Untap Step Granter", 1, 1, UNTAP_STEP_GRANT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(granter).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&exiled].zone,
        Zone::Exile,
        "reach guard"
    );
    assert_eq!(
        permission_durations(&runner, exiled),
        vec![Some(Duration::UntilNextStepOf {
            step: Phase::Untap,
            player: PlayerScope::AnyTurn,
        })],
        "reach guard"
    );
    assert!(can_cast(&runner, exiled), "reach guard");
    runner.cast(spell).modes(&[0]).resolve();

    let turn = runner.state().turn_number;
    let steps = drive_until(&mut runner, at_step_of(turn, Phase::BeginCombat));

    assert_eq!(steps, vec![Phase::Untap, Phase::BeginCombat], "reach guard");
    assert_eq!(
        permission_durations(&runner, exiled),
        Vec::<Option<Duration>>::new()
    );
    assert!(!can_cast(&runner, exiled));
}
