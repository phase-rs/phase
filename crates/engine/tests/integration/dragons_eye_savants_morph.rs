//! CR 702.37a + CR 702.37e + CR 701.20a: Dragon's Eye Savants.
//!
//! "Morph—Reveal a blue card in your hand.
//! When this creature is turned face up, look at target opponent's hand."
//!
//! Covers the whole class of "Morph—Reveal a [color] card in your hand"
//! creatures: the card is castable face down for {3}, the reveal cost can be
//! paid only with a matching card from hand, and the turned-face-up trigger
//! fires after the non-mana payment.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const SAVANTS_ORACLE: &str = "Morph—Reveal a blue card in your hand. (You may cast this card face down as a 2/2 creature for {3}. Turn it face up any time for its morph cost.)\nWhen this creature is turned face up, look at target opponent's hand.";

struct Setup {
    runner: GameRunner,
    savants: ObjectId,
    blue_card: Option<ObjectId>,
    red_card: ObjectId,
    opponent_card: ObjectId,
    third_player_card: ObjectId,
}

fn setup(with_blue_card: bool) -> Setup {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let savants = scenario
        .add_creature_to_hand(P0, "Dragon's Eye Savants", 0, 6)
        .from_oracle_text(SAVANTS_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let blue_card = with_blue_card.then(|| {
        scenario
            .add_creature_to_hand(P0, "Blue Creature", 1, 1)
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Blue],
            })
            .id()
    });
    let red_card = scenario
        .add_creature_to_hand(P0, "Red Creature", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let opponent_card = scenario
        .add_creature_to_hand(P1, "Opponent Card", 1, 1)
        .id();
    let third_player_card = scenario
        .add_creature_to_hand(P2, "Third Player Card", 1, 1)
        .id();

    // Three colorless mana: only the {3} face-down cast is affordable.
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, savants, false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    cast_face_down(&mut runner, savants);

    Setup {
        runner,
        savants,
        blue_card,
        red_card,
        opponent_card,
        third_player_card,
    }
}

/// CR 702.37a: Cast the morph card face down as a 2/2 for {3}.
fn cast_face_down(runner: &mut GameRunner, savants: ObjectId) {
    let card_id = runner.state().objects[&savants].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: savants,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Dragon's Eye Savants must be castable face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();

    let obj = &runner.state().objects[&savants];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.face_down, "must enter the battlefield face down");
    assert_eq!(obj.power, Some(2));
    assert_eq!(obj.toughness, Some(2));
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "face-down cast must spend exactly {{3}}"
    );
}

#[test]
fn dragons_eye_savants_reveal_blue_card_turns_face_up_and_looks_at_opponent_hand() {
    let Setup {
        mut runner,
        savants,
        blue_card,
        red_card,
        opponent_card,
        third_player_card,
    } = setup(true);
    let blue_card = blue_card.unwrap();

    // CR 702.37e: Turning face up is a special action paid with the morph cost.
    runner
        .act(GameAction::TurnFaceUp {
            object_id: savants,
            x: 0,
        })
        .expect("turn face up must be offered with a blue card in hand");

    // CR 701.20a: Only blue cards in hand can be revealed for this cost.
    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::Reveal,
            count,
            choices,
            ..
        } => {
            assert_eq!(*count, 1);
            assert!(choices.contains(&blue_card));
            assert!(
                !choices.contains(&red_card),
                "a non-blue card must not pay the reveal cost"
            );
        }
        other => panic!("expected PayCost(Reveal), got {other:?}"),
    }
    assert!(
        runner
            .act(GameAction::SelectCards {
                cards: vec![red_card],
            })
            .is_err(),
        "revealing a red card must be rejected"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![blue_card],
        })
        .expect("reveal the blue card");

    // CR 701.20b: The revealed card stays in hand.
    assert_eq!(runner.state().objects[&blue_card].zone, Zone::Hand);
    let obj = &runner.state().objects[&savants];
    assert!(!obj.face_down);
    assert_eq!(obj.name, "Dragon's Eye Savants");
    assert_eq!(obj.power, Some(0));
    assert_eq!(obj.toughness, Some(6));

    // CR 702.37e: Turning face up is a special action; the "When this creature is turned face up" trigger fires afterward.
    // CR 603.3d: With two opponents, the target opponent must be chosen; select P1.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "three-player game must prompt for the target opponent, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![engine::types::ability::TargetRef::Player(P1)],
        })
        .expect("target P1");
    assert!(
        !runner.state().stack.is_empty(),
        "turned-face-up trigger must be on the stack"
    );
    runner.resolve_top();

    // CR 701.20e: Looking at the opponent's hand shows it to the controller only.
    assert_eq!(runner.state().private_look_player, Some(P0));
    assert!(runner.state().private_look_ids.contains(&opponent_card));
    assert!(!runner.state().private_look_ids.contains(&blue_card));
    assert!(!runner.state().private_look_ids.contains(&third_player_card));
    assert_eq!(runner.state().objects[&opponent_card].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&third_player_card].zone, Zone::Hand);
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected priority after the look resolves, got {:?}",
        runner.state().waiting_for
    );

    // CR 701.20e: Verify player-visible projections through filter_state_for_viewer:
    // P0 (looker) sees the privately looked-at card in P1's hand, but not P2's card.
    let p0_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P0);
    assert_eq!(p0_view.objects[&opponent_card].name, "Opponent Card");
    assert!(!p0_view.objects[&opponent_card].face_down);
    assert_eq!(p0_view.objects[&third_player_card].name, "Hidden Card");
    assert!(p0_view.objects[&third_player_card].face_down);

    // P2 (uninvolved third player) does NOT see P1's card.
    let p2_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P2);
    assert_eq!(p2_view.objects[&opponent_card].name, "Hidden Card");
    assert!(p2_view.objects[&opponent_card].face_down);
    assert_eq!(
        p2_view.objects[&third_player_card].name,
        "Third Player Card"
    );
    assert!(!p2_view.objects[&third_player_card].face_down);

    // P1 (owner of opponent_card) continues to see their own card.
    let p1_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P1);
    assert_eq!(p1_view.objects[&opponent_card].name, "Opponent Card");
    assert!(!p1_view.objects[&opponent_card].face_down);
    assert_eq!(p1_view.objects[&third_player_card].name, "Hidden Card");
    assert!(p1_view.objects[&third_player_card].face_down);
}

#[test]
fn dragons_eye_savants_without_blue_card_cannot_turn_face_up() {
    let Setup {
        mut runner,
        savants,
        red_card,
        ..
    } = setup(false);

    // CR 118.3: The morph cost can't be paid without a blue card in hand.
    assert!(
        runner
            .act(GameAction::TurnFaceUp {
                object_id: savants,
                x: 0,
            })
            .is_err(),
        "turn face up must be rejected with no blue card in hand"
    );
    assert!(runner.state().objects[&savants].face_down);
    assert_eq!(runner.state().objects[&red_card].zone, Zone::Hand);
}
