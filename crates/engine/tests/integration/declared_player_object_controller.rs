//! "that player controls" / "they control" after a declared target player names that player
//! (CR 608.2c + CR 115.1a), never the caster, never a second slot; "you control" still names the
//! caster. Three seats: caster P0, declared player P1, bystander P2.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const P2: PlayerId = PlayerId(2);

const KROOG: &str = "Choose target opponent. Destroy target land that player controls. The Fall of Kroog deals 3 damage to that player and 1 damage to each creature they control.";
const KEEPER: &str = "{B}, {T}: Choose target opponent who has at least two fewer creature cards in their graveyard than you do as you activate this ability. Destroy target nonblack creature that player controls.";
const DOWN_FOR_REPAIRS: &str = "Target opponent reveals their hand. You choose a nonland card from it. That player discards that card. Destroy up to one target Attraction that player controls. (It's put into their junkyard.)";
const YOSEI: &str = "Flying\nWhen Yosei dies, target player skips their next untap step. Tap up to five target permanents that player controls.";
const RADIATING_LIGHTNING: &str = "Radiating Lightning deals 3 damage to target player and 1 damage to each creature that player controls.";
const AGGRESSIVE_NEGOTIATIONS: &str = "Target opponent reveals their hand. You choose a nonland card from it and exile that card. Put a +1/+1 counter on up to one target creature you control.";

/// Per-seat permanents: a land and a toughness-9 creature.
struct Board {
    lands: [ObjectId; 3],
    creatures: [ObjectId; 3],
}

fn board(sc: &mut GameScenario) -> Board {
    let mut lands = [ObjectId(0); 3];
    let mut creatures = [ObjectId(0); 3];
    for (i, p) in [P0, P1, P2].into_iter().enumerate() {
        lands[i] = sc.add_basic_land(p, ManaColor::Black);
        creatures[i] = sc.add_creature(p, &format!("C{i}"), 2, 9).id();
        for n in 0..2 {
            sc.add_card_to_hand(p, &format!("H{i}{n}"));
        }
    }
    Board { lands, creatures }
}

fn three_player() -> GameScenario {
    let mut sc = GameScenario::new_n_player(3, 7);
    sc.at_phase(Phase::PreCombatMain);
    sc
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

fn controller(r: &GameRunner, t: &TargetRef) -> Option<PlayerId> {
    match t {
        TargetRef::Object(id) => Some(r.state().objects[id].controller),
        TargetRef::Player(_) => None,
    }
}

/// Every offered target slot, as `(slot index, legal targets)`.
type Offers = Vec<(usize, Vec<TargetRef>)>;

/// Drives prompts to quiescence: player slots take P1 (the declared player), object slots take
/// the first offer controlled by `want`; `eliminate` removes a seat once the stack is first
/// passed. Before taking its pick, a slot first tries one offered object controlled by another
/// seat and requires the rejection.
fn drive(r: &mut GameRunner, want: PlayerId, eliminate: Option<usize>) -> Offers {
    let mut offers = Offers::new();
    let mut eliminate = eliminate;
    for _ in 0..60 {
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
                let legal = target_slots[selection.current_slot].legal_targets.clone();
                offers.push((selection.current_slot, legal.clone()));
                let target = if legal.iter().all(|t| matches!(t, TargetRef::Player(_))) {
                    legal.iter().find(|t| **t == TargetRef::Player(P1)).cloned()
                } else {
                    selection
                        .current_legal_targets
                        .iter()
                        .find(|t| controller(r, t) == Some(want))
                        .cloned()
                };
                if let Some(wrong) = legal
                    .iter()
                    .find(|t| controller(r, t).is_some_and(|c| c != want))
                {
                    assert!(
                        r.act(GameAction::ChooseTarget {
                            target: Some(wrong.clone())
                        })
                        .is_err(),
                        "an object not controlled by {want:?} was accepted"
                    );
                }
                r.act(GameAction::ChooseTarget { target }).expect("target");
            }
            WaitingFor::RevealChoice { cards, .. } => {
                r.act(GameAction::SelectCards {
                    cards: vec![cards[0]],
                })
                .expect("reveal");
            }
            WaitingFor::DiscardChoice { cards, count, .. } => {
                r.act(GameAction::SelectCards {
                    cards: cards.into_iter().take(count).collect(),
                })
                .expect("discard");
            }
            WaitingFor::Priority { .. } => {
                if r.state().stack.is_empty() {
                    break;
                }
                if let Some(seat) = eliminate.take() {
                    r.state_mut().players[seat].is_eliminated = true;
                }
                r.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    offers
}

fn cast_row(text: &str, want: PlayerId, eliminate: Option<usize>) -> (GameRunner, Board, Offers) {
    let mut sc = three_player();
    let b = board(&mut sc);
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Row", false, text)
        .id();
    let mut r = sc.build();
    hand_cards_become_creatures(&mut r);
    let card_id = r.state().objects[&spell].card_id;
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast");
    let offers = drive(&mut r, want, eliminate);
    (r, b, offers)
}

fn zone_of(r: &GameRunner, id: ObjectId) -> Zone {
    r.state().objects[&id].zone
}

fn lives(r: &GameRunner) -> Vec<i32> {
    r.state().players.iter().map(|p| p.life).collect()
}

fn damage(r: &GameRunner, id: ObjectId) -> u32 {
    r.state().objects[&id].damage_marked
}

/// Slot `slot` offered something, and nothing controlled by `seat`.
fn offered_not_to(r: &GameRunner, offers: &Offers, slot: usize, seat: PlayerId) {
    let (_, legal) = offers
        .iter()
        .find(|(s, _)| *s == slot)
        .unwrap_or_else(|| panic!("slot {slot} was never offered: {offers:?}"));
    assert!(!legal.is_empty(), "slot {slot} offered nothing");
    assert!(
        legal.iter().all(|t| controller(r, t) != Some(seat)),
        "slot {slot} offered a target controlled by {seat:?}: {legal:?}"
    );
}

/// Slot `slot` offered something, all of it controlled by `seat`.
fn offered_only_to(r: &GameRunner, offers: &Offers, slot: usize, seat: PlayerId) {
    let (_, legal) = offers
        .iter()
        .find(|(s, _)| *s == slot)
        .unwrap_or_else(|| panic!("slot {slot} was never offered: {offers:?}"));
    assert!(!legal.is_empty(), "slot {slot} offered nothing");
    assert!(
        legal.iter().all(|t| controller(r, t) == Some(seat)),
        "slot {slot} offered a target not controlled by {seat:?}: {legal:?}"
    );
}

/// CR 608.2c + CR 115.1a: the land, the damage and "each creature they control" all name the
/// player the first clause chose, across the intervening object-target clause.
#[test]
fn the_fall_of_kroog_acts_on_the_chosen_opponent() {
    let (r, b, offers) = cast_row(KROOG, P1, None);
    offered_not_to(&r, &offers, 1, P0);
    assert_eq!(offers.len(), 2, "one player slot and one land slot");
    assert_eq!(zone_of(&r, b.lands[1]), Zone::Graveyard, "P1's land died");
    assert_eq!(zone_of(&r, b.lands[0]), Zone::Battlefield);
    assert_eq!(zone_of(&r, b.lands[2]), Zone::Battlefield);
    assert_eq!(lives(&r), vec![20, 17, 20], "only P1 took the 3 damage");
    assert_eq!(
        [
            damage(&r, b.creatures[0]),
            damage(&r, b.creatures[1]),
            damage(&r, b.creatures[2])
        ],
        [0, 1, 0],
        "only P1's creature took the 1 damage"
    );
}

/// CR 608.2b: with the declared player gone, nothing that names that player happens; the
/// bystanders and the caster are untouched.
#[test]
fn the_fall_of_kroog_affects_no_one_once_the_chosen_opponent_is_gone() {
    let (r, b, offers) = cast_row(KROOG, P1, Some(1));
    offered_not_to(&r, &offers, 1, P0);
    assert_eq!(
        zone_of(&r, b.lands[1]),
        Zone::Battlefield,
        "no land destroyed"
    );
    assert_eq!(zone_of(&r, b.lands[0]), Zone::Battlefield);
    assert_eq!(zone_of(&r, b.lands[2]), Zone::Battlefield);
    assert_eq!(lives(&r)[0], 20, "the caster took no damage");
    assert_eq!(lives(&r)[2], 20, "the bystander took no damage");
    assert_eq!(damage(&r, b.creatures[0]), 0);
    assert_eq!(damage(&r, b.creatures[2]), 0);
}

fn keeper_row(eliminate: Option<usize>) -> (GameRunner, Board, Offers) {
    let mut sc = three_player();
    let b = board(&mut sc);
    let keeper = sc
        .add_creature_from_oracle(P0, "Keeper of the Dead", 1, 2, KEEPER)
        .id();
    let mut r = sc.build();
    hand_cards_become_creatures(&mut r);
    r.act(GameAction::ActivateAbility {
        source_id: keeper,
        ability_index: 0,
    })
    .expect("activate");
    let offers = drive(&mut r, P1, eliminate);
    (r, b, offers)
}

/// CR 608.2c: "that player" is the opponent the activation chose; the creature slot offers only
/// that opponent's creatures.
#[test]
fn keeper_of_the_dead_destroys_the_chosen_opponents_creature() {
    let (r, b, offers) = keeper_row(None);
    offered_not_to(&r, &offers, 1, P0);
    assert_eq!(zone_of(&r, b.creatures[1]), Zone::Graveyard);
    assert_eq!(zone_of(&r, b.creatures[0]), Zone::Battlefield);
    assert_eq!(zone_of(&r, b.creatures[2]), Zone::Battlefield);
}

/// CR 608.2b: the chosen opponent is gone, so "that player controls" determines nothing.
#[test]
fn keeper_of_the_dead_destroys_nothing_once_the_chosen_opponent_is_gone() {
    let (r, b, offers) = keeper_row(Some(1));
    offered_not_to(&r, &offers, 1, P0);
    for c in b.creatures {
        assert_eq!(zone_of(&r, c), Zone::Battlefield);
    }
}

fn attraction(sc: &mut GameScenario, p: PlayerId) -> ObjectId {
    sc.add_artifact_from_oracle(p, "Attraction", "")
        .with_subtypes(vec!["Attraction"])
        .id()
}

/// CR 608.2c: the Attraction slot offers only the revealed opponent's Attractions.
#[test]
fn down_for_repairs_destroys_the_chosen_opponents_attraction() {
    let mut sc = three_player();
    let _b = board(&mut sc);
    let attractions = [P0, P1, P2].map(|p| attraction(&mut sc, p));
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Row", false, DOWN_FOR_REPAIRS)
        .id();
    let mut r = sc.build();
    hand_cards_become_creatures(&mut r);
    let card_id = r.state().objects[&spell].card_id;
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast");
    let offers = drive(&mut r, P1, None);
    offered_not_to(&r, &offers, 1, P0);
    assert_eq!(
        zone_of(&r, attractions[1]),
        Zone::Command,
        "destroyed Attraction goes to its junkyard"
    );
    assert_eq!(zone_of(&r, attractions[0]), Zone::Battlefield);
    assert_eq!(zone_of(&r, attractions[2]), Zone::Battlefield);
}

/// CR 608.2c: Yosei's trigger taps only the declared player's permanents.
#[test]
fn yosei_taps_the_declared_players_permanents() {
    let mut sc = three_player();
    let b = board(&mut sc);
    let yosei = sc
        .add_creature_from_oracle(P0, "Yosei, the Morning Star", 5, 5, YOSEI)
        .id();
    let mut r = sc.build();
    r.state_mut().objects.get_mut(&yosei).unwrap().damage_marked = 5;
    r.act(GameAction::PassPriority).expect("pass");
    drive(&mut r, P1, None);
    assert_eq!(
        zone_of(&r, yosei),
        Zone::Graveyard,
        "reach guard: Yosei died"
    );
    let tapped = |id: ObjectId| r.state().objects[&id].tapped;
    assert!(
        tapped(b.creatures[1]) || tapped(b.lands[1]),
        "P1's permanent tapped"
    );
    for id in [b.lands[0], b.creatures[0], b.lands[2], b.creatures[2]] {
        assert!(
            !tapped(id),
            "{id:?} belongs to a player the trigger did not name"
        );
    }
}

/// CR 115.1 + CR 109.4: "you control" names the caster even after a declared player; the
/// counter slot offers the caster's creature, not the revealed opponent's.
#[test]
fn aggressive_negotiations_counter_goes_on_the_casters_creature() {
    let (r, b, offers) = cast_row(AGGRESSIVE_NEGOTIATIONS, P0, None);
    offered_only_to(&r, &offers, 1, P0);
    let counters = |id: ObjectId| {
        r.state().objects[&id]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    };
    assert_eq!(counters(b.creatures[0]), 1);
    assert_eq!(counters(b.creatures[1]), 0);
}

/// CR 608.2c + CR 115.1a: a continuation in the announcing clause names the player that clause
/// announced.
#[test]
fn radiating_lightning_hits_only_the_chosen_players_creatures() {
    let (r, b, _) = cast_row(RADIATING_LIGHTNING, P1, None);
    assert_eq!(lives(&r), vec![20, 17, 20], "only P1 took the 3 damage");
    assert_eq!(
        [
            damage(&r, b.creatures[0]),
            damage(&r, b.creatures[1]),
            damage(&r, b.creatures[2])
        ],
        [0, 1, 0],
        "only P1's creature took the 1 damage"
    );
}

fn parse_json(text: &str) -> String {
    serde_json::to_string(&parse_oracle_text(
        text,
        "Row",
        &[],
        &["Sorcery".into()],
        &[],
    ))
    .unwrap()
}

/// With no prior player declaration, neither the object-target clause's controller nor the
/// anaphor names a declared player (reach guard: the declared control leg does).
#[test]
fn without_a_player_declaration_nothing_binds_a_declared_player() {
    for text in [
        "Destroy target creature. That player loses 1 life.",
        "Destroy target creature that player controls.",
    ] {
        let json = parse_json(text);
        assert!(json.contains("Destroy"), "reach guard: {text}: {json}");
        assert!(!json.contains("DeclaredPlayer"), "{text}: {json}");
    }
    let json = parse_json(
        "Row deals 3 damage to target creature and 1 damage to each creature its controller controls.",
    );
    assert!(json.contains("DamageAll"), "reach guard: {json}");
    assert!(!json.contains("DeclaredPlayer"), "{json}");
    for text in [
        "Choose target opponent. Destroy target creature. That player loses 1 life.",
        "Choose target opponent. Destroy target creature that player controls.",
    ] {
        assert!(parse_json(text).contains("DeclaredPlayer"), "{text}");
    }
}
