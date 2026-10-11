//! Scroll Rack — "the exiled cards" after Scroll Rack was blinked in response.
//!
//! Oracle:
//!   "{1}, {T}: Exile any number of cards from your hand face down. Put that
//!    many cards from the top of your library into your hand. Then look at the
//!    exiled cards and put them on top of your library in any order."
//!
//! CR 113.7a: the activated ability exists independently of Scroll Rack, so it
//! still resolves after Ghostly Flicker returned Rack as a new object (CR 400.7).
//! CR 607.2a: "that many" and "the exiled cards" are the cards this ability's
//! own instruction exiled, which it finds in the same resolution.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SCROLL_RACK: &str = "{1}, {T}: Exile any number of cards from your hand face down. Put that \
     many cards from the top of your library into your hand. Then look at the exiled cards and \
     put them on top of your library in any order.";

const GHOSTLY_FLICKER: &str = "Exile two target artifacts, creatures, and/or lands you control, \
     then return those cards to the battlefield under your control.";

/// CR 113.7a + CR 607.2a: Rack's ability resolves after Ghostly Flicker
/// returned Rack. It exiles both hand cards, draws two, and puts the two exiled
/// cards back on top of the library.
#[test]
fn blinked_scroll_rack_still_returns_the_cards_it_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rack = scenario
        .add_artifact_from_oracle(P0, "Scroll Rack", SCROLL_RACK)
        .id();
    let land = scenario.add_land_from_oracle(P0, "Blank Land", "").id();
    let flicker = scenario
        .add_spell_to_hand_from_oracle(P0, "Ghostly Flicker", true, GHOSTLY_FLICKER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let hand_cards = [
        scenario.add_spell_to_hand(P0, "Hand One", true).id(),
        scenario.add_spell_to_hand(P0, "Hand Two", true).id(),
    ];
    // Added bottom first: the last two end up on top of the library.
    let library: Vec<ObjectId> = (0..4)
        .map(|i| {
            scenario
                .add_spell_to_library_top(P0, &format!("Library {i}"), true)
                .id()
        })
        .collect();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    let incarnation_before = runner.state().objects[&rack].incarnation;

    runner
        .act(GameAction::ActivateAbility {
            source_id: rack,
            ability_index: 0,
        })
        .expect("activate Scroll Rack");
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner
            .act(GameAction::PassPriority)
            .expect("pay {1} from the pool");
    }
    assert_eq!(runner.state().stack.len(), 1, "Rack's ability waits");

    let outcome = runner
        .cast(flicker)
        .target_objects(&[rack, land])
        .effect_zone(&hand_cards)
        .resolve();

    let state = outcome.state();
    assert!(state.stack.is_empty(), "Rack's ability resolved");
    assert_ne!(
        state.objects[&rack].incarnation, incarnation_before,
        "reach-guard: Rack came back as a new object before its ability resolved"
    );
    for card in hand_cards {
        assert!(
            outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged { object_id, from: Some(Zone::Hand), to: Zone::Exile, .. }
                    if *object_id == card
            )),
            "reach-guard: the ability exiled {card:?} from hand"
        );
    }
    let top_two: Vec<ObjectId> = state.players[0].library.iter().take(2).copied().collect();
    for card in hand_cards {
        assert!(
            top_two.contains(&card),
            "the exiled card {card:?} is back on top of the library; top = {top_two:?}"
        );
    }
    for card in &library[2..] {
        assert_eq!(
            state.objects[card].zone,
            Zone::Hand,
            "\"that many\" drew the top two library cards"
        );
    }
}
