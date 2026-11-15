//! A later implied or anaphoric "that player" continuation after a declared target player
//! names that player (CR 115.1 + CR 608.2c), never a second target slot and never the caster.
//! Three players: caster P0, declared/searched player P1 (P2 in the name-hate rows), bystander.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ChosenGroupId, Effect,
    QuantityExpr, SubAbilityLink, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost};
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
    optional_by: Vec<PlayerId>,
    zone_offers: Vec<(PlayerId, Zone, Vec<ObjectId>)>,
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

/// Picks one slot's target from its legal set; `last` is whether it is the final slot.
type Picker = fn(&[TargetRef], bool) -> Option<TargetRef>;

#[derive(Default)]
struct Plan<'a> {
    prefs: &'a [TargetRef],
    /// Slot `i` takes `slots[i]` (absent: none chosen); overrides `prefs`.
    slots: &'a [Option<TargetRef>],
    /// Overrides `prefs` and `slots` for every slot.
    picker: Option<Picker>,
    name: &'a str,
    eliminate_on_stack: Option<usize>,
    decline_optional: bool,
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
                let legal = &target_slots[selection.current_slot].legal_targets;
                let last = selection.current_slot + 1 == target_slots.len();
                let target = if let Some(pick) = plan.picker {
                    pick(legal, last)
                } else if plan.slots.is_empty() {
                    legal.iter().find(|t| plan.prefs.contains(t)).cloned()
                } else {
                    plan.slots
                        .get(selection.current_slot)
                        .cloned()
                        .flatten()
                        .filter(|t| legal.contains(t))
                };
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
            WaitingFor::OptionalEffectChoice { player, .. } => {
                seen.optional_by.push(player);
                seen.events.extend(
                    r.act(GameAction::DecideOptionalEffect {
                        accept: !plan.decline_optional,
                    })
                    .expect("decide")
                    .events,
                );
            }
            WaitingFor::EffectZoneChoice {
                player,
                zone,
                cards,
                count,
                ..
            } => {
                seen.zone_offers.push((player, zone, cards.clone()));
                let cards = cards.into_iter().take(count).collect();
                seen.events.extend(
                    r.act(GameAction::SelectCards { cards })
                        .expect("zone choice")
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
    assert!(
        seen.searches.is_empty(),
        "the Search names an illegal slot and opens no prompt"
    );
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

const GAIN_DISCARD_DRAW: &str =
    "Target player gains 2 life. That player discards a card, then draws a card.";
const TWO_DECLARATIONS: &str =
    "Target player gains 2 life. Target opponent loses 2 life. That player discards a card.";
const GAIN_DISCARD_DRAW_THEN_COUNTER: &str = "Target player gains 2 life. That player discards a card, then draws a card. Put a +1/+1 counter on up to one target creature.";
const OBJECT_FIRST_SEARCH: &str = "Destroy target creature. Target player gains 2 life. That player searches their library for a basic land card, puts it onto the battlefield tapped, then shuffles.";
const DRAW_THEN_OPPONENT_LOSES: &str = "Target player draws a card. Target opponent loses 2 life.";
const DRAW_THEN_UP_TO_OPPONENT_LOSES: &str =
    "Target player draws a card. Up to one target opponent loses 2 life.";

/// A spell in P0's hand, a P0 creature `C0`, and two hand and six library cards per seat.
fn setup_row(text: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut sc = three_player(7);
    let creature = sc.add_creature(P0, "C0", 3, 9).id();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Row", false, text)
        .id();
    seat_all(&mut sc);
    (sc.build(), spell, creature)
}

/// Casts `text` and drives it with `plan`.
fn run_row(text: &str, plan: &Plan) -> (GameRunner, Seen, ObjectId) {
    let (mut r, spell, creature) = setup_row(text);
    cast(&mut r, spell);
    let seen = drive(&mut r, plan);
    (r, seen, creature)
}

/// Player-only slots take P2, except the last slot, which takes P1 (the declaring clause's
/// player in every text that uses this picker); object slots take the first object.
fn declared_player_last(legal: &[TargetRef], last: bool) -> Option<TargetRef> {
    if legal.iter().all(|t| matches!(t, TargetRef::Player(_))) {
        Some(TargetRef::Player(if last { P1 } else { P2 }))
    } else {
        legal
            .iter()
            .find(|t| matches!(t, TargetRef::Object(_)))
            .cloned()
    }
}

/// CR 608.2c + CR 115.1: "that player" names the clause that announced the player, however many
/// engine target slots (players or objects) the chain announces before or inside it.
#[test]
fn that_player_after_earlier_target_slots_acts_on_the_declared_player() {
    let cases = [
        (GAIN_DISCARD_DRAW.to_string(), 0),
        (
            format!("You draw cards equal to the number of cards in target opponent's hand. {GAIN_DISCARD_DRAW}"),
            2,
        ),
        (
            format!("Exchange control of two target creatures. {GAIN_DISCARD_DRAW}"),
            0,
        ),
        (
            format!("Destroy all creatures target player controls. {GAIN_DISCARD_DRAW}"),
            0,
        ),
        (
            "Target player loses life equal to target creature's power. That player discards a card, then draws a card.".to_string(),
            0,
        ),
    ];
    for (text, caster_draws) in cases {
        let (r, seen, _) = run_row(
            &text,
            &Plan {
                picker: Some(declared_player_last),
                ..Default::default()
            },
        );
        assert_eq!(
            seen.discard_by,
            vec![P1],
            "{text}: the discard prompt reached P1"
        );
        assert_eq!(
            hands(&r),
            vec![2 + caster_draws, 2, 2],
            "{text}: P1's discard and draw net out; no other seat draws"
        );
        assert_eq!(
            libraries(&r),
            vec![6 - caster_draws, 5, 6],
            "{text}: only P1 drew its card"
        );
    }
}

/// CR 608.2c: with two declaring clauses, "that player" names the nearer one.
#[test]
fn that_player_names_the_nearest_declaring_clause() {
    let (r, seen, _) = run_row(
        TWO_DECLARATIONS,
        &Plan {
            slots: &[Some(TargetRef::Player(P2)), Some(TargetRef::Player(P1))],
            ..Default::default()
        },
    );
    assert_eq!(lives(&r), vec![20, 18, 22], "P2 gained, P1 lost");
    assert_eq!(
        seen.discard_by,
        vec![P1],
        "the nearer declaration, target opponent"
    );
}

/// CR 608.2b: the declared player is gone, so "that player" affects no one; the creature target
/// stays legal, so the chain still resolves its last clause.
#[test]
fn an_illegal_declared_player_affects_no_one_while_the_rest_of_the_chain_resolves() {
    let (mut r, spell, creature) = setup_row(GAIN_DISCARD_DRAW_THEN_COUNTER);
    cast(&mut r, spell);
    let seen = drive(
        &mut r,
        &Plan {
            prefs: &[TargetRef::Player(P1), TargetRef::Object(creature)],
            eliminate_on_stack: Some(1),
            ..Default::default()
        },
    );
    assert_eq!(
        r.state().objects[&creature]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1),
        "the chain ran to its last clause"
    );
    assert!(seen.discard_by.is_empty(), "no one discards");
    assert_eq!(hands(&r), vec![2, 2, 2]);
    assert_eq!(libraries(&r), vec![6, 6, 6], "no one draws");
}

/// CR 608.2c: an anaphoric search after an object target and a declared player searches the
/// declared player's library.
#[test]
fn an_anaphoric_search_after_an_object_target_searches_the_declared_player() {
    let mut sc = three_player(7);
    let bear = sc.add_creature(P2, "Bear", 2, 2).id();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Row", false, OBJECT_FIRST_SEARCH)
        .id();
    let forests = [P0, P1, P2].map(|p| sc.add_card_to_library_top(p, "Forest"));
    let mut r = sc.build();
    for id in forests {
        make_basic_land(&mut r, id);
    }
    cast(&mut r, spell);
    let seen = drive(
        &mut r,
        &Plan {
            prefs: &[TargetRef::Object(bear), TargetRef::Player(P1)],
            ..Default::default()
        },
    );
    assert_eq!(seen.searches, vec![(P1, Some(P1))]);
    assert_eq!(r.state().objects[&forests[1]].zone, Zone::Battlefield);
    assert_eq!(seen.shuffled(), vec![P1], "only P1's library is shuffled");
    assert_eq!(lives(&r), vec![20, 22, 20]);
}

/// CR 608.2b: the declaring clause itself affects no one when its player is illegal; the first
/// declaration's draw lands.
#[test]
fn a_second_declared_player_that_is_gone_affects_no_one() {
    let plan = |eliminate| Plan {
        slots: &[Some(TargetRef::Player(P2)), Some(TargetRef::Player(P1))],
        eliminate_on_stack: eliminate,
        ..Default::default()
    };
    let (r, _, _) = run_row(DRAW_THEN_OPPONENT_LOSES, &plan(None));
    assert_eq!(hands(&r)[2], 3, "the first player drew");
    assert_eq!(lives(&r), vec![20, 18, 20], "the declared opponent lost");
    let (r, _, _) = run_row(DRAW_THEN_OPPONENT_LOSES, &plan(Some(1)));
    assert_eq!(hands(&r)[2], 3, "the first player's draw landed");
    assert_eq!(lives(&r), vec![20, 20, 20], "no one lost life");
}

/// CR 115.6: "up to one target opponent" with none chosen declares no player; the continuation
/// must not fall onto the first declaration's player.
#[test]
fn an_unchosen_up_to_declaration_affects_no_one() {
    let plan = |slots| Plan {
        slots,
        ..Default::default()
    };
    let (r, _, _) = run_row(
        DRAW_THEN_UP_TO_OPPONENT_LOSES,
        &plan(&[Some(TargetRef::Player(P2))]),
    );
    assert_eq!(hands(&r)[2], 3, "the first player drew");
    assert_eq!(lives(&r), vec![20, 20, 20], "no one lost life");
    let (r, _, _) = run_row(
        DRAW_THEN_UP_TO_OPPONENT_LOSES,
        &plan(&[Some(TargetRef::Player(P2)), Some(TargetRef::Player(P1))]),
    );
    assert_eq!(
        lives(&r),
        vec![20, 18, 20],
        "chosen: the declared opponent lost"
    );
}

const MAY_DRAW_THEN_COUNTER: &str = "Target player gains 2 life. That player may draw a card. Put a +1/+1 counter on up to one target creature.";

/// CR 608.2d + CR 608.2b: "that player may ..." is offered to the declared player, and to no one
/// when that player is gone (the creature target keeps the spell resolving).
#[test]
fn an_optional_that_player_instruction_is_offered_to_the_declared_player() {
    let drive_with = |eliminate, decline_optional| {
        let (mut r, spell, creature) = setup_row(MAY_DRAW_THEN_COUNTER);
        cast(&mut r, spell);
        let seen = drive(
            &mut r,
            &Plan {
                prefs: &[TargetRef::Player(P1), TargetRef::Object(creature)],
                eliminate_on_stack: eliminate,
                decline_optional,
                ..Default::default()
            },
        );
        let counters = r.state().objects[&creature]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied();
        (hands(&r), counters, seen.optional_by)
    };
    let (hands_after, counters, offered_to) = drive_with(None, false);
    assert_eq!(
        offered_to,
        vec![P1],
        "the declared player is offered the draw"
    );
    assert_eq!(hands_after, vec![2, 3, 2], "accepted: P1 drew");
    assert_eq!(counters, Some(1));
    let (hands_after, _, offered_to) = drive_with(None, true);
    assert_eq!(offered_to, vec![P1]);
    assert_eq!(hands_after, vec![2, 2, 2], "declined: no one drew");
    let (hands_after, counters, offered_to) = drive_with(Some(1), false);
    assert_eq!(
        counters,
        Some(1),
        "the spell still resolved its last clause"
    );
    assert!(offered_to.is_empty(), "no one is offered the draw");
    assert_eq!(hands_after, vec![2, 2, 2]);
}

const GATE_THEN_TWO_DECLARATIONS: &str = "You may discard a card. If you do, you gain 5 life. Target player gains 2 life. Target opponent gains 2 life.";
const DECLARE_THEN_CHOOSE_OPPONENT: &str = "Target player draws a card. Choose target opponent who has more life than you do as you cast this spell. This spell deals 2 damage to that player.";

/// CR 118.12 + CR 608.2c: the "if you do" rider governs only its own sentence, so declining the
/// discard still leaves both later declarations in force.
#[test]
fn declining_an_if_you_do_gate_keeps_the_later_declaring_clauses() {
    let (mut r, spell, _) = setup_row(GATE_THEN_TWO_DECLARATIONS);
    cast(&mut r, spell);
    let seen = drive(
        &mut r,
        &Plan {
            slots: &[Some(TargetRef::Player(P2)), Some(TargetRef::Player(P1))],
            decline_optional: true,
            ..Default::default()
        },
    );
    assert_eq!(seen.optional_by, vec![P0], "the decline path was taken");
    assert_eq!(lives(&r), vec![20, 22, 22], "both declared players gained");
}

/// A declaring clause that already owns a chosen-clause group keeps `ParentTarget` readers; its
/// reader acts on the chosen opponent and, once that opponent is gone, on no one.
#[test]
fn a_chosen_clause_declaration_affects_no_one_once_its_player_is_gone() {
    let run_with = |eliminate| {
        let (mut r, spell, _) = setup_row(DECLARE_THEN_CHOOSE_OPPONENT);
        r.state_mut().players[1].life = 25;
        cast(&mut r, spell);
        drive(
            &mut r,
            &Plan {
                slots: &[Some(TargetRef::Player(P2)), Some(TargetRef::Player(P1))],
                eliminate_on_stack: eliminate,
                ..Default::default()
            },
        );
        (hands(&r)[2], lives(&r))
    };
    assert_eq!(
        run_with(None),
        (3, vec![20, 23, 20]),
        "the chosen opponent took 2"
    );
    assert_eq!(
        run_with(Some(1)),
        (3, vec![20, 25, 20]),
        "the first player's draw landed; no one took damage"
    );
}

const MAY_PAY_UP_TO_THREE: &str = "Choose target player. That player may pay {1} up to three times. When you do, you draw a card.";

/// CR 608.2d + CR 603.12a: every "may pay" offer of a repeated optional payment goes to the named
/// player, who also pays and may stop at any offer (reach guard: the accepted payments happen).
#[test]
fn each_repeated_payment_offer_goes_to_the_player_who_may_pay() {
    let run_with = |decline_optional| {
        let mut sc = three_player(7);
        let spell = sc
            .add_spell_to_hand_from_oracle(P0, "Row", false, MAY_PAY_UP_TO_THREE)
            .id();
        seat_all(&mut sc);
        let lands: Vec<ObjectId> = (0..3)
            .map(|_| sc.add_basic_land(P1, ManaColor::Green))
            .collect();
        let mut r = sc.build();
        cast(&mut r, spell);
        let seen = drive(
            &mut r,
            &Plan {
                prefs: &[TargetRef::Player(P1)],
                decline_optional,
                ..Default::default()
            },
        );
        let tapped = lands
            .iter()
            .filter(|id| r.state().objects[id].tapped)
            .count();
        (seen.optional_by, tapped)
    };
    assert_eq!(
        run_with(false),
        (vec![P1, P1, P1], 3),
        "three offers, three payments by P1"
    );
    assert_eq!(run_with(true), (vec![P1], 0), "P1 declined the first offer");
}

const UNDERCITY_CLAUSE: &str = "Target opponent discards a card. Then they may discard an additional card. Put a +1/+1 counter on up to one target creature.";
const GAIN_THEY_DRAW: &str =
    "Target player gains 2 life. They draw a card. Put a +1/+1 counter on up to one target creature.";

fn counters_on(r: &GameRunner, id: ObjectId) -> Option<u32> {
    r.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
}

/// CR 608.2c + CR 608.2d: a bare "they" after a declared player is that player, who is also the one
/// offered the "may"; once that player is gone the instruction reaches no one (CR 608.2b).
#[test]
fn a_they_instruction_after_a_declared_player_belongs_to_that_player() {
    let run_with = |eliminate| {
        let (mut r, spell, creature) = setup_row(UNDERCITY_CLAUSE);
        cast(&mut r, spell);
        let seen = drive(
            &mut r,
            &Plan {
                prefs: &[TargetRef::Player(P1), TargetRef::Object(creature)],
                eliminate_on_stack: eliminate,
                ..Default::default()
            },
        );
        (seen, hands(&r), counters_on(&r, creature))
    };
    let (seen, hands_after, counters) = run_with(None);
    assert_eq!(
        seen.optional_by,
        vec![P1],
        "the declared player is offered the may"
    );
    assert_eq!(seen.discard_by, vec![P1]);
    assert_eq!(hands_after, vec![2, 0, 2]);
    assert_eq!(counters, Some(1));
    let (seen, hands_after, counters) = run_with(Some(1));
    assert!(seen.optional_by.is_empty(), "no one is offered the may");
    assert!(seen.discard_by.is_empty());
    assert_eq!(hands_after, vec![2, 2, 2]);
    assert_eq!(counters, Some(1), "the rest of the spell resolved");

    let run_draw = |eliminate| {
        let (mut r, spell, creature) = setup_row(GAIN_THEY_DRAW);
        cast(&mut r, spell);
        drive(
            &mut r,
            &Plan {
                prefs: &[TargetRef::Player(P1), TargetRef::Object(creature)],
                eliminate_on_stack: eliminate,
                ..Default::default()
            },
        );
        (hands(&r), counters_on(&r, creature))
    };
    assert_eq!(
        run_draw(None),
        (vec![2, 3, 2], Some(1)),
        "the declared player drew"
    );
    assert_eq!(run_draw(Some(1)), (vec![2, 2, 2], Some(1)), "no one drew");
}

const CHOOSE_THEN_THEY_MAY: &str =
    "Choose target player. They may discard up to two cards. Then they draw a card for each card discarded this way.";
const DECLARE_CHOOSE_THEN_THEY_MAY: &str = "Target player draws a card. Choose target opponent who has more life than you do as you cast this spell. They may discard a card.";

/// CR 608.2d: a reader of a chosen-clause declaration keeps `ParentTarget` in its effect slot, but
/// its "may" is offered to the chosen player.
#[test]
fn a_they_may_after_a_chosen_clause_declaration_is_offered_to_the_chosen_player() {
    let (mut r, spell, _) = setup_row(CHOOSE_THEN_THEY_MAY);
    cast(&mut r, spell);
    let seen = drive(
        &mut r,
        &Plan {
            prefs: &[TargetRef::Player(P1)],
            ..Default::default()
        },
    );
    assert_eq!(seen.optional_by, vec![P1]);
    assert_eq!(seen.discard_by, vec![P1]);
    assert_eq!(hands(&r), vec![2, 2, 2], "P1 discarded two and drew two");

    let (r, seen, _) = run_row(
        CHOOSE_THEN_THEY_MAY,
        &Plan {
            prefs: &[TargetRef::Player(P1)],
            eliminate_on_stack: Some(1),
            ..Default::default()
        },
    );
    assert!(
        seen.optional_by.is_empty(),
        "the chosen player is gone: no prompt"
    );
    assert!(seen.discard_by.is_empty());
    assert_eq!(hands(&r), vec![2, 2, 2]);

    let (mut r, spell, _) = setup_row(DECLARE_CHOOSE_THEN_THEY_MAY);
    r.state_mut().players[1].life = 25;
    cast(&mut r, spell);
    let seen = drive(
        &mut r,
        &Plan {
            slots: &[Some(TargetRef::Player(P2)), Some(TargetRef::Player(P1))],
            ..Default::default()
        },
    );
    assert_eq!(
        seen.optional_by,
        vec![P1],
        "the chosen opponent, not the declared player"
    );
    assert_eq!(seen.discard_by, vec![P1]);
    assert_eq!(hands(&r), vec![2, 1, 3]);
}

/// CR 608.2b + CR 603.12a: a repeated "may pay" addressed to a declared player who is gone is offered
/// to no one, so no payment is made. Hand-built because a text with a lone player target fizzles
/// before the driver is reached.
#[test]
fn a_repeated_payment_addressed_to_a_gone_declared_player_is_offered_to_no_one() {
    let declared = TargetFilter::DeclaredPlayer {
        group: ChosenGroupId::declared_player(0),
    };
    let def = |stamp_optional_player: bool| {
        let mut reflexive = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        reflexive.condition = Some(AbilityCondition::WhenYouDo);
        let mut pay = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PayCost {
                cost: AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                scale: None,
                payer: declared.clone(),
            },
        );
        pay.optional = true;
        pay.optional_player = stamp_optional_player.then(|| declared.clone());
        pay.repeat_for = Some(QuantityExpr::Fixed { value: 3 });
        let mut pay = pay.sub_ability(reflexive);
        pay.sub_link = SubAbilityLink::SequentialSibling;
        let mut creature_pick = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            },
        );
        creature_pick.sub_link = SubAbilityLink::SequentialSibling;
        let creature_pick = creature_pick.sub_ability(pay);
        let mut pick = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Player,
            },
        );
        pick.declares_chosen_group = Some(ChosenGroupId::declared_player(0));
        pick.sub_link = SubAbilityLink::SequentialSibling;
        pick.sub_ability(creature_pick)
    };
    let run_with = |eliminate, stamp_optional_player| {
        let mut sc = three_player(7);
        let creature = sc.add_creature(P0, "C0", 3, 9).id();
        let spell = sc
            .add_spell_to_hand(P0, "Row", false)
            .with_ability_definition(def(stamp_optional_player))
            .id();
        seat_all(&mut sc);
        let lands: Vec<ObjectId> = [P0, P0, P0, P1, P1, P1]
            .into_iter()
            .map(|p| sc.add_basic_land(p, ManaColor::Green))
            .collect();
        let mut r = sc.build();
        cast(&mut r, spell);
        let seen = drive(
            &mut r,
            &Plan {
                prefs: &[TargetRef::Player(P1), TargetRef::Object(creature)],
                eliminate_on_stack: eliminate,
                ..Default::default()
            },
        );
        let tapped_by = |owner| {
            lands
                .iter()
                .filter(|id| r.state().objects[id].owner == owner && r.state().objects[id].tapped)
                .count()
        };
        (seen.optional_by, [tapped_by(P0), tapped_by(P1)])
    };
    // The payer alone names the declared player when the "may" subject is not stamped.
    for stamp in [true, false] {
        assert_eq!(
            run_with(None, stamp),
            (vec![P1, P1, P1], [0, 3]),
            "reach (stamped optional_player: {stamp}): the declared player is offered three payments and makes them"
        );
        assert_eq!(
            run_with(Some(1), stamp),
            (Vec::<PlayerId>::new(), [0, 0]),
            "gone (stamped optional_player: {stamp}): no offer to anyone, no payment by anyone"
        );
    }
}

const EXILE_THEN_THAT_PLAYER_EXILES: &str = "Target opponent exiles a nontoken creature they control. That player exiles a nonland card from their graveyard.";
const AZULA_CLAUSE: &str = "Target opponent exiles a nontoken creature they control, then they exile a nonland card from their graveyard.";

/// CR 108.4a + CR 109.5: "their graveyard" is owner-scoped, so a card P1 owns is offered to P1
/// even though P0 controlled it when it died (the LKI at-exit controller is P0).
fn graveyard_offer_after_steal(text: &str) -> Vec<(PlayerId, Zone, Vec<ObjectId>)> {
    let mut sc = three_player(7);
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Row", false, text)
        .id();
    sc.add_creature(P1, "B1", 2, 2);
    sc.add_creature(P1, "B2", 2, 2);
    let stolen = sc.add_creature(P1, "Stolen", 2, 2).id();
    let plain = sc.add_creature_to_graveyard(P1, "Plain", 1, 1).id();
    let p0_card = sc.add_creature_to_graveyard(P0, "Mine", 1, 1).id();
    let p2_card = sc.add_creature_to_graveyard(P2, "Theirs", 1, 1).id();
    let mut r = sc.build();
    {
        let o = r.state_mut().objects.get_mut(&stolen).unwrap();
        o.controller = P0;
        o.base_controller = Some(P0);
    }
    let mut events = vec![];
    engine::game::zones::move_to_zone(r.state_mut(), stolen, Zone::Graveyard, &mut events);
    assert_eq!(
        r.state().lki_cache[&stolen].controller,
        P0,
        "reach: the at-exit controller is the thief"
    );
    assert_eq!(r.state().objects[&stolen].owner, P1);
    cast(&mut r, spell);
    let seen = drive(
        &mut r,
        &Plan {
            prefs: &[TargetRef::Player(P1)],
            ..Default::default()
        },
    );
    let offers: Vec<_> = seen
        .zone_offers
        .into_iter()
        .filter(|(_, zone, _)| *zone == Zone::Graveyard)
        .collect();
    assert_eq!(
        offers.len(),
        1,
        "both P1-owned cards are offered (a lone candidate is taken without a prompt)"
    );
    let (_, _, cards) = &offers[0];
    assert!(
        !cards.contains(&p0_card) && !cards.contains(&p2_card),
        "other players' graveyard cards are never offered"
    );
    assert!(
        cards.contains(&plain) && cards.contains(&stolen),
        "both of P1's cards are offered"
    );
    assert!(
        [p0_card, p2_card]
            .iter()
            .all(|id| r.state().objects[id].zone == Zone::Graveyard),
        "P0's and P2's graveyard cards are untouched"
    );
    offers
}

/// CR 108.4a: a card that died under another player's control is still its owner's card in the
/// owner's graveyard, for a "that player ... from their graveyard" instruction.
#[test]
fn that_player_graveyard_offer_includes_a_card_that_died_under_a_thief() {
    let offers = graveyard_offer_after_steal(EXILE_THEN_THAT_PLAYER_EXILES);
    assert_eq!(offers[0].0, P1, "the declared player chooses");
}

/// Azula, Cunning Usurper's enters clause, same state.
#[test]
fn azula_graveyard_offer_includes_a_card_that_died_under_a_thief() {
    let offers = graveyard_offer_after_steal(AZULA_CLAUSE);
    assert_eq!(offers[0].0, P1, "the declared player chooses");
}

const BREAK_THE_SPELL: &str = "Destroy target enchantment. If a permanent you controlled or a token was destroyed this way, draw a card.";

/// CR 608.2h: "a permanent you controlled ... destroyed this way" reads the at-exit controller,
/// so P0 controlling P1's enchantment draws even though the card now sits in its owner's graveyard.
#[test]
fn destroyed_this_way_reads_the_at_exit_controller_of_a_stolen_permanent() {
    let mut sc = three_player(7);
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Row", false, BREAK_THE_SPELL)
        .id();
    let ench = sc.add_enchantment_from_oracle(P1, "Aura-ish", "").id();
    seat_all(&mut sc);
    let mut r = sc.build();
    {
        let o = r.state_mut().objects.get_mut(&ench).unwrap();
        o.controller = P0;
        o.base_controller = Some(P0);
    }
    let (hand, lib) = (hands(&r)[0], libraries(&r)[0]);
    cast(&mut r, spell);
    drive(
        &mut r,
        &Plan {
            prefs: &[TargetRef::Object(ench)],
            ..Default::default()
        },
    );
    assert_eq!(
        r.state().objects[&ench].zone,
        Zone::Graveyard,
        "reach: the enchantment was destroyed"
    );
    assert_eq!(
        (hands(&r)[0], libraries(&r)[0]),
        (hand, lib - 1),
        "P0 controlled it when destroyed: exactly one card drawn (spell leaves hand)"
    );
}
