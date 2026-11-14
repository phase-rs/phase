//! Regression for issue #3873: Teferi's Ageless Insight must double multi-card draws.
//!
//! https://github.com/phase-rs/phase/issues/3873

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::resolution::FrameKind;
use engine::types::zones::Zone;

use crate::draw_from_general_post_replacement::{
    choose_replacement, draw_then_gain_life, dredge_draw_and_life_order, frame_kinds,
    APPLY_REPLACEMENT, EIGHT_CARD_LIBRARY,
};

const TEFERI_ORACLE: &str =
    "If you would draw a card except the first one you draw in each of your draw steps, draw two cards instead.";

const DRAW_FOUR_ORACLE: &str = "Draw four cards.";

#[test]
fn teferi_ageless_insight_doubles_multi_card_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..12 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {i}"), true);
    }
    scenario
        .add_creature_from_oracle(P0, "Teferi's Ageless Insight", 0, 0, TEFERI_ORACLE)
        .as_enchantment();
    let draw_four = scenario
        .add_spell_to_hand_from_oracle(P0, "Inspiration", false, DRAW_FOUR_ORACLE)
        .id();
    let mana: Vec<ManaUnit> = (0..8)
        .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(P0, mana);

    let mut runner = scenario.build();
    runner.cast(draw_four).resolve();

    assert_eq!(
        runner.state().players[0].hand.len(),
        8,
        "Teferi must double a draw-four spell into eight cards"
    );
}

/// A "draw `cards`, then gain 3 life" effect with Stinkweed Imp in P0's graveyard and Teferi's Ageless Insight
/// on P0's battlefield, paused on the first draw's ordering choice. In the main phase every draw is replaceable.
/// Returns the runner, the Imp and Teferi's Ageless Insight.
fn teferi_and_dredge_draw(cards: i32) -> (GameRunner, ObjectId, ObjectId) {
    let mut teferi = None;
    let (runner, imp) = draw_then_gain_life(cards, |scenario| {
        teferi = Some(
            scenario
                .add_enchantment_from_oracle(P0, "Teferi's Ageless Insight", TEFERI_ORACLE)
                .id(),
        );
    });
    (
        runner,
        imp,
        teferi.expect("Teferi's Ageless Insight was built"),
    )
}

/// CR 616.1: Dredge and Teferi's Ageless Insight both apply to the pending draw; choose Teferi's.
fn choose_teferi(runner: &mut GameRunner, teferi: ObjectId, events: &mut Vec<GameEvent>) {
    let candidates = &runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the draw's ordering record owns the slot")
        .candidates;
    let substitute = candidates
        .iter()
        .position(|candidate| candidate.source == teferi)
        .expect("Teferi's Ageless Insight is offered");
    choose_replacement(runner, substitute, events);
}

/// CR 616.1 + CR 614.11a + CR 121.6b: a one-card draw is replaced by Teferi's Ageless Insight, whose "draw two
/// cards instead" is itself a draw instruction; its first draw is dredged. This row pins the end state of a
/// nested substituted draw with Dredge applied: every draw settles before the effect's next instruction and
/// nothing is left on the resolution stack. Teferi's draw two is a nested sequence inside the same draw frame,
/// so no post-replacement frame separates two draw frames here.
#[test]
fn a_dredged_draw_inside_a_substituted_draw_settles_before_the_effects_next_instruction() {
    let (mut runner, imp, teferi) = teferi_and_dredge_draw(1);
    let mut events = Vec::new();
    choose_teferi(&mut runner, teferi, &mut events);
    // Dredge the substitute instruction's first draw.
    assert_eq!(
        frame_kinds(runner.state()),
        vec![
            FrameKind::AbilityContinuation,
            FrameKind::PostReplacement,
            FrameKind::MultiDraw
        ]
    );
    choose_replacement(&mut runner, APPLY_REPLACEMENT, &mut events);

    let state = runner.state();
    assert_eq!(
        dredge_draw_and_life_order(&events, imp),
        vec!["imp returned", "drawn", "life"]
    );
    assert_eq!(state.objects[&imp].zone, Zone::Hand);
    assert_eq!(state.players[0].library.len(), EIGHT_CARD_LIBRARY.len() - 6);
    assert_eq!(state.players[0].hand.len(), 2, "the Imp and one drawn card");
    assert_eq!(state.players[0].life, 23);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(
        state.resolution_stack.is_empty(),
        "got {:?}",
        frame_kinds(state)
    );
}
