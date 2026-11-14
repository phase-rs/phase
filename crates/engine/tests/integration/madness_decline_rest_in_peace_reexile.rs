//! CR 702.35a + CR 400.8: a declined madness card is put into its owner's
//! graveyard from exile. Under Rest in Peace that move is replaced with exile,
//! and "if an object in the exile zone is exiled, it doesn't change zones, but
//! it becomes a new object that has just been exiled." The shared zone mover
//! must therefore treat the exile→exile delivery as a real move: a new
//! incarnation, with the previous exile's record (who exiled it) cleared.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim printed Oracle text (Scryfall).
const FIERY_TEMPER: &str = "Fiery Temper deals 3 damage to any target.\nMadness {R} (If you discard this card, discard it into exile. When you do, cast it for its madness cost or put it into your graveyard.)";
const PUTRID_IMP: &str = "Discard a card: This creature gains flying until end of turn.\nThreshold — As long as there are seven or more cards in your graveyard, this creature gets +1/+1 and can't block.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

#[test]
fn declined_madness_card_under_rest_in_peace_is_re_exiled_as_a_new_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let imp = scenario
        .add_creature_from_oracle(P0, "Putrid Imp", 1, 1, PUTRID_IMP)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
    let temper = scenario
        .add_spell_to_hand_from_oracle(P0, "Fiery Temper", true, FIERY_TEMPER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();

    // Discard Fiery Temper to pay Putrid Imp's cost.
    runner
        .act(GameAction::ActivateAbility {
            source_id: imp,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Putrid Imp's discard ability must be activatable");

    let mut offered = None;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![temper],
                    })
                    .expect("select Fiery Temper to discard");
            }
            WaitingFor::CastOffer {
                kind: CastOfferKind::Madness { object_id, .. },
                ..
            } => {
                offered = Some(object_id);
                break;
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the stack");
            }
            other => panic!("unexpected prompt before the madness offer: {other:?}"),
        }
    }
    let offered = offered.expect("the madness trigger must offer the cast");
    assert_eq!(offered, temper);

    let exiled = &runner.state().objects[&temper];
    assert_eq!(exiled.zone, Zone::Exile, "madness discards into exile");
    let incarnation_before = exiled.incarnation;
    let exiled_by_before = exiled.exiled_by;

    // CR 702.35a: decline — the card would go to its owner's graveyard.
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the madness cast must be legal");

    let card = &runner.state().objects[&temper];
    // CR 614.6: Rest in Peace exiles it instead of putting it into the graveyard.
    assert_eq!(card.zone, Zone::Exile);
    assert!(runner.state().players[0].graveyard.is_empty());
    // CR 400.8: re-exiling makes a new object that has just been exiled.
    assert_ne!(
        card.incarnation, incarnation_before,
        "the re-exiled card must be a new object"
    );
    // CR 400.7 + CR 406.6: the old exile's record does not carry over; no player
    // performs the declined madness move.
    assert_eq!(
        card.exiled_by, None,
        "the new exile object must not inherit the previous exile record \
         (was {exiled_by_before:?})"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the decline must return to priority"
    );
}
