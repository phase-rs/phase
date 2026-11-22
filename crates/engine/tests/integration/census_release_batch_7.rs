//! Runtime witnesses for the `confirmed-misparses.toml` rows released in batch 7 (census slice 2),
//! grouped by clause shape:
//!
//! 1. "mill N, you may put a <type> card from among the cards milled this way into your hand"
//!    (Blanchwood Prowler, Cache Grab): a choice over the tracked milled set only, filtered by type.
//! 2. "search your library for up to three creature cards, reveal them, then shuffle and put those
//!    cards on top in any order" (Congregation at Dawn): the found cards, in the searcher's order.
//! 3. "Enchanted creature gets +0/+2 for each other creature you control" (Conclave's Blessing):
//!    the count is of the creatures other than the enchanted one, the opponent's not included.
//! 4. "Target player sacrifices an attacking creature of their choice. You create X 1/1 Soldier
//!    tokens, where X is that creature's toughness" (Entrapment Maneuver): X is the toughness of
//!    the creature the target player actually chose.
//!
//! Every card is the printed card (printed cost, type line, P/T and verbatim Oracle text, loaded
//! from the card database through `add_real_card`) driven through the public cast, pass-priority,
//! choose-target and select-cards actions of `GameRunner`. Every support card is a printed card too.
//! The prompts the engine shows (who chooses, from which options) are asserted for every card, the
//! cards' choices are answered discriminatingly, the downstream effect is asserted and every
//! witness ends with an empty stack at `Priority`.
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn db() -> &'static engine::database::card_db::CardDatabase {
    shared_card_db().expect("the committed integration card fixture is present")
}

/// Pool mana for the printed costs used below (CR 106.4): casts are paid from the pool, so no land
/// a test needs untapped is tapped.
fn pool() -> Vec<ManaUnit> {
    exact_pool(&[
        (ManaType::White, 8),
        (ManaType::Blue, 8),
        (ManaType::Black, 8),
        (ManaType::Red, 8),
        (ManaType::Green, 8),
        (ManaType::Colorless, 8),
    ])
}

fn exact_pool(spec: &[(ManaType, usize)]) -> Vec<ManaUnit> {
    let mut units = Vec::new();
    for (kind, n) in spec {
        for _ in 0..*n {
            let i = units.len() as u64;
            units.push(ManaUnit::new(*kind, ObjectId(9900 + i), false, vec![]));
        }
    }
    units
}

/// Two players, P0 in its precombat main phase holding `pool()`.
fn table<T>(setup: impl FnOnce(&mut GameScenario) -> T) -> (GameRunner, T) {
    table_with_pool(pool(), setup)
}

/// Like `table`, with exactly `units` in P0's pool.
fn table_with_pool<T>(
    units: Vec<ManaUnit>,
    setup: impl FnOnce(&mut GameScenario) -> T,
) -> (GameRunner, T) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, units);
    let made = setup(&mut scenario);
    (scenario.build(), made)
}

/// Put exactly `ids` (top first) in `player`'s library.
fn set_library(r: &mut GameRunner, player: PlayerId, ids: &[ObjectId]) {
    r.state_mut().players[player.0 as usize].library = ids.iter().copied().collect();
}

fn library(r: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    r.state().players[player.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

fn hand(r: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    r.state().players[player.0 as usize]
        .hand
        .iter()
        .copied()
        .collect()
}

fn zone_of(r: &GameRunner, id: ObjectId) -> Zone {
    r.state().objects[&id].zone
}

fn counters(r: &GameRunner, id: ObjectId) -> u32 {
    r.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn power_toughness(r: &GameRunner, id: ObjectId) -> (i32, i32) {
    let o = &r.state().objects[&id];
    (o.power.unwrap_or(0), o.toughness.unwrap_or(0))
}

fn sorted(mut v: Vec<ObjectId>) -> Vec<ObjectId> {
    v.sort_by_key(|o| o.0);
    v
}

/// What an object-selection prompt showed: who chooses, from which cards, and the fewest and most
/// cards that may be chosen.
#[derive(Debug, PartialEq)]
struct Shown {
    actor: PlayerId,
    cards: Vec<ObjectId>,
    min: usize,
    max: usize,
}

/// The selection prompt `wf` is, if it is one of the engine's object-selection prompts.
fn shown(wf: &WaitingFor) -> Option<Shown> {
    match wf {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            count,
            min_count,
            up_to,
            ..
        } => Some(Shown {
            actor: *player,
            cards: sorted(cards.clone()),
            min: if *up_to { *min_count } else { *count },
            max: *count,
        }),
        WaitingFor::SearchChoice {
            player,
            cards,
            count,
            up_to,
            ..
        } => Some(Shown {
            actor: *player,
            cards: sorted(cards.clone()),
            min: if *up_to { 0 } else { *count },
            max: *count,
        }),
        _ => None,
    }
}

/// The announcements and answers of one cast, in the order the engine asks.
#[derive(Default)]
struct Plan {
    /// Targets, taken as the prompts for them come: the first still-legal entry answers each slot.
    targets: Vec<TargetRef>,
    /// Answer to an `OptionalEffectChoice` ("you may"), should one be shown.
    accept: bool,
    /// Cards chosen at an object-selection prompt (those of them that are offered), in this order.
    picks: Vec<ObjectId>,
    /// Every selection prompt that was shown, in order.
    prompts: Vec<Shown>,
    /// Every target prompt that was shown, in order: who announced it and what was legal.
    target_prompts: Vec<(PlayerId, Vec<TargetRef>)>,
}

fn plan_answer(plan: &mut Plan, wf: &WaitingFor) -> Option<GameAction> {
    match wf {
        WaitingFor::TargetSelection {
            player, selection, ..
        }
        | WaitingFor::TriggerTargetSelection {
            player, selection, ..
        } => {
            plan.target_prompts
                .push((*player, selection.current_legal_targets.clone()));
            let at = plan
                .targets
                .iter()
                .position(|t| selection.current_legal_targets.contains(t));
            Some(GameAction::ChooseTarget {
                target: at.map(|i| plan.targets.remove(i)),
            })
        }
        WaitingFor::OptionalEffectChoice { .. } => Some(GameAction::DecideOptionalEffect {
            accept: plan.accept,
        }),
        // CR 702.51a: a convoke window; passing declines further convoke taps and finalizes the
        // cost from the pool.
        WaitingFor::ManaPayment { .. } => Some(GameAction::PassPriority),
        _ => {
            let prompt = shown(wf)?;
            let cards = plan
                .picks
                .iter()
                .copied()
                .filter(|c| prompt.cards.contains(c))
                .collect();
            plan.prompts.push(prompt);
            Some(GameAction::SelectCards { cards })
        }
    }
}

/// Pass priority and answer every prompt through `answer` until the stack is empty at `Priority`.
fn settle(
    r: &mut GameRunner,
    label: &str,
    mut answer: impl FnMut(&WaitingFor) -> Option<GameAction>,
) {
    for _ in 0..300 {
        let wf = r.state().waiting_for.clone();
        match &wf {
            WaitingFor::Priority { .. } if r.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).expect("pass priority");
                continue;
            }
            _ => {}
        }
        let action = answer(&wf).unwrap_or_else(|| panic!("{label}: unanswered prompt {wf:?}"));
        r.act(action.clone())
            .unwrap_or_else(|e| panic!("{label}: {action:?} rejected at {wf:?}: {e:?}"));
    }
    panic!("{label}: the stack never emptied");
}

fn cast(r: &mut GameRunner, spell: ObjectId) {
    let card_id = r.state().objects[&spell].card_id;
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast");
}

/// Cast `spell` and settle it under `plan`.
fn cast_plan(r: &mut GameRunner, label: &str, spell: ObjectId, plan: &mut Plan) {
    cast(r, spell);
    settle(r, label, |wf| plan_answer(plan, wf));
}

fn expect_priority_empty(r: &GameRunner, label: &str) {
    assert!(
        r.state().stack.is_empty(),
        "{label}: the stack is empty at the end"
    );
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "{label}: ends at Priority, not {:?}",
        r.state().waiting_for
    );
}

// ---- Shape 1a: "When this creature enters, mill three cards. You may put a land card from among
// the cards milled this way into your hand. If you don't, put a +1/+1 counter on this creature."
// (Blanchwood Prowler). The choice is over the three milled cards only, and only their land. ----
struct Prowler {
    spell: ObjectId,
    bears: ObjectId,
    land: ObjectId,
    shock: ObjectId,
    island: ObjectId,
    /// A land card that starts in the graveyard, before the mill: eligible by type, but not milled
    /// "this way", so it must never be offered.
    old_land: ObjectId,
}

/// Library: Grizzly Bears, <Forest or Hill Giant>, Shock, then Island (not milled). The graveyard
/// starts with a Mountain.
fn prowler_table(with_land: bool) -> (GameRunner, Prowler) {
    let (mut r, t) = table(|s| Prowler {
        spell: s.add_real_card(P0, "Blanchwood Prowler", Zone::Hand, db()),
        bears: s.add_real_card(P0, "Grizzly Bears", Zone::Library, db()),
        land: s.add_real_card(
            P0,
            if with_land { "Forest" } else { "Hill Giant" },
            Zone::Library,
            db(),
        ),
        shock: s.add_real_card(P0, "Shock", Zone::Library, db()),
        island: s.add_real_card(P0, "Island", Zone::Library, db()),
        old_land: s.add_real_card(P0, "Mountain", Zone::Graveyard, db()),
    });
    set_library(&mut r, P0, &[t.bears, t.land, t.shock, t.island]);
    (r, t)
}

#[test]
fn prowler_may_put_the_milled_land_into_hand() {
    let (mut r, t) = prowler_table(true);
    let mut plan = Plan {
        accept: true,
        picks: vec![t.land],
        ..Plan::default()
    };
    cast_plan(&mut r, "prowler accept", t.spell, &mut plan);
    assert_eq!(
        plan.prompts,
        vec![Shown {
            actor: P0,
            cards: vec![t.land],
            min: 0,
            max: 1,
        }],
        "the land among the milled cards is the only choice: not the nonland ones, not the Island below, not the Mountain already in the graveyard"
    );
    assert_eq!(
        zone_of(&r, t.old_land),
        Zone::Graveyard,
        "the pre-existing land was not milled this way, so it stays"
    );
    assert_eq!(zone_of(&r, t.land), Zone::Hand);
    assert_eq!(zone_of(&r, t.bears), Zone::Graveyard);
    assert_eq!(zone_of(&r, t.shock), Zone::Graveyard);
    assert_eq!(
        zone_of(&r, t.island),
        Zone::Library,
        "only three were milled"
    );
    assert_eq!(
        counters(&r, t.spell),
        0,
        "the counter is the 'if you don't' branch"
    );
    expect_priority_empty(&r, "prowler accept");
}

#[test]
fn prowler_declining_the_land_puts_a_counter_on_it() {
    let (mut r, t) = prowler_table(true);
    let mut plan = Plan::default();
    cast_plan(&mut r, "prowler decline", t.spell, &mut plan);
    assert_eq!(
        plan.prompts,
        vec![Shown {
            actor: P0,
            cards: vec![t.land],
            min: 0,
            max: 1,
        }],
        "the choice was offered, over the milled land only, not the Mountain already in the graveyard"
    );
    assert_eq!(zone_of(&r, t.old_land), Zone::Graveyard);
    assert_eq!(
        zone_of(&r, t.land),
        Zone::Graveyard,
        "left in the graveyard"
    );
    assert_eq!(counters(&r, t.spell), 1);
    assert_eq!(power_toughness(&r, t.spell), (2, 2));
    assert!(hand(&r, P0).is_empty());
    expect_priority_empty(&r, "prowler decline");
}

#[test]
fn prowler_without_a_milled_land_gets_the_counter() {
    let (mut r, t) = prowler_table(false);
    let mut plan = Plan {
        accept: true,
        picks: vec![t.land],
        ..Plan::default()
    };
    cast_plan(&mut r, "prowler no land", t.spell, &mut plan);
    assert!(
        plan.prompts.is_empty(),
        "no land milled: nothing to choose, so no choice is offered, although a Mountain sits in the graveyard"
    );
    assert_eq!(
        zone_of(&r, t.old_land),
        Zone::Graveyard,
        "the pre-existing land is not a milled card"
    );
    assert_eq!(
        zone_of(&r, t.land),
        Zone::Graveyard,
        "Hill Giant is not a land card"
    );
    assert_eq!(
        zone_of(&r, t.island),
        Zone::Library,
        "the Island was not milled"
    );
    assert_eq!(counters(&r, t.spell), 1);
    assert!(hand(&r, P0).is_empty());
    expect_priority_empty(&r, "prowler no land");
}

// ---- Shape 1b: "Mill four cards. You may put a permanent card from among the cards milled this
// way into your hand. If you control a Squirrel or returned a Squirrel card to your hand this way,
// create a Food token." (Cache Grab). Permanent cards only: the instant is not offered. ----
struct Grab {
    spell: ObjectId,
    island: ObjectId,
    shock: ObjectId,
    bears: ObjectId,
    squirrel: ObjectId,
    forest: ObjectId,
    /// A permanent card that starts in the graveyard, before the mill: eligible by type, but not
    /// milled "this way", so it must never be offered.
    old_permanent: ObjectId,
}

fn grab_table(squirrel_on_board: bool) -> (GameRunner, Grab) {
    let (mut r, g) = table(|s| {
        if squirrel_on_board {
            s.add_real_card(P0, "Bonecache Overseer", Zone::Battlefield, db());
        }
        Grab {
            spell: s.add_real_card(P0, "Cache Grab", Zone::Hand, db()),
            island: s.add_real_card(P0, "Island", Zone::Library, db()),
            shock: s.add_real_card(P0, "Shock", Zone::Library, db()),
            bears: s.add_real_card(P0, "Grizzly Bears", Zone::Library, db()),
            squirrel: s.add_real_card(P0, "Bushy Bodyguard", Zone::Library, db()),
            forest: s.add_real_card(P0, "Forest", Zone::Library, db()),
            old_permanent: s.add_real_card(P0, "Llanowar Elves", Zone::Graveyard, db()),
        }
    });
    set_library(
        &mut r,
        P0,
        &[g.island, g.shock, g.bears, g.squirrel, g.forest],
    );
    (r, g)
}

fn foods(r: &GameRunner) -> usize {
    r.state()
        .objects
        .values()
        .filter(|o| {
            o.is_token && o.name == "Food" && o.zone == Zone::Battlefield && o.controller == P0
        })
        .count()
}

/// Cast Cache Grab, taking `pick` (or nothing) from the milled permanent cards; returns the prompt.
fn grab_run(r: &mut GameRunner, g: &Grab, pick: Option<ObjectId>) -> Shown {
    let mut plan = Plan {
        accept: pick.is_some(),
        picks: pick.into_iter().collect(),
        ..Plan::default()
    };
    cast_plan(r, "cache grab", g.spell, &mut plan);
    assert_eq!(
        plan.prompts,
        vec![Shown {
            actor: P0,
            cards: sorted(vec![g.island, g.bears, g.squirrel]),
            min: 0,
            max: 1,
        }],
        "exactly one choice, over the milled permanent cards only: not the Shock, not the Llanowar Elves already in the graveyard"
    );
    assert_eq!(
        zone_of(r, g.old_permanent),
        Zone::Graveyard,
        "the pre-existing permanent card was not milled this way, so it stays"
    );
    plan.prompts.remove(0)
}

#[test]
fn cache_grab_takes_one_milled_permanent_and_no_food_without_a_squirrel() {
    let (mut r, g) = grab_table(false);
    let prompt = grab_run(&mut r, &g, Some(g.bears));
    assert_eq!(
        prompt,
        Shown {
            actor: P0,
            cards: sorted(vec![g.island, g.bears, g.squirrel]),
            min: 0,
            max: 1,
        },
        "permanent cards only: the Shock is not offered, nor the pre-existing Llanowar Elves"
    );
    assert_eq!(zone_of(&r, g.bears), Zone::Hand);
    assert_eq!(zone_of(&r, g.shock), Zone::Graveyard);
    assert_eq!(zone_of(&r, g.island), Zone::Graveyard);
    assert_eq!(zone_of(&r, g.squirrel), Zone::Graveyard);
    assert_eq!(
        zone_of(&r, g.forest),
        Zone::Library,
        "only four were milled"
    );
    assert_eq!(foods(&r), 0, "no Squirrel controlled or returned");
    expect_priority_empty(&r, "cache grab no squirrel");
}

#[test]
fn cache_grab_returning_a_squirrel_makes_a_food() {
    let (mut r, g) = grab_table(false);
    grab_run(&mut r, &g, Some(g.squirrel));
    assert_eq!(zone_of(&r, g.squirrel), Zone::Hand);
    assert_eq!(foods(&r), 1);
    expect_priority_empty(&r, "cache grab squirrel");
}

#[test]
fn cache_grab_controlling_a_squirrel_makes_a_food_even_when_nothing_is_taken() {
    let (mut r, g) = grab_table(true);
    grab_run(&mut r, &g, None);
    assert_eq!(zone_of(&r, g.bears), Zone::Graveyard);
    assert!(hand(&r, P0).is_empty());
    assert_eq!(foods(&r), 1);
    expect_priority_empty(&r, "cache grab controls squirrel");
}

// ---- Shape 2: "Search your library for up to three creature cards, reveal them, then shuffle and
// put those cards on top in any order." (Congregation at Dawn). The creatures sit under two lands,
// so only a real search brings them to the top, and the order is the searcher's. ----
struct Congregation {
    r: GameRunner,
    creatures: [ObjectId; 4],
    forest: ObjectId,
    island: ObjectId,
    search: Shown,
    revealed: bool,
}

/// Search, taking the creatures at `order` (indices into the four creature cards) in that order.
fn congregation_run(order: &[usize]) -> Congregation {
    let (mut r, (spell, creatures, forest, island)) = table(|s| {
        (
            s.add_real_card(P0, "Congregation at Dawn", Zone::Hand, db()),
            [
                s.add_real_card(P0, "Grizzly Bears", Zone::Library, db()),
                s.add_real_card(P0, "Runeclaw Bear", Zone::Library, db()),
                s.add_real_card(P0, "Hill Giant", Zone::Library, db()),
                s.add_real_card(P0, "Gray Ogre", Zone::Library, db()),
            ],
            s.add_real_card(P0, "Forest", Zone::Library, db()),
            s.add_real_card(P0, "Island", Zone::Library, db()),
        )
    });
    set_library(
        &mut r,
        P0,
        &[
            forest,
            island,
            creatures[0],
            creatures[1],
            creatures[2],
            creatures[3],
        ],
    );
    let mut plan = Plan {
        picks: order.iter().map(|i| creatures[*i]).collect(),
        ..Plan::default()
    };
    let mut revealed = false;
    cast(&mut r, spell);
    settle(&mut r, "congregation", |wf| {
        if let WaitingFor::SearchChoice { reveal, .. } = wf {
            revealed = *reveal;
        }
        plan_answer(&mut plan, wf)
    });
    assert_eq!(zone_of(&r, spell), Zone::Graveyard);
    let search = plan.prompts.remove(0);
    Congregation {
        r,
        creatures,
        forest,
        island,
        search,
        revealed,
    }
}

#[test]
fn congregation_puts_the_three_found_creatures_on_top_in_the_chosen_order() {
    // Two different orders: the order is the player's choice, not the library's or the ids'.
    for order in [[2usize, 3, 0], [0, 2, 3]] {
        let g = congregation_run(&order);
        let c = g.creatures;
        assert_eq!(g.search.actor, P0);
        assert_eq!(
            g.search.cards,
            sorted(c.to_vec()),
            "every creature card in the library, no land"
        );
        assert_eq!((g.search.min, g.search.max), (0, 3), "up to three");
        assert!(g.revealed, "the found cards are revealed");
        let lib = library(&g.r, P0);
        assert_eq!(lib.len(), 6, "nothing left the library");
        assert_eq!(
            lib[..3].to_vec(),
            order.iter().map(|i| c[*i]).collect::<Vec<_>>(),
            "on top, in the order given"
        );
        assert_eq!(
            sorted(lib[3..].to_vec()),
            sorted(vec![g.forest, g.island, c[1]]),
            "the lands and the creature left behind are below"
        );
        expect_priority_empty(&g.r, "congregation three");
    }
}

#[test]
fn congregation_may_find_fewer_than_three() {
    let g = congregation_run(&[1]);
    assert_eq!(g.search.actor, P0);
    assert_eq!(g.search.cards, sorted(g.creatures.to_vec()));
    assert_eq!((g.search.min, g.search.max), (0, 3), "up to three");
    let lib = library(&g.r, P0);
    assert_eq!(lib[0], g.creatures[1], "the one found creature is on top");
    assert_eq!(lib.len(), 6);
    expect_priority_empty(&g.r, "congregation one");
}

// ---- Shape 3: "Enchanted creature gets +0/+2 for each other creature you control." (Conclave's
// Blessing). The count excludes the enchanted creature and the opponent's creatures, and includes
// tapped ones (CR 702.51a: convoke taps them to pay). ----
/// "Enchant creature": P0 announces one target slot, and every creature on the battlefield (the
/// opponent's included) is legal.
fn assert_enchant_creature_prompt(plan: &Plan, r: &GameRunner) {
    let mut creatures: Vec<TargetRef> = r
        .state()
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield && o.card_types.core_types.contains(&CoreType::Creature)
        })
        .map(|o| TargetRef::Object(o.id))
        .collect();
    creatures.sort_by_key(|t| match t {
        TargetRef::Object(o) => o.0,
        TargetRef::Player(p) => u64::from(p.0),
    });
    assert_eq!(plan.target_prompts.len(), 1, "one target prompt");
    let (actor, mut legal) = plan.target_prompts[0].clone();
    legal.sort_by_key(|t| match t {
        TargetRef::Object(o) => o.0,
        TargetRef::Player(p) => u64::from(p.0),
    });
    assert_eq!(actor, P0);
    assert_eq!(
        legal, creatures,
        "every creature on the battlefield is a legal enchant target"
    );
}

#[test]
fn conclaves_blessing_counts_the_other_creatures_you_control() {
    let (mut r, (aura, bears, opp)) = table(|s| {
        s.add_real_card(P0, "Runeclaw Bear", Zone::Battlefield, db());
        s.add_real_card(P0, "Gray Ogre", Zone::Battlefield, db());
        (
            s.add_real_card(P0, "Conclave's Blessing", Zone::Hand, db()),
            s.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db()),
            s.add_real_card(P1, "Hill Giant", Zone::Battlefield, db()),
        )
    });
    let mut plan = Plan {
        targets: vec![TargetRef::Object(bears)],
        ..Plan::default()
    };
    cast_plan(&mut r, "blessing", aura, &mut plan);
    assert_enchant_creature_prompt(&plan, &r);
    assert_eq!(zone_of(&r, aura), Zone::Battlefield);
    assert_eq!(
        power_toughness(&r, bears),
        (2, 2 + 2 * 2),
        "+0/+2 for each of the two other creatures; not for itself, not for the opponent's"
    );
    assert_eq!(
        power_toughness(&r, opp),
        (3, 3),
        "only the enchanted creature is buffed"
    );
    expect_priority_empty(&r, "blessing");
}

#[test]
fn conclaves_blessing_convoked_counts_tapped_creatures_too() {
    let (mut r, (aura, bears, others)) =
        table_with_pool(exact_pool(&[(ManaType::White, 1)]), |s| {
            (
                s.add_real_card(P0, "Conclave's Blessing", Zone::Hand, db()),
                s.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db()),
                [
                    s.add_real_card(P0, "Runeclaw Bear", Zone::Battlefield, db()),
                    s.add_real_card(P0, "Gray Ogre", Zone::Battlefield, db()),
                    s.add_real_card(P0, "Goblin Piker", Zone::Battlefield, db()),
                ],
            )
        });
    cast(&mut r, aura);
    let mut taps = others.to_vec();
    let mut plan = Plan {
        targets: vec![TargetRef::Object(bears)],
        ..Plan::default()
    };
    settle(&mut r, "blessing convoke", |wf| match wf {
        WaitingFor::ManaPayment { .. } => match taps.pop() {
            Some(c) => Some(GameAction::TapForConvoke {
                object_id: c,
                mana_type: ManaType::Colorless,
            }),
            None => Some(GameAction::PassPriority),
        },
        _ => plan_answer(&mut plan, wf),
    });
    assert_enchant_creature_prompt(&plan, &r);
    for c in others {
        assert!(r.state().objects[&c].tapped, "convoked: tapped to pay");
    }
    assert_eq!(zone_of(&r, aura), Zone::Battlefield);
    assert_eq!(
        power_toughness(&r, bears),
        (2, 2 + 2 * 3),
        "three other creatures, tapped or not"
    );
    expect_priority_empty(&r, "blessing convoke");
}

// ---- Shape 4: "Target player sacrifices an attacking creature of their choice. You create X 1/1
// white Soldier creature tokens, where X is that creature's toughness." (Entrapment Maneuver).
// P0 attacks P1 with a 3/3 and a 2/1 and keeps a 2/2 home; P1 casts the instant at P0 in the
// declare-attackers step. The target player chooses among the attackers only, and the
// Soldiers are P1's, one for each point of the sacrificed creature's toughness. ----
fn maneuver_run(sacrifice: usize) -> (GameRunner, Vec<ObjectId>, Vec<Shown>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..4 {
        scenario.add_basic_land(P1, ManaColor::White);
    }
    let spell = scenario.add_real_card(P1, "Entrapment Maneuver", Zone::Hand, db());
    let attackers = vec![
        scenario.add_real_card(P0, "Hill Giant", Zone::Battlefield, db()),
        scenario.add_real_card(P0, "Goblin Piker", Zone::Battlefield, db()),
    ];
    let idle = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db());
    scenario.add_real_card(P1, "Gray Ogre", Zone::Battlefield, db());
    let mut r = scenario.build();
    // P0 declares both attackers and passes; P1 then has priority in the declare-attackers step.
    for _ in 0..40 {
        let s = r.state();
        if s.phase == Phase::DeclareAttackers
            && matches!(s.waiting_for, WaitingFor::Priority { player } if player == P1)
        {
            break;
        }
        let action = match &s.waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: attackers
                    .iter()
                    .map(|a| (*a, AttackTarget::Player(P1)))
                    .collect(),
                bands: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("maneuver: unexpected prompt {other:?}"),
        };
        r.act(action).expect("drive to the declare-attackers step");
    }
    assert_eq!(r.state().phase, Phase::DeclareAttackers, "reached the step");
    let mut plan = Plan {
        targets: vec![TargetRef::Player(P0)],
        picks: vec![attackers[sacrifice]],
        ..Plan::default()
    };
    cast_plan(&mut r, "maneuver", spell, &mut plan);
    assert_eq!(
        plan.target_prompts,
        vec![(P1, vec![TargetRef::Player(P0), TargetRef::Player(P1)])],
        "P1 announces the one target player slot"
    );
    let mut all = attackers.clone();
    all.push(idle);
    (r, all, plan.prompts)
}

/// The Soldier tokens `p` controls, as (power, toughness); each is checked to be the printed
/// token: a white Soldier creature.
fn soldiers(r: &GameRunner, p: PlayerId) -> Vec<(i32, i32)> {
    r.state()
        .objects
        .values()
        .filter(|o| {
            o.is_token && o.name == "Soldier" && o.zone == Zone::Battlefield && o.controller == p
        })
        .map(|o| {
            assert_eq!(o.color, vec![ManaColor::White]);
            assert!(o.card_types.core_types.contains(&CoreType::Creature));
            assert_eq!(o.card_types.subtypes, vec!["Soldier".to_string()]);
            (o.power.unwrap_or(0), o.toughness.unwrap_or(0))
        })
        .collect()
}

#[test]
fn entrapment_maneuver_makes_tokens_equal_to_the_sacrificed_attackers_toughness() {
    let (r, ids, prompts) = maneuver_run(0);
    assert_eq!(
        prompts,
        vec![Shown {
            actor: P0,
            cards: sorted(vec![ids[0], ids[1]]),
            min: 1,
            max: 1,
        }],
        "the target player chooses, among the attacking creatures only"
    );
    assert_eq!(
        zone_of(&r, ids[0]),
        Zone::Graveyard,
        "the Hill Giant was sacrificed"
    );
    assert_eq!(zone_of(&r, ids[1]), Zone::Battlefield);
    assert_eq!(
        zone_of(&r, ids[2]),
        Zone::Battlefield,
        "the idle Bears was not an option"
    );
    assert_eq!(
        soldiers(&r, P1),
        vec![(1, 1); 3],
        "toughness 3: three Soldiers for the caster"
    );
    assert!(soldiers(&r, P0).is_empty());
    expect_priority_empty(&r, "maneuver giant");
}

#[test]
fn entrapment_maneuver_with_the_other_attacker_makes_one_token() {
    let (r, ids, _prompts) = maneuver_run(1);
    assert_eq!(
        zone_of(&r, ids[1]),
        Zone::Graveyard,
        "the Goblin Piker was sacrificed"
    );
    assert_eq!(zone_of(&r, ids[0]), Zone::Battlefield);
    assert_eq!(
        soldiers(&r, P1),
        vec![(1, 1); 1],
        "toughness 1: one Soldier"
    );
    expect_priority_empty(&r, "maneuver piker");
}
