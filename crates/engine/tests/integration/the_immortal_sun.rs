//! The Immortal Sun (CMM #393) — line 1 gap: "Players can't activate
//! planeswalkers' loyalty abilities." (CR 602.5 + CR 606.2).
//!
//! Lines 2-4 (draw-step extra card, {1} cost reduction, +1/+1 anthem) already
//! shipped and work; line 1 was `Effect::Unimplemented`. These tests drive the
//! REAL parse → runtime pipeline: The Immortal Sun is parsed from its verbatim
//! MTGJSON Oracle text (ASCII apostrophes) via `from_oracle_text`, and loyalty /
//! normal activations are submitted through `GameAction::ActivateAbility`
//! (apply()), so a revert of either the parser or the runtime kind-gate flips an
//! assertion.
//!
//! Oracle text verified against `data/mtgjson/AtomicCards.json` (type Legendary
//! Artifact).

use std::sync::Arc;

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

/// Verbatim MTGJSON Oracle text (ASCII apostrophes, as AtomicCards ships it).
const SUN_ORACLE: &str = "Players can't activate planeswalkers' loyalty abilities.\nAt the beginning of your draw step, draw an additional card.\nSpells you cast cost {1} less to cast.\nCreatures you control get +1/+1.";

/// A printed-style loyalty ability: CR 606.1 loyalty cost (`[+1]`, an
/// `AbilityCost::Loyalty`) with a non-targeted draw payoff, sorcery-speed like
/// every loyalty ability (CR 606.3). Classified `Loyalty` by the single-authority
/// `is_loyalty_ability_cost`.
fn loyalty_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Loyalty { amount: 1 })
    .sorcery_speed()
}

/// A NON-loyalty activated ability (`{T}`, no loyalty symbol) → kind == Normal.
fn tap_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Tap)
}

/// Place The Immortal Sun (parsed from Oracle text through the real pipeline) on
/// P0's battlefield as a Legendary Artifact.
fn scenario_with_sun() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sun = scenario
        .add_creature_from_oracle(P0, "The Immortal Sun", 0, 0, SUN_ORACLE)
        .as_artifact()
        .id();
    let mut runner = scenario.build();
    // Populate the static-presence index / layers so the CR 604.1 O(1) presence
    // gate in `is_blocked_by_cant_be_activated` sees the Sun's static (production
    // does this via the ETB/layers pipeline; scenario seeding needs it explicitly).
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    (runner, sun)
}

/// Imperatively place a planeswalker with the given loyalty and ability list
/// under `owner`. Returns its `ObjectId`.
fn place_planeswalker(
    runner: &mut GameRunner,
    owner: PlayerId,
    name: &str,
    subtype: &str,
    loyalty: u32,
    abilities: Vec<AbilityDefinition>,
) -> ObjectId {
    let state = runner.state_mut();
    let id = ObjectId(state.next_object_id);
    create_object(
        state,
        CardId(id.0),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Planeswalker);
    obj.card_types.subtypes.push(subtype.to_string());
    obj.base_card_types = obj.card_types.clone();
    // CR 306.5b: loyalty IS the loyalty-counter count; seed both in sync.
    obj.loyalty = Some(loyalty);
    obj.counters.insert(CounterType::Loyalty, loyalty);
    obj.abilities = Arc::new(abilities.clone());
    obj.base_abilities = Arc::new(abilities);
    obj.entered_battlefield_turn = Some(state.turn_number.saturating_sub(1));
    obj.summoning_sick = false;
    id
}

/// Imperatively place a non-planeswalker permanent (artifact) carrying `ability`.
fn place_artifact(
    runner: &mut GameRunner,
    owner: PlayerId,
    name: &str,
    ability: AbilityDefinition,
) -> ObjectId {
    let state = runner.state_mut();
    let id = ObjectId(state.next_object_id);
    create_object(
        state,
        CardId(id.0),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.base_card_types = obj.card_types.clone();
    obj.abilities = Arc::new(vec![ability.clone()]);
    obj.base_abilities = Arc::new(vec![ability]);
    obj.entered_battlefield_turn = Some(state.turn_number.saturating_sub(1));
    obj.summoning_sick = false;
    id
}

fn activate(runner: &mut GameRunner, source: ObjectId, index: usize) -> bool {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
        })
        .is_ok()
}

/// item 2: "Players" includes the Sun's OWN controller — P0 is also blocked.
#[test]
fn own_controllers_loyalty_activation_is_blocked() {
    // Parser coupling: if line 1 did not parse to the loyalty static, the block
    // below would not fire and this test fails. `full_card_parses_with_zero_unimplemented`
    // asserts the static's shape directly.
    let (mut runner, _sun) = scenario_with_sun();
    let pw = place_planeswalker(
        &mut runner,
        P0,
        "Test Walker",
        "Jace",
        5,
        vec![loyalty_ability()],
    );
    assert!(
        !activate(&mut runner, pw, 0),
        "the Sun's own controller must be blocked from activating a loyalty ability"
    );
}

/// Reach-guard: the SAME loyalty ability, same PreCombatMain setup, WITHOUT the
/// Sun is legally activatable — proving the block above is the Sun's prohibition,
/// not an unrelated priority/timing/CR-606.3 gate. Also stands in for item 5
/// (loyalty activation works when the Sun is not on the battlefield).
#[test]
fn loyalty_activation_allowed_without_the_immortal_sun() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    let pw = place_planeswalker(
        &mut runner,
        P0,
        "Test Walker",
        "Jace",
        5,
        vec![loyalty_ability()],
    );
    assert!(
        activate(&mut runner, pw, 0),
        "loyalty activation must succeed with no Immortal Sun present"
    );
}

/// item 1: an opponent (P1) is also blocked — the prohibition is player-agnostic
/// (who = AllPlayers). Driven through apply() with P1 holding priority.
#[test]
fn opponents_loyalty_activation_is_blocked() {
    let (mut runner, _sun) = scenario_with_sun();
    let pw = place_planeswalker(
        &mut runner,
        P1,
        "Foe Walker",
        "Bolas",
        5,
        vec![loyalty_ability()],
    );
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    assert!(
        !activate(&mut runner, pw, 0),
        "an opponent must be blocked from activating a loyalty ability while the Sun is out"
    );
}

/// item 3 (the kind axis): on the SAME planeswalker, under the SAME Sun, the
/// loyalty ability is blocked but a NON-loyalty ({T}) activated ability is NOT.
/// Isolates the kind gate from the source_filter axis (both abilities share the
/// planeswalker source). Revert the runtime kind-gate and the `{T}` ability would
/// also be blocked — this assertion flips.
#[test]
fn planeswalker_non_loyalty_ability_is_not_blocked() {
    let (mut runner, _sun) = scenario_with_sun();
    let pw = place_planeswalker(
        &mut runner,
        P0,
        "Test Walker",
        "Jace",
        5,
        vec![loyalty_ability(), tap_ability()],
    );
    // index 0 = loyalty → blocked.
    assert!(
        !activate(&mut runner, pw, 0),
        "the planeswalker's loyalty ability must be blocked by the Sun"
    );
    // index 1 = non-loyalty {T} → NOT blocked (kind gate declines to block).
    assert!(
        activate(&mut runner, pw, 1),
        "the planeswalker's NON-loyalty activated ability must NOT be blocked"
    );
}

/// item 4 (the source axis): a non-planeswalker's activated ability still works —
/// the source_filter is Typed(Planeswalker).
#[test]
fn non_planeswalker_activated_ability_is_not_blocked() {
    let (mut runner, _sun) = scenario_with_sun();
    let artifact = place_artifact(&mut runner, P0, "Mana Rock", tap_ability());
    assert!(
        activate(&mut runner, artifact, 0),
        "a non-planeswalker's activated ability must NOT be blocked by the Sun"
    );
}

/// item 5: once the Sun leaves the battlefield the prohibition ends (CR 604.1 —
/// the static is only true while it functions on the battlefield).
#[test]
fn loyalty_activation_restored_after_the_sun_leaves() {
    let (mut runner, sun) = scenario_with_sun();
    let pw = place_planeswalker(
        &mut runner,
        P0,
        "Test Walker",
        "Jace",
        5,
        vec![loyalty_ability()],
    );
    assert!(
        !activate(&mut runner, pw, 0),
        "baseline: loyalty activation blocked while the Sun is out"
    );

    // The Sun leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != sun);
        state.objects.remove(&sun);
    }
    // Recompute the static-presence index / layers after the removal.
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    assert!(
        activate(&mut runner, pw, 0),
        "loyalty activation must work again after the Sun leaves the battlefield"
    );
}

/// item 7 (anthem sanity piggyback): "Creatures you control get +1/+1" buffs the
/// controller's creature, not the opponent's, while the Sun is out.
#[test]
fn anthem_buffs_controlled_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "The Immortal Sun", 0, 0, SUN_ORACLE)
        .as_artifact();
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let foe = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    assert_eq!(
        (
            runner.state().objects[&ally].power,
            runner.state().objects[&ally].toughness
        ),
        (Some(3), Some(3)),
        "a creature you control gets +1/+1: 2/2 → 3/3"
    );
    assert_eq!(
        (
            runner.state().objects[&foe].power,
            runner.state().objects[&foe].toughness
        ),
        (Some(2), Some(2)),
        "an opponent's creature is unaffected by \"Creatures you control\""
    );
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .hand
        .len()
}

/// Build The Immortal Sun (P0) on the battlefield at `active`'s Upkeep, with
/// `active`'s library stocked, and presence/layers populated. `auto_advance_to_main_phase`
/// then crosses only Upkeep → Draw → Main (no combat), draining that player's draw step.
fn sun_scenario_at_upkeep(active: PlayerId, library: &[&str]) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    scenario
        .add_creature_from_oracle(P0, "The Immortal Sun", 0, 0, SUN_ORACLE)
        .as_artifact();
    scenario.with_library_top(active, library);
    let mut runner = scenario.build();
    {
        // CR 505.1 + CR 500.1: run the draw step for `active`'s own turn. turn_number
        // stays 2 (set by at_phase) so no CR 103.8a first-turn draw skip applies.
        let s = runner.state_mut();
        s.active_player = active;
        s.priority_player = active;
        s.phase = Phase::Upkeep;
        s.waiting_for = WaitingFor::Priority { player: active };
    }
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    runner
}

/// item 6 (draw sanity piggyback): the controller draws an ADDITIONAL card in
/// their own draw step (2 total); an opponent draws only 1 in theirs. Asserted
/// "both ways" through the real draw-step turn machinery. The trigger is
/// controller-scoped ("your draw step"), so it never fires on the opponent's turn.
#[test]
fn controller_draws_extra_in_draw_step_opponent_does_not() {
    // Controller (P0): 1 turn-based draw + 1 from the Sun = 2.
    let mut ctrl = sun_scenario_at_upkeep(P0, &["Forest", "Island", "Mountain", "Plains"]);
    let before = hand_len(&ctrl, P0);
    ctrl.auto_advance_to_main_phase();
    assert_eq!(
        hand_len(&ctrl, P0) - before,
        2,
        "controller draws 1 (turn-based) + 1 (the Sun) in their own draw step"
    );

    // Opponent (P1): only the 1 turn-based draw — the Sun's \"your\" trigger is P0's.
    let mut opp = sun_scenario_at_upkeep(P1, &["Swamp", "Wastes", "Forest", "Island"]);
    let before = hand_len(&opp, P1);
    opp.auto_advance_to_main_phase();
    assert_eq!(
        hand_len(&opp, P1) - before,
        1,
        "opponent draws only the turn-based card (the Sun's draw trigger is controller-scoped)"
    );
}

/// Full-card parser probe: all four lines parse, zero `Effect::Unimplemented`,
/// and the four expected shapes are present (line 1 loyalty prohibition, the
/// draw-step trigger, the {1} cost reduction, and the anthem).
#[test]
fn full_card_parses_with_zero_unimplemented() {
    let parsed = parse_oracle_text(
        SUN_ORACLE,
        "The Immortal Sun",
        &[],
        &["Artifact".to_string()],
        &[],
    );

    let has_unimplemented = parsed
        .abilities
        .iter()
        .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. }))
        || parsed.triggers.iter().any(|t| {
            t.execute
                .as_ref()
                .is_some_and(|a| matches!(&*a.effect, Effect::Unimplemented { .. }))
        });
    assert!(
        !has_unimplemented,
        "no line of The Immortal Sun may fall to Effect::Unimplemented: {parsed:#?}"
    );

    // Line 1: loyalty-narrowed activation prohibition.
    assert!(
        parsed.statics.iter().any(|s| matches!(
            s.mode,
            StaticMode::CantBeActivated {
                kind: Some(engine::types::events::ActivatedAbilityKind::Loyalty),
                ..
            }
        )),
        "line 1 must parse to CantBeActivated {{ kind: Some(Loyalty) }}: {:#?}",
        parsed.statics
    );
    // Line 3: a cost-reduction static (Spells you cast cost {1} less).
    assert!(
        parsed
            .statics
            .iter()
            .any(|s| matches!(s.mode, StaticMode::ModifyCost { .. })),
        "line 3 must parse to a cost-reduction static: {:#?}",
        parsed.statics
    );
    // Line 2: a draw-step trigger (the extra card).
    assert!(
        !parsed.triggers.is_empty(),
        "line 2 must parse to a draw-step trigger: {:#?}",
        parsed.triggers
    );
    // Line 4: an anthem (continuous P/T static).
    assert!(
        parsed
            .statics
            .iter()
            .any(|s| matches!(s.mode, StaticMode::Continuous)),
        "line 4 must parse to a continuous anthem static: {:#?}",
        parsed.statics
    );
}
