//! Issue #8059: a "reveal the top N cards, keep one, put the rest away" flow
//! must not leave the unpicked cards visible to the casting player after
//! resolution finishes.
//!
//! CR 701.20a: a card that has been revealed stays revealed only as long as
//! the instruction that revealed it needs it. CR 701.20d: revealed cards that
//! are shuffled or otherwise reordered within a library stop being revealed.
//! CR 401.2: once the instruction has put the unkept cards back into the
//! library, no player may know that library's order — including the owner who
//! was just shown the cards.
//!
//! The engine records a reveal-dig's cards in `GameState::revealed_cards`,
//! which is the channel `filter_state_for_viewer` consults when it decides
//! whether a library card's face is visible. Closing the reveal window is the
//! reveal-markers cleanup on the dig's completion tail.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall, `Ancient Stirrings`): reveal the top five,
/// keep one from among them, put the rest on the bottom in any order.
const ANCIENT_STIRRINGS: &str = "Look at the top five cards of your library. You may reveal a colorless card from among them and put it into your hand. Then put the rest on the bottom of your library in any order.";

struct Fixture {
    runner: GameRunner,
    /// The five cards the spell revealed, topmost first.
    revealed: Vec<ObjectId>,
    /// A card beneath them that the spell never reveals.
    deep: ObjectId,
}

/// Cast Ancient Stirrings and stop at its keep decision, with the top card
/// (`revealed[0]`) still unchosen.
fn cast_ancient_stirrings_to_its_choice() -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The never-revealed card goes in first, then the five revealed cards are
    // stacked on top of it (`library[0]` is the top).
    let deep = scenario.add_card_to_library_top(P0, "Deep Library Card");
    let mut revealed = Vec::new();
    for index in 0..5 {
        let name = format!("Revealed Card {index}");
        revealed.push(scenario.add_card_to_library_top(P0, &name));
    }
    revealed.reverse();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Ancient Stirrings", false, ANCIENT_STIRRINGS)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Ancient Stirrings must cast");

    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while resolving");
            }
            WaitingFor::DigChoice { .. } => break,
            ref other => panic!("unexpected prompt before the keep decision: {other:?}"),
        }
    }

    Fixture {
        runner,
        revealed,
        deep,
    }
}

/// The fixture's reach guard, and the leak's precondition: the spell really is
/// showing five cards, and that reveal is still open while the choice is
/// pending.
#[test]
fn reveal_pick_window_is_open_while_the_choice_is_pending() {
    let fixture = cast_ancient_stirrings_to_its_choice();

    let WaitingFor::DigChoice {
        player,
        cards,
        up_to,
        ..
    } = fixture.runner.state().waiting_for.clone()
    else {
        panic!(
            "Ancient Stirrings must reach its keep decision; got {:?}",
            fixture.runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert!(up_to, "the reveal is optional: \"you may reveal ...\"");
    assert_eq!(
        cards, fixture.revealed,
        "the dig must offer exactly the five cards it revealed"
    );
    for id in &fixture.revealed {
        assert!(
            fixture.runner.state().revealed_cards.contains(id),
            "the revealed card {id:?} must be publicly revealed during the choice"
        );
    }
    assert!(
        !fixture.revealed.contains(&fixture.deep),
        "the card beneath the reveal window is never looked at"
    );
}

/// CR 701.20a + CR 701.20d + CR 401.2 (issue #8059): after the keep decision
/// reorders the unkept cards onto the bottom of the library, neither the caster
/// nor the opponent keeps seeing their faces.
#[test]
fn reveal_pick_closes_the_window_on_the_unkept_library_cards() {
    let mut fixture = cast_ancient_stirrings_to_its_choice();

    let WaitingFor::DigChoice { cards, .. } = fixture.runner.state().waiting_for.clone() else {
        panic!("the dig's keep decision must be pending");
    };
    let kept = cards[0];
    let unkept: Vec<ObjectId> = cards[1..].to_vec();

    fixture
        .runner
        .act(GameAction::SelectCards { cards: vec![kept] })
        .expect("keep the top revealed card");
    for _ in 0..8 {
        if matches!(
            fixture.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ) && fixture.runner.state().stack.is_empty()
        {
            break;
        }
        fixture
            .runner
            .act(GameAction::PassPriority)
            .expect("pass priority to finish the spell");
    }

    assert_eq!(
        fixture.runner.state().objects[&kept].zone,
        Zone::Hand,
        "the chosen card must reach the caster's hand"
    );
    let library = fixture.runner.state().library_of(P0).clone();
    assert_eq!(
        library.last(),
        unkept.last(),
        "the unkept cards go to the bottom of the library; library = {library:?}"
    );

    let caster_view = filter_state_for_viewer(fixture.runner.state(), P0);
    let opponent_view = filter_state_for_viewer(fixture.runner.state(), P1);
    for id in &unkept {
        assert_eq!(
            caster_view.objects[id].name, "Hidden Card",
            "the unkept card {id:?} went back into the library and must not stay visible to the caster"
        );
        assert_eq!(
            opponent_view.objects[id].name, "Hidden Card",
            "the unkept card {id:?} must not stay visible to the opponent either"
        );
        assert!(
            !fixture.runner.state().revealed_cards.contains(id),
            "the reveal marker for {id:?} must be dropped once the instruction ends"
        );
    }
    assert_eq!(
        caster_view.objects[&kept].name, "Revealed Card 4",
        "the kept card sits in the caster's hand and stays visible to them"
    );
}
