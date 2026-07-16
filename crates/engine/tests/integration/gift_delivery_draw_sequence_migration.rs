//! Integration coverage for Gift a card using the shared draw-sequence path.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{StaticDefinition, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{ProhibitionScope, StaticMode};
use engine::types::zones::Zone;
use engine::types::ObjectId;

use crate::support::shared_card_db as load_db;

const COILING_REBIRTH: &str =
    "Gift a card (You may promise an opponent a gift as you cast this spell. \
If you do, they draw a card before its other effects.)\n\
Return target creature card from your graveyard to the battlefield. Then if the gift was promised \
and that creature isn't legendary, create a token that's a copy of that creature, except it's 1/1.";

const STINKWEED_IMP_ORACLE: &str = "Flying\n\
Whenever this creature deals combat damage to a creature, destroy that creature.\n\
Dredge 5 (If you would draw a card, you may mill five cards instead. If you do, return this card from your graveyard to your hand.)";

#[derive(Debug, PartialEq, Eq)]
enum GiftResolution {
    ReplacementChoice,
    Resolved,
}

fn cast_promised_coiling(
    runner: &mut GameRunner,
    spell: ObjectId,
    target: ObjectId,
) -> GiftResolution {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Coiling Rebirth");

    for _ in 0..200 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("complete mana payment");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("promise Gift a card");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose Coiling Rebirth target");
            }
            WaitingFor::OrderTriggers { .. } | WaitingFor::Priority { .. }
                if !runner.state().stack.is_empty() =>
            {
                runner
                    .act(GameAction::PassPriority)
                    .expect("advance promised Gift spell");
            }
            WaitingFor::ReplacementChoice { .. } => return GiftResolution::ReplacementChoice,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                return GiftResolution::Resolved;
            }
            other => panic!("unexpected Gift resolution prompt: {other:?}"),
        }
    }

    panic!("promised Gift spell did not resolve within 200 actions");
}

fn finish_resolving_stack(runner: &mut GameRunner) {
    for _ in 0..200 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finish resolving Gift spell");
            }
            other => panic!("unexpected prompt while finishing Gift spell: {other:?}"),
        }
    }

    panic!("Gift spell did not finish within 200 actions");
}

fn hand_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|entry| entry.id == player)
        .expect("player exists")
        .hand
        .len()
}

fn library_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|entry| entry.id == player)
        .expect("player exists")
        .library
        .len()
}

fn object_zone(runner: &GameRunner, object_id: ObjectId) -> Zone {
    runner.state().objects[&object_id].zone
}

fn promised_gift_scenario() -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature_to_graveyard(P0, "Coiling Target", 3, 3)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Coiling Rebirth", false, COILING_REBIRTH)
        .id();
    (scenario, spell, target)
}

/// CR 702.52a + CR 121.6b: A Dredge replacement on the Gift recipient's draw
/// pauses the Gift resolution; declining it resumes and performs the normal draw.
#[test]
fn gift_card_draw_pauses_on_dredge_and_resumes() {
    let Some(db) = load_db() else {
        return;
    };

    let (mut scenario, spell, target) = promised_gift_scenario();
    let drawn_card = scenario.add_real_card(P1, "Plains", Zone::Library, db);
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    scenario
        .add_creature_to_graveyard(P1, "Stinkweed Imp", 1, 1)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert_eq!(
        cast_promised_coiling(&mut runner, spell, target),
        GiftResolution::ReplacementChoice,
        "Dredge must pause the Gift draw for its optional replacement choice"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline Dredge");
    finish_resolving_stack(&mut runner);

    assert_eq!(object_zone(&runner, drawn_card), Zone::Hand);
    assert_eq!(
        hand_count(&runner, P1),
        1,
        "declined Dredge must draw the card"
    );
}

/// CR 121.1: Pins the `start_draw_sequence` migration bug fix. Gift used to
/// call `select_cards_to_draw` directly, skipping `allowed_draw_count` and
/// incorrectly drawing through a `CantDraw` static.
#[test]
fn gift_card_draw_respects_cant_draw_static() {
    let (mut scenario, spell, target) = promised_gift_scenario();
    let gift_card = scenario.add_card_to_library_top(P1, "Gift Recipient Card");
    let cant_draw_source = scenario.add_creature(P1, "Cant Draw Source", 1, 1).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&cant_draw_source)
        .unwrap()
        .static_definitions
        .push(StaticDefinition::new(StaticMode::CantDraw {
            who: ProhibitionScope::AllPlayers,
        }));

    let hand_before = hand_count(&runner, P1);
    let library_before = library_count(&runner, P1);
    assert_eq!(
        cast_promised_coiling(&mut runner, spell, target),
        GiftResolution::Resolved
    );

    assert_eq!(hand_count(&runner, P1), hand_before);
    assert_eq!(library_count(&runner, P1), library_before);
    assert_eq!(object_zone(&runner, gift_card), Zone::Library);
}
