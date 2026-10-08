//! Untap, Upkeep, Draw — each mode adds one step after this phase, in a phase
//! created to hold only that step (CR 500.10). Entwined, all three are added
//! in written order, so the most recently added one runs first (CR 702.42b +
//! CR 500.8).

use engine::game::scenario::{CardBuilder, GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, ExtraPhaseAnchor, ExtraPhaseRecipient};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::{Phase, TurnSegment};

const NAME: &str = "Untap, Upkeep, Draw";
const ORACLE: &str = "Choose one \u{2014}\n\u{2022} After this phase, there is an additional untap step.\n\u{2022} After this phase, there is an additional upkeep step.\n\u{2022} After this phase, there is an additional draw step.\nEntwine {3} (Choose all of them if you pay the entwine cost.)";

/// A board in P0's precombat main phase holding Untap, Upkeep, Draw (mana cost
/// {1}{U}), `islands` untapped Islands to pay for it, and two library cards.
fn board(islands: usize) -> (GameScenario, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lands = (0..islands)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Blue))
        .collect();
    scenario.add_card_to_library_top(P0, "Island");
    scenario.add_card_to_library_top(P0, "Island");
    let spell = add_spell(&mut scenario).id();
    (scenario, spell, lands)
}

/// Puts a copy of Untap, Upkeep, Draw into P0's hand.
fn add_spell(scenario: &mut GameScenario) -> CardBuilder<'_> {
    let mut spell = scenario.add_spell_to_hand_from_oracle(P0, NAME, false, ORACLE);
    spell
        .from_oracle_text_with_keywords(&["Entwine"], ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        });
    spell
}

/// Passes priority until the beginning of combat, returning every step the
/// turn entered on the way (starting with the current one).
fn steps_to_combat(runner: &mut GameRunner) -> Vec<Phase> {
    let turn = runner.state().turn_number;
    let mut steps = vec![runner.state().phase];
    while runner.state().phase != Phase::BeginCombat {
        let result = runner.act(GameAction::PassPriority).expect("pass priority");
        steps.extend(result.events.iter().filter_map(|event| match event {
            GameEvent::PhaseChanged { phase } => Some(*phase),
            _ => None,
        }));
        assert_eq!(runner.state().turn_number, turn, "left the turn: {steps:?}");
    }
    steps
}

fn tapped(runner: &GameRunner, lands: &[ObjectId]) -> Vec<bool> {
    lands
        .iter()
        .map(|id| runner.state().objects[id].tapped)
        .collect()
}

/// CR 500.10 + CR 700.2: each of the card's three modes adds its step in a
/// phase created after this phase, for the turn in progress ("there is").
#[test]
fn each_mode_parses_to_a_created_phase_after_this_phase() {
    let parsed = parse_oracle_text(
        ORACLE,
        NAME,
        &["Entwine".to_string()],
        &["Sorcery".to_string()],
        &[],
    );
    let segments: Vec<TurnSegment> = parsed
        .abilities
        .iter()
        .map(|ability| match &*ability.effect {
            Effect::AdditionalPhase {
                recipient: ExtraPhaseRecipient::NoPlayer,
                segment,
                after: ExtraPhaseAnchor::ThisPhase { named: None },
                followed_by,
                ..
            } if followed_by.is_empty() => *segment,
            other => panic!("expected an added step after this phase, got {other:?}"),
        })
        .collect();
    assert_eq!(
        segments,
        vec![
            TurnSegment::CreatedPhase(Phase::Untap),
            TurnSegment::CreatedPhase(Phase::Upkeep),
            TurnSegment::CreatedPhase(Phase::Draw),
        ]
    );
}

/// CR 500.10 + CR 502.3: the created phase holds only the untap step, so the
/// lands tapped to cast the spell untap and the turn goes on to combat with no
/// upkeep or draw step.
#[test]
fn untap_mode_untaps_in_a_created_phase_and_resumes_at_combat() {
    let (scenario, spell, lands) = board(2);
    let mut runner = scenario.build();
    runner.cast(spell).modes(&[0]).resolve();
    assert_eq!(tapped(&runner, &lands), vec![true, true], "paid with both");

    let steps = steps_to_combat(&mut runner);

    assert_eq!(
        steps,
        vec![Phase::PreCombatMain, Phase::Untap, Phase::BeginCombat]
    );
    assert_eq!(tapped(&runner, &lands), vec![false, false]);
}

/// CR 500.10 + CR 503.1: the upkeep mode adds only an upkeep step.
#[test]
fn upkeep_mode_adds_only_an_upkeep_step() {
    let (scenario, spell, _) = board(2);
    let mut runner = scenario.build();
    runner.cast(spell).modes(&[1]).resolve();

    assert_eq!(
        steps_to_combat(&mut runner),
        vec![Phase::PreCombatMain, Phase::Upkeep, Phase::BeginCombat]
    );
}

/// CR 500.10 + CR 504.1: the draw mode adds a draw step, whose turn-based
/// action draws a card.
#[test]
fn draw_mode_adds_a_draw_step_that_draws() {
    let (scenario, spell, _) = board(2);
    let mut runner = scenario.build();
    runner.cast(spell).modes(&[2]).resolve();
    let hand = runner.state().players[0].hand.len();

    let steps = steps_to_combat(&mut runner);

    assert_eq!(
        steps,
        vec![Phase::PreCombatMain, Phase::Draw, Phase::BeginCombat]
    );
    assert_eq!(runner.state().players[0].hand.len(), hand + 1);
}

/// CR 702.42a + CR 702.42b + CR 500.8: entwined, the modes are followed in
/// written order, so the draw step, added last, runs first. The entwine cost
/// is paid on top of the mana cost ({1}{U} + {3}).
#[test]
fn entwined_cast_runs_draw_upkeep_then_untap() {
    let (scenario, spell, lands) = board(5);
    let mut runner = scenario.build();
    runner.cast(spell).modes(&[0, 1, 2]).resolve();
    assert_eq!(tapped(&runner, &lands), vec![true; 5], "entwine cost paid");
    let hand = runner.state().players[0].hand.len();

    let steps = steps_to_combat(&mut runner);

    assert_eq!(
        steps,
        vec![
            Phase::PreCombatMain,
            Phase::Draw,
            Phase::Upkeep,
            Phase::Untap,
            Phase::BeginCombat,
        ]
    );
    assert_eq!(tapped(&runner, &lands), vec![false; 5]);
    assert_eq!(runner.state().players[0].hand.len(), hand + 1);
}

/// CR 500.8 + CR 500.10 + CR 702.8a: with flash granted, a second copy cast
/// inside the upkeep-only phase the first copy created resolves there, so its
/// draw mode's "after this phase" is after that created phase: the draw step
/// runs before the natural combat, this turn.
#[test]
fn draw_mode_cast_inside_the_created_upkeep_phase_runs_after_it() {
    let (mut scenario, first, _) = board(4);
    let second = add_spell(&mut scenario).flash().id();
    let mut runner = scenario.build();
    runner.cast(first).modes(&[1]).resolve();
    while runner.state().phase != Phase::Upkeep {
        runner.act(GameAction::PassPriority).expect("pass priority");
        assert_ne!(
            runner.state().phase,
            Phase::BeginCombat,
            "no created upkeep"
        );
    }
    runner.cast(second).modes(&[2]).resolve();
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "resolved in the upkeep"
    );
    let hand = runner.state().players[0].hand.len();

    let steps = steps_to_combat(&mut runner);

    assert_eq!(steps, vec![Phase::Upkeep, Phase::Draw, Phase::BeginCombat]);
    assert_eq!(runner.state().players[0].hand.len(), hand + 1);
}
