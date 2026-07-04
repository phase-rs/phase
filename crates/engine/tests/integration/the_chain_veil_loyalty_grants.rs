//! Integration tests for The Chain Veil (Commander 2014).
//!
//! Oracle text:
//!   At the beginning of your end step, if you didn't activate a loyalty
//!   ability of a planeswalker this turn, you lose 2 life.
//!   {4}, {T}: For each planeswalker you control, you may activate one of
//!   its loyalty abilities once this turn as though none of its loyalty
//!   abilities have been activated this turn.
//!
//! These tests pin the end-to-end mechanics layered on top of the new
//! infrastructure:
//!   - `GameState::loyalty_abilities_activated_this_turn` — per-player
//!     activation history, read by `QuantityRef::LoyaltyAbilitiesActivatedThisTurn`.
//!   - `GameState::extra_loyalty_activations_this_turn` — per-player grant
//!     budget, written by `Effect::GrantExtraLoyaltyActivations` and consumed
//!     by `planeswalker::can_activate_loyalty_ability`.
//!   - `GameObject::loyalty_activations_this_turn` — per-planeswalker counter
//!     (migrated from the old `loyalty_activated_this_turn: bool`).
//!
//! Parser tests covering The Chain Veil's Oracle text dispatch live in the
//! corresponding parser unit modules; these tests focus on the resolution and
//! activation-gate behavior.

use engine::game::effects;
use engine::game::engine::apply;
use engine::game::planeswalker;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CopyCountStatus, Effect, QuantityExpr,
    ResolvedAbility, SubAbilityLink, TargetFilter, TargetRef, TargetSelectionMode,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::sync::Arc;

fn setup_main_phase() -> GameState {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state
}

fn make_loyalty_ability(loyalty_amount: i32) -> AbilityDefinition {
    // CR 606.3: Loyalty abilities are gated by the per-permanent
    // `loyalty_activations_this_turn` counter (set by the planeswalker
    // activation path), NOT by per-ability `OnlyOnceEachTurn`. The parser
    // mirrors this — it attaches `AsSorcery` only. See
    // `parser::oracle::apply_loyalty_restrictions`.
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Loyalty {
        amount: loyalty_amount,
    })
    .sorcery_speed()
}

fn make_targeted_loyalty_ability(loyalty_amount: i32) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Player,
            damage_source: None,
        },
    )
    .cost(AbilityCost::Loyalty {
        amount: loyalty_amount,
    })
    .sorcery_speed()
}

fn create_planeswalker(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
    loyalty: u32,
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Planeswalker);
    obj.loyalty = Some(loyalty);
    obj.counters.insert(CounterType::Loyalty, loyalty);
    obj.abilities = Arc::new(vec![make_loyalty_ability(1)]);
    obj.entered_battlefield_turn = Some(state.turn_number);
    id
}

fn make_grant_ability(controller: PlayerId, source: ObjectId) -> ResolvedAbility {
    ResolvedAbility {
        effect: Effect::GrantExtraLoyaltyActivations {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        controller,
        original_controller: None,
        scoped_player: None,
        target_chooser: None,
        source_id: source,
        source_incarnation: None,
        targets: vec![],
        kind: AbilityKind::Activated,
        sub_ability: None,
        else_ability: None,
        duration: None,
        condition: None,
        context: Default::default(),
        optional_targeting: false,
        optional: false,
        optional_for: None,
        multi_target: None,
        target_constraints: Vec::new(),
        target_choice_timing: engine::types::ability::TargetChoiceTiming::Stack,
        description: None,
        player_scope: None,
        starting_with: None,
        chosen_x: None,
        cost_paid_object: None,
        effect_context_object: None,
        ability_index: None,
        may_trigger_origin: None,
        repeat_for: None,
        min_x_value: 0,
        cant_be_copied: false,
        copy_count_status: CopyCountStatus::Pending,
        forward_result: false,
        unless_pay: None,
        distribution: None,
        target_selection_mode: TargetSelectionMode::Chosen,
        chosen_players: Vec::new(),
        repeat_until: None,
        sub_link: SubAbilityLink::ContinuationStep,
        modal: None,
        mode_abilities: vec![],
        dig_found_nothing_for_parent_target: false,
    }
}

/// CR 606.3 + CR 606.1: After one loyalty activation, the same planeswalker
/// is locked out under the printed cap (1). After resolving The Chain Veil
/// (+1 grant), the same planeswalker is activatable a second time.
#[test]
fn chain_veil_grant_raises_per_planeswalker_cap() {
    let mut state = setup_main_phase();
    let pw = create_planeswalker(&mut state, PlayerId(0), "Jace", 3);
    let veil_card = CardId(state.next_object_id);
    let veil = create_object(
        &mut state,
        veil_card,
        PlayerId(0),
        "The Chain Veil".to_string(),
        Zone::Battlefield,
    );

    // First activation succeeds and ticks the per-permanent counter to 1.
    let mut events = Vec::new();
    planeswalker::handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events).unwrap();
    state.stack.clear();
    assert_eq!(
        state.objects[&pw].loyalty_activations_this_turn, 1,
        "loyalty activation must increment the per-permanent counter"
    );
    assert_eq!(
        state
            .loyalty_abilities_activated_this_turn
            .get(&PlayerId(0))
            .copied(),
        Some(1),
        "per-player activation history must also increment"
    );

    // Without a grant, the second activation is denied (CR 606.3 cap = 1).
    assert!(
        !planeswalker::can_activate_loyalty_ability(&state, pw, PlayerId(0), 0),
        "CR 606.3: second activation denied under default cap"
    );

    // Resolve The Chain Veil's grant: +1 to the controller's per-planeswalker cap.
    let grant = make_grant_ability(PlayerId(0), veil);
    effects::grant_extra_loyalty_activations::resolve(&mut state, &grant, &mut events).unwrap();
    assert_eq!(
        state
            .extra_loyalty_activations_this_turn
            .get(&PlayerId(0))
            .copied(),
        Some(1)
    );

    // Now the second activation is permitted.
    assert!(
        planeswalker::can_activate_loyalty_ability(&state, pw, PlayerId(0), 0),
        "after the Chain Veil grant, the planeswalker's loyalty cap is 2"
    );
    planeswalker::handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events).unwrap();
    state.stack.clear();
    assert_eq!(state.objects[&pw].loyalty_activations_this_turn, 2);

    // CR 606.3: After two activations and a +1 grant, a third is denied.
    assert!(
        !planeswalker::can_activate_loyalty_ability(&state, pw, PlayerId(0), 0),
        "third activation denied: cap = 1 + 1 grant = 2"
    );
}

/// CR 606.3 + CR 601.2c: A targeted loyalty ability is announced and recorded,
/// waits for target choice, then pays its loyalty cost when the ability is
/// pushed to the stack.
#[test]
fn targeted_loyalty_activation_records_once_across_target_selection() {
    let mut state = setup_main_phase();
    let pw = create_planeswalker(&mut state, PlayerId(0), "Jace", 3);
    {
        let obj = state.objects.get_mut(&pw).unwrap();
        obj.abilities = Arc::new(vec![make_targeted_loyalty_ability(1)]);
    }

    let mut events = Vec::new();
    let waiting =
        planeswalker::handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events).unwrap();
    assert!(matches!(waiting, WaitingFor::TargetSelection { .. }));
    assert_eq!(state.objects[&pw].loyalty_activations_this_turn, 1);
    assert_eq!(
        state
            .loyalty_abilities_activated_this_turn
            .get(&PlayerId(0))
            .copied(),
        Some(1)
    );

    state.waiting_for = waiting;
    apply(
        &mut state,
        PlayerId(0),
        GameAction::SelectTargets {
            targets: vec![TargetRef::Player(PlayerId(1))],
        },
    )
    .unwrap();

    assert_eq!(state.objects[&pw].loyalty_activations_this_turn, 1);
    assert_eq!(
        state
            .loyalty_abilities_activated_this_turn
            .get(&PlayerId(0))
            .copied(),
        Some(1)
    );
}

/// CR 606.3: Two Chain Veil activations stack to grant +2 — three activations
/// per planeswalker per turn.
#[test]
fn two_chain_veil_grants_stack_to_plus_two() {
    let mut state = setup_main_phase();
    let pw = create_planeswalker(&mut state, PlayerId(0), "Jace", 3);
    let veil_card = CardId(state.next_object_id);
    let veil = create_object(
        &mut state,
        veil_card,
        PlayerId(0),
        "The Chain Veil".to_string(),
        Zone::Battlefield,
    );

    let grant = make_grant_ability(PlayerId(0), veil);
    let mut events = Vec::new();
    effects::grant_extra_loyalty_activations::resolve(&mut state, &grant, &mut events).unwrap();
    effects::grant_extra_loyalty_activations::resolve(&mut state, &grant, &mut events).unwrap();

    // Cap = 1 + 2 = 3. Three activations all succeed.
    planeswalker::handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events).unwrap();
    state.stack.clear();
    planeswalker::handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events).unwrap();
    state.stack.clear();
    planeswalker::handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events).unwrap();
    state.stack.clear();
    assert_eq!(state.objects[&pw].loyalty_activations_this_turn, 3);
    assert!(!planeswalker::can_activate_loyalty_ability(
        &state,
        pw,
        PlayerId(0),
        0
    ));
}

/// CR 606.3 + CR 606.1: The grant applies to *every* planeswalker the
/// controller controls — the printed "For each planeswalker you control"
/// preamble names the beneficiaries.
#[test]
fn chain_veil_grant_applies_to_each_planeswalker_independently() {
    let mut state = setup_main_phase();
    let pw1 = create_planeswalker(&mut state, PlayerId(0), "Jace", 3);
    let pw2 = create_planeswalker(&mut state, PlayerId(0), "Liliana", 4);
    let veil_card = CardId(state.next_object_id);
    let veil = create_object(
        &mut state,
        veil_card,
        PlayerId(0),
        "The Chain Veil".to_string(),
        Zone::Battlefield,
    );

    let grant = make_grant_ability(PlayerId(0), veil);
    let mut events = Vec::new();
    effects::grant_extra_loyalty_activations::resolve(&mut state, &grant, &mut events).unwrap();

    // Each planeswalker can be activated twice independently.
    planeswalker::handle_activate_loyalty(&mut state, PlayerId(0), pw1, 0, &mut events).unwrap();
    state.stack.clear();
    planeswalker::handle_activate_loyalty(&mut state, PlayerId(0), pw1, 0, &mut events).unwrap();
    state.stack.clear();
    planeswalker::handle_activate_loyalty(&mut state, PlayerId(0), pw2, 0, &mut events).unwrap();
    state.stack.clear();
    planeswalker::handle_activate_loyalty(&mut state, PlayerId(0), pw2, 0, &mut events).unwrap();
    state.stack.clear();

    assert_eq!(state.objects[&pw1].loyalty_activations_this_turn, 2);
    assert_eq!(state.objects[&pw2].loyalty_activations_this_turn, 2);
    assert_eq!(
        state
            .loyalty_abilities_activated_this_turn
            .get(&PlayerId(0))
            .copied(),
        Some(4)
    );
}

/// CR 606.3: The cap raise only applies to the controller of The Chain Veil.
/// The opposing player's planeswalkers remain at the printed CR 606.3 cap.
#[test]
fn chain_veil_grant_does_not_leak_to_opponent() {
    let mut state = setup_main_phase();
    let veil_card = CardId(state.next_object_id);
    let veil = create_object(
        &mut state,
        veil_card,
        PlayerId(0),
        "The Chain Veil".to_string(),
        Zone::Battlefield,
    );
    let opponent_pw = create_planeswalker(&mut state, PlayerId(1), "Jace", 3);

    let grant = make_grant_ability(PlayerId(0), veil);
    let mut events = Vec::new();
    effects::grant_extra_loyalty_activations::resolve(&mut state, &grant, &mut events).unwrap();

    // The opponent's planeswalker is on PlayerId(1)'s side; even after
    // PlayerId(0) grants themselves +1, PlayerId(1) sees the default cap of 1.
    // Simulate it being PlayerId(1)'s turn so the activation is legal.
    state.active_player = PlayerId(1);
    state.priority_player = PlayerId(1);
    planeswalker::handle_activate_loyalty(&mut state, PlayerId(1), opponent_pw, 0, &mut events)
        .unwrap();
    state.stack.clear();
    assert!(
        !planeswalker::can_activate_loyalty_ability(&state, opponent_pw, PlayerId(1), 0),
        "PlayerId(1)'s loyalty cap is unaffected by PlayerId(0)'s grant"
    );
}

/// CR 514.2 + CR 606.3: All loyalty-history maps clear at turn start so the
/// next turn starts from the printed default cap.
#[test]
fn turn_start_clears_loyalty_history_and_extra_grants() {
    let mut state = setup_main_phase();
    let pw = create_planeswalker(&mut state, PlayerId(0), "Jace", 3);
    let veil_card = CardId(state.next_object_id);
    let veil = create_object(
        &mut state,
        veil_card,
        PlayerId(0),
        "The Chain Veil".to_string(),
        Zone::Battlefield,
    );

    let grant = make_grant_ability(PlayerId(0), veil);
    let mut events = Vec::new();
    effects::grant_extra_loyalty_activations::resolve(&mut state, &grant, &mut events).unwrap();
    planeswalker::handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events).unwrap();
    state.stack.clear();

    // Pre-reset: all counters are populated.
    assert_eq!(state.objects[&pw].loyalty_activations_this_turn, 1);
    assert!(state
        .loyalty_abilities_activated_this_turn
        .contains_key(&PlayerId(0)));
    assert!(state
        .extra_loyalty_activations_this_turn
        .contains_key(&PlayerId(0)));

    // Reset at turn boundary.
    engine::game::turns::start_next_turn(&mut state, &mut events);
    assert_eq!(state.objects[&pw].loyalty_activations_this_turn, 0);
    assert!(state.loyalty_abilities_activated_this_turn.is_empty());
    assert!(state.extra_loyalty_activations_this_turn.is_empty());
}
