//! Runtime witnesses for the `confirmed-misparses.toml` rows released in batch 8: census rows of the
//! `wrong_count` class whose cited clause the engine already carries out correctly. Every released
//! card is the printed card (printed cost, type line, P/T and verbatim Oracle text, loaded from the
//! card database through `add_real_card`) and is driven through the public `GameRunner` actions.
//! Only the textless library fillers (`P1 Draw N`) and the stack entry injected for the opponent's
//! printed spell are synthetic.
//!
//! One group per clause shape. The Goblin Recruiter group asserts the prompt the engine shows
//! (actor, offered cards, count, reveal, ordering) before the effect; every group has controls
//! that would fail if the clause counted wrongly (a card the filter must not offer, a different
//! count, an empty answer), and ends with an empty stack and priority.
use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::move_to_zone;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn db() -> &'static CardDatabase {
    shared_card_db().expect("the committed integration card fixture is present")
}

/// Pool mana for every printed cost used below (CR 106.4: mana in the pool pays costs), so that a
/// cast pays exactly its printed cost and never taps a land the test needs untapped.
fn pool() -> Vec<ManaUnit> {
    let mut kinds = Vec::new();
    for kind in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ] {
        kinds.extend([kind; 8]);
    }
    kinds.extend([ManaType::Colorless; 12]);
    kinds
        .into_iter()
        .enumerate()
        .map(|(i, kind)| ManaUnit::new(kind, ObjectId(9900 + i as u64), false, vec![]))
        .collect()
}

/// Two players, P0 to act in the precombat main phase with `pool()`; P1 has a few draws so the turn
/// structure can be played out.
fn table() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool());
    scenario.with_library_top(P1, &["P1 Draw 1", "P1 Draw 2", "P1 Draw 3", "P1 Draw 4"]);
    scenario
}

/// Library cards, top first, as printed cards.
fn stack_library(
    scenario: &mut GameScenario,
    player: PlayerId,
    top_first: &[&str],
) -> Vec<ObjectId> {
    top_first
        .iter()
        .map(|name| scenario.add_real_card(player, name, Zone::Library, db()))
        .collect()
}

fn library_of(r: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    r.state().players[player.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

fn hand_of(r: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    r.state().players[player.0 as usize]
        .hand
        .iter()
        .copied()
        .collect()
}

fn zone_of(r: &GameRunner, id: ObjectId) -> Zone {
    r.state().objects[&id].zone
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort_by_key(|o| o.0);
    ids
}

/// The run finished: nothing is left on the stack and `player` holds priority (a suspended
/// resolution can leave the stack empty, so both are asserted).
fn assert_settled(r: &GameRunner, player: PlayerId, what: &str) {
    assert!(
        r.state().stack.is_empty(),
        "{what}: the stack is empty, found {:?}",
        r.state().stack
    );
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { player: p } if p == player),
        "{what}: P{} has priority, found {:?}",
        player.0,
        r.state().waiting_for
    );
}

/// Pass priority until the stack is empty or the engine stops at a prompt; returns the state it
/// stopped in.
fn resolve_stack(r: &mut GameRunner) -> WaitingFor {
    for _ in 0..40 {
        if !matches!(r.state().waiting_for, WaitingFor::Priority { player: _ })
            || r.state().stack.is_empty()
        {
            break;
        }
        r.act(GameAction::PassPriority).expect("pass priority");
    }
    r.state().waiting_for.clone()
}

fn cast_from_hand(r: &mut GameRunner, spell: ObjectId) {
    let card_id = r.state().objects[&spell].card_id;
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast");
}

/// A policy answer for one prompt, or `None` when the policy does not know the prompt.
type Policy<'a> = &'a mut dyn FnMut(&WaitingFor) -> Option<GameAction>;

/// Answer prompts through `policy` (passing priority while something is on the stack) until the
/// stack is empty with priority; returns the short names of the prompts the policy answered.
fn resolve_with(r: &mut GameRunner, label: &str, policy: Policy) -> Vec<String> {
    let mut seen = Vec::new();
    for _ in 0..80 {
        let waiting = r.state().waiting_for.clone();
        if matches!(waiting, WaitingFor::Priority { player: _ }) && r.state().stack.is_empty() {
            return seen;
        }
        let action = if matches!(waiting, WaitingFor::Priority { player: _ }) {
            GameAction::PassPriority
        } else {
            let name: String = format!("{waiting:?}").chars().take(24).collect();
            seen.push(name);
            policy(&waiting).unwrap_or_else(|| panic!("{label}: unexpected prompt {waiting:?}"))
        };
        r.act(action)
            .unwrap_or_else(|e| panic!("{label}: action rejected ({e:?}) at {waiting:?}"));
    }
    panic!(
        "{label}: did not settle, stuck at {:?}",
        r.state().waiting_for
    );
}

// ---- Shape 1: "When you cast this spell, if you have fewer than seven cards in hand, draw cards
// equal to the difference." (Kozilek, the Great Distortion) ----

/// Cast Kozilek holding `others` other cards (Kozilek has left the hand when the trigger resolves,
/// CR 601.2a) and report how many cards the cast trigger drew.
fn kozilek_draw(others: usize) -> (usize, GameRunner) {
    let mut s = table();
    let kozilek = s.add_real_card(P0, "Kozilek, the Great Distortion", Zone::Hand, db());
    let hand_names = [
        "Forest",
        "Island",
        "Mountain",
        "Plains",
        "Swamp",
        "Grizzly Bears",
        "Llanowar Elves",
        "Shock",
        "Giant Growth",
    ];
    for name in hand_names.iter().take(others) {
        s.add_real_card(P0, name, Zone::Hand, db());
    }
    stack_library(
        &mut s,
        P0,
        &[
            "Hill Giant",
            "Serra Angel",
            "Lightning Bolt",
            "Divination",
            "Pacifism",
            "Sol Ring",
            "Mind Stone",
            "Phyrexian Arena",
        ],
    );
    let mut r = s.build();
    let before = hand_of(&r, P0).len() - 1;
    assert_eq!(before, others);
    cast_from_hand(&mut r, kozilek);
    // CR 603.3: the "when you cast" trigger is put on the stack above the spell; CR 405.5: the
    // top object resolves first.
    assert_eq!(
        r.state().stack.len(),
        2,
        "the spell and its cast trigger, or just the spell"
    );
    let drawn_before = hand_of(&r, P0).len();
    resolve_stack(&mut r);
    let drawn = hand_of(&r, P0).len() - drawn_before;
    assert_settled(&r, P0, "Kozilek");
    (drawn, r)
}

#[test]
fn kozilek_cast_trigger_draws_cards_equal_to_the_difference_from_seven() {
    // 3 other cards in hand -> 7 - 3 = 4 cards.
    let (drawn, r) = kozilek_draw(3);
    assert_eq!(drawn, 4, "draws 7 - 3");
    assert_eq!(hand_of(&r, P0).len(), 7, "the hand is back to seven");
    let top_four = ["Hill Giant", "Serra Angel", "Lightning Bolt", "Divination"];
    let hand: Vec<String> = hand_of(&r, P0)
        .iter()
        .map(|i| r.state().objects[i].name.clone())
        .collect();
    for name in top_four {
        assert!(
            hand.iter().any(|card| card == name),
            "{name} was drawn: {hand:?}"
        );
    }
    assert_eq!(
        r.state().objects[&library_of(&r, P0)[0]].name,
        "Pacifism",
        "the fifth card down was not drawn"
    );
    // 5 other cards -> 2 cards.
    let (drawn, _) = kozilek_draw(5);
    assert_eq!(drawn, 2, "draws 7 - 5");
    // 6 other cards -> 1 card.
    let (drawn, _) = kozilek_draw(6);
    assert_eq!(drawn, 1, "draws 7 - 6");
}

/// Control (the intervening "if", CR 603.4): with seven or more other cards in hand the trigger
/// does not fire and nothing is drawn.
#[test]
fn kozilek_cast_trigger_draws_nothing_with_seven_or_more_other_cards_in_hand() {
    for others in [7, 8] {
        let mut s = table();
        let kozilek = s.add_real_card(P0, "Kozilek, the Great Distortion", Zone::Hand, db());
        for name in [
            "Forest",
            "Island",
            "Mountain",
            "Plains",
            "Swamp",
            "Grizzly Bears",
            "Llanowar Elves",
            "Shock",
        ]
        .iter()
        .take(others)
        {
            s.add_real_card(P0, name, Zone::Hand, db());
        }
        stack_library(&mut s, P0, &["Hill Giant", "Serra Angel"]);
        let mut r = s.build();
        cast_from_hand(&mut r, kozilek);
        assert_eq!(
            r.state().stack.len(),
            1,
            "{others} other cards: no cast trigger"
        );
        resolve_stack(&mut r);
        assert_settled(&r, P0, "Kozilek, full hand");
        assert_eq!(hand_of(&r, P0).len(), others, "nothing was drawn");
        assert_eq!(zone_of(&r, kozilek), Zone::Battlefield);
    }
}

/// Kozilek's other two printed clauses, on the printed card: Menace, and "Discard a card with mana
/// value X: Counter target spell with mana value X." (a discard of the announced mana value counters
/// a spell of that mana value; an X no card in hand matches cannot be paid).
fn kozilek_on_battlefield_facing_a_spell() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut s = table();
    let kozilek = s.add_real_card(P0, "Kozilek, the Great Distortion", Zone::Battlefield, db());
    let discard = s.add_real_card(P0, "Divination", Zone::Hand, db());
    let off_value = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db());
    let target = s.add_real_card(P1, "Phyrexian Arena", Zone::Hand, db());
    let mut r = s.build();
    // The opponent's spell is put on the stack directly (as the existing Kozilek counter witness
    // does): Divination and Phyrexian Arena both have mana value three.
    let state = r.state_mut();
    let card_id = state.objects[&target].card_id;
    let mut events = Vec::new();
    move_to_zone(state, target, Zone::Stack, &mut events);
    state.stack.push_back(StackEntry {
        id: target,
        source_id: target,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id,
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    (r, kozilek, discard, off_value, target)
}

#[test]
fn kozilek_has_menace() {
    let (r, kozilek, _, _, _) = kozilek_on_battlefield_facing_a_spell();
    assert!(
        r.state().objects[&kozilek].has_keyword(&Keyword::Menace),
        "Kozilek has menace"
    );
}

#[test]
fn kozilek_discards_a_card_of_the_announced_mana_value_to_counter_a_spell_of_that_value() {
    let (mut r, kozilek, discard, off_value, target) = kozilek_on_battlefield_facing_a_spell();
    let outcome = r
        .activate(kozilek, 0)
        .x(3)
        .target_object(target)
        .pay_with(&[discard])
        .resolve();
    outcome.assert_zone(&[discard, target], Zone::Graveyard);
    assert_eq!(
        zone_of(&r, off_value),
        Zone::Hand,
        "the card with another mana value was not discarded"
    );
    assert_settled(&r, P0, "Kozilek counter");
}

/// Control: with X = 4 no card in hand has the announced mana value, so the cost cannot be paid.
#[test]
fn kozilek_rejects_an_announced_x_that_no_card_in_hand_matches() {
    let (mut r, kozilek, discard, _, target) = kozilek_on_battlefield_facing_a_spell();
    r.act(GameAction::ActivateAbility {
        source_id: kozilek,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("activation reaches the X announcement");
    assert!(
        r.act(GameAction::ChooseX { value: 4 }).is_err(),
        "X = 4 has no matching discard"
    );
    assert_eq!(zone_of(&r, discard), Zone::Hand);
    assert_eq!(
        zone_of(&r, target),
        Zone::Stack,
        "the spell was not countered"
    );
}

// ---- Shape 2: "search your library for any number of Goblin cards, reveal them, then shuffle and
// put those cards on top in any order." (Goblin Recruiter) ----

fn recruiter_run(pick: impl Fn(&[ObjectId]) -> Vec<ObjectId>) -> (GameRunner, Vec<ObjectId>) {
    let mut s = table();
    let recruiter = s.add_real_card(P0, "Goblin Recruiter", Zone::Hand, db());
    let lib = stack_library(
        &mut s,
        P0,
        &[
            "Forest",
            "Mons's Goblin Raiders",
            "Island",
            "Goblin Piker",
            "Mountain",
            "Raging Goblin",
            "Grizzly Bears",
        ],
    );
    let mut r = s.build();
    cast_from_hand(&mut r, recruiter);
    let goblins = vec![lib[1], lib[3], lib[5]];
    let mut search_prompts = 0usize;
    let mut policy = |w: &WaitingFor| {
        let WaitingFor::SearchChoice {
            player,
            library_owner,
            cards,
            count,
            reveal,
            up_to,
            allows_partial_find: _,
            constraint,
            ordering_hint,
            split,
        } = w
        else {
            return None;
        };
        assert_eq!(*player, P0);
        assert_eq!(
            *library_owner,
            Some(P0),
            "the caster searches their own library"
        );
        assert_eq!(
            *constraint,
            engine::types::ability::SearchSelectionConstraint::default(),
            "no relation among the chosen cards is required"
        );
        assert!(
            split.is_none(),
            "the chosen cards are not split between destinations"
        );
        assert!(*reveal, "the found cards are revealed");
        assert_eq!(
            *ordering_hint,
            engine::types::ability::SearchOrderingHint::OrderedToLibraryTop,
            "the prompt carries the \"in any order\" placement"
        );
        assert_eq!(
            sorted(cards.clone()),
            sorted(goblins.clone()),
            "only Goblin cards are offered"
        );
        assert_eq!(*count, 3, "any number of them");
        assert!(*up_to, "zero to all three");
        search_prompts += 1;
        Some(GameAction::SelectCards { cards: pick(&lib) })
    };
    let prompts = resolve_with(&mut r, "Goblin Recruiter", &mut policy);
    assert_eq!(search_prompts, 1, "exactly one search prompt: {prompts:?}");
    (r, lib)
}

/// Two Goblins found: both end on top of the library in the order the player named, and the third
/// Goblin and the other cards were shuffled below them (CR 401.4: the owner orders the pile).
#[test]
fn goblin_recruiter_puts_the_found_goblins_on_top_in_the_chosen_order_after_the_shuffle() {
    for (first, second) in [(5usize, 3usize), (3, 5)] {
        let (r, lib) = recruiter_run(|lib| vec![lib[first], lib[second]]);
        assert_settled(&r, P0, "Goblin Recruiter");
        let library = library_of(&r, P0);
        assert_eq!(library.len(), 7, "nothing left the library");
        assert_eq!(
            library[..2].to_vec(),
            vec![lib[first], lib[second]],
            "the two chosen Goblins are the top two cards, in the chosen order"
        );
        let below: Vec<ObjectId> = library[2..].to_vec();
        assert!(!below.contains(&lib[first]) && !below.contains(&lib[second]));
        assert!(
            below.contains(&lib[1]),
            "the Goblin that was not taken is shuffled below"
        );
        let original_rest: Vec<ObjectId> = vec![lib[0], lib[1], lib[2], lib[4], lib[6]];
        assert_ne!(
            below, original_rest,
            "the rest of the library was shuffled (non-identity for this seed)"
        );
    }
}

/// Control: an empty selection is accepted and all seven cards stay in the library.
#[test]
fn goblin_recruiter_accepts_an_empty_selection_and_keeps_all_seven_cards_in_the_library() {
    let (r, lib) = recruiter_run(|_| vec![]);
    assert_settled(&r, P0, "Goblin Recruiter, nothing found");
    let library = library_of(&r, P0);
    assert_eq!(
        sorted(library),
        sorted(lib),
        "all seven cards are still in the library"
    );
}
