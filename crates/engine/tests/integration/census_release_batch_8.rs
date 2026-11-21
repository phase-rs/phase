//! Runtime witnesses for the `confirmed-misparses.toml` rows released in batch 8: census rows of the
//! `wrong_count` class whose cited clause the engine already carries out correctly. Every released
//! card is the printed card (printed cost, type line, P/T and verbatim Oracle text, loaded from the
//! card database through `add_real_card`) and is driven through the public `GameRunner` actions.
//! Only textless fillers (vanilla creatures, basic lands) are synthetic.
//!
//! One group per clause shape. Every group asserts the prompt the engine shows (actor and offered
//! cards) before the effect, has controls that would fail if the clause counted wrongly (a card the
//! filter must not offer, a different count, a declined optional step), and ends with an empty stack
//! and priority.
use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn db() -> &'static CardDatabase {
    shared_card_db().expect("the committed integration card fixture is present")
}

/// Pool mana for every printed cost used below (CR 106.4), so that a cast pays exactly its printed
/// cost and never taps a land the test needs untapped.
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

fn life(r: &GameRunner, player: PlayerId) -> i32 {
    r.state().players[player.0 as usize].life
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
        let action = match &waiting {
            WaitingFor::Priority { player: _ } => GameAction::PassPriority,
            other => {
                let name: String = format!("{other:?}").chars().take(24).collect();
                seen.push(name);
                policy(other).unwrap_or_else(|| panic!("{label}: unexpected prompt {other:?}"))
            }
        };
        r.act(action)
            .unwrap_or_else(|e| panic!("{label}: action rejected ({e:?}) at {waiting:?}"));
    }
    panic!(
        "{label}: did not settle, stuck at {:?}",
        r.state().waiting_for
    );
}

// ---- Shape 1: "Exile all creature cards from target player's graveyard in a face-down pile,
// shuffle that pile, then manifest those cards." (Ghastly Conscription) ----

/// CR 701.40a + CR 701.24a: every creature card in the targeted graveyard is manifested (a face-down
/// 2/2 under the CASTER's control, the owner unchanged); a noncreature card in that graveyard and
/// the caster's own graveyard are left alone.
#[test]
fn ghastly_conscription_manifests_every_creature_card_in_the_targeted_graveyard() {
    let mut s = table();
    let spell = s.add_real_card(P0, "Ghastly Conscription", Zone::Hand, db());
    let creatures: Vec<ObjectId> = ["Grizzly Bears", "Llanowar Elves", "Serra Angel"]
        .iter()
        .map(|n| s.add_real_card(P1, n, Zone::Graveyard, db()))
        .collect();
    let shock = s.add_real_card(P1, "Shock", Zone::Graveyard, db());
    let own = s.add_real_card(P0, "Hill Giant", Zone::Graveyard, db());
    let mut r = s.build();
    r.cast(spell).target_player(P1).resolve();
    assert_settled(&r, P0, "Ghastly Conscription");
    for id in &creatures {
        let o = &r.state().objects[id];
        assert_eq!(o.zone, Zone::Battlefield, "{id:?} was manifested");
        assert!(o.face_down, "{id:?} is face down");
        assert_eq!(
            (o.power, o.toughness),
            (Some(2), Some(2)),
            "a face-down 2/2"
        );
        assert_eq!(o.controller, P0, "the caster manifests the pile");
        assert_eq!(o.owner, P1, "ownership does not change");
    }
    assert_eq!(
        zone_of(&r, shock),
        Zone::Graveyard,
        "a noncreature card stays"
    );
    assert_eq!(
        zone_of(&r, own),
        Zone::Graveyard,
        "the caster's own graveyard is not the target"
    );
    assert_eq!(
        r.state().battlefield.len(),
        3,
        "exactly the three creature cards"
    );
}

/// Control: a graveyard with no creature card manifests nothing.
#[test]
fn ghastly_conscription_with_no_creature_card_in_the_graveyard_manifests_nothing() {
    let mut s = table();
    let spell = s.add_real_card(P0, "Ghastly Conscription", Zone::Hand, db());
    let shock = s.add_real_card(P1, "Shock", Zone::Graveyard, db());
    let mut r = s.build();
    r.cast(spell).target_player(P1).resolve();
    assert_settled(&r, P0, "Ghastly Conscription, empty target");
    assert!(r.state().battlefield.is_empty());
    assert_eq!(zone_of(&r, shock), Zone::Graveyard);
}

// ---- Shape 2: "When you cast this spell, if you have fewer than seven cards in hand, draw cards
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
    // The "when you cast" trigger goes above the spell (CR 603.3b): it resolves first.
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
            hand.contains(&name.to_string()),
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

// ---- Shape 3: "search your library for any number of Goblin cards, reveal them, then shuffle and
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
    let mut policy = |w: &WaitingFor| {
        let WaitingFor::SearchChoice {
            player,
            cards,
            count,
            up_to,
            ..
        } = w
        else {
            return None;
        };
        assert_eq!(*player, P0);
        assert_eq!(
            sorted(cards.clone()),
            sorted(goblins.clone()),
            "only Goblin cards are offered"
        );
        assert_eq!(*count, 3, "any number of them");
        assert!(*up_to, "zero to all three");
        Some(GameAction::SelectCards { cards: pick(&lib) })
    };
    let prompts = resolve_with(&mut r, "Goblin Recruiter", &mut policy);
    assert!(
        prompts.iter().any(|p| p.starts_with("SearchChoice")),
        "{prompts:?}"
    );
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

/// Control: finding no Goblin puts nothing on top.
#[test]
fn goblin_recruiter_finding_no_goblin_leaves_the_library_shuffled_with_nothing_placed() {
    let (r, lib) = recruiter_run(|_| vec![]);
    assert_settled(&r, P0, "Goblin Recruiter, nothing found");
    let library = library_of(&r, P0);
    assert_eq!(
        sorted(library),
        sorted(lib),
        "all seven cards are still in the library"
    );
}
