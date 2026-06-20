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
//! THE "you control target opponent during their next turn" rider on the
//! seventh-plan-counter trigger remains honestly `Effect::Unimplemented` (a heavy
//! deferred mechanic) — this test covers only the second-draw body, which is
//! fully supported.
//!
//! THE BUG this discriminates: assertion (a) — a 2/1 Villain token with menace
//! is created — and assertion (b) — Construct gains exactly one plan counter —
//! both flip to failure if the second-draw trigger body fails to parse/resolve.
//! Assertion (c) — only ONE draw does not fire — discriminates the
//! NthDrawThisTurn=2 gate.

use engine::game::effects::draw::resolve as resolve_draw;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::stack::resolve_top;
use engine::game::triggers::process_triggers;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

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
