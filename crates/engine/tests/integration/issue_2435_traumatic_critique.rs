//! Traumatic Critique (MKM) — draw-then-discard must fire after damage.
//!
//! Oracle:
//!   "Traumatic Critique deals X damage to any target. Draw two cards, then discard a card."
//!
//! Issue #2435: the mandatory discard step was silently skipped after damage + draw.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply_as_current;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, ResolvedAbility, SubAbilityLink, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

const TRAUMATIC_CRITIQUE_ORACLE: &str =
    "Traumatic Critique deals X damage to any target. Draw two cards, then discard a card.";

fn traumatic_critique_chain(
    source_id: ObjectId,
    controller: PlayerId,
    damage_target: TargetRef,
    x: u32,
) -> ResolvedAbility {
    let def = parse_effect_chain(TRAUMATIC_CRITIQUE_ORACLE, AbilityKind::Spell);
    let mut ability = build_resolved_from_def(&def, source_id, controller);
    ability.targets = vec![damage_target];
    ability.chosen_x = Some(x);
    ability
}

#[test]
fn traumatic_critique_draw_then_discard_prompts_after_damage() {
    let controller = P0;
    let source_id = ObjectId(100);

    let mut scenario = GameScenario::new();
    scenario.add_card_to_hand(controller, "HandA");
    let hand_b = scenario.add_card_to_hand(controller, "HandB");
    scenario.add_card_to_library_top(controller, "Lib1");
    scenario.add_card_to_library_top(controller, "Lib2");
    let target = scenario.add_creature(P1, "Target", 2, 2).id();

    let mut runner = scenario.build();
    let state = runner.state_mut();

    let hand_before = state.players[0].hand.len();
    let lib_before = state.players[0].library.len();

    let chain = traumatic_critique_chain(source_id, controller, TargetRef::Object(target), 2);
    let draw = chain
        .sub_ability
        .as_ref()
        .expect("DealDamage should chain to Draw");
    assert!(
        matches!(draw.effect, Effect::Draw { .. }),
        "second step should be Draw, got {:?}",
        draw.effect
    );
    assert_eq!(
        draw.sub_link,
        SubAbilityLink::SequentialSibling,
        "Draw follows damage as the next sentence"
    );
    let discard = draw
        .sub_ability
        .as_ref()
        .expect("Draw should chain to Discard");
    assert!(matches!(discard.effect, Effect::Discard { .. }));

    let mut events = Vec::new();
    resolve_ability_chain(state, &chain, &mut events, 0).unwrap();

    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamageDealt { .. })),
        "damage step should resolve"
    );
    assert_eq!(
        state.players[0].library.len(),
        lib_before - 2,
        "draw two cards should shrink library by 2"
    );
    assert_eq!(
        state.players[0].hand.len(),
        hand_before + 2,
        "drew two cards before discard"
    );

    match &state.waiting_for {
        WaitingFor::DiscardChoice { player, count, .. } => {
            assert_eq!(
                *player, controller,
                "controller must discard, not damage target"
            );
            assert_eq!(*count, 1);
        }
        other => panic!("expected DiscardChoice after draw, got {other:?}"),
    }

    apply_as_current(
        state,
        GameAction::SelectCards {
            cards: vec![hand_b],
        },
    )
    .unwrap();

    assert!(state.players[0].graveyard.contains(&hand_b));
    assert_eq!(
        state.players[0].hand.len(),
        hand_before + 1,
        "drew two, discarded one"
    );
}
