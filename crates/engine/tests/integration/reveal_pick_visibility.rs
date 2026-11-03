//! Issue #8059: a "reveal the top N cards, keep one, put the rest away" flow
//! must not leave the unpicked cards visible after resolution finishes.
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
//!
//! The cleanup regressions below use a genuine "Reveal the top N" instruction
//! (Gather the Pack), so the public reveal markers they assert while the choice
//! is pending are the ones the card's own text creates. The pick-and-bottom
//! flow is covered separately by Ancient Stirrings, the card shape the report's
//! thread is titled after: its "look at" wording is never used to justify a
//! public-reveal assertion while the choice is pending — that test asserts only
//! that the cards it puts back on the bottom stop being visible afterwards.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall, `Gather the Pack`).
const GATHER_THE_PACK: &str = "Reveal the top five cards of your library. You may put a creature card from among them into your hand. Put the rest into your graveyard.\nSpell mastery — If there are two or more instant and/or sorcery cards in your graveyard, put up to two creature cards from among the revealed cards into your hand instead of one.";

/// Verbatim Oracle text (Scryfall, `Ancient Stirrings`): look at the top five,
/// optionally reveal one of them, bottom the rest.
const ANCIENT_STIRRINGS: &str = "Look at the top five cards of your library. You may reveal a colorless card from among them and put it into your hand. Then put the rest on the bottom of your library in any order.";

struct Fixture {
    runner: GameRunner,
    /// The five cards the spell reveals, topmost first.
    revealed: Vec<ObjectId>,
    /// Their printed names, in the same order.
    revealed_names: Vec<String>,
    /// A card beneath them that the spell never reveals.
    deep: ObjectId,
}

/// Cast a reveal/look dig and stop at its keep decision, with the five cards
/// still on top of the library and nothing chosen yet. The library holds five
/// untyped cards the dig's filter never matches, so the keep choice offers no
/// eligible card — every test drives the "put the rest away" branch.
fn cast_dig_to_its_choice(
    name: &str,
    oracle_text: &str,
    mana_cost: ManaCost,
    mana: &[ManaType],
) -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The never-revealed card goes in first, then the five revealed cards are
    // stacked on top of it (`library[0]` is the top).
    let deep = scenario.add_card_to_library_top(P0, "Deep Library Card");
    let mut revealed = Vec::new();
    let mut revealed_names = Vec::new();
    for index in 0..5 {
        let card_name = format!("Revealed Card {index}");
        revealed.push(scenario.add_card_to_library_top(P0, &card_name));
        revealed_names.push(card_name);
    }
    revealed.reverse();
    revealed_names.reverse();

    let spell = {
        let mut builder = scenario.add_spell_to_hand_from_oracle(P0, name, false, oracle_text);
        builder.with_mana_cost(mana_cost);
        builder.id()
    };
    scenario.with_mana_pool(
        P0,
        mana.iter()
            .map(|kind| ManaUnit::new(*kind, spell, false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the dig spell must cast");

    drive_to_choice(&mut runner);

    Fixture {
        runner,
        revealed,
        revealed_names,
        deep,
    }
}

/// Run the spell to its `WaitingFor::DigChoice`, passing priority as needed.
fn drive_to_choice(runner: &mut GameRunner) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DigChoice { .. } => return,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while resolving");
            }
            ref other => panic!("unexpected prompt before the keep decision: {other:?}"),
        }
    }
    panic!(
        "the dig never reached its keep decision; waiting_for={:?}",
        runner.state().waiting_for
    );
}

/// Finish the spell after the keep decision: pass priority until the stack is
/// empty and the caster holds priority again.
fn finish_resolution(runner: &mut GameRunner) {
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
        {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to finish the spell");
    }
    panic!(
        "the spell never finished resolving; waiting_for={:?}",
        runner.state().waiting_for
    );
}

/// CR 701.20a: a genuine "Reveal the top five cards" instruction makes the
/// looked-at pile public while its keep decision is pending — every player sees
/// the faces, and the caster can see them well enough to choose.
#[test]
fn reveal_top_five_is_public_while_the_choice_is_pending() {
    let mut fixture = cast_dig_to_its_choice(
        "Gather the Pack",
        GATHER_THE_PACK,
        ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        },
        &[ManaType::Green, ManaType::Colorless],
    );

    assert!(
        fixture.runner.state().players[0].mana_pool.is_empty(),
        "the cast must actually pay {{1}}{{G}}, not bypass payment"
    );

    let WaitingFor::DigChoice {
        player,
        cards,
        up_to,
        selectable_cards,
        ..
    } = fixture.runner.state().waiting_for.clone()
    else {
        panic!(
            "Gather the Pack must reach its keep decision; got {:?}",
            fixture.runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert!(
        up_to,
        "the reveal is optional: \"you may put ... into your hand\""
    );
    assert_eq!(
        cards, fixture.revealed,
        "the dig must offer exactly the five cards it revealed"
    );
    assert!(
        selectable_cards.is_empty(),
        "none of the untyped library cards is a creature, so no card is keepable"
    );

    let opponent_view = filter_state_for_viewer(fixture.runner.state(), P1);
    for id in &fixture.revealed {
        assert!(
            fixture.runner.state().revealed_cards.contains(id),
            "the revealed card {id:?} must be publicly revealed during the choice"
        );
        assert_ne!(
            opponent_view.objects[id].name, "Hidden Card",
            "a real reveal instruction shows the card to the opponent while the choice is pending"
        );
    }
    assert!(
        !fixture.revealed.contains(&fixture.deep),
        "the card beneath the reveal window is never looked at"
    );

    // Leave the game in a settled state so the runner's drop checks stay quiet.
    fixture
        .runner
        .act(GameAction::SelectCards { cards: Vec::new() })
        .expect("decline to keep a card");
    finish_resolution(&mut fixture.runner);
}

/// CR 701.20a + CR 401.2 (issue #8059): once the keep decision settles, no card
/// the dig revealed stays marked as revealed. The unkept cards land in the
/// graveyard, where their faces are public zone information — not because a
/// stale reveal marker follows them there.
#[test]
fn reveal_pick_closes_the_window_when_the_instruction_settles() {
    let mut fixture = cast_dig_to_its_choice(
        "Gather the Pack",
        GATHER_THE_PACK,
        ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        },
        &[ManaType::Green, ManaType::Colorless],
    );

    fixture
        .runner
        .act(GameAction::SelectCards { cards: Vec::new() })
        .expect("decline to keep a card");
    finish_resolution(&mut fixture.runner);

    let library: Vec<ObjectId> = fixture
        .runner
        .state()
        .library_of(P0)
        .iter()
        .copied()
        .collect();
    assert_eq!(
        library,
        vec![fixture.deep],
        "every revealed card leaves the library; only the never-revealed card remains"
    );
    for id in &fixture.revealed {
        assert_eq!(
            fixture.runner.state().objects[id].zone,
            Zone::Graveyard,
            "the unkept revealed card {id:?} goes to the graveyard"
        );
        assert!(
            !fixture.runner.state().revealed_cards.contains(id),
            "the reveal marker for {id:?} must be dropped once the instruction ends"
        );
    }

    // Positive guard: the cards are visible again strictly as graveyard
    // contents (a public zone), which proves the projection is still willing to
    // show them once the reveal window itself is closed.
    let caster_view = filter_state_for_viewer(fixture.runner.state(), P0);
    let opponent_view = filter_state_for_viewer(fixture.runner.state(), P1);
    for id in &fixture.revealed {
        assert_ne!(
            caster_view.objects[id].name, "Hidden Card",
            "a card in the graveyard is public to the caster"
        );
        assert_ne!(
            opponent_view.objects[id].name, "Hidden Card",
            "a card in the graveyard is public to the opponent"
        );
    }
}

/// CR 701.20a + CR 701.20d + CR 401.2 (issue #8059): Ancient Stirrings looks at
/// the top five cards, keeps the one its controller reveals, and puts the rest
/// on the bottom of the library. Once that instruction finishes, neither player
/// keeps seeing the four cards that went back into the library.
///
/// This is the shape the report's thread is titled after. It deliberately says
/// nothing about what the pending choice shows — that is the card's own
/// reveal-vs-look question, covered above by a genuine "Reveal the top N" card
/// — so the test cannot enshrine a hidden-information leak as expected.
#[test]
fn ancient_stirrings_private_look_ends_when_the_unkept_cards_are_bottomed() {
    let mut fixture = cast_dig_to_its_choice(
        "Ancient Stirrings",
        ANCIENT_STIRRINGS,
        ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Green],
        },
        &[ManaType::Green],
    );

    assert!(
        fixture.runner.state().players[0].mana_pool.is_empty(),
        "the cast must actually pay {{G}}, not bypass payment"
    );

    let WaitingFor::DigChoice { cards, up_to, .. } = fixture.runner.state().waiting_for.clone()
    else {
        panic!("the look spell's keep decision must be pending");
    };
    assert!(
        up_to,
        "the keep is optional: \"you may reveal ... and put it into your hand\""
    );
    assert_eq!(
        cards, fixture.revealed,
        "the look must offer exactly the five cards it looked at"
    );

    // Positive guard: the caster can see the five cards they are choosing from.
    let caster_view = filter_state_for_viewer(fixture.runner.state(), P0);
    for id in &cards {
        assert_ne!(
            caster_view.objects[id].name, "Hidden Card",
            "the caster must see the card {id:?} they are choosing from"
        );
    }

    let kept = cards[0];
    let unkept: Vec<ObjectId> = cards[1..].to_vec();
    fixture
        .runner
        .act(GameAction::SelectCards { cards: vec![kept] })
        .expect("keep the top looked-at card");
    finish_resolution(&mut fixture.runner);

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

    // CR 701.20d + CR 401.2: a looked-at card put back into the library stops
    // being lookable — even for the player who just looked at it.
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
            "no reveal marker may be left for {id:?}"
        );
    }
    assert_eq!(
        caster_view.objects[&kept].name, fixture.revealed_names[0],
        "the kept card sits in the caster's hand and stays visible to them"
    );
}
