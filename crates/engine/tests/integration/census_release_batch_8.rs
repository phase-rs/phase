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
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
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

/// Attack P1 with `attackers` from P0's declare-attackers prompt.
fn attack_with(r: &mut GameRunner, attackers: &[ObjectId]) {
    r.advance_to_combat();
    assert!(
        matches!(
            r.state().waiting_for,
            WaitingFor::DeclareAttackers { player: P0, .. }
        ),
        "P0 declares attackers, found {:?}",
        r.state().waiting_for
    );
    let attacks: Vec<_> = attackers
        .iter()
        .map(|a| (*a, AttackTarget::Player(P1)))
        .collect();
    r.declare_attackers(&attacks).expect("declare attackers");
}

/// Pass priority (resolving whatever is on the stack) until `pred` holds on the prompt.
fn pass_until_prompt(r: &mut GameRunner, label: &str, pred: impl Fn(&WaitingFor) -> bool) {
    for _ in 0..40 {
        if pred(&r.state().waiting_for) {
            return;
        }
        match r.state().waiting_for {
            WaitingFor::Priority { player: _ } => {
                r.act(GameAction::PassPriority).expect(label);
            }
            ref other => panic!("{label}: stopped at {other:?}"),
        }
    }
    panic!("{label}: the expected prompt never appeared");
}

// ---- Shape 1: "Look at / Mill ... put a <type> card from among them onto the battlefield" ----
// (Hit It!, Fetch Quest)

/// CR 701.20e + CR 608.2c: Hit It!'s enters trigger looks at five cards, offers exactly the lands
/// among them, puts the chosen one onto the battlefield TAPPED and puts the other four (a second
/// land included) into the graveyard. One card, not one per land.
#[test]
fn hit_it_puts_one_land_from_among_the_top_five_onto_the_battlefield_tapped_and_the_rest_into_the_graveyard(
) {
    let mut s = table();
    let hit_it = s.add_real_card(P0, "Hit It!", Zone::Hand, db());
    let bears = s.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db());
    let lib = stack_library(
        &mut s,
        P0,
        &[
            "Llanowar Elves",
            "Forest",
            "Shock",
            "Mountain",
            "Giant Growth",
            "Island",
        ],
    );
    let (elves, forest, shock, mountain, growth, island) =
        (lib[0], lib[1], lib[2], lib[3], lib[4], lib[5]);
    let mut r = s.build();
    cast_from_hand(&mut r, hit_it);
    pass_until_prompt(&mut r, "Hit It! enters trigger", |w| {
        matches!(w, WaitingFor::DigChoice { .. })
    });
    let WaitingFor::DigChoice {
        player,
        cards,
        selectable_cards,
        keep_count,
        ..
    } = r.state().waiting_for.clone()
    else {
        unreachable!()
    };
    assert_eq!(player, P0, "the controller chooses");
    assert_eq!(
        sorted(cards),
        sorted(vec![elves, forest, shock, mountain, growth]),
        "the top five cards are looked at; the sixth is not"
    );
    assert_eq!(
        sorted(selectable_cards),
        sorted(vec![forest, mountain]),
        "only the two lands among them are offered"
    );
    assert_eq!(keep_count, 1, "one card is kept");
    assert!(
        r.act(GameAction::SelectCards {
            cards: vec![forest, mountain]
        })
        .is_err(),
        "both lands cannot be kept"
    );
    assert!(
        r.act(GameAction::SelectCards { cards: vec![shock] })
            .is_err(),
        "a nonland card cannot be kept"
    );
    r.act(GameAction::SelectCards {
        cards: vec![mountain],
    })
    .expect("keep the Mountain");
    assert_settled(&r, P0, "Hit It!");
    assert_eq!(zone_of(&r, mountain), Zone::Battlefield);
    assert!(
        r.state().objects[&mountain].tapped,
        "the land enters tapped"
    );
    for (name, id) in [
        ("Llanowar Elves", elves),
        ("Forest", forest),
        ("Shock", shock),
        ("Giant Growth", growth),
    ] {
        assert_eq!(
            zone_of(&r, id),
            Zone::Graveyard,
            "{name}: the rest go into the graveyard"
        );
    }
    assert_eq!(
        library_of(&r, P0),
        vec![island],
        "the sixth card was never looked at and is still the top of the library"
    );
    assert!(
        r.state().objects[&bears].keywords.contains(&Keyword::Haste),
        "the static half of the card: creatures you control have haste"
    );
}

/// Control: with no land among the five, nothing enters and the whole pile goes to the graveyard.
#[test]
fn hit_it_with_no_land_among_the_top_five_puts_everything_into_the_graveyard() {
    let mut s = table();
    let hit_it = s.add_real_card(P0, "Hit It!", Zone::Hand, db());
    let lib = stack_library(
        &mut s,
        P0,
        &[
            "Llanowar Elves",
            "Shock",
            "Giant Growth",
            "Grizzly Bears",
            "Lightning Bolt",
            "Forest",
        ],
    );
    let mut r = s.build();
    cast_from_hand(&mut r, hit_it);
    let waiting = resolve_stack(&mut r);
    let WaitingFor::DigChoice {
        player,
        selectable_cards,
        cards,
        ..
    } = waiting
    else {
        panic!("the look still opens its choice, found {waiting:?}")
    };
    assert_eq!(player, P0);
    assert_eq!(cards.len(), 5, "five cards were looked at");
    assert!(selectable_cards.is_empty(), "no land is offered");
    r.act(GameAction::SelectCards { cards: vec![] })
        .expect("keep nothing");
    assert_settled(&r, P0, "Hit It! with no land");
    for id in &lib[..5] {
        assert_eq!(
            zone_of(&r, *id),
            Zone::Graveyard,
            "{id:?} goes to the graveyard"
        );
    }
    assert_eq!(
        library_of(&r, P0),
        vec![lib[5]],
        "the sixth card (a Forest) was not looked at and stays in the library"
    );
    assert!(
        !r.state()
            .battlefield
            .iter()
            .any(|id| r.state().objects[id].name == "Forest"),
        "no land entered the battlefield"
    );
}

/// CR 701.17a + CR 608.2c: Fetch Quest (the adventure of Bramble Familiar) mills seven cards, then
/// puts ONE creature, enchantment or land card from among the cards milled this way onto the
/// battlefield. The offered set is exactly the milled cards of those types: an instant, a sorcery,
/// and a creature card that was already in the graveyard are not.
#[test]
fn fetch_quest_puts_one_milled_creature_enchantment_or_land_onto_the_battlefield() {
    let mut s = table();
    let familiar = s.add_real_card(P0, "Bramble Familiar", Zone::Hand, db());
    let old = s.add_real_card(P0, "Serra Angel", Zone::Graveyard, db());
    let lib = stack_library(
        &mut s,
        P0,
        &[
            "Shock",
            "Grizzly Bears",
            "Forest",
            "Phyrexian Arena",
            "Island",
            "Lightning Bolt",
            "Llanowar Elves",
            "Mountain",
        ],
    );
    let (shock, bears, forest, arena, island, bolt, elves, mountain) = (
        lib[0], lib[1], lib[2], lib[3], lib[4], lib[5], lib[6], lib[7],
    );
    let mut r = s.build();
    let out = r
        .cast(familiar)
        .adventure_face(false)
        .commit()
        .stop_at_optional()
        .resolve();
    let WaitingFor::EffectZoneChoice {
        player,
        cards,
        count,
        ..
    } = out.final_waiting_for().clone()
    else {
        panic!(
            "the milled-card choice is the prompt, found {:?}",
            out.final_waiting_for()
        )
    };
    assert_eq!(player, P0, "the caster chooses");
    assert_eq!(
        sorted(cards),
        sorted(vec![bears, forest, arena, island, elves]),
        "the creature, enchantment and land cards milled this way, and nothing else"
    );
    assert_eq!(count, 1, "one card");
    assert!(
        r.act(GameAction::SelectCards {
            cards: vec![bears, forest]
        })
        .is_err(),
        "two cards cannot be put onto the battlefield"
    );
    assert!(
        r.act(GameAction::SelectCards { cards: vec![old] }).is_err(),
        "the Serra Angel that was already in the graveyard was not milled this way"
    );
    r.act(GameAction::SelectCards { cards: vec![arena] })
        .expect("take the enchantment");
    assert_settled(&r, P0, "Fetch Quest");
    assert_eq!(zone_of(&r, arena), Zone::Battlefield);
    for (name, id) in [
        ("Shock", shock),
        ("Grizzly Bears", bears),
        ("Forest", forest),
        ("Island", island),
        ("Lightning Bolt", bolt),
        ("Llanowar Elves", elves),
    ] {
        assert_eq!(zone_of(&r, id), Zone::Graveyard, "{name} stays milled");
    }
    assert_eq!(zone_of(&r, old), Zone::Graveyard);
    assert_eq!(
        library_of(&r, P0),
        vec![mountain],
        "exactly seven cards were milled"
    );
    assert_eq!(
        zone_of(&r, familiar),
        Zone::Exile,
        "the adventure card is exiled on an adventure (CR 715.4)"
    );
}

/// Control: with no creature, enchantment or land among the seven milled cards nothing enters.
#[test]
fn fetch_quest_with_only_instants_and_sorceries_milled_puts_nothing_onto_the_battlefield() {
    let mut s = table();
    let familiar = s.add_real_card(P0, "Bramble Familiar", Zone::Hand, db());
    let lib = stack_library(
        &mut s,
        P0,
        &[
            "Shock",
            "Lightning Bolt",
            "Giant Growth",
            "Divination",
            "Shock",
            "Lightning Bolt",
            "Giant Growth",
            "Mountain",
        ],
    );
    let mut r = s.build();
    let out = r
        .cast(familiar)
        .adventure_face(false)
        .commit()
        .stop_at_optional()
        .resolve();
    assert!(
        matches!(out.final_waiting_for(), WaitingFor::Priority { player: P0 }),
        "nothing to take, so no prompt: {:?}",
        out.final_waiting_for()
    );
    assert_settled(&r, P0, "Fetch Quest with nothing to take");
    assert!(
        r.state().battlefield.is_empty(),
        "nothing entered the battlefield"
    );
    for id in &lib[..7] {
        assert_eq!(zone_of(&r, *id), Zone::Graveyard);
    }
    assert_eq!(library_of(&r, P0), vec![lib[7]]);
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

// ---- Shape 2: "Exile all creature cards from target player's graveyard in a face-down pile,
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

// ---- Shape 3: "When you cast this spell, if you have fewer than seven cards in hand, draw cards
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

/// Answer a (trigger) target prompt with P1, asserting P1 is among the legal targets; `None` for
/// any other prompt.
fn target_p1(w: &WaitingFor) -> Option<GameAction> {
    let (WaitingFor::TargetSelection { selection, .. }
    | WaitingFor::TriggerTargetSelection { selection, .. }) = w
    else {
        return None;
    };
    let target = selection
        .current_legal_targets
        .iter()
        .find(|t| matches!(t, engine::types::ability::TargetRef::Player(p) if *p == P1))
        .cloned();
    assert!(target.is_some(), "P1 is a legal target: {selection:?}");
    Some(GameAction::ChooseTarget { target })
}

// ---- Shape 4: "sacrifice any number of other creatures, then exile the top X cards of your
// library, where X is their total power. You may cast any number of instant and/or sorcery spells
// from among the exiled cards without paying their mana costs." (Kylox, Visionary Inventor) ----

fn kylox_board() -> (GameRunner, ObjectId, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut s = table();
    let kylox = s.add_real_card(P0, "Kylox, Visionary Inventor", Zone::Battlefield, db());
    let spider = s.add_real_card(P0, "Giant Spider", Zone::Battlefield, db());
    let giant = s.add_real_card(P0, "Hill Giant", Zone::Battlefield, db());
    let lib = stack_library(
        &mut s,
        P0,
        &[
            "Shock",
            "Lightning Bolt",
            "Grizzly Bears",
            "Forest",
            "Island",
            "Giant Growth",
            "Mountain",
        ],
    );
    (s.build(), kylox, spider, giant, lib)
}

/// Sacrificing the 2/4 Giant Spider and the 3/3 Hill Giant makes X = 5, their total POWER (toughness
/// would give 7): exactly the top five cards are exiled (the sixth stays), the offer lists only the two
/// instants among them (not the creature card or the lands), and both can be cast: "any number".
#[test]
fn kylox_exiles_cards_equal_to_the_sacrificed_creatures_total_power_and_casts_any_number_of_instants(
) {
    let (mut r, kylox, spider, giant, lib) = kylox_board();
    let (shock, bolt, bears_card, forest, island, growth) =
        (lib[0], lib[1], lib[2], lib[3], lib[4], lib[5]);
    attack_with(&mut r, &[kylox]);
    let mut offers = 0;
    let mut policy = |w: &WaitingFor| match w {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            count,
            ..
        } => {
            assert_eq!(*player, P0, "the controller sacrifices");
            assert_eq!(
                sorted(cards.clone()),
                sorted(vec![spider, giant]),
                "other creatures only"
            );
            assert_eq!(*count, 2, "any number: up to every other creature");
            Some(GameAction::SelectCards {
                cards: vec![spider, giant],
            })
        }
        WaitingFor::CastOffer {
            player,
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
            ..
        } => {
            assert_eq!(*player, P0);
            offers += 1;
            let expected = if offers == 1 {
                vec![shock, bolt]
            } else {
                vec![bolt]
            };
            assert_eq!(
                sorted(candidates.clone()),
                sorted(expected),
                "offer {offers}: only instants and sorceries among the exiled cards"
            );
            Some(GameAction::FreeCastWindowChoice {
                selection: Some(if offers == 1 { shock } else { bolt }),
            })
        }
        other => target_p1(other),
    };
    resolve_with(&mut r, "Kylox, both sacrificed", &mut policy);
    assert_eq!(offers, 2, "both spells were cast, one offer each");
    assert_eq!(
        life(&r, P1),
        15,
        "Shock (2) and Lightning Bolt (3) resolved"
    );
    for id in [spider, giant] {
        assert_eq!(zone_of(&r, id), Zone::Graveyard, "sacrificed");
    }
    for (name, id) in [
        ("Grizzly Bears", bears_card),
        ("Forest", forest),
        ("Island", island),
    ] {
        assert_eq!(
            zone_of(&r, id),
            Zone::Exile,
            "{name} was exiled and cannot be cast"
        );
    }
    assert_eq!(zone_of(&r, shock), Zone::Graveyard);
    assert_eq!(zone_of(&r, bolt), Zone::Graveyard);
    assert_eq!(
        library_of(&r, P0)[0],
        growth,
        "exactly five cards were exiled: the sixth is still on top"
    );
}

/// Control: sacrificing only the 2-power creature gives X = 2, so only the top two cards are
/// exiled; declining the free casts leaves the spells in exile.
#[test]
fn kylox_with_one_sacrificed_creature_exiles_only_its_power_and_declining_casts_nothing() {
    let (mut r, kylox, spider, giant, lib) = kylox_board();
    let (shock, bolt, bears_card) = (lib[0], lib[1], lib[2]);
    attack_with(&mut r, &[kylox]);
    let mut policy = |w: &WaitingFor| match w {
        WaitingFor::EffectZoneChoice { player, .. } => {
            assert_eq!(*player, P0);
            Some(GameAction::SelectCards {
                cards: vec![spider],
            })
        }
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
            ..
        } => {
            assert_eq!(sorted(candidates.clone()), sorted(vec![shock, bolt]));
            Some(GameAction::FreeCastWindowChoice { selection: None })
        }
        other => target_p1(other),
    };
    resolve_with(&mut r, "Kylox, one sacrificed", &mut policy);
    assert_eq!(life(&r, P1), 20, "nothing was cast");
    assert_eq!(
        zone_of(&r, giant),
        Zone::Battlefield,
        "the Hill Giant was not sacrificed"
    );
    assert_eq!(zone_of(&r, shock), Zone::Exile);
    assert_eq!(zone_of(&r, bolt), Zone::Exile);
    assert_eq!(
        library_of(&r, P0)[0],
        bears_card,
        "X = 2: the third card was not exiled"
    );
}

/// Control: sacrificing nothing makes X = 0 and exiles nothing, with no cast offer.
#[test]
fn kylox_sacrificing_nothing_exiles_nothing() {
    let (mut r, kylox, _bears, _giant, lib) = kylox_board();
    attack_with(&mut r, &[kylox]);
    let mut policy = |w: &WaitingFor| {
        let WaitingFor::EffectZoneChoice { player, .. } = w else {
            return None;
        };
        assert_eq!(*player, P0);
        Some(GameAction::SelectCards { cards: vec![] })
    };
    let prompts = resolve_with(&mut r, "Kylox, none sacrificed", &mut policy);
    assert_eq!(prompts.len(), 1, "only the sacrifice prompt: {prompts:?}");
    assert_eq!(library_of(&r, P0)[0], lib[0], "nothing was exiled");
    assert_eq!(r.state().exile.len(), 0);
}

// ---- Shape 5: "search your library for any number of Goblin cards, reveal them, then shuffle and
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
