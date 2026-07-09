//! Kaya, Geist Hunter [−2] — until-end-of-turn token-creation doubling.
//!
//! Oracle (verbatim, Scryfall):
//!   +1: Creatures you control gain deathtouch until end of turn. Put a +1/+1
//!       counter on up to one target creature token you control.
//!   −2: Until end of turn, if one or more tokens would be created under your
//!       control, twice that many of those tokens are created instead.
//!   −6: Exile all cards from all graveyards, then create a 1/1 white Spirit
//!       creature token with flying for each card exiled this way.
//!
//! The [−2] installs a floating (controller-anchored, non-object-hosted)
//! until-EOT `CreateToken` replacement in `pending_damage_replacements`
//! (CR 614.1a "instead", CR 111.2 "under your control", CR 514.2 EOT expiry).
//! These tests drive the real production path — `handle_activate_loyalty`
//! → stack resolution installs the replacement, then a real token-creating
//! effect resolves through the replacement pipeline.

use std::sync::Arc;

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::planeswalker;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityCost, AbilityKind, ControllerRef, Effect, QuantityModification, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;

const KAYA_ORACLE: &str = "+1: Creatures you control gain deathtouch until end of turn. Put a +1/+1 counter on up to one target creature token you control.\n\
−2: Until end of turn, if one or more tokens would be created under your control, twice that many of those tokens are created instead.\n\
−6: Exile all cards from all graveyards, then create a 1/1 white Spirit creature token with flying for each card exiled this way.";

const MINUS_TWO_EFFECT: &str =
    "Until end of turn, if one or more tokens would be created under your control, twice that many of those tokens are created instead.";

const SOLDIER_TOKEN: &str = "Create a 1/1 white Soldier creature token.";

fn parsed_kaya() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        KAYA_ORACLE,
        "Kaya, Geist Hunter",
        &[],
        &["Legendary".to_string()],
        &["Kaya".to_string()],
    )
}

/// Find the index of Kaya's [−2] loyalty ability.
fn minus_two_index(parsed: &engine::parser::oracle::ParsedAbilities) -> usize {
    parsed
        .abilities
        .iter()
        .position(|a| matches!(a.cost, Some(AbilityCost::Loyalty { amount: -2 })))
        .expect("Kaya must parse a [−2] loyalty ability")
}

/// Wire Kaya as a planeswalker with 3 starting loyalty and her parsed abilities.
fn wire_kaya(
    state: &mut engine::types::game_state::GameState,
    kaya: ObjectId,
    parsed: &engine::parser::oracle::ParsedAbilities,
) {
    let obj = state.objects.get_mut(&kaya).expect("kaya");
    obj.card_types.core_types = vec![CoreType::Planeswalker];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.loyalty = Some(3);
    obj.counters.insert(CounterType::Loyalty, 3);
    obj.abilities = Arc::new(parsed.abilities.clone());
    obj.base_abilities = Arc::new(parsed.abilities.clone());
}

/// Resolve a real "Create a 1/1 white Soldier creature token." effect under
/// `controller`, driving through the `CreateToken` replacement pipeline.
fn create_one_token(runner: &mut GameRunner, source: ObjectId, controller: PlayerId) {
    let def = parse_effect_chain(SOLDIER_TOKEN, AbilityKind::Spell);
    let resolved = build_resolved_from_def(&def, source, controller);
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("token creation resolves");
}

/// Count token objects controlled by `player`.
fn token_count_for(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.controller == player)
        .count()
}

/// Activate Kaya's [−2] for `player`, resolve the install, and assert the
/// reach guard (loyalty 3 → 1) proving the ability actually resolved.
fn activate_minus_two(runner: &mut GameRunner, kaya: ObjectId, player: PlayerId, index: usize) {
    let mut events = Vec::<GameEvent>::new();
    let waiting =
        planeswalker::handle_activate_loyalty(runner.state_mut(), player, kaya, index, &mut events)
            .expect("activate [−2]");
    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "loyalty activation should reach priority with ability on stack, got {waiting:?}"
    );
    // CR 606.4: loyalty cost paid at activation — reach guard proving install.
    assert_eq!(
        runner.state().objects.get(&kaya).unwrap().loyalty,
        Some(1),
        "[−2] must drop Kaya's loyalty 3 → 1 (proves the ability was activated)"
    );
    runner.advance_until_stack_empty();
}

#[test]
fn kaya_minus2_doubles_tokens_under_your_control() {
    let parsed = parsed_kaya();
    let index = minus_two_index(&parsed);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kaya = scenario.add_creature(P0, "Kaya, Geist Hunter", 0, 0).id();
    let mut runner = scenario.build();
    wire_kaya(runner.state_mut(), kaya, &parsed);

    activate_minus_two(&mut runner, kaya, P0, index);

    let before = token_count_for(&runner, P0);
    create_one_token(&mut runner, kaya, P0);
    let after = token_count_for(&runner, P0);

    // CR 614.1a + CR 111.2: one proposed token under P0's control is doubled.
    // Reverting the applier sentinel, the pending-scan gate, or the parser
    // helper drops this delta to 1.
    assert_eq!(
        after - before,
        2,
        "Kaya [−2] must double a single token created under her controller"
    );
}

#[test]
fn kaya_minus2_does_not_double_opponents_tokens() {
    let parsed = parsed_kaya();
    let index = minus_two_index(&parsed);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kaya = scenario.add_creature(P0, "Kaya, Geist Hunter", 0, 0).id();
    // A P1-controlled source object so the opponent's token is created under P1.
    let opp_source = scenario.add_creature(P1, "Opp Source", 1, 1).id();
    let mut runner = scenario.build();
    wire_kaya(runner.state_mut(), kaya, &parsed);

    activate_minus_two(&mut runner, kaya, P0, index);

    // Positive reach-guard (non-vacuous negative): the replacement really is
    // installed and doubling — a token under P0's control IS doubled. Without
    // this, a silently-failed install would make the P1 assertion below pass
    // vacuously.
    let p0_before = token_count_for(&runner, P0);
    create_one_token(&mut runner, kaya, P0);
    assert_eq!(
        token_count_for(&runner, P0) - p0_before,
        2,
        "reach guard: Kaya [−2] doubling must be installed and active for P0"
    );

    // CR 111.2 + CR 614.1a: "under YOUR control" — the token_owner_scope(You)
    // gate rejects P1's token (owner != source_controller). Not doubled.
    let p1_before = token_count_for(&runner, P1);
    create_one_token(&mut runner, opp_source, P1);
    assert_eq!(
        token_count_for(&runner, P1) - p1_before,
        1,
        "Kaya [−2] must NOT double a token created under the opponent's control"
    );
}

#[test]
fn kaya_minus2_doubling_lapses_at_end_of_turn() {
    let parsed = parsed_kaya();
    let index = minus_two_index(&parsed);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kaya = scenario.add_creature(P0, "Kaya, Geist Hunter", 0, 0).id();
    let mut runner = scenario.build();
    wire_kaya(runner.state_mut(), kaya, &parsed);

    activate_minus_two(&mut runner, kaya, P0, index);

    // Reach guard: doubling is active THIS turn (proves the install happened,
    // so a green post-cleanup result can't come from a silently-failed install).
    let before_same_turn = token_count_for(&runner, P0);
    create_one_token(&mut runner, kaya, P0);
    assert_eq!(
        token_count_for(&runner, P0) - before_same_turn,
        2,
        "doubling must be active during the same turn"
    );

    // CR 514.2: advance through this turn's cleanup into the next turn — the
    // EOT-expiry pending replacement is pruned by `execute_cleanup`.
    runner.advance_to_upkeep();

    let before_next_turn = token_count_for(&runner, P0);
    create_one_token(&mut runner, kaya, P0);
    assert_eq!(
        token_count_for(&runner, P0) - before_next_turn,
        1,
        "doubling must lapse at cleanup — next turn a single token is not doubled"
    );
}

#[test]
fn kaya_minus2_parses_to_floating_token_replacement() {
    // Parser SHAPE test: the verbatim [−2] effect text parses to an
    // AddTargetReplacement{target: None} floating token-creation replacement.
    let def = parse_effect_chain(MINUS_TWO_EFFECT, AbilityKind::Spell);

    let Effect::AddTargetReplacement {
        replacement,
        target,
    } = &*def.effect
    else {
        panic!("expected AddTargetReplacement, got {:?}", def.effect);
    };
    assert_eq!(
        *target,
        TargetFilter::None,
        "floating (non-object-hosted) install uses target: None"
    );
    assert_eq!(replacement.event, ReplacementEvent::CreateToken);
    assert_eq!(
        replacement.quantity_modification,
        Some(QuantityModification::Times { factor: 2 }),
        "twice that many → Times {{ factor: 2 }}"
    );
    assert_eq!(
        replacement.token_owner_scope,
        Some(ControllerRef::You),
        "under your control → token_owner_scope(You)"
    );

    // Reach guard: the clause is no longer swallowed to Unimplemented.
    assert!(
        !matches!(&*def.effect, Effect::Unimplemented { .. }),
        "Kaya [−2] must not fall through to Effect::Unimplemented"
    );
}
