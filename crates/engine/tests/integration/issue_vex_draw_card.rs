use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const VEX_ORACLE: &str = "Counter target spell. That spell's controller may draw a card.";

fn put_instant_on_stack(
    runner: &mut engine::game::scenario::GameRunner,
    controller: PlayerId,
) -> ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(501),
        controller,
        "Shock".to_string(),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&spell) {
        obj.card_types.core_types = vec![CoreType::Instant];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(501),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

/// CR 608.2c + CR 109.4 + CR 608.2b: When Vex resolves, it counters the target spell,
/// and that spell's controller is prompted to optionally draw a card.
/// If accepted, that controller draws a card.
#[test]
fn vex_presents_draw_option_and_draws_card_when_accepted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let vex_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Vex", true, VEX_ORACLE)
        .id();

    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    // Give P1 a library card to draw
    scenario.with_library_top(P1, &["Mountain"]);

    let mut runner = scenario.build();

    let p1_hand_before = runner.state().players[1].hand.len();

    let shock_id = put_instant_on_stack(&mut runner, P1);

    // Cast Vex targeting Shock and accept the optional draw
    runner
        .cast(vex_id)
        .target_objects(&[shock_id])
        .accept_optional()
        .resolve();

    let p1_hand_after = runner.state().players[1].hand.len();

    assert_eq!(
        p1_hand_after,
        p1_hand_before + 1,
        "P1 (countered spell's controller) should draw 1 card when accepting"
    );
}

/// CR 608.2d + CR 608.2c: When Vex resolves and the countered spell's controller declines
/// the optional draw, no card is drawn.
#[test]
fn vex_declining_optional_draw_draws_no_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let vex_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Vex", true, VEX_ORACLE)
        .id();

    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    scenario.with_library_top(P1, &["Mountain"]);

    let mut runner = scenario.build();

    let p1_hand_before = runner.state().players[1].hand.len();

    let shock_id = put_instant_on_stack(&mut runner, P1);

    // CR 608.2d: observe this resolution's choice before explicitly declining it.
    let mut commit = runner.cast(vex_id).target_objects(&[shock_id]).commit();
    commit.act(GameAction::PassPriority).unwrap();
    commit.act(GameAction::PassPriority).unwrap();
    assert!(
        matches!(commit.state().waiting_for, WaitingFor::OptionalEffectChoice { player, .. } if player == P1),
        "the countered spell's controller must receive the draw choice before declining"
    );
    assert_eq!(
        commit.state().objects[&shock_id].zone,
        Zone::Graveyard,
        "the target spell was countered before the optional draw"
    );
    commit
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    assert!(
        matches!(commit.state().waiting_for, WaitingFor::Priority { .. }),
        "declining must finish the optional draw and return priority"
    );
    drop(commit);

    let p1_hand_after = runner.state().players[1].hand.len();

    assert_eq!(
        p1_hand_after, p1_hand_before,
        "P1 should draw 0 cards when declining optional draw"
    );
}

/// CR 608.2c + CR 109.4 + CR 608.2d: Verifies that when Vex resolves, the engine pauses with
/// `WaitingFor::OptionalEffectChoice` targeted specifically at the countered spell's controller (P1).
#[test]
fn vex_stops_at_optional_effect_choice_for_countered_spell_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let vex_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Vex", true, VEX_ORACLE)
        .id();

    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    scenario.with_library_top(P1, &["Mountain"]);

    let mut runner = scenario.build();

    let shock_id = put_instant_on_stack(&mut runner, P1);

    // Commit Vex without driving resolution automatically
    let mut commit = runner.cast(vex_id).target_objects(&[shock_id]).commit();

    // Pass priority until Vex resolves
    commit
        .act(engine::types::actions::GameAction::PassPriority)
        .unwrap();
    commit
        .act(engine::types::actions::GameAction::PassPriority)
        .unwrap();

    match &commit.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(*player, P1, "P1 must receive the prompt");
        }
        other => panic!("expected WaitingFor::OptionalEffectChoice, got {other:?}"),
    }
}
