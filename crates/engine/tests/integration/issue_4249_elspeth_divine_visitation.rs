//! Issue #4249 — Elspeth, Storm Slayer + Divine Visitation: activating Elspeth's
//! +1 with both on the battlefield must not strand the game on a "Resolution
//! Order" prompt (CR 616.1 ReplacementChoice or CR 603.3b OrderTriggers).
//!
//! Both replacements commute on creature-token creation (double count then
//! substitute characteristics, or substitute then double — same 4/4 Angels).
//! The engine must auto-resolve without a degenerate ordering prompt.

use std::sync::Arc;

use engine::game::planeswalker;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ELSPETH_ORACLE: &str = "If one or more tokens would be created under your control, twice that many of those tokens are created instead.\n\
+1: Create a 1/1 white Soldier creature token.\n\
0: Put a +1/+1 counter on each creature you control. Those creatures gain flying until your next turn.\n\
−3: Destroy target creature an opponent controls with mana value 3 or greater.";

const DIVINE_VISITATION: &str = "If one or more creature tokens would be created under your control, that many 4/4 white Angel creature tokens with flying and vigilance are created instead.";

fn parsed_elspeth() -> engine::parser::oracle::ParsedAbilities {
    let parsed = parse_oracle_text(
        ELSPETH_ORACLE,
        "Elspeth, Storm Slayer",
        &[],
        &["Legendary".to_string()],
        &["Elspeth".to_string()],
    );
    assert!(
        !parsed.replacements.is_empty(),
        "Elspeth static doubler must parse as a replacement"
    );
    assert!(
        parsed.abilities.len() >= 3,
        "Elspeth must parse loyalty abilities, got {}",
        parsed.abilities.len()
    );
    parsed
}

fn install_divine_visitation(
    state: &mut engine::types::game_state::GameState,
) -> engine::types::identifiers::ObjectId {
    let parsed = parse_oracle_text(
        DIVINE_VISITATION,
        "Divine Visitation",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let id = create_object(
        state,
        CardId(900),
        P0,
        "Divine Visitation".to_string(),
        Zone::Battlefield,
    );
    let reps = parsed.replacements.clone();
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Enchantment];
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);
    id
}

fn wire_elspeth(
    state: &mut engine::types::game_state::GameState,
    elspeth: engine::types::identifiers::ObjectId,
    parsed: &engine::parser::oracle::ParsedAbilities,
) {
    let obj = state.objects.get_mut(&elspeth).expect("elspeth");
    obj.card_types.core_types = vec![CoreType::Planeswalker];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.loyalty = Some(5);
    obj.counters.insert(CounterType::Loyalty, 5);
    obj.abilities = Arc::new(parsed.abilities.clone());
    obj.base_abilities = Arc::new(parsed.abilities.clone());
    let reps = parsed.replacements.clone();
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);
}

#[test]
fn elspeth_plus_one_with_divine_visitation_auto_resolves_replacements() {
    let parsed = parsed_elspeth();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elspeth = scenario
        .add_creature(P0, "Elspeth, Storm Slayer", 0, 0)
        .id();
    let mut runner = scenario.build();
    wire_elspeth(runner.state_mut(), elspeth, &parsed);
    install_divine_visitation(runner.state_mut());

    let plus_one_index = parsed
        .abilities
        .iter()
        .position(|a| {
            matches!(
                a.cost,
                Some(engine::types::ability::AbilityCost::Loyalty { amount: 1 })
            )
        })
        .expect("+1 loyalty ability");

    let mut events = Vec::<GameEvent>::new();
    let waiting = planeswalker::handle_activate_loyalty(
        runner.state_mut(),
        P0,
        elspeth,
        plus_one_index,
        &mut events,
    )
    .expect("activate +1");
    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "loyalty activation should reach priority with ability on stack, got {waiting:?}"
    );

    runner.advance_until_stack_empty();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "commuting replacements must auto-resolve; got ReplacementChoice: {:?}",
        runner.state().waiting_for
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "must not strand on OrderTriggers; got {:?}",
        runner.state().waiting_for
    );

    let tokens: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.is_token)
        .collect();
    assert_eq!(
        tokens.len(),
        2,
        "double + substitute must create two tokens; got {:?}",
        tokens
            .iter()
            .map(|t| (&t.name, t.power, t.toughness))
            .collect::<Vec<_>>()
    );
    for token in &tokens {
        assert_eq!(token.power, Some(4), "substituted token is 4/4");
        assert_eq!(token.toughness, Some(4), "substituted token is 4/4");
    }
}
