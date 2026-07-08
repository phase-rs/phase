//! Runtime regression for `RepeatContinuation::WhileCondition` with a BOUNDED
//! ("once") cap and an interactive (library-search) iteration body, via Claim
//! Jumper — "When this creature enters, if an opponent controls more lands than
//! you, you may search your library for a Plains card and put it onto the
//! battlefield tapped. Then if an opponent controls more lands than you, repeat
//! this process once." (CR 608.2c).
//!
//! Exercises the paused-iteration resume path (`drain_pending_repeat_until`):
//! the search parks a `WaitingFor`, the controller submits the real
//! `GameAction`, and only then does the loop re-evaluate the condition and run
//! the single permitted repeat. Discriminating assertion: with the opponent
//! holding two lands and the controller none, the process runs twice (two
//! Plains enter), capped at one repeat. Reverting `WhileCondition` collapses it
//! to a single search (one Plains).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CLAIM_JUMPER_ORACLE: &str = "Vigilance\nWhen this creature enters, if an opponent controls more lands than you, you may search your library for a Plains card and put it onto the battlefield tapped. Then if an opponent controls more lands than you, repeat this process once. If you search your library this way, shuffle.";

fn claim_jumper_trigger_ability(source: ObjectId) -> engine::types::ability::ResolvedAbility {
    let parsed = parse_oracle_text(
        CLAIM_JUMPER_ORACLE,
        "Claim Jumper",
        &["Vigilance".to_string()],
        &["Creature".to_string()],
        &[],
    );
    let def = parsed
        .triggers
        .iter()
        .find_map(|t| t.execute.clone())
        .expect("Claim Jumper's enters trigger has an execute body");
    assert!(
        matches!(
            def.repeat_until,
            Some(engine::types::ability::RepeatContinuation::WhileCondition {
                max_iterations: Some(1),
                ..
            })
        ),
        "precondition: parsed trigger carries a bounded (once) WhileCondition, got {:?}",
        def.repeat_until
    );
    build_resolved_from_def(&def, source, P0)
}

/// Add a Plains land card (Land + Plains subtype) to P0's library.
fn add_plains_to_library(runner: &mut GameRunner) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Plains".to_string(),
        Zone::Library,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.card_types.supertypes.push(Supertype::Basic);
    obj.card_types.subtypes.push("Plains".to_string());
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Add the Claim Jumper source creature to P0's battlefield.
fn add_claim_jumper_source(runner: &mut GameRunner) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Claim Jumper".to_string(),
        Zone::Battlefield,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    id
}

fn plains_on_battlefield(state: &engine::types::game_state::GameState) -> usize {
    state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == P0
                && o.card_types.subtypes.iter().any(|s| s == "Plains")
        })
        .count()
}

/// Drive every parked search/optional choice to completion: accept each optional
/// prompt and select the first legal card at each search prompt.
fn drive_to_priority(runner: &mut GameRunner) {
    for _ in 0..16 {
        let action = match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                Some(GameAction::DecideOptionalEffect { accept: true })
            }
            WaitingFor::SearchChoice { cards, .. } => cards
                .first()
                .copied()
                .map(|c| GameAction::SelectCards { cards: vec![c] }),
            _ => None,
        };
        match action {
            Some(a) => {
                runner
                    .act(a)
                    .expect("search/optional choice should resolve");
            }
            None => return,
        }
    }
    panic!("did not reach priority after draining choices");
}

#[test]
fn claim_jumper_repeats_search_once_while_opponent_has_more_lands() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Opponent controls two lands; controller starts with none — the
    // intervening-if and the repeat predicate both hold initially.
    scenario.add_basic_land(P1, ManaColor::Green);
    scenario.add_basic_land(P1, ManaColor::Blue);

    let mut runner = scenario.build();
    let source = add_claim_jumper_source(&mut runner);
    // Two Plains in the library plus filler, so the bounded repeat can fetch a
    // second land.
    add_plains_to_library(&mut runner);
    add_plains_to_library(&mut runner);
    add_plains_to_library(&mut runner);

    let ability = claim_jumper_trigger_ability(source);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();
    drive_to_priority(&mut runner);

    // First search puts a Plains (controller now has 1 land). Opponent still has
    // 2, so the once-capped repeat runs a second search (controller -> 2 lands).
    // The condition is then false (2 == 2), so it does not repeat again.
    assert_eq!(
        plains_on_battlefield(runner.state()),
        2,
        "the bounded repeat must fetch exactly two Plains (one per process run)"
    );
}

#[test]
fn claim_jumper_does_not_repeat_when_lands_equalize_after_first_search() {
    // Opponent controls exactly one land; controller none. The first search puts
    // a Plains (controller -> 1 land == opponent), so the repeat predicate is
    // false and the process does NOT run a second time. Sibling/negative case
    // proving the condition gate discriminates against the bounded cap alone.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P1, ManaColor::Green);

    let mut runner = scenario.build();
    let source = add_claim_jumper_source(&mut runner);
    add_plains_to_library(&mut runner);
    add_plains_to_library(&mut runner);

    let ability = claim_jumper_trigger_ability(source);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();
    drive_to_priority(&mut runner);

    assert_eq!(
        plains_on_battlefield(runner.state()),
        1,
        "once the controller matches the opponent's land count the process must not repeat"
    );
}
