//! Regression (#9213): Null Summoner's threshold line — "As long as there are
//! seven or more cards in your graveyard, you may cast the exiled card, and
//! mana of any type can be spent to cast that spell." — lowered to a
//! `Continuous` static with the threshold gate and no modification: neither
//! the permission nor its concession existed; coverage showed only a generic
//! `Swallow:Optional_YouMay` warning.
//!
//! Fix: "the exiled card" names the pool the card's own enters trigger exiled
//! into (CR 607.2a), so the line is the persistent exile-cast permission with
//! the any-type concession (CR 609.4b + CR 118.14), gated by threshold
//! (CR 611.3a). The tests drive the real enters trigger and then cast the
//! exiled {G} sorcery with a Swamp; the same board with six cards in the
//! graveyard is the paired "no".

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const NULL_SUMMONER: &str = "When this creature enters, if you cast it, target opponent reveals \
their hand. You choose a nonland card from it. Exile that card.\n\
Threshold — As long as there are seven or more cards in your graveyard, you may cast the exiled \
card, and mana of any type can be spent to cast that spell.";

struct Board {
    runner: GameRunner,
    green: ObjectId,
    swamps: [ObjectId; 2],
}

/// P0 casts Null Summoner for free with `graveyard` cards in their graveyard;
/// the enters trigger exiles P1's {G} sorcery (P1's land stays in hand).
fn summon_and_exile(graveyard: usize) -> Board {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..graveyard {
        scenario.add_spell_to_graveyard(P0, &format!("Filler {i}"), false);
    }
    let green = {
        let mut b = scenario.add_spell_to_hand(P1, "Green Sorcery", false);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        });
        b.id()
    };
    scenario.add_land_to_hand(P1, "Forest");
    let swamps = [
        scenario.add_basic_land(P0, ManaColor::Black),
        scenario.add_basic_land(P0, ManaColor::Black),
    ];
    let summoner = {
        let mut b =
            scenario.add_creature_to_hand_from_oracle(P0, "Null Summoner", 4, 3, NULL_SUMMONER);
        b.with_mana_cost(ManaCost::default());
        b.id()
    };
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&summoner].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: summoner,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell accepted");

    let mut chose = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Player(P1)),
                    })
                    .expect("ChooseTarget accepted");
            }
            WaitingFor::RevealChoice { cards, .. } => {
                assert!(
                    cards.contains(&green),
                    "the revealed hand offers the sorcery"
                );
                runner
                    .act(GameAction::SelectCards { cards: vec![green] })
                    .expect("SelectCards accepted");
                chose = true;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt while the enters trigger resolves: {other:?}"),
        }
    }
    assert!(chose, "the enters trigger asked for the nonland card");
    assert_eq!(
        runner.state().objects[&green].zone,
        Zone::Exile,
        "the chosen card was exiled"
    );
    Board {
        runner,
        green,
        swamps,
    }
}

fn cast_is_offered(runner: &GameRunner, card: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == card),
    )
}

/// Threshold met: the exiled {G} sorcery is cast with one Swamp.
#[test]
fn threshold_casts_the_exiled_card_with_any_type_mana() {
    let Board {
        mut runner,
        green,
        swamps,
    } = summon_and_exile(7);

    assert!(
        cast_is_offered(&runner, green),
        "with seven cards in the graveyard the exiled card is offered"
    );
    let card_id = runner.state().objects[&green].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: green,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the exiled card is cast with a Swamp");
    for _ in 0..10 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt while the exiled card resolves: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().objects[&green].zone,
        Zone::Graveyard,
        "the sorcery resolved into its owner's graveyard"
    );
    assert_eq!(
        swamps
            .iter()
            .filter(|swamp| runner.state().objects[swamp].tapped)
            .count(),
        1,
        "exactly one Swamp paid for {{G}}"
    );
}

/// Same board, six cards: the gate is closed, so the card is not offered.
/// Green without the fix too (there was no permission at all); it pins the
/// threshold gate, the test above pins the permission.
#[test]
fn below_threshold_the_exiled_card_is_not_offered() {
    let Board { runner, green, .. } = summon_and_exile(6);
    assert!(
        !cast_is_offered(&runner, green),
        "with six cards in the graveyard the exiled card is not offered"
    );
}
