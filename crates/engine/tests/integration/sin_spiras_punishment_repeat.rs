//! Runtime regression for the `RepeatContinuation::WhileCondition` primitive
//! via Sin, Spira's Punishment — "exile a permanent card from your graveyard at
//! random, then create a tapped token that's a copy of that card. If the exiled
//! card is a land card, repeat this process." (CR 608.2c).
//!
//! Discriminating assertion: the number of tapped copy tokens created equals the
//! number of consecutive land cards exiled before a non-land is hit (or the
//! graveyard empties). Reverting the `WhileCondition` loop collapses every case
//! to a single token, so the multi-land assertion below flips.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::parse_oracle_text;
use engine::types::card_type::CoreType;
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SIN_ORACLE: &str = "Flying\nWhenever Sin enters or attacks, exile a permanent card from your graveyard at random, then create a tapped token that's a copy of that card. If the exiled card is a land card, repeat this process.";

/// Build the resolved trigger body for Sin (the enters-or-attacks ability),
/// carrying its `WhileCondition` repeat predicate and `Random` exile selection.
fn sin_trigger_ability(
    source: engine::types::identifiers::ObjectId,
) -> engine::types::ability::ResolvedAbility {
    let parsed = parse_oracle_text(
        SIN_ORACLE,
        "Sin, Spira's Punishment",
        &["Flying".to_string()],
        &["Creature".to_string()],
        &[],
    );
    let def = parsed
        .triggers
        .iter()
        .find_map(|t| t.execute.clone())
        .expect("Sin's enters-or-attacks trigger has an execute body");
    assert!(
        matches!(
            def.repeat_until,
            Some(engine::types::ability::RepeatContinuation::WhileCondition { .. })
        ),
        "precondition: parsed trigger carries a WhileCondition repeat, got {:?}",
        def.repeat_until
    );
    build_resolved_from_def(&def, source, P0)
}

fn add_card(
    runner: &mut GameRunner,
    name: &str,
    zone: Zone,
    core_types: &[CoreType],
) -> engine::types::identifiers::ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(runner.state_mut(), card_id, P0, name.to_string(), zone);
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types = core_types.to_vec();
    obj.base_card_types = obj.card_types.clone();
    id
}

fn token_count(state: &engine::types::game_state::GameState) -> usize {
    state
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield)
        .count()
}

#[test]
fn sin_repeats_while_exiled_card_is_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    // The source lives on the battlefield (not in the candidate graveyard).
    let sin = add_card(
        &mut runner,
        "Sin, Spira's Punishment",
        Zone::Battlefield,
        &[CoreType::Creature],
    );
    // Three lands in the graveyard. Every exile picks a land (all candidates are
    // lands), so the WhileCondition holds until the graveyard empties — exactly
    // three iterations, three tapped copy tokens.
    add_card(&mut runner, "Island", Zone::Graveyard, &[CoreType::Land]);
    add_card(&mut runner, "Mountain", Zone::Graveyard, &[CoreType::Land]);
    add_card(&mut runner, "Forest", Zone::Graveyard, &[CoreType::Land]);

    let ability = sin_trigger_ability(sin);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    assert_eq!(
        token_count(runner.state()),
        3,
        "exiling three consecutive land cards must create three tapped copy tokens"
    );
    // All three land cards ended up exiled.
    assert_eq!(
        runner.state().players[P0.0 as usize].graveyard.len(),
        0,
        "every land card must have been exiled by the repeated process"
    );
}

#[test]
fn sin_stops_after_one_token_when_exiled_card_is_nonland() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    let sin = add_card(
        &mut runner,
        "Sin, Spira's Punishment",
        Zone::Battlefield,
        &[CoreType::Creature],
    );
    // The only exile candidate is a non-land permanent card: the process runs
    // once (one token) and the WhileCondition's land check is false, so it does
    // NOT repeat — the sibling/negative case that proves the gate discriminates.
    add_card(
        &mut runner,
        "Grizzly Bears",
        Zone::Graveyard,
        &[CoreType::Creature],
    );

    let ability = sin_trigger_ability(sin);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    assert_eq!(
        token_count(runner.state()),
        1,
        "exiling a single non-land card must create exactly one token and not repeat"
    );
}
