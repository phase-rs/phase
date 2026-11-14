//! Regression for issue #8056: a public reveal must stay visible to the
//! OPPONENTS, including after the revealed card is put into a hand.
//!
//! https://github.com/phase-rs/phase/issues/8056
//!
//! CR 701.20a: "To reveal a card, show that card to all players for a brief
//! time. If an effect causes a card to be revealed, it remains revealed for as
//! long as necessary to complete the parts of the effect that card is relevant
//! to." Revealing is public, so opponents are entitled to know WHICH card was
//! taken — not left to infer it from the game log and the graveyard.
//!
//! Two reported cards reach the same reveal primitive through different
//! wordings (fixture-free verbatim Oracle text for both):
//!
//! * Malevolent Rumble — "Reveal the top four cards of your library. You may
//!   put a permanent card from among them into your hand. Put the rest into
//!   your graveyard." → `Dig { reveal: true, destination: Hand }`.
//! * Narset, Parter of Veils, −2 — "Look at the top four cards of your library.
//!   You may reveal a noncreature, nonland card from among them and put it into
//!   your hand. Put the rest on the bottom of your library in a random order."
//!   → the same `Dig { reveal: true, destination: Hand }` shape.
//!
//! Pre-fix both exposed the revealed pile during the choice and then redacted
//! the taken card the instant it changed zones, because the zone-exit authority
//! retired the public reveal for EVERY destination. The fix keeps a PUBLIC
//! reveal alive on a move into a hand (the table already saw the card) while a
//! move into a library still retires it (CR 401.2: no player may know a
//! library's order).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::AbilityKind;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const HIDDEN: &str = "Hidden Card";

/// The name a viewer's projection reports for `id` ("Hidden Card" when redacted).
fn viewed_name(
    runner: &GameRunner,
    viewer: engine::types::player::PlayerId,
    id: ObjectId,
) -> String {
    filter_state_for_viewer(runner.state(), viewer)
        .objects
        .get(&id)
        .map(|object| object.name.clone())
        .expect("every zone occupant has a projected object")
}

/// Drive `runner` to the first `DigChoice`, passing priority on the way.
fn advance_to_dig_choice(runner: &mut GameRunner) {
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::DigChoice { .. } | WaitingFor::Priority { .. } => {
                if matches!(runner.state().waiting_for, WaitingFor::DigChoice { .. }) {
                    return;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is legal");
            }
            other => panic!("expected a DigChoice or Priority window, got {other:?}"),
        }
    }
    panic!("the reveal-dig never reached its choice");
}

/// Finish the resolution after a selection: drain the stack back to priority.
fn settle_to_priority(runner: &mut GameRunner) {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::DigChoice { .. } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .expect("declining the optional keep is legal");
            }
            _ => {
                let _ = runner.act(GameAction::PassPriority);
            }
        }
    }
    panic!("the reveal-dig never finished resolving");
}

fn add_green_mana(runner: &mut GameRunner, count: usize) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));
    }
}

fn set_creature_card_types(state: &mut engine::types::game_state::GameState, id: ObjectId) {
    let object = state.objects.get_mut(&id).expect("just created");
    object.card_types.core_types = vec![CoreType::Creature];
    object.base_card_types = object.card_types.clone();
}

/// Malevolent Rumble (Diabolic Intent-free wording): the four revealed cards
/// are public during the choice, and the card taken into hand stays public
/// afterwards — the opponent must not have to infer it from the log.
#[test]
fn malevolent_rumble_taken_card_stays_revealed_to_the_opponent() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let library: Vec<ObjectId> = ["Grizzly Bears", "Hill Giant", "Grizzly Bears", "Hill Giant"]
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Library, db))
        .collect();
    assert_eq!(library.len(), 4, "four permanents sit under the reveal");
    // Control: an unrelated card in the caster's hand that was never revealed.
    let private_card = scenario.add_card_to_hand(P0, "Private Filler");
    let rumble = scenario.add_real_card(P0, "Malevolent Rumble", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_green_mana(&mut runner, 2);

    let card_id = runner.state().objects[&rumble].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: rumble,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Malevolent Rumble");
    advance_to_dig_choice(&mut runner);

    let revealed_cards = match &runner.state().waiting_for {
        WaitingFor::DigChoice {
            cards,
            selectable_cards,
            ..
        } => {
            assert_eq!(
                selectable_cards.len(),
                cards.len(),
                "every revealed permanent is a legal keep"
            );
            cards.clone()
        }
        other => panic!("expected DigChoice, got {other:?}"),
    };
    assert_eq!(revealed_cards.len(), 4, "the top four cards are revealed");

    // During the choice the whole revealed pile is public to the opponent.
    for id in &revealed_cards {
        let real_name = runner.state().objects[id].name.clone();
        assert_eq!(
            viewed_name(&runner, P1, *id),
            real_name,
            "CR 701.20a: the revealed card must stay visible to the opponent during the choice"
        );
    }

    let taken = revealed_cards[0];
    let taken_name = runner.state().objects[&taken].name.clone();
    runner
        .act(GameAction::SelectCards { cards: vec![taken] })
        .expect("keeping one revealed permanent is legal");
    settle_to_priority(&mut runner);

    // The card the caster took ends up in hand ...
    assert_eq!(
        runner.state().objects[&taken].zone,
        Zone::Hand,
        "the kept card is put into its controller's hand"
    );
    // ... and CR 701.20a keeps the table's knowledge of WHICH card it is.
    assert_eq!(
        viewed_name(&runner, P1, taken),
        taken_name,
        "CR 701.20a: the revealed card taken into hand must stay identified to the opponent"
    );

    // The unrevealed control card in the same hand must stay hidden, so the
    // assertion above is reveal-scoped and not a blanket hand leak.
    assert_eq!(
        viewed_name(&runner, P1, private_card),
        HIDDEN,
        "an unrevealed card in the caster's hand must stay hidden from the opponent"
    );

    // The rest pile is in the graveyard — a public zone — so it stays named.
    for id in revealed_cards.iter().skip(1) {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Graveyard,
            "every unkept revealed card goes to the graveyard"
        );
        assert_ne!(
            viewed_name(&runner, P1, *id),
            HIDDEN,
            "graveyard cards remain public to the opponent"
        );
    }
}

/// Narset, Parter of Veils, −2: the same reveal primitive through a
/// planeswalker activation. The chosen card stays identified to the opponent
/// once it reaches the caster's hand, while the rest pile (bottom of a library)
/// is re-hidden — CR 401.2 keeps library order secret.
#[test]
fn narset_parter_of_veils_minus_two_taken_card_stays_revealed_to_the_opponent() {
    // Verbatim Oracle text (Scryfall, ab26fbe2-e808-48b9-8d0d-3fbb6c3d554f).
    const NARSET_ORACLE: &str = "Each opponent can't draw more than one card each turn.\n\u{2212}2: Look at the top four cards of your library. You may reveal a noncreature, nonland card from among them and put it into your hand. Put the rest on the bottom of your library in a random order.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Library, bottom-up (each call inserts at the top): the dig sees the
    // noncreature spell plus three creatures; "Deep Control" sits below them.
    let deep_card = scenario.add_card_to_library_top(P0, "Deep Control");
    let rest_creatures: Vec<ObjectId> = (0..3)
        .map(|i| scenario.add_card_to_library_top(P0, &format!("Library Bear {i}")))
        .collect();
    let spell = scenario.add_spell_to_library_top(P0, "Opt", true).id();

    let narset = scenario
        .add_planeswalker_from_oracle(P0, "Narset, Parter of Veils", "Narset", 5, NARSET_ORACLE)
        .id();
    let mut runner = scenario.build();
    // Only the noncreature, nonland card is a legal reveal in the looked-at pile.
    for id in &rest_creatures {
        set_creature_card_types(runner.state_mut(), *id);
    }
    runner.state_mut().layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(runner.state_mut());

    let minus_two = runner.state().objects[&narset]
        .abilities
        .iter()
        .position(|ability| matches!(ability.kind, AbilityKind::Activated))
        .expect("Narset's −2 parses to an activated loyalty ability");

    runner
        .act(GameAction::ActivateAbility {
            source_id: narset,
            ability_index: minus_two,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating Narset's −2 is legal");
    advance_to_dig_choice(&mut runner);

    let selectable = match &runner.state().waiting_for {
        WaitingFor::DigChoice {
            selectable_cards, ..
        } => selectable_cards.clone(),
        other => panic!("expected DigChoice, got {other:?}"),
    };
    assert_eq!(
        selectable,
        vec![spell],
        "only the noncreature, nonland card is a legal reveal"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![spell] })
        .expect("revealing the spell is legal");
    settle_to_priority(&mut runner);

    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Hand,
        "the revealed card is put into the caster's hand"
    );
    assert_eq!(
        viewed_name(&runner, P1, spell),
        "Opt",
        "CR 701.20a: the card revealed from among the looked-at pile must stay \
         identified to the opponent after it reaches hand"
    );

    // The rest pile went to the bottom of the library: CR 401.2 keeps the
    // library's order secret, so those identities are re-hidden. Close the
    // action boundary first — the momentary `revealed_cards` window a
    // reveal-dig opens ends with the instruction (CR 701.20a).
    let _ = runner.act(GameAction::PassPriority);
    for id in &rest_creatures {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Library,
            "the unkept looked-at cards go to the bottom of the library"
        );
        assert_eq!(
            viewed_name(&runner, P1, *id),
            HIDDEN,
            "a card put back into a library is hidden again (CR 401.2)"
        );
    }
    // The card below the look window was never looked at or revealed.
    assert_eq!(runner.state().objects[&deep_card].zone, Zone::Library);
    assert_eq!(
        viewed_name(&runner, P1, deep_card),
        HIDDEN,
        "a card outside the look window stays hidden throughout"
    );
}
