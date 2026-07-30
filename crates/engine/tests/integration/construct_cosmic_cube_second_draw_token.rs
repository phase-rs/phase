//! Construct a Cosmic Cube — "Whenever you draw your second card each turn,
//! create a 2/1 black Villain creature token with menace and put a plan counter
//! on this enchantment."
//!
//! This drives the REAL parse → trigger → stack pipeline: Construct is built from
//! Oracle text via the scenario harness (production synthesis path). The
//! second-card-each-turn trigger (`TriggerConstraint::NthDrawThisTurn { n: 2 }`)
//! fires off real `draw::resolve` events; the triggered ability resolves off the
//! stack, creating a 2/1 black Villain token with menace and putting a plan
//! counter on Construct.
//!
//! The "you control target opponent during their next turn" rider on the
//! seventh-plan-counter trigger now lowers to `Effect::ControlNextTurn` (CR 723)
//! via the shared turn-control subsystem and is driven end to end by
//! `construct_seventh_counter_rider_controls_opponents_next_turn` below.
//!
//! THE BUG `construct_second_draw_creates_villain_token_and_plan_counter`
//! discriminates: assertion (a) — a 2/1 Villain token with menace is created —
//! and assertion (b) — Construct gains exactly one plan counter — both flip to
//! failure if the second-draw trigger body fails to parse/resolve. Assertion
//! (c) — only ONE draw does not fire — discriminates the NthDrawThisTurn=2 gate.

use engine::game::effects::control_next_turn::resolve as resolve_control_next_turn;
use engine::game::effects::draw::resolve as resolve_draw;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::stack::resolve_top;
use engine::game::triggers::process_triggers;
use engine::game::turns::start_next_turn;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const CONSTRUCT: &str = "Whenever you draw your second card each turn, create a 2/1 black Villain creature token with menace and put a plan counter on this enchantment.\n\
When the seventh plan counter is put on this enchantment, sacrifice it. When you do, you control target opponent during their next turn.";

fn draw_one(runner: &mut GameRunner) {
    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        ObjectId(0),
        P0,
    );
    let mut events = Vec::new();
    resolve_draw(runner.state_mut(), &ability, &mut events).expect("draw resolves");
    process_triggers(runner.state_mut(), &events);
}

/// Count P0's battlefield Villain creature tokens with the given P/T.
fn villain_token_count(runner: &GameRunner, power: i32, toughness: i32) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| {
            obj.is_token
                && obj.controller == P0
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && obj.card_types.subtypes.iter().any(|s| s == "Villain")
                && obj.power == Some(power)
                && obj.toughness == Some(toughness)
        })
        .count()
}

#[test]
fn construct_second_draw_creates_villain_token_and_plan_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Construct a Cosmic Cube — an enchantment built from Oracle text through the
    // real parse + synthesis pipeline so the NthDraw trigger is installed. The
    // core type must be Enchantment BEFORE `from_oracle_text` runs so the parser
    // sees the enchantment type ("...on this enchantment" self-reference).
    let construct = scenario
        .add_creature(P0, "Construct a Cosmic Cube", 0, 0)
        .as_enchantment()
        .from_oracle_text(CONSTRUCT)
        .id();

    for i in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
    }

    let mut runner = scenario.build();

    // Baseline: no Villain tokens, no plan counters.
    assert_eq!(villain_token_count(&runner, 2, 1), 0);
    assert_eq!(
        runner.state().objects[&construct]
            .counters
            .get(&CounterType::Generic("plan".to_string()))
            .copied()
            .unwrap_or(0),
        0
    );

    // (c) First draw of the turn: trigger must NOT fire.
    draw_one(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        0,
        "first draw must not queue the second-draw trigger"
    );

    // Second draw: the trigger fires.
    draw_one(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the second draw fires 'draw your second card each turn'"
    );

    let mut events = Vec::new();
    resolve_top(runner.state_mut(), &mut events);

    // (a) A 2/1 black Villain creature token with menace is created.
    assert_eq!(
        villain_token_count(&runner, 2, 1),
        1,
        "CR 111.1: a 2/1 Villain creature token is created on the second draw"
    );
    let token = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .find(|obj| obj.is_token && obj.card_types.subtypes.iter().any(|s| s == "Villain"))
        .expect("the Villain token exists");
    assert!(
        token.keywords.contains(&Keyword::Menace),
        "CR 702.111: the Villain token has menace"
    );
    assert!(
        token.color.contains(&engine::types::mana::ManaColor::Black),
        "the Villain token is black"
    );

    // (b) Construct gains exactly one plan counter (CR 122.1).
    assert_eq!(
        runner.state().objects[&construct]
            .counters
            .get(&CounterType::Generic("plan".to_string()))
            .copied()
            .unwrap_or(0),
        1,
        "CR 122.1: a plan counter is put on Construct on the second draw"
    );

    // Sanity: Construct stays on the battlefield (the seventh-counter sacrifice
    // has not been reached — only one plan counter so far).
    assert_eq!(runner.state().objects[&construct].zone, Zone::Battlefield);
}

/// Recursively find the first `ControlNextTurn` effect in an ability tree
/// (the reflexive "When you do, ..." rider lives in `sub_ability`).
fn find_control_next_turn(def: &AbilityDefinition) -> Option<&Effect> {
    if matches!(*def.effect, Effect::ControlNextTurn { .. }) {
        return Some(&def.effect);
    }
    def.sub_ability
        .as_deref()
        .and_then(find_control_next_turn)
        .or_else(|| def.else_ability.as_deref().and_then(find_control_next_turn))
        .or_else(|| def.mode_abilities.iter().find_map(find_control_next_turn))
}

/// CR 723.1: Construct a Cosmic Cube's seventh-plan-counter reflexive rider
/// ("you control target opponent during their next turn") drives the real
/// turn-control subsystem end to end.
///
/// THE BUG this discriminates: the rider lowers to `Effect::ControlNextTurn`
/// only because the suffix combinator accepts the "their next turn" possessive
/// (Construct's phrasing). Reverting the parser fix makes the rider
/// `Effect::Unimplemented`, so `find_control_next_turn` returns `None` and the
/// `.expect(...)` below panics — the test fails.
///
/// Beyond parsing, this resolves the parsed effect through the production
/// `control_next_turn::resolve` and then advances turns through the real
/// `start_next_turn`, asserting that on the targeted opponent's next turn
/// `turn_decision_controller` is the Cube's controller (CR 723.1). Reverting any
/// of: the parse, the schedule push, or the turn-start activation breaks an
/// assertion here.
#[test]
fn construct_seventh_counter_rider_controls_opponents_next_turn() {
    // Parse the FULL card through the production parser, then extract the
    // ControlNextTurn effect from the seventh-counter reflexive rider.
    let parsed = parse_oracle_text(
        CONSTRUCT,
        "Construct a Cosmic Cube",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let rider_effect = parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find_map(find_control_next_turn)
        .expect("seventh-counter rider must lower to Effect::ControlNextTurn");
    // The target is "target opponent" → an opponent-scoped player filter.
    assert!(
        matches!(rider_effect, Effect::ControlNextTurn { .. }),
        "rider effect must be ControlNextTurn"
    );

    // Drive the parsed effect through the real resolver + turn engine.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let construct = scenario
        .add_creature(P0, "Construct a Cosmic Cube", 0, 0)
        .as_enchantment()
        .from_oracle_text(CONSTRUCT)
        .id();
    let mut runner = scenario.build();

    // P0 (Cube's controller) is the active player; the opponent is P1.
    assert_eq!(runner.state().active_player, P0);
    assert!(
        runner.state().scheduled_turn_controls.is_empty(),
        "precondition: no turn-control scheduled yet"
    );

    // Resolve the parsed ControlNextTurn effect targeting the opponent (P1),
    // exactly as the resolved reflexive ability would (target opponent → P1 in
    // two-player).
    let ability = ResolvedAbility::new(
        rider_effect.clone(),
        vec![TargetRef::Player(P1)],
        construct,
        P0,
    );
    let mut events = Vec::new();
    resolve_control_next_turn(runner.state_mut(), &ability, &mut events)
        .expect("ControlNextTurn resolves");

    // CR 723.1: a turn-control over P1 is scheduled for P1's next turn.
    assert_eq!(runner.state().scheduled_turn_controls.len(), 1);
    let scheduled = runner.state().scheduled_turn_controls[0];
    assert_eq!(scheduled.target_player, P1);
    assert_eq!(scheduled.controller, P0);
    assert!(
        !scheduled.grant_extra_turn_after,
        "Construct's rider does not grant an extra turn"
    );

    // CR 723.1: the control does NOT apply to the current (P0's) turn — it waits
    // for the affected player's next turn.
    assert_eq!(
        runner.state().turn_decision_controller,
        None,
        "control must not activate during the controller's own turn"
    );

    // Advance to P1's turn through the real turn engine.
    let mut events = Vec::new();
    start_next_turn(runner.state_mut(), &mut events);

    // CR 723.1 / CR 723.5: P1 is the active player but P0 makes P1's decisions.
    assert_eq!(runner.state().active_player, P1, "it is now P1's turn");
    assert_eq!(
        runner.state().turn_decision_controller,
        Some(P0),
        "CR 723.1: P0 controls P1 during P1's next turn"
    );
    // CR 723: the authorized submitter for P1's seat is the controller P0.
    assert_eq!(
        engine::game::turn_control::turn_decision_maker(runner.state()),
        P0,
        "CR 723.5: P0 is the decision-maker during the controlled turn"
    );

    // CR 723.1: the effect "doesn't end until the beginning of the next turn",
    // so the schedule is consumed only when the controlled turn COMPLETES.
    // Advance once more: control reverts and the schedule is cleared.
    let mut events = Vec::new();
    start_next_turn(runner.state_mut(), &mut events);
    assert_eq!(
        runner.state().active_player,
        P0,
        "control returns to P0's turn"
    );
    assert_eq!(
        runner.state().turn_decision_controller,
        None,
        "CR 723.1: control ends at the beginning of the turn after the controlled turn"
    );
    assert!(
        runner.state().scheduled_turn_controls.is_empty(),
        "the scheduled control is consumed once the controlled turn ends"
    );
}
