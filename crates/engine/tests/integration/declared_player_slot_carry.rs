//! A later implied or anaphoric "that player" continuation after a declared target player
//! names that player (CR 115.1 + CR 608.2c), never a second target slot and never the caster.
//! Three players: caster P0, declared/searched player P1 (P2 in the name-hate rows), bystander.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const P2: PlayerId = PlayerId(2);

const UNMOORED_EGO: &str = "Choose a card name. Search target opponent's graveyard, hand, and library for up to four cards with that name and exile them. That player shuffles, then draws a card for each card exiled from their hand this way.";
const NECROMENTIA: &str = "Choose a card name other than a basic land card name. Search target opponent's graveyard, hand, and library for any number of cards with that name and exile them. That player shuffles, then creates a 2/2 black Zombie creature token for each card exiled from their hand this way.";
const RESTORATIVE_TECHNIQUE: &str = "Target player gains 2 life, then searches their library for a basic land card, puts it onto the battlefield tapped, then shuffles. Put a +1/+1 counter on up to one target creature.";
const FERTILIDS_FAVOR: &str = "Target player searches their library for a basic land card, puts it onto the battlefield tapped, then shuffles. Put two +1/+1 counters on up to one target artifact or creature.";
const OILDEEP_CLAUSE: &str = "look at target player's hand. You may choose a card from it. If you do, that player discards that card, then draws a card.";
const SALT_VAMPIRE_CLAUSE: &str = "look at target opponent's hand. You may choose a nonland card from it. If you do, that player exiles that card, then draws a card.";
const TOURACHS_CANTICLE: &str = "Target opponent reveals their hand. You choose a card from it. That player discards that card, then discards a card at random.";
const MEMORY_WORM_CLAUSE: &str =
    "deals 2 damage to target player. That player discards a card, then draws a card.";
const CAREFUL_CONSIDERATION: &str = "Target player draws four cards, then discards three cards. If you cast this spell during your main phase, instead that player draws four cards, then discards two cards.";
const OB_NIXILIS: &str = "Flying, trample\nWhenever an opponent searches their library, that player sacrifices a creature of their choice and loses 10 life.\nWhenever another creature dies, put a +1/+1 counter on Ob Nixilis.";
const TUTOR: &str =
    "Search your library for a basic land card, put it into your hand, then shuffle.";

#[derive(Default)]
struct Seen {
    events: Vec<GameEvent>,
    first_slot_count: Option<usize>,
    reveal_by: Vec<PlayerId>,
    discard_by: Vec<PlayerId>,
    searches: Vec<(PlayerId, Option<PlayerId>)>,
}

impl Seen {
    fn shuffled(&self) -> Vec<PlayerId> {
        self.events
            .iter()
            .filter_map(|e| match e {
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } => Some(*player_id),
                _ => None,
            })
            .collect()
    }
}

#[derive(Default)]
struct Plan<'a> {
    prefs: &'a [TargetRef],
    name: &'a str,
    eliminate_on_stack: Option<usize>,
}

fn cast(r: &mut GameRunner, id: ObjectId) -> Vec<GameEvent> {
    let card_id = r.state().objects[&id].card_id;
    r.act(GameAction::CastSpell {
        object_id: id,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast")
    .events
}

/// Hand-driven because the name, search, reveal and discard prompts stop `resolve()`.
fn drive(r: &mut GameRunner, plan: &Plan) -> Seen {
    let mut seen = Seen::default();
    let mut eliminate = plan.eliminate_on_stack;
    for _ in 0..80 {
        match r.state().waiting_for.clone() {
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                seen.first_slot_count.get_or_insert(target_slots.len());
                let target = target_slots[selection.current_slot]
                    .legal_targets
                    .iter()
                    .find(|t| plan.prefs.contains(t))
                    .cloned();
                seen.events.extend(
                    r.act(GameAction::ChooseTarget { target })
                        .expect("target")
                        .events,
                );
            }
            WaitingFor::NamedChoice { .. } => {
                seen.events.extend(
                    r.act(GameAction::ChooseOption {
                        choice: plan.name.to_string(),
                    })
                    .expect("name")
                    .events,
                );
            }
            WaitingFor::SearchChoice {
                player,
                library_owner,
                cards,
                ..
            } => {
                seen.searches.push((player, library_owner));
                seen.events.extend(
                    r.act(GameAction::SelectCards { cards })
                        .expect("search")
                        .events,
                );
            }
            WaitingFor::RevealChoice { player, cards, .. } => {
                seen.reveal_by.push(player);
                seen.events.extend(
                    r.act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .expect("reveal")
                    .events,
                );
            }
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } => {
                seen.discard_by.push(player);
                let cards = cards.into_iter().take(count).collect();
                seen.events.extend(
                    r.act(GameAction::SelectCards { cards })
                        .expect("discard")
                        .events,
                );
            }
            WaitingFor::Priority { .. } => {
                if r.state().stack.is_empty() {
                    break;
                }
                if let Some(seat) = eliminate.take() {
                    r.state_mut().players[seat].is_eliminated = true;
                }
                seen.events
                    .extend(r.act(GameAction::PassPriority).expect("pass").events);
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    seen
}

fn hands(r: &GameRunner) -> Vec<usize> {
    r.state().players.iter().map(|p| p.hand.len()).collect()
}

fn libraries(r: &GameRunner) -> Vec<usize> {
    r.state().players.iter().map(|p| p.library.len()).collect()
}

fn graveyards(r: &GameRunner) -> Vec<usize> {
    r.state()
        .players
        .iter()
        .map(|p| p.graveyard.len())
        .collect()
}

fn lives(r: &GameRunner) -> Vec<i32> {
    r.state().players.iter().map(|p| p.life).collect()
}

fn make_basic_land(r: &mut GameRunner, id: ObjectId) {
    let o = r.state_mut().objects.get_mut(&id).unwrap();
    o.card_types.core_types.push(CoreType::Land);
    o.card_types.supertypes.push(Supertype::Basic);
    o.base_card_types = o.card_types.clone();
}

/// Two hand cards and six library cards per seat; hand cards are creatures so "nonland" admits them.
fn seat_all(sc: &mut GameScenario) {
    for p in [P0, P1, P2] {
        for i in 0..2 {
            sc.add_card_to_hand(p, &format!("H{}{i}", p.0));
        }
        for i in 0..6 {
            sc.add_card_to_library_top(p, &format!("L{}{i}", p.0));
        }
    }
}

fn hand_cards_become_creatures(r: &mut GameRunner) {
    let ids: Vec<ObjectId> = r.state().objects.keys().copied().collect();
    for id in ids {
        let o = r.state_mut().objects.get_mut(&id).unwrap();
        if o.zone == Zone::Hand && o.card_types.core_types.is_empty() {
            o.card_types.core_types.push(CoreType::Creature);
            o.base_card_types = o.card_types.clone();
        }
    }
}

fn three_player(seed: u64) -> GameScenario {
    let mut sc = GameScenario::new_n_player(3, seed);
    sc.at_phase(Phase::PreCombatMain);
    sc
}

/// R1. One announced target; the searched opponent (P2) exiles its Filler and draws.
#[test]
fn unmoored_ego_announces_one_target_and_the_searched_opponent_draws() {
    let mut sc = three_player(7);
    let ego = sc
        .add_spell_to_hand_from_oracle(P0, "Unmoored Ego", false, UNMOORED_EGO)
        .id();
    let filler = sc.add_card_to_hand(P2, "Filler");
    for _ in 0..3 {
        sc.add_card_to_library_top(P2, "Deep");
        sc.add_card_to_library_top(P1, "Other");
        sc.add_card_to_library_top(P0, "Mine");
    }
    let mut r = sc.build();
    r.state_mut().all_card_names = std::sync::Arc::from(["Filler".to_string()]);
    let (hands_before, libs_before) = (hands(&r), libraries(&r));
    cast(&mut r, ego);
    let seen = drive(
        &mut r,
        &Plan {
            prefs: &[TargetRef::Player(P2)],
            name: "Filler",
            ..Default::default()
        },
    );
    assert_eq!(seen.first_slot_count, Some(1), "one declared target slot");
    assert_eq!(r.state().objects[&filler].zone, Zone::Exile);
    assert_eq!(
        hands(&r),
        vec![hands_before[0] - 1, hands_before[1], hands_before[2]],
        "caster spent the spell; P2 exiled Filler and drew one; P1 untouched"
    );
    assert_eq!(
        libraries(&r),
        vec![libs_before[0], libs_before[1], libs_before[2] - 1]
    );
}

/// R6. The tokens belong to the searched opponent.
#[test]
fn necromentia_zombies_go_to_the_searched_opponent() {
    let mut sc = three_player(7);
    let card = sc
        .add_spell_to_hand_from_oracle(P0, "Necromentia", false, NECROMENTIA)
        .id();
    sc.add_card_to_hand(P2, "Filler");
    sc.add_card_to_hand(P2, "Filler");
    for _ in 0..3 {
        sc.add_card_to_library_top(P2, "Deep");
    }
    let mut r = sc.build();
    r.state_mut().all_card_names = std::sync::Arc::from(["Filler".to_string()]);
    cast(&mut r, card);
    drive(
        &mut r,
        &Plan {
            prefs: &[TargetRef::Player(P2)],
            name: "Filler",
            ..Default::default()
        },
    );
    let zombies = |p: PlayerId| {
        r.state()
            .objects
            .values()
            .filter(|o| o.is_token && o.zone == Zone::Battlefield && o.controller == p)
            .count()
    };
    assert_eq!(zombies(P2), 2, "one Zombie per card exiled from P2's hand");
    assert_eq!(zombies(P0), 0);
    assert_eq!(zombies(P1), 0);
}

/// R2. The declared player (P1) gains the life, searches and shuffles its own library; the
/// creature target belongs to P2.
#[test]
fn restorative_technique_declared_player_searches_its_own_library() {
    let mut sc = three_player(7);
    let card = sc
        .add_spell_to_hand_from_oracle(P0, "Restorative Technique", false, RESTORATIVE_TECHNIQUE)
        .id();
    let bear = sc.add_creature(P2, "Bear", 2, 2).id();
    let f1 = sc.add_card_to_library_top(P1, "Forest1");
    let f2 = sc.add_card_to_library_top(P2, "Forest2");
    let f0 = sc.add_card_to_library_top(P0, "Forest0");
    let mut r = sc.build();
    for id in [f0, f1, f2] {
        make_basic_land(&mut r, id);
    }
    cast(&mut r, card);
    let seen = drive(
        &mut r,
        &Plan {
            prefs: &[TargetRef::Player(P1), TargetRef::Object(bear)],
            ..Default::default()
        },
    );
    assert_eq!(seen.searches, vec![(P1, Some(P1))]);
    assert_eq!(r.state().objects[&f1].zone, Zone::Battlefield);
    assert_eq!(r.state().objects[&f1].controller, P1);
    assert_eq!(r.state().objects[&f0].zone, Zone::Library);
    assert_eq!(r.state().objects[&f2].zone, Zone::Library);
    assert_eq!(seen.shuffled(), vec![P1], "only P1's library is shuffled");
    assert_eq!(lives(&r), vec![20, 22, 20]);
}

/// R3. The searched player leaves the game while the spell is on the stack; the creature target
/// stays legal, so the spell resolves but "that player shuffles" affects no one (CR 608.2b).
#[test]
fn fertilids_favor_shuffle_is_dropped_when_the_declared_player_is_gone() {
    let mut sc = three_player(7);
    let card = sc
        .add_spell_to_hand_from_oracle(P0, "Fertilid's Favor", false, FERTILIDS_FAVOR)
        .id();
    let bear = sc.add_creature(P2, "Bear", 2, 2).id();
    let f1 = sc.add_card_to_library_top(P1, "Forest1");
    let f0 = sc.add_card_to_library_top(P0, "Forest0");
    let mut r = sc.build();
    for id in [f0, f1] {
        make_basic_land(&mut r, id);
    }
    cast(&mut r, card);
    let seen = drive(
        &mut r,
        &Plan {
            prefs: &[TargetRef::Player(P1), TargetRef::Object(bear)],
            eliminate_on_stack: Some(1),
            ..Default::default()
        },
    );
    assert!(!seen.searches.is_empty(), "the search prompt was reached");
    assert_eq!(
        r.state().objects[&bear]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&2),
        "the chain ran to its last clause"
    );
    assert!(
        seen.shuffled().is_empty(),
        "the Shuffle names an illegal slot"
    );
}

struct Shape {
    label: &'static str,
    oracle: String,
    creature: bool,
}

fn run_shape(shape: &Shape) -> (GameRunner, Seen, [Vec<usize>; 3]) {
    let mut sc = three_player(7);
    let id = if shape.creature {
        sc.add_creature_to_hand_from_oracle(P0, shape.label, 2, 2, &shape.oracle)
            .id()
    } else {
        sc.add_spell_to_hand_from_oracle(P0, shape.label, false, &shape.oracle)
            .id()
    };
    seat_all(&mut sc);
    let mut r = sc.build();
    hand_cards_become_creatures(&mut r);
    cast(&mut r, id);
    let seen = drive(
        &mut r,
        &Plan {
            prefs: &[TargetRef::Player(P1)],
            ..Default::default()
        },
    );
    let counts = [hands(&r), libraries(&r), graveyards(&r)];
    (r, seen, counts)
}

/// R4. Reflexive-choice continuations: the node's own targets are the chosen card, so the
/// continuation must name the declared player's slot. Counts are [P0, P1, P2].
#[test]
fn reflexive_choice_continuations_act_on_the_declared_player() {
    let etb = format!("When this creature enters, {OILDEEP_CLAUSE}");
    let cases = [
        // (shape, hands, libraries, graveyards)
        (
            Shape {
                label: "Oildeep spell wording",
                oracle: capitalize(OILDEEP_CLAUSE),
                creature: false,
            },
            [2, 2, 2],
            [6, 5, 6],
            [1, 1, 0],
        ),
        (
            Shape {
                label: "Oildeep Gearhulk enters",
                oracle: etb,
                creature: true,
            },
            [2, 2, 2],
            [6, 5, 6],
            [0, 1, 0],
        ),
        (
            Shape {
                label: "Salt Vampire wording",
                oracle: capitalize(SALT_VAMPIRE_CLAUSE),
                creature: false,
            },
            [2, 2, 2],
            [6, 5, 6],
            [1, 0, 0],
        ),
        (
            Shape {
                label: "Tourach's Canticle",
                oracle: TOURACHS_CANTICLE.to_string(),
                creature: false,
            },
            [2, 0, 2],
            [6, 6, 6],
            [1, 2, 0],
        ),
    ];
    for (shape, hands_after, libs_after, gys_after) in cases {
        let (_, seen, [h, l, g]) = run_shape(&shape);
        assert_eq!(
            seen.reveal_by,
            vec![P0],
            "{}: reveal prompt reached",
            shape.label
        );
        assert_eq!(h, hands_after, "{}: hands", shape.label);
        assert_eq!(l, libs_after, "{}: libraries", shape.label);
        assert_eq!(g, gys_after, "{}: graveyards", shape.label);
    }
}

/// R4 controls: no reflexive choice sits between the declaration and the continuation, so
/// `ParentTarget` resolved correctly before too; the slot form must keep it right.
#[test]
fn declared_player_continuation_without_a_reflexive_choice_acts_on_the_declared_player() {
    let worm = Shape {
        label: "Memory Worm wording",
        oracle: format!("This spell {MEMORY_WORM_CLAUSE}"),
        creature: false,
    };
    let (r, seen, [h, l, _]) = run_shape(&worm);
    assert_eq!(seen.discard_by, vec![P1], "discard prompt reached");
    assert_eq!(h, vec![2, 2, 2]);
    assert_eq!(l, vec![6, 5, 6]);
    assert_eq!(lives(&r), vec![20, 18, 20]);

    let careful = Shape {
        label: "Careful Consideration",
        oracle: CAREFUL_CONSIDERATION.to_string(),
        creature: false,
    };
    let (_, seen, [h, l, g]) = run_shape(&careful);
    assert_eq!(seen.discard_by, vec![P1], "discard prompt reached");
    assert_eq!(h, vec![2, 4, 2]);
    assert_eq!(l, vec![6, 2, 6]);
    assert_eq!(g, vec![1, 2, 0]);
}

/// R5. The opponent that searched (P0), not the trigger's controller (P1), loses the life.
#[test]
fn ob_nixilis_drains_the_searching_opponent_not_its_controller() {
    let mut sc = three_player(7);
    sc.add_creature_from_oracle(P1, "Ob Nixilis, Unshackled", 4, 3, OB_NIXILIS);
    let bear = sc.add_creature(P0, "Bear", 2, 2).id();
    let tutor = sc
        .add_spell_to_hand_from_oracle(P0, "Tutor", false, TUTOR)
        .id();
    let forest = sc.add_card_to_library_top(P0, "Forest");
    let mut r = sc.build();
    make_basic_land(&mut r, forest);
    cast(&mut r, tutor);
    let seen = drive(&mut r, &Plan::default());
    assert_eq!(seen.searches.len(), 1, "the search prompt was reached");
    assert_eq!(r.state().objects[&bear].zone, Zone::Graveyard);
    assert_eq!(lives(&r), vec![10, 20, 20]);
}

fn capitalize(clause: &str) -> String {
    let mut chars = clause.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}
