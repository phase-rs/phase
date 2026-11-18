//! Issue #8068: Kaalia, Zenith Seeker — every listed card type is selectable
//! from the revealed six.
//!
//! Oracle (verbatim, Scryfall-verified): "Flying, vigilance / When Kaalia
//! enters, look at the top six cards of your library. You may reveal an Angel
//! card, a Demon card, and/or a Dragon card from among them and put them into
//! your hand. Put the rest on the bottom of your library in a random order."
//!
//! The reveal clause is a card-TYPE LIST (CR 205.3a), so each leg's
//! informational "card" noun sits immediately before the list separator. The
//! parser only recognized that noun when a SPACE followed it, so the leftmost
//! leg's noun stayed in the remainder, the type-union fold never saw its
//! separator, and the published keep filter collapsed to `Angel` alone. Every
//! Demon and Dragon among the revealed six was unselectable in the DigChoice
//! prompt — the reported defect.
//!
//! This test drives the real pipeline (cast → ETB trigger → CR 701.20e
//! look/select pause) and answers the prompt with a Demon, the type the report
//! says cannot be picked.
//!
//! CR 701.20e + CR 608.2c: the looked-at cards are shown only to the looking
//! player, who follows the instruction as written.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::DigRestOrder;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text — a paraphrase can take a different parser branch and go
/// green while the real card stays broken.
const KAALIA_ORACLE: &str = "Flying, vigilance\nWhen Kaalia enters, look at the \
top six cards of your library. You may reveal an Angel card, a Demon card, \
and/or a Dragon card from among them and put them into your hand. Put the rest \
on the bottom of your library in a random order.";

/// Kaalia's printed cost, {R}{W}{B}.
fn kaalia_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![
            ManaCostShard::Red,
            ManaCostShard::White,
            ManaCostShard::Black,
        ],
        generic: 0,
    }
}

/// Put one unit of each colored mana into P0's pool so the cast genuinely pays
/// Kaalia's printed {R}{W}{B} cost rather than a zero-cost shortcut.
fn seed_kaalia_mana(runner: &mut GameRunner) {
    for ty in [ManaType::Red, ManaType::White, ManaType::Black] {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&id)
        .unwrap_or_else(|| panic!("{id:?} must still exist"))
        .zone
}

#[test]
fn kaalia_reveal_list_makes_angel_demon_and_dragon_selectable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut builder =
        scenario.add_creature_to_hand_from_oracle(P0, "Kaalia, Zenith Seeker", 3, 3, KAALIA_ORACLE);
    builder.with_mana_cost(kaalia_cost());
    let kaalia = builder.id();

    // Staged bottom-up: each call puts the card on top of the library. The top
    // six end up Angel, Demon, Dragon, then three non-matching fillers.
    let filler_bottom = scenario.add_card_to_library_top(P0, "Library Filler A");
    let filler_middle = scenario.add_card_to_library_top(P0, "Library Filler B");
    let filler_top = scenario.add_card_to_library_top(P0, "Library Filler C");
    let dragon = scenario
        .add_spell_to_library_top(P0, "Test Dragon", false)
        .as_creature()
        .with_subtypes(vec!["Dragon"])
        .id();
    let demon = scenario
        .add_spell_to_library_top(P0, "Test Demon", false)
        .as_creature()
        .with_subtypes(vec!["Demon"])
        .id();
    let angel = scenario
        .add_spell_to_library_top(P0, "Test Angel", false)
        .as_creature()
        .with_subtypes(vec!["Angel"])
        .id();

    let mut runner = scenario.build();
    seed_kaalia_mana(&mut runner);

    let outcome = runner.cast(kaalia).resolve();

    // Kaalia resolved onto the battlefield, so the cast paid its cost.
    assert_eq!(
        zone_of(&runner, kaalia),
        Zone::Battlefield,
        "Kaalia must resolve onto the battlefield after the paid cast"
    );
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "the {{R}}{{W}}{{B}} cost must be paid from the seeded pool"
    );

    let WaitingFor::DigChoice {
        cards,
        selectable_cards,
        keep_count,
        up_to,
        kept_destination,
        rest_destination,
        rest_order,
        ..
    } = outcome.final_waiting_for().clone()
    else {
        panic!(
            "the ETB trigger must pause on the reveal choice, got {:?}",
            outcome.final_waiting_for()
        );
    };

    assert_eq!(cards.len(), 6, "all six looked-at cards are shown");
    assert_eq!(
        selectable_cards.len(),
        3,
        "exactly the Angel, Demon and Dragon are selectable; got {selectable_cards:?}"
    );
    for (id, label) in [(angel, "Angel"), (demon, "Demon"), (dragon, "Dragon")] {
        assert!(
            selectable_cards.contains(&id),
            "issue #8068: the {label} card must be selectable, got {selectable_cards:?}"
        );
    }
    for filler in [filler_top, filler_middle, filler_bottom] {
        assert!(
            !selectable_cards.contains(&filler),
            "a non-matching filler must not be selectable"
        );
    }

    // The instruction is "you may reveal <one card from the list> … and put it
    // into your hand. Put the rest on the bottom of your library in a random
    // order." Optional pick, hand-bound, library remainder.
    //
    // KNOWN GAP, deliberately not encoded as correct: the printed instruction is
    // per-category (WotC ruling 2019-07-12: "You can put up to three cards into
    // your hand this way: one Angel card, one Demon card, and/or one Dragon
    // card"), while `Dig` models one `keep_count`. This change restores the
    // SELECTABILITY of every listed type — the reported defect — and the single
    // keep count is today's lowering; taking one card of each type needs a
    // per-leg selection model, which is a separate change.
    assert_eq!(keep_count, 1, "current single-pick lowering");
    assert!(up_to, "\"you may reveal\" is optional");
    assert_eq!(kept_destination, Some(Zone::Hand));
    assert_eq!(rest_destination, Some(Zone::Library));
    assert_eq!(
        rest_order,
        DigRestOrder::Random,
        "the rest go on the bottom in a random order"
    );

    // Answer with the Demon — the type the report says cannot be picked.
    runner
        .act(GameAction::SelectCards { cards: vec![demon] })
        .expect("the Demon must be a legal reveal");

    assert_eq!(
        zone_of(&runner, demon),
        Zone::Hand,
        "the revealed Demon must be put into the controller's hand"
    );
    for rest in [angel, dragon, filler_top, filler_middle, filler_bottom] {
        assert_eq!(
            zone_of(&runner, rest),
            Zone::Library,
            "unchosen looked-at cards go to the bottom of the library, not the graveyard"
        );
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the trigger must fully resolve back to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "no ability may be left on the stack"
    );
}
