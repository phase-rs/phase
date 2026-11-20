//! CR 603.2c, 614.6, 701.17a: a combat damage substitute mills physical cards;
//! every qualifying card move triggers each printed observer once.

use std::collections::BTreeMap;

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, PayCostKind, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ALCHEMIST: &str = "If a Zombie you control would deal combat damage to a player, instead that player mills that many cards.\nWhenever a creature card is put into an opponent's graveyard from their library, exile that card and create a 2/2 black Zombie creature token.";
const SZADEK: &str = "Flying\nIf Szadek would deal combat damage to a player, instead put that many +1/+1 counters on Szadek and that player mills that many cards.";

fn alchemist(s: &mut GameScenario) -> ObjectId {
    s.add_creature_from_oracle(P0, "Undead Alchemist", 4, 2, ALCHEMIST)
        .with_subtypes(vec!["Zombie"])
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Blue],
        })
        .with_color(vec![ManaColor::Blue])
        .id()
}

fn printed(s: &mut GameScenario, player: PlayerId, name: &str, zone: Zone) -> ObjectId {
    s.add_real_card(
        player,
        name,
        zone,
        crate::support::shared_card_db().expect("committed card fixture"),
    )
}

fn library(s: &mut GameScenario, player: PlayerId, names: &[&str]) -> Vec<ObjectId> {
    names
        .iter()
        .map(|name| printed(s, player, name, Zone::Library))
        .collect()
}

fn assert_library(r: &GameRunner, player: PlayerId, ids: &[ObjectId]) {
    let library = &r
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .library;
    assert!(
        library.iter().eq(ids),
        "library {player:?}: actual {library:?}, expected {ids:?}"
    );
    for id in ids {
        let card = &r.state().objects[id];
        assert_eq!((card.owner, card.zone), (player, Zone::Library));
    }
}

#[derive(Default)]
struct Trace {
    events: Vec<GameEvent>,
    triggers: BTreeMap<ObjectId, (ObjectId, ObjectId)>,
    orders: Vec<(PlayerId, usize)>,
}

impl Trace {
    fn step(&mut self, r: &mut GameRunner, action: GameAction) {
        self.events
            .extend(r.act(action).expect("public game action").events);
        for entry in &r.state().stack {
            if let StackEntryKind::TriggeredAbility {
                trigger_event:
                    Some(GameEvent::ZoneChanged {
                        object_id,
                        from: Some(Zone::Library),
                        to: Zone::Graveyard,
                        record,
                    }),
                ..
            } = &entry.kind
            {
                assert_eq!(record.object_id, *object_id);
                self.triggers
                    .insert(entry.id, (entry.source_id, *object_id));
            }
        }
    }

    fn pass_or_order(&mut self, r: &mut GameRunner) {
        let action = match &r.state().waiting_for {
            WaitingFor::Priority { player: _ } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { player, triggers } => {
                self.orders.push((*player, triggers.len()));
                GameAction::OrderTriggers {
                    order: (0..triggers.len()).rev().collect(),
                }
            }
            wait => panic!("unexpected prompt: {wait:?}"),
        };
        self.step(r, action);
    }

    fn step_combat(&mut self, r: &mut GameRunner) {
        if matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. }) {
            self.step(
                r,
                GameAction::DeclareBlockers {
                    assignments: Vec::new(),
                },
            );
        } else {
            self.pass_or_order(r);
        }
    }

    fn settle(&mut self, r: &mut GameRunner) {
        for _ in 0..64 {
            if r.state().stack.is_empty()
                && matches!(r.state().waiting_for, WaitingFor::Priority { player: _ })
            {
                return;
            }
            self.pass_or_order(r);
        }
        panic!("stack did not settle");
    }

    fn assert_milled(&self, owner: PlayerId, cards: &[ObjectId]) {
        let milled: Vec<_> = self
            .events
            .iter()
            .filter_map(|event| {
                if let GameEvent::Milled {
                    player_id,
                    object_id,
                    to,
                } = event
                {
                    Some((*player_id, *object_id, *to))
                } else {
                    None
                }
            })
            .collect();
        let moved: Vec<_> = self
            .events
            .iter()
            .filter_map(|event| {
                if let GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Library),
                    to: Zone::Graveyard,
                    record,
                } = event
                {
                    Some((record.owner, *object_id, Zone::Graveyard))
                } else {
                    None
                }
            })
            .collect();
        let expected: Vec<_> = cards
            .iter()
            .map(|id| (owner, *id, Zone::Graveyard))
            .collect();
        eprintln!(
            "physical mill={milled:?}; Library→Graveyard={moved:?}; stack triggers={:?}",
            self.triggers
        );
        assert_eq!(milled, expected, "one mill delivery per physical card");
        assert_eq!(
            moved, expected,
            "one library-to-graveyard move per physical card"
        );
    }

    fn assert_triggers(&self, source: ObjectId, cards: &[ObjectId]) {
        let mut actual: Vec<_> = self
            .triggers
            .values()
            .filter_map(|(id, card)| (*id == source).then_some(*card))
            .collect();
        actual.sort_unstable();
        let mut expected = cards.to_vec();
        expected.sort_unstable();
        assert_eq!(
            actual, expected,
            "one stack trigger per qualifying graveyard arrival, source {source:?}"
        );
    }

    fn assert_zombies(&self, r: &GameRunner, source: ObjectId, count: usize) {
        let created: Vec<_> = self
            .events
            .iter()
            .filter_map(|event| {
                if let GameEvent::TokenCreated {
                    object_id,
                    name: _,
                    source_id,
                } = event
                {
                    assert_eq!(*source_id, source);
                    Some(*object_id)
                } else {
                    None
                }
            })
            .collect();
        let zombies: Vec<_> = r
            .state()
            .objects
            .values()
            .filter(|o| o.is_token && o.zone == Zone::Battlefield)
            .collect();
        eprintln!("Zombie token IDs={created:?}");
        assert_eq!(
            created.len(),
            count,
            "one Zombie created per qualifying card"
        );
        assert_eq!(zombies.len(), count);
        for token in zombies {
            assert!(created.contains(&token.id));
            assert_eq!(
                (token.controller, token.power, token.toughness),
                (P0, Some(2), Some(2))
            );
            assert_eq!(token.card_types.core_types.as_slice(), [CoreType::Creature]);
            assert_eq!(token.card_types.subtypes.as_slice(), ["Zombie"]);
            assert_eq!(token.color.as_slice(), [ManaColor::Black]);
        }
    }
}

fn combat(r: &mut GameRunner, attackers: &[ObjectId]) -> Trace {
    r.advance_to_combat();
    r.declare_attackers(
        &attackers
            .iter()
            .map(|id| (*id, AttackTarget::Player(P1)))
            .collect::<Vec<_>>(),
    )
    .expect("legal attackers");
    let mut trace = Trace::default();
    for _ in 0..64 {
        if r.state().phase == Phase::EndCombat {
            assert!(r.state().stack.is_empty() && r.state().resolution_stack.is_empty());
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::Priority { player: P0 }
            ));
            return trace;
        }
        trace.step_combat(r);
    }
    panic!("combat did not finish");
}

fn assert_final_zones(r: &GameRunner, milled: &[ObjectId], exiled: &[ObjectId]) {
    for id in milled {
        let expected = if exiled.contains(id) {
            Zone::Exile
        } else {
            Zone::Graveyard
        };
        assert_eq!(r.state().objects[id].zone, expected, "physical card {id:?}");
    }
}

fn root_alchemist_mill(top: &[&str], count: usize, attack_with_bear: bool) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let source = alchemist(&mut s);
    let ids = library(&mut s, P1, top);
    let mut attackers = vec![source];
    if attack_with_bear {
        attackers.push(printed(&mut s, P0, "Grizzly Bears", Zone::Battlefield));
    }
    let mut r = s.build();
    assert_library(&r, P1, &ids);
    let trace = combat(&mut r, &attackers);
    let (milled, remaining) = ids.split_at(ids.len().min(4));
    let exiled = &milled[..count];
    assert_eq!(r.life(P1), if attack_with_bear { 18 } else { 20 });
    assert!(trace.events.iter().any(|event| matches!(event, GameEvent::ReplacementApplied { source_id, event_type: _ } if *source_id == source)));
    trace.assert_milled(P1, milled);
    assert_library(&r, P1, remaining);
    eprintln!(
        "final zones {:?}; battlefield Zombie count={}",
        milled
            .iter()
            .map(|id| (*id, r.state().objects[id].zone))
            .collect::<Vec<_>>(),
        r.state()
            .objects
            .values()
            .filter(|o| o.is_token && o.zone == Zone::Battlefield)
            .count()
    );
    trace.assert_triggers(source, exiled);
    trace.assert_zombies(&r, source, count);
    assert_final_zones(&r, milled, exiled);
}

#[test]
fn own_combat_substitution_one_creature() {
    root_alchemist_mill(
        &[
            "Grizzly Bears",
            "Island",
            "Island",
            "Island",
            "Runeclaw Bear",
        ],
        1,
        false,
    );
}

#[test]
fn own_combat_substitution_two_creatures_plus_ordinary_bear_hit() {
    root_alchemist_mill(
        &[
            "Grizzly Bears",
            "Runeclaw Bear",
            "Island",
            "Island",
            "Grizzly Bears",
        ],
        2,
        true,
    );
}

#[test]
fn own_combat_substitution_zero_cards_or_only_noncreatures() {
    root_alchemist_mill(&[], 0, false);
    root_alchemist_mill(
        &["Island", "Island", "Island", "Island", "Grizzly Bears"],
        0,
        false,
    );
}

#[test]
fn ordinary_mill_watches_only_the_opponents_library() {
    for target in [P0, P1] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = alchemist(&mut s);
        let spell = printed(&mut s, P0, "Tome Scour", Zone::Hand);
        let island = s.add_basic_land(P0, ManaColor::Blue);
        s.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Blue, island, false, vec![])],
        );
        let ids = library(
            &mut s,
            target,
            &[
                "Grizzly Bears",
                "Runeclaw Bear",
                "Island",
                "Island",
                "Island",
                "Grizzly Bears",
            ],
        );
        let mut r = s.build();
        assert_library(&r, target, &ids);
        let mut trace = Trace::default();
        trace
            .events
            .extend_from_slice(r.cast(spell).target_player(target).commit().events());
        trace.settle(&mut r);
        let exiled = if target == P1 { &ids[..2] } else { &[] };
        trace.assert_milled(target, &ids[..5]);
        assert_library(&r, target, &ids[5..]);
        assert_final_zones(&r, &ids[..5], exiled);
        assert!(!trace.events.iter().any(|event| matches!(event, GameEvent::ReplacementApplied { source_id, event_type: _ } if *source_id == source)));
        trace.assert_triggers(source, exiled);
        trace.assert_zombies(&r, source, exiled.len());
    }
}

#[test]
fn noncombat_zombie_damage_is_not_a_mill() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let source = alchemist(&mut s);
    let tymaret = printed(&mut s, P0, "Tymaret, the Murder King", Zone::Battlefield);
    let fodder = printed(&mut s, P0, "Grizzly Bears", Zone::Battlefield);
    let red_sources = [
        s.add_basic_land(P0, ManaColor::Red),
        s.add_basic_land(P0, ManaColor::Red),
    ];
    s.with_mana_pool(
        P0,
        red_sources
            .into_iter()
            .map(|land| ManaUnit::new(ManaType::Red, land, false, vec![]))
            .collect(),
    );
    let ids = library(&mut s, P1, &["Grizzly Bears", "Island"]);
    let mut r = s.build();
    assert_library(&r, P1, &ids);
    let mut trace = Trace::default();
    trace.step(
        &mut r,
        GameAction::ActivateAbility {
            source_id: tymaret,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    let mut chose_target = false;
    let mut paid_sacrifice = false;
    for _ in 0..8 {
        let action = match &r.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                assert!(!chose_target);
                chose_target = true;
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1)),
                }
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                choices,
                count: 1,
                ..
            } => {
                assert!(!paid_sacrifice && choices.contains(&fodder));
                paid_sacrifice = true;
                GameAction::SelectCards {
                    cards: vec![fodder],
                }
            }
            WaitingFor::Priority { player: _ } => break,
            wait => panic!("unexpected activation prompt: {wait:?}"),
        };
        trace.step(&mut r, action);
    }
    assert!(chose_target && paid_sacrifice);
    assert_eq!(r.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(r.state().stack.last().unwrap().source_id, tymaret);
    trace.settle(&mut r);
    assert!(trace.events.iter().any(|event| matches!(event, GameEvent::DamageDealt { source_id, target: TargetRef::Player(P1), amount: 2, is_combat: false, excess: _, source_incarnation: _ } if *source_id == tymaret)));
    assert_eq!(r.life(P1), 18);
    assert_library(&r, P1, &ids);
    trace.assert_milled(P1, &[]);
    trace.assert_zombies(&r, source, 0);
}

#[test]
fn szadek_mill_reaches_two_independent_printed_watchers() {
    let p2 = PlayerId(2);
    let mut s = GameScenario::new_n_player(3, 196);
    s.at_phase(Phase::PreCombatMain);
    let source = alchemist(&mut s);
    let szadek = s
        .add_creature_from_oracle(P0, "Szadek, Lord of Secrets", 5, 5, SZADEK)
        .as_legendary()
        .with_subtypes(vec!["Vampire"])
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![
                ManaCostShard::Blue,
                ManaCostShard::Blue,
                ManaCostShard::Black,
                ManaCostShard::Black,
            ],
        })
        .with_color(vec![ManaColor::Blue, ManaColor::Black])
        .id();
    let watcher = printed(&mut s, p2, "Vulturous Zombie", Zone::Battlefield);
    let ids = library(
        &mut s,
        P1,
        &[
            "Grizzly Bears",
            "Runeclaw Bear",
            "Island",
            "Island",
            "Island",
            "Grizzly Bears",
        ],
    );
    let mut r = s.build();
    assert_library(&r, P1, &ids);
    r.advance_to_combat();
    r.declare_attackers(&[(szadek, AttackTarget::Player(P1))])
        .expect("Szadek can attack P1");
    let mut trace = Trace::default();
    for _ in 0..24 {
        if matches!(
            r.state().waiting_for,
            WaitingFor::OrderTriggers {
                player: P0,
                triggers: _
            }
        ) {
            break;
        }
        trace.step_combat(&mut r);
    }
    // CR 603.3b: check the actual groups at the first public ordering choice.
    // Resolution order alone cannot exclude a second, later deferred drain.
    let ordering = r
        .state()
        .pending_trigger_order
        .as_ref()
        .expect("first combat trigger-ordering batch");
    assert_eq!(ordering.groups.len(), 2);
    for (seat, observer, cards) in [(P0, source, &ids[..2]), (p2, watcher, &ids[..5])] {
        let group = ordering
            .groups
            .iter()
            .find(|group| group.controller == seat)
            .expect("observer's controller is in this batch");
        let mut actual = Vec::new();
        for context in &group.triggers {
            assert_eq!(context.pending.source_id, observer);
            let cause = context
                .pending
                .trigger_event
                .as_ref()
                .or_else(|| context.trigger_events.first());
            let Some(GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Library),
                to: Zone::Graveyard,
                record,
            }) = cause
            else {
                panic!("observer must be bound to its specific milled card: {cause:?}");
            };
            assert_eq!(record.owner, P1);
            actual.push(*object_id);
        }
        actual.sort_unstable();
        assert_eq!(
            actual, cards,
            "every physical card reaches its observer in the first batch"
        );
    }
    trace.settle(&mut r);
    assert!(r.state().resolution_stack.is_empty());
    assert_eq!(
        r.state().objects[&szadek]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&5)
    );
    trace.assert_milled(P1, &ids[..5]);
    assert_library(&r, P1, &ids[5..]);
    assert_final_zones(&r, &ids[..5], &ids[..2]);
    trace.assert_triggers(source, &ids[..2]);
    trace.assert_triggers(watcher, &ids[..5]);
    trace.assert_zombies(&r, source, 2);
    assert_eq!(
        r.state().objects[&watcher]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&5)
    );
    // CR 603.3b: Vulturous Zombie's identical self-counter triggers commute;
    // only the Alchemist needs a choice. Both groups still stack together.
    assert_eq!(trace.orders, [(P0, 2)]);
    let resolved_sources: Vec<_> = trace
        .events
        .iter()
        .filter_map(|event| {
            if let GameEvent::StackResolved { object_id } = event {
                trace.triggers.get(object_id).map(|(source, _)| *source)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        resolved_sources,
        [watcher, watcher, watcher, watcher, watcher, source, source],
        "nonactive player's triggers resolve first"
    );
    assert_eq!((r.life(P0), r.life(P1), r.life(p2)), (20, 20, 20));
}

/// CR 510.2 + CR 616.1: the first substitution's graveyard observer waits
/// across the second defender's live replacement choice, then stacks once.
#[test]
fn early_mill_waits_for_later_defenders_replacement_choice() {
    let p2 = PlayerId(2);
    let mut s = GameScenario::new_n_player(3, 196);
    s.at_phase(Phase::PreCombatMain);
    let source = alchemist(&mut s);
    let szadek = printed(&mut s, P0, "Szadek, Lord of Secrets", Zone::Battlefield);
    let shield = printed(&mut s, p2, "Solitary Confinement", Zone::Battlefield);
    let ids = library(
        &mut s,
        P1,
        &[
            "Grizzly Bears",
            "Island",
            "Island",
            "Island",
            "Runeclaw Bear",
        ],
    );
    let untouched = library(&mut s, p2, &["Runeclaw Bear"]);
    let mut r = s.build();
    assert_library(&r, P1, &ids);
    assert_library(&r, p2, &untouched);
    r.advance_to_combat();
    r.declare_attackers(&[
        (source, AttackTarget::Player(P1)),
        (szadek, AttackTarget::Player(p2)),
    ])
    .expect("legal attacks at both defenders");

    let mut trace = Trace::default();
    for _ in 0..24 {
        if matches!(r.state().waiting_for, WaitingFor::ReplacementChoice { .. }) {
            break;
        }
        trace.step_combat(&mut r);
    }
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &r.state().waiting_for
    else {
        panic!(
            "expected an ordering choice, got {:?}",
            r.state().waiting_for
        );
    };
    assert_eq!(*player, p2);
    assert_eq!(candidates.len(), 2);
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == szadek));
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == shield)
        .expect("Solitary Confinement is offered");
    assert!(r.state().pending_combat_lifelink.is_some());
    trace.assert_milled(P1, &ids[..4]);
    assert_final_zones(&r, &ids[..4], &[]);
    trace.assert_triggers(source, &[]);
    trace.assert_zombies(&r, source, 0);
    assert!(r.state().stack.is_empty());

    trace.step(&mut r, GameAction::ChooseReplacement { index });
    trace.settle(&mut r);
    assert!(r.state().pending_combat_lifelink.is_none());
    assert!(r.state().resolution_stack.is_empty());
    trace.assert_milled(P1, &ids[..4]);
    assert_library(&r, P1, &ids[4..]);
    assert_library(&r, p2, &untouched);
    assert_final_zones(&r, &ids[..4], &ids[..1]);
    trace.assert_triggers(source, &ids[..1]);
    trace.assert_zombies(&r, source, 1);
    assert_eq!((r.life(P1), r.life(p2)), (20, 20));
    assert!(!r.state().objects[&szadek]
        .counters
        .contains_key(&CounterType::Plus1Plus1));
}

/// CR 510.4 + CR 702.15b: a replacement mill in the regular step cannot
/// swallow or replay the ordinary combat hits, their draws, or lifelink.
#[test]
fn combat_mill_preserves_repeat_hits_draw_triggers_and_lifelink() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let alchemist = alchemist(&mut s);
    let ace = printed(&mut s, P0, "Fencing Ace", Zone::Battlefield);
    let cat = printed(&mut s, P0, "Sacred Cat", Zone::Battlefield);
    printed(&mut s, P0, "Ohran Frostfang", Zone::Battlefield);
    let draws = library(
        &mut s,
        P0,
        &[
            "Island",
            "Grizzly Bears",
            "Runeclaw Bear",
            "Island",
            "Island",
        ],
    );
    let milled = library(
        &mut s,
        P1,
        &[
            "Grizzly Bears",
            "Island",
            "Island",
            "Island",
            "Runeclaw Bear",
        ],
    );
    let mut r = s.build();
    assert_library(&r, P0, &draws);
    assert_library(&r, P1, &milled);

    let trace = combat(&mut r, &[ace, cat, alchemist]);
    trace.assert_milled(P1, &milled[..4]);
    trace.assert_triggers(alchemist, &milled[..1]);
    trace.assert_zombies(&r, alchemist, 1);
    assert_final_zones(&r, &milled[..4], &milled[..1]);
    assert_library(&r, P1, &milled[4..]);
    let hits = trace
        .events
        .iter()
        .filter_map(|event| {
            if let GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Player(P1),
                amount,
                is_combat: true,
                excess: _,
                source_incarnation: _,
            } = event
            {
                Some((*source_id, *amount))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(hits, [(ace, 1), (ace, 1), (cat, 1)]);
    let drawn = trace
        .events
        .iter()
        .filter_map(|event| {
            if let GameEvent::CardDrawn {
                player_id: P0,
                object_id,
                nth_in_turn: _,
                nth_in_step: _,
            } = event
            {
                Some(*object_id)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        drawn,
        draws[..3],
        "Ohran Frostfang draws exactly once for each unreplaced combat hit"
    );
    assert_library(&r, P0, &draws[3..]);
    assert_eq!((r.life(P0), r.life(P1)), (21, 17));
}

/// CR 510.3a + CR 603.3b + CR 704.3: Blood Artist's two trade deaths join
/// the Alchemist's already-observed mill trigger in one ordering choice.
#[test]
fn mill_and_combat_deaths_share_one_ordering_prompt() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let alchemist = alchemist(&mut s);
    let artist = printed(&mut s, P0, "Blood Artist", Zone::Battlefield);
    let attacking_bear = printed(&mut s, P0, "Grizzly Bears", Zone::Battlefield);
    let blocking_bear = printed(&mut s, P1, "Grizzly Bears", Zone::Battlefield);
    let cards = library(
        &mut s,
        P1,
        &[
            "Runeclaw Bear",
            "Island",
            "Island",
            "Island",
            "Grizzly Bears",
        ],
    );
    let mut r = s.build();
    assert_library(&r, P1, &cards);
    r.advance_to_combat();
    r.declare_attackers(&[
        (alchemist, AttackTarget::Player(P1)),
        (attacking_bear, AttackTarget::Player(P1)),
    ])
    .expect("both attackers declared");
    let mut trace = Trace::default();
    let mut deaths = Vec::new();
    for _ in 0..64 {
        if r.state().phase == Phase::EndCombat {
            break;
        }
        match &r.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => trace.step(
                &mut r,
                GameAction::DeclareBlockers {
                    assignments: vec![(blocking_bear, attacking_bear)],
                },
            ),
            WaitingFor::OrderTriggers { player, triggers } => {
                // Guard the physical mill and trade before checking the current public prompt.
                assert_eq!(r.state().objects[&cards[0]].zone, Zone::Graveyard);
                assert_eq!(r.state().objects[&attacking_bear].zone, Zone::Graveyard);
                assert_eq!(r.state().objects[&blocking_bear].zone, Zone::Graveyard);
                assert_eq!(*player, P0);
                let mut sources = triggers
                    .iter()
                    .map(|trigger| trigger.source_id)
                    .collect::<Vec<_>>();
                sources.sort_unstable();
                assert_eq!(
                    sources,
                    [alchemist, artist, artist],
                    "one ordering prompt includes the mill and both deaths"
                );
                trace.pass_or_order(&mut r);
            }
            WaitingFor::TriggerTargetSelection {
                player,
                source_id,
                trigger_event,
                ..
            } => {
                assert_eq!((*player, *source_id), (P0, Some(artist)));
                let Some(GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Battlefield),
                    to: Zone::Graveyard,
                    record: _,
                }) = trigger_event
                else {
                    panic!("Blood Artist did not observe a separate death");
                };
                deaths.push(*object_id);
                trace.step(
                    &mut r,
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    },
                );
            }
            wait => {
                assert!(
                    matches!(wait, WaitingFor::Priority { player: _ }),
                    "unexpected prompt: {wait:?}"
                );
                trace.pass_or_order(&mut r);
            }
        }
    }
    assert_eq!(r.state().phase, Phase::EndCombat);
    assert!(r.state().stack.is_empty() && r.state().resolution_stack.is_empty());
    assert_eq!(trace.orders, [(P0, 3)]);
    deaths.sort_unstable();
    assert_eq!(deaths, [attacking_bear, blocking_bear]);
    assert_eq!((r.life(P0), r.life(P1)), (22, 18));
    trace.assert_milled(P1, &cards[..4]);
    trace.assert_triggers(alchemist, &cards[..1]);
    trace.assert_zombies(&r, alchemist, 1);
    assert_final_zones(&r, &cards[..4], &cards[..1]);
}

/// CR 113.7a + CR 603.10: the departure is itself already observed, but
/// the departing permanent still saw the earlier mill in this combat batch.
#[test]
fn earlier_mill_reaches_an_observer_whose_departure_is_claimed() {
    let p2 = PlayerId(2);
    let mut s = GameScenario::new_n_player(3, 196);
    s.at_phase(Phase::PreCombatMain);
    let alchemist = alchemist(&mut s);
    let equipment = printed(&mut s, P0, "Fireshrieker", Zone::Battlefield);
    let angel = printed(&mut s, P0, "Weeping Angel", Zone::Battlefield);
    let glowing = printed(&mut s, p2, "Glowing One", Zone::Battlefield);
    let ozolith = printed(&mut s, p2, "The Ozolith", Zone::Battlefield);
    // A counter remaining from a Snakeskin Veil resolved on a previous turn.
    s.with_counter(glowing, CounterType::Plus1Plus1, 1);
    let land = s.add_basic_land(P0, ManaColor::Blue);
    let other_land = s.add_basic_land(P0, ManaColor::Blue);
    s.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, land, false, vec![]),
            ManaUnit::new(ManaType::Blue, other_land, false, vec![]),
        ],
    );
    let cards = library(
        &mut s,
        P1,
        &[
            "Grizzly Bears",
            "Island",
            "Island",
            "Island",
            "Island",
            "Island",
            "Island",
            "Island",
            "Runeclaw Bear",
        ],
    );
    let mut r = s.build();
    assert_library(&r, P1, &cards);
    r.activate(equipment, 0).target_object(alchemist).resolve();
    assert!(r.state().objects[&alchemist]
        .attachments
        .contains(&equipment));
    r.advance_to_combat();
    r.declare_attackers(&[
        (alchemist, AttackTarget::Player(P1)),
        (angel, AttackTarget::Player(p2)),
    ])
    .expect("both creatures attack distinct opponents");
    let mut trace = Trace::default();
    for _ in 0..64 {
        if r.state().phase == Phase::EndCombat {
            break;
        }
        if let WaitingFor::DeclareBlockers { player, .. } = &r.state().waiting_for {
            let assignments = if *player == p2 {
                vec![(glowing, angel)]
            } else {
                Vec::new()
            };
            trace.step(&mut r, GameAction::DeclareBlockers { assignments });
        } else {
            trace.pass_or_order(&mut r);
        }
    }
    assert_eq!(r.state().phase, Phase::EndCombat);
    assert!(r.state().stack.is_empty() && r.state().resolution_stack.is_empty());
    trace.assert_milled(P1, &cards[..8]);
    let mill_position = trace.events.iter().position(|event| matches!(event,
        GameEvent::Milled { player_id: P1, object_id, to: Zone::Graveyard } if *object_id == cards[0]
    )).expect("the nonland card is milled");
    let departure = trace.events.iter().position(|event| matches!(event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Library, record: _ } if *object_id == glowing
    )).expect("Weeping Angel shuffled the observer out");
    assert!(
        mill_position < departure,
        "the observer was present for the earlier mill"
    );
    assert_eq!(r.state().objects[&glowing].zone, Zone::Library);
    assert_eq!(
        r.state().objects[&ozolith]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1)
    );
    trace.assert_triggers(alchemist, &cards[..1]);
    trace.assert_zombies(&r, alchemist, 1);
    assert_eq!(
        r.life(p2),
        21,
        "the departed Glowing One observed its nonland mill exactly once"
    );
}

/// CR 603.3b + CR 603.7b: the normal collector skips already-owned moves;
/// a delayed observer still sees every raw occurrence in the same batch.
#[test]
fn delayed_observer_reaches_claimed_combat_mill_moves() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let alchemist = alchemist(&mut s);
    let guildmage = printed(&mut s, P0, "Duskmantle Guildmage", Zone::Battlefield);
    let mana = [
        (ManaColor::Blue, ManaType::Blue),
        (ManaColor::Black, ManaType::Black),
        (ManaColor::Red, ManaType::Red),
    ]
    .into_iter()
    .map(|(color, kind)| {
        let land = s.add_basic_land(P0, color);
        ManaUnit::new(kind, land, false, vec![])
    })
    .collect();
    s.with_mana_pool(P0, mana);
    let cards = library(
        &mut s,
        P1,
        &[
            "Grizzly Bears",
            "Island",
            "Island",
            "Island",
            "Runeclaw Bear",
        ],
    );
    let mut r = s.build();
    assert_library(&r, P1, &cards);
    let ability = r.state().objects[&guildmage]
        .abilities
        .iter()
        .position(|ability| matches!(*ability.effect, Effect::CreateDelayedTrigger { .. }))
        .expect("printed {1}{U}{B} delayed ability");
    r.activate(guildmage, ability).resolve();
    assert_eq!(r.life(P1), 20);
    r.advance_to_combat();
    r.declare_attackers(&[(alchemist, AttackTarget::Player(P1))])
        .expect("Alchemist attacks P1 unblocked");
    let mut trace = Trace::default();
    for _ in 0..24 {
        if matches!(
            r.state().waiting_for,
            WaitingFor::OrderTriggers {
                player: P0,
                triggers: _
            }
        ) {
            break;
        }
        trace.step_combat(&mut r);
    }
    trace.assert_milled(P1, &cards[..4]);
    let ordering = r
        .state()
        .pending_trigger_order
        .as_ref()
        .expect("five triggers joined the first combat ordering batch");
    assert_eq!(ordering.groups.len(), 1);
    let group = &ordering.groups[0];
    assert_eq!(group.controller, P0);
    let mut observed = Vec::new();
    for context in &group.triggers {
        let cause = context
            .pending
            .trigger_event
            .as_ref()
            .or_else(|| context.trigger_events.first());
        let Some(GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Library),
            to: Zone::Graveyard,
            record,
        }) = cause
        else {
            panic!("expected a physical combat mill occurrence: {cause:?}");
        };
        assert_eq!(record.owner, P1);
        observed.push((context.pending.source_id, *object_id));
    }
    observed.sort_unstable();
    assert_eq!(
        observed,
        [
            (alchemist, cards[0]),
            (guildmage, cards[0]),
            (guildmage, cards[1]),
            (guildmage, cards[2]),
            (guildmage, cards[3]),
        ]
    );
    trace.settle(&mut r);
    assert_eq!((r.life(P0), r.life(P1)), (20, 16));
    trace.assert_triggers(alchemist, &cards[..1]);
    trace.assert_triggers(guildmage, &cards[..4]);
    trace.assert_zombies(&r, alchemist, 1);
    assert_final_zones(&r, &cards[..4], &cards[..1]);
}
