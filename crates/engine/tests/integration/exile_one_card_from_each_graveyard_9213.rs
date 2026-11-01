//! Issue #9213 — "exile a[n] <type> card [at random] from each [opponent's]
//! graveyard" exiles ONE card from EVERY graveyard of the named population
//! (CR 101.4c + CR 404.1 + CR 608.2c), not one card from any single graveyard.
//!
//! Before this fix the clause lowered to a single `ChangeZone` whose filter was
//! just "in a graveyard", so exactly one card left exactly one graveyard. It now
//! lowers to the per-player `ChooseFromZone { Each(..) }` + `ChangeZoneAll
//! { TrackedSet }` pair Kaya, Spirits' Justice already uses, and the exiled
//! cards stay linked to the source (CR 607.2a) for the card's next ability.
//!
//! Cards are built from their printed text (CI has no card database);
//! Summon: Esper Valigarmanda's chapter I is covered in
//! `summon_esper_valigarmanda_9213.rs`.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Kefka, Dancing Mad, verbatim (`client/public/card-data.json`).
const KEFKA: &str = "During your turn, Kefka has indestructible.\n\
At the beginning of your end step, exile a card at random from each opponent's graveyard. You \
may cast any number of spells from among cards exiled this way without paying their mana costs. \
Then each player who owns a spell you cast this way loses life equal to its mana value.";

/// King Narfi's Betrayal, verbatim.
const KING_NARFIS_BETRAYAL: &str = "(As this Saga enters and after your draw step, add a lore \
counter. Sacrifice after III.)\n\
I — Each player mills four cards. Then you may exile a creature or planeswalker card from each \
graveyard.\n\
II, III — Until end of turn, you may cast spells from among cards exiled with this Saga, and you \
may spend mana as though it were mana of any color to cast those spells.";

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// Pass priority until the stack is empty or a non-priority prompt opens,
/// answering trigger-order prompts.
fn settle(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return,
        }
    }
    panic!("stack did not settle: {:?}", runner.state().waiting_for);
}

fn graveyard_spell(
    scenario: &mut GameScenario,
    owner: PlayerId,
    name: &str,
    mana_value: u32,
) -> ObjectId {
    scenario
        .add_spell_to_graveyard(owner, name, false)
        .with_mana_cost(ManaCost::generic(mana_value))
        .from_oracle_text("You gain 1 life.")
        .id()
}

/// CR 404.1 + CR 608.2c: Kefka exiles one card at random from EACH opponent's
/// graveyard — one of P1's two cards and P2's one card — and none of its
/// controller's own. The exiled pair is exactly the free-cast window's pool.
/// (The life-loss sentence after it is an honest `unbound_subject` gap.)
#[test]
fn kefka_exiles_one_random_card_from_each_opponents_graveyard() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PostCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Kefka, Dancing Mad", 6, 6, KEFKA)
        .id();
    let own = graveyard_spell(&mut scenario, P0, "Own Sorcery", 1);
    let p1_cards = [
        graveyard_spell(&mut scenario, P1, "P1 Sorcery A", 2),
        graveyard_spell(&mut scenario, P1, "P1 Sorcery B", 2),
    ];
    let p2_card = graveyard_spell(&mut scenario, P2, "P2 Sorcery", 3);
    let mut runner = scenario.build();

    runner.advance_to_end_step();
    settle(&mut runner);

    assert_eq!(zone_of(&runner, own), Zone::Graveyard, "not your graveyard");
    assert_eq!(zone_of(&runner, p2_card), Zone::Exile, "P2's only card");
    let p1_exiled: Vec<ObjectId> = p1_cards
        .iter()
        .copied()
        .filter(|id| zone_of(&runner, *id) == Zone::Exile)
        .collect();
    assert_eq!(p1_exiled.len(), 1, "exactly one of P1's two cards");

    let WaitingFor::CastOffer {
        kind: CastOfferKind::FreeCastWindow { candidates, .. },
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the exiled cards open the free-cast window, found {:?}",
            runner.state().waiting_for
        );
    };
    let mut pool = candidates.clone();
    pool.sort();
    let mut expected = vec![p1_exiled[0], p2_card];
    expected.sort();
    assert_eq!(
        pool, expected,
        "the window offers the cards exiled this way"
    );

    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(p2_card),
        })
        .expect("cast P2's card");
    assert_eq!(zone_of(&runner, p2_card), Zone::Stack, "cast from the pool");
}

/// King Narfi's Betrayal's chapter I resolving, with two creature cards in
/// P0's graveyard and one in P1's. Returns the runner, P0's cards and P1's.
fn reach_narfi_chapter_one() -> (GameRunner, [ObjectId; 2], ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(
            player,
            &[
                "Forest", "Forest", "Forest", "Forest", "Forest", "Forest", "Forest", "Forest",
                "Forest", "Forest",
            ],
        );
    }
    scenario
        .add_creature(P0, "King Narfi's Betrayal", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(KING_NARFIS_BETRAYAL);
    let p0_creatures = [
        scenario.add_creature_to_graveyard(P0, "P0 Bear", 2, 2).id(),
        scenario.add_creature_to_graveyard(P0, "P0 Wolf", 2, 2).id(),
    ];
    let p1_creature = scenario.add_creature_to_graveyard(P1, "P1 Bear", 2, 2).id();
    let mut runner = scenario.build();
    // The Saga entered on an earlier turn with no lore counter yet; the next
    // precombat main adds the first (CR 714.3c).
    park_for_next_p0_precombat_main(&mut runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);
    settle(&mut runner);
    (runner, p0_creatures, p1_creature)
}

/// Answer chapter I: P0 orders the graveyards, then `pick` answers each
/// graveyard's choice from its offered cards. Returns how many choices opened.
fn answer_narfi_chapter_one(
    runner: &mut GameRunner,
    pick: impl Fn(&[ObjectId]) -> Vec<ObjectId>,
) -> usize {
    let mut choices = 0;
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseFromZoneOpponentChooser { candidates, .. } => {
                assert_eq!(candidates, vec![P0, P1], "both graveyards are offered");
                runner
                    .act(GameAction::ChooseZoneOpponentChooser { opponent: P0 })
                    .expect("order the picks");
            }
            WaitingFor::ChooseFromZoneChoice {
                player,
                cards,
                up_to,
                ..
            } => {
                assert_eq!(player, P0, "the Saga's controller picks");
                assert!(up_to, "each graveyard's exile is optional");
                runner
                    .act(GameAction::SelectCards {
                        cards: pick(&cards),
                    })
                    .expect("answer a graveyard");
                choices += 1;
            }
            _ => break,
        }
        settle(runner);
    }
    choices
}

/// The exile zone, sorted: only the chosen cards, never a milled one (CR
/// 608.2c: the mill's cards are not "exiled this way").
fn exiled(runner: &GameRunner) -> Vec<ObjectId> {
    let mut exile: Vec<ObjectId> = runner.state().exile.iter().copied().collect();
    exile.sort();
    exile
}

/// Chapter II's castable cards: the ones linked to the Saga.
fn narfi_chapter_two_castable(runner: &mut GameRunner) -> Vec<ObjectId> {
    park_for_next_p0_precombat_main(runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);
    settle(runner);
    spell_objects_available_to_cast(runner.state(), P0)
}

/// CR 101.4c + CR 607.2a: King Narfi's Betrayal's chapter I exiles one creature
/// card from each graveyard — P0 picks one per graveyard, in an order P0
/// chooses (CR 101.4c) — exactly once even though "Each player mills four
/// cards" before it iterates every player. Chapter II then lets P0 cast both,
/// because they are linked to the Saga.
#[test]
fn king_narfis_betrayal_exiles_one_creature_card_from_each_graveyard() {
    let (mut runner, p0_creatures, p1_creature) = reach_narfi_chapter_one();
    let picks = answer_narfi_chapter_one(&mut runner, |cards| {
        if cards.contains(&p1_creature) {
            vec![p1_creature]
        } else {
            assert!(cards.contains(&p0_creatures[0]), "{cards:?}");
            vec![p0_creatures[0]]
        }
    });
    assert_eq!(picks, 2, "one pick per graveyard, once");
    assert_eq!(zone_of(&runner, p0_creatures[0]), Zone::Exile);
    assert_eq!(zone_of(&runner, p0_creatures[1]), Zone::Graveyard);
    assert_eq!(zone_of(&runner, p1_creature), Zone::Exile);
    let mut expected = vec![p0_creatures[0], p1_creature];
    expected.sort();
    assert_eq!(exiled(&runner), expected);

    // Chapter II: the linked cards are castable until end of turn.
    let castable = narfi_chapter_two_castable(&mut runner);
    assert!(
        castable.contains(&p0_creatures[0]) && castable.contains(&p1_creature),
        "both exiled cards are linked and castable: {castable:?}"
    );
}

/// CR 608.2c + CR 608.2d: "you may exile a creature or planeswalker card from
/// each graveyard", ruling: "For each graveyard, you may exile a creature or
/// planeswalker card that was just milled or one that was already there."
/// P0 exiles from its own graveyard and leaves P1's card. Only P0's card is
/// exiled and linked.
#[test]
fn king_narfis_betrayal_exiles_from_one_graveyard_and_skips_another() {
    let (mut runner, p0_creatures, p1_creature) = reach_narfi_chapter_one();
    let choices = answer_narfi_chapter_one(&mut runner, |cards| {
        if cards.contains(&p1_creature) {
            vec![]
        } else {
            vec![p0_creatures[0]]
        }
    });
    assert_eq!(choices, 2);
    assert_eq!(zone_of(&runner, p0_creatures[0]), Zone::Exile);
    assert_eq!(zone_of(&runner, p0_creatures[1]), Zone::Graveyard);
    assert_eq!(zone_of(&runner, p1_creature), Zone::Graveyard);
    assert_eq!(exiled(&runner), vec![p0_creatures[0]]);
    assert_eq!(
        runner.state().players[1].graveyard.len(),
        5,
        "P1's card and its four milled"
    );

    let castable = narfi_chapter_two_castable(&mut runner);
    assert!(castable.contains(&p0_creatures[0]), "{castable:?}");
    assert!(!castable.contains(&p1_creature), "{castable:?}");
}

/// Declining every graveyard exiles nothing and links nothing.
#[test]
fn king_narfis_betrayal_can_exile_nothing() {
    let (mut runner, p0_creatures, p1_creature) = reach_narfi_chapter_one();
    let choices = answer_narfi_chapter_one(&mut runner, |_| vec![]);
    assert_eq!(choices, 2);
    for card in [p0_creatures[0], p0_creatures[1], p1_creature] {
        assert_eq!(zone_of(&runner, card), Zone::Graveyard);
    }
    assert_eq!(exiled(&runner), Vec::<ObjectId>::new());
    assert_eq!(
        runner.state().players[0].graveyard.len(),
        6,
        "two cards and four milled"
    );
    assert_eq!(
        runner.state().players[1].graveyard.len(),
        5,
        "one card and four milled"
    );
    let castable = narfi_chapter_two_castable(&mut runner);
    for card in [p0_creatures[0], p0_creatures[1], p1_creature] {
        assert!(!castable.contains(&card), "{castable:?}");
    }
}

/// Park the game at the end of P0's turn, so the next two `advance_to_phase`
/// calls reach P1's, then P0's, precombat main (CR 714.3c).
fn park_for_next_p0_precombat_main(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.turn_number = 1;
    state.active_player = P0;
    state.phase = Phase::End;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}
