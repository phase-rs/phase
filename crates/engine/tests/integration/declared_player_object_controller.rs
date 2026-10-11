//! "that player controls" / "they control" after a declared target player names that player
//! (CR 608.2c + CR 115.1a), never the caster, never a second slot; "you control" still names the
//! caster. Three seats: caster P0, declared player P1, bystander P2.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::static_abilities::player_has_hexproof;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ControllerRef, StaticDefinition, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, GameState, LayersDirty, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
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

/// A change to the game applied once, after targets are announced and before the stack first
/// resolves.
#[derive(Clone, Copy)]
enum Invalidate {
    /// Marks the seat as having left the game; its permanents stay on the battlefield.
    Leave(PlayerId),
    /// Runs the real elimination, which exiles the seat's objects (CR 800.4a).
    Eliminate(PlayerId),
    /// The player becomes an illegal target while remaining in the game (CR 702.11c).
    Hexproof(PlayerId),
    /// Every permanent `from` controls changes controller to `to`.
    Steal { from: PlayerId, to: PlayerId },
}

const HEXPROOF_SOURCE: &str = "You Have Hexproof Source";

/// "You have hexproof" (the Leyline of Sanctity shape); the new static needs a full layer pass.
fn grant_hexproof(state: &mut GameState, player: PlayerId) {
    let grantor = engine::game::zones::create_object(
        state,
        CardId(951),
        player,
        HEXPROOF_SOURCE.to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&grantor)
        .expect("the grantor was just created")
        .static_definitions =
        vec![
            StaticDefinition::new(StaticMode::Hexproof).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        ]
        .into();
    state.layers_dirty = LayersDirty::Full;
    engine::game::layers::flush_layers(state);
    assert!(
        player_has_hexproof(state, player),
        "reach guard: {player:?} has hexproof"
    );
}

fn apply_invalidation(state: &mut GameState, change: Invalidate) {
    match change {
        Invalidate::Leave(p) => state.players[p.0 as usize].is_eliminated = true,
        Invalidate::Eliminate(p) => {
            engine::game::elimination::eliminate_player(state, p, &mut Vec::new());
        }
        Invalidate::Hexproof(p) => grant_hexproof(state, p),
        Invalidate::Steal { from, to } => {
            let ids: Vec<ObjectId> = state.battlefield.iter().copied().collect();
            for id in ids {
                let o = state.objects.get_mut(&id).expect("battlefield object");
                if o.controller == from && o.name != HEXPROOF_SOURCE {
                    o.controller = to;
                }
            }
        }
    }
}

/// Drives prompts to quiescence: player slots take P1 (the declared player), object slots take
/// the first offer controlled by `want`; `invalidate` lands once the stack is first passed.
/// Before taking its pick, a slot first tries one offered object controlled by another seat and
/// requires the rejection.
fn drive(r: &mut GameRunner, want: PlayerId, invalidate: &[Invalidate]) -> Offers {
    drive_with(r, want, invalidate, true)
}

/// `drive`, with the wrong-controller rejection required only when `strict` (an unrestricted
/// "target creature" slot legitimately accepts any seat's creature).
fn drive_with(
    r: &mut GameRunner,
    want: PlayerId,
    invalidate: &[Invalidate],
    strict: bool,
) -> Offers {
    let mut offers = Offers::new();
    let mut pending = invalidate;
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
                    .find(|t| strict && controller(r, t).is_some_and(|c| c != want))
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
                for change in std::mem::take(&mut pending) {
                    apply_invalidation(r.state_mut(), *change);
                }
                r.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    offers
}

fn cast_row(text: &str, want: PlayerId, invalidate: &[Invalidate]) -> (GameRunner, Board, Offers) {
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
    let offers = drive(&mut r, want, invalidate);
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
    let (r, b, offers) = cast_row(KROOG, P1, &[]);
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
    let (r, b, offers) = cast_row(KROOG, P1, &[Invalidate::Leave(P1)]);
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

fn keeper_row(invalidate: &[Invalidate]) -> (GameRunner, Board, Offers) {
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
    let offers = drive(&mut r, P1, invalidate);
    (r, b, offers)
}

/// CR 608.2c: "that player" is the opponent the activation chose; the creature slot offers only
/// that opponent's creatures.
#[test]
fn keeper_of_the_dead_destroys_the_chosen_opponents_creature() {
    let (r, b, offers) = keeper_row(&[]);
    offered_not_to(&r, &offers, 1, P0);
    assert_eq!(zone_of(&r, b.creatures[1]), Zone::Graveyard);
    assert_eq!(zone_of(&r, b.creatures[0]), Zone::Battlefield);
    assert_eq!(zone_of(&r, b.creatures[2]), Zone::Battlefield);
}

/// CR 608.2b: the chosen opponent is gone, so "that player controls" determines nothing.
#[test]
fn keeper_of_the_dead_destroys_nothing_once_the_chosen_opponent_is_gone() {
    let (r, b, offers) = keeper_row(&[Invalidate::Leave(P1)]);
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
    let offers = drive(&mut r, P1, &[]);
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
    drive(&mut r, P1, &[]);
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
    assert_eq!(
        r.state().steps_to_skip[1].get(&Phase::Untap).copied(),
        Some(1),
        "P1 skips their next untap step"
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
    let (r, b, offers) = cast_row(AGGRESSIVE_NEGOTIATIONS, P0, &[]);
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
    let (r, b, _) = cast_row(RADIATING_LIGHTNING, P1, &[]);
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

const TREASURE_CHAIN: &str = "Destroy target creature. Target opponent loses 2 life. Create a Treasure token. That player discards a card.";

fn hand_sizes(r: &GameRunner) -> Vec<usize> {
    [P0, P1, P2]
        .map(|p| {
            r.state()
                .objects
                .values()
                .filter(|o| o.zone == Zone::Hand && o.owner == p)
                .count()
        })
        .to_vec()
}

fn treasure_chain(invalidate: &[Invalidate]) -> (GameRunner, Vec<usize>) {
    let mut sc = three_player();
    board(&mut sc);
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Row", false, TREASURE_CHAIN)
        .id();
    let mut r = sc.build();
    hand_cards_become_creatures(&mut r);
    let before = hand_sizes(&r);
    let card_id = r.state().objects[&spell].card_id;
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast");
    drive_with(&mut r, P2, invalidate, false);
    (r, before)
}

/// CR 608.2c: "That player" names the declared opponent across the token clause; the legal
/// opponent discards (reach guard for the eliminated leg).
#[test]
fn that_player_after_a_token_clause_discards_the_declared_opponent() {
    let (r, before) = treasure_chain(&[]);
    let after = hand_sizes(&r);
    assert_eq!(lives(&r), vec![20, 18, 20], "reach guard: P1 lost 2 life");
    assert_eq!(
        [
            before[0] - after[0],
            before[1] - after[1],
            before[2] - after[2]
        ],
        [1, 1, 0],
        "the cast spell left P0's hand; only the declared opponent discarded: {before:?} -> {after:?}"
    );
}

/// CR 608.2b: with the declared opponent gone, the token clause does not turn "that player"
/// into a bystander.
#[test]
fn that_player_after_a_token_clause_affects_no_one_once_the_declared_opponent_is_gone() {
    let (r, before) = treasure_chain(&[Invalidate::Leave(P1)]);
    assert_eq!(
        hand_sizes(&r),
        vec![before[0] - 1, before[1], before[2]],
        "only the cast spell left a hand; no one discarded"
    );
    assert_eq!(
        lives(&r)[0],
        20,
        "reach guard: the spell resolved, caster untouched"
    );
    assert!(
        r.state().objects.values().any(|o| o.zone == Zone::Graveyard
            && o.controller == P2
            && o.card_types.core_types.contains(&CoreType::Creature)),
        "reach guard: the Destroy clause ran"
    );
}

fn untap_skips(r: &GameRunner, p: PlayerId) -> Option<u32> {
    r.state().steps_to_skip[p.0 as usize]
        .get(&Phase::Untap)
        .copied()
}

fn yosei_row(invalidate: &[Invalidate]) -> (GameRunner, Board) {
    let mut sc = three_player();
    let b = board(&mut sc);
    let yosei = sc
        .add_creature_from_oracle(P0, "Yosei, the Morning Star", 5, 5, YOSEI)
        .id();
    let mut r = sc.build();
    r.state_mut().objects.get_mut(&yosei).unwrap().damage_marked = 5;
    r.act(GameAction::PassPriority).expect("pass");
    drive(&mut r, P1, invalidate);
    assert_eq!(
        zone_of(&r, yosei),
        Zone::Graveyard,
        "reach guard: Yosei died"
    );
    assert!(
        r.state().stack.is_empty(),
        "reach guard: the trigger resolved"
    );
    (r, b)
}

fn tapped_among(r: &GameRunner, ids: &[ObjectId]) -> Vec<bool> {
    ids.iter().map(|id| r.state().objects[id].tapped).collect()
}

/// CR 608.2b: only the declared player is an illegal target, so the permanents that player
/// controls are still tapped (Yosei ruling), while the player's instruction does not happen.
#[test]
fn yosei_taps_the_permanents_when_only_the_declared_player_is_illegal() {
    let (r, b) = yosei_row(&[Invalidate::Hexproof(P1)]);
    assert_eq!(
        tapped_among(&r, &[b.creatures[1], b.lands[1]]),
        [true, true],
        "P1's permanents stay legal targets"
    );
    for id in [b.lands[0], b.creatures[0], b.lands[2], b.creatures[2]] {
        assert!(!r.state().objects[&id].tapped, "{id:?} was not targeted");
    }
    assert_eq!(
        untap_skips(&r, P1),
        None,
        "the illegal player skips nothing"
    );
}

/// CR 608.2b: each permanent is revalidated on its own controller, so with every target illegal
/// nothing happens.
#[test]
fn yosei_does_nothing_when_the_player_and_every_permanent_are_illegal() {
    let (r, b) = yosei_row(&[
        Invalidate::Hexproof(P1),
        Invalidate::Steal { from: P1, to: P2 },
    ]);
    let all = [
        b.creatures[0],
        b.lands[0],
        b.creatures[1],
        b.lands[1],
        b.creatures[2],
        b.lands[2],
    ];
    assert_eq!(tapped_among(&r, &all), [false; 6]);
    assert_eq!(untap_skips(&r, P1), None);
}

/// Yosei ruling: if all the permanents are illegal, the player still skips their untap step.
#[test]
fn yosei_skips_the_untap_step_when_only_the_permanents_are_illegal() {
    let (r, b) = yosei_row(&[Invalidate::Steal { from: P1, to: P2 }]);
    let all = [
        b.creatures[0],
        b.lands[0],
        b.creatures[1],
        b.lands[1],
        b.creatures[2],
        b.lands[2],
    ];
    assert_eq!(tapped_among(&r, &all), [false; 6]);
    assert_eq!(
        untap_skips(&r, P1),
        Some(1),
        "reach guard: the player was legal"
    );
}

/// CR 800.4a: a player who left the game takes their permanents with them, so nothing is tapped
/// and no untap step is skipped.
#[test]
fn yosei_does_nothing_once_the_declared_player_has_been_eliminated() {
    let (r, b) = yosei_row(&[Invalidate::Eliminate(P1)]);
    assert!(r.state().players[1].is_eliminated, "reach guard");
    assert_eq!(zone_of(&r, b.creatures[1]), Zone::Exile);
    assert_eq!(zone_of(&r, b.lands[1]), Zone::Exile);
    assert_eq!(untap_skips(&r, P1), None);
}

fn hand_names(r: &GameRunner, p: PlayerId) -> Vec<String> {
    let mut names: Vec<String> = r
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Hand && o.owner == p)
        .map(|o| o.name.clone())
        .collect();
    names.sort();
    names
}

struct DownForRepairs {
    runner: GameRunner,
    attraction: ObjectId,
    hands_before: Vec<usize>,
    names_before: [Vec<String>; 2],
}

fn down_for_repairs_row(invalidate: &[Invalidate]) -> DownForRepairs {
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
    // The spell stays in hand until its targets are chosen, so the baseline is taken without it.
    let mut hands_before = hand_sizes(&r);
    hands_before[0] -= 1;
    let mut names_before = [hand_names(&r, P0), hand_names(&r, P1)];
    names_before[0].retain(|name| name != "Row");
    drive(&mut r, P1, invalidate);
    assert!(
        r.state().stack.is_empty(),
        "reach guard: the spell resolved"
    );
    assert_eq!(
        zone_of(&r, spell),
        Zone::Graveyard,
        "reach guard: it left the stack"
    );
    DownForRepairs {
        runner: r,
        attraction: attractions[1],
        hands_before,
        names_before,
    }
}

/// CR 608.2b: only the revealed opponent is an illegal target, so the Attraction is still
/// destroyed, while the reveal and the discard that name the opponent do nothing, and the
/// caster discards nothing in the opponent's place.
#[test]
fn down_for_repairs_destroys_the_attraction_when_only_the_opponent_is_illegal() {
    let row = down_for_repairs_row(&[Invalidate::Hexproof(P1)]);
    let r = &row.runner;
    assert_eq!(zone_of(r, row.attraction), Zone::Command, "destroyed");
    assert_eq!(hand_sizes(r), row.hands_before, "no card was discarded");
    assert_eq!(hand_names(r, P0), row.names_before[0]);
    assert_eq!(hand_names(r, P1), row.names_before[1]);
}

/// CR 608.2b: with the opponent and the Attraction both illegal, nothing happens.
#[test]
fn down_for_repairs_does_nothing_when_the_opponent_and_attraction_are_illegal() {
    let row = down_for_repairs_row(&[
        Invalidate::Hexproof(P1),
        Invalidate::Steal { from: P1, to: P2 },
    ]);
    let r = &row.runner;
    assert_eq!(zone_of(r, row.attraction), Zone::Battlefield);
    assert_eq!(hand_sizes(r), row.hands_before);
}

/// Ruling: do as much as possible to the remaining legal target. Only the Attraction is illegal,
/// so the opponent still reveals and discards.
#[test]
fn down_for_repairs_discards_when_only_the_attraction_is_illegal() {
    let row = down_for_repairs_row(&[Invalidate::Steal { from: P1, to: P2 }]);
    let r = &row.runner;
    assert_eq!(zone_of(r, row.attraction), Zone::Battlefield);
    assert_eq!(hand_sizes(r), vec![2, 1, 2], "P1 discarded");
}

/// CR 800.4a: the caster's hand is untouched once the revealed opponent has been eliminated.
#[test]
fn down_for_repairs_leaves_the_caster_alone_once_the_opponent_is_eliminated() {
    let row = down_for_repairs_row(&[Invalidate::Eliminate(P1)]);
    let r = &row.runner;
    assert!(r.state().players[1].is_eliminated, "reach guard");
    assert_eq!(hand_names(r, P0), row.names_before[0]);
    assert_eq!(hand_sizes(r)[2], row.hands_before[2]);
}

/// CR 608.2b: "each creature they control" requires information about the illegal player and
/// does not happen; the independently targeted land still dies.
#[test]
fn the_fall_of_kroog_destroys_the_land_when_only_the_chosen_opponent_is_illegal() {
    let (r, b, _) = cast_row(KROOG, P1, &[Invalidate::Hexproof(P1)]);
    assert_eq!(zone_of(&r, b.lands[1]), Zone::Graveyard, "P1's land died");
    assert_eq!(lives(&r), vec![20, 20, 20], "no damage to the player");
    assert_eq!(
        [
            damage(&r, b.creatures[0]),
            damage(&r, b.creatures[1]),
            damage(&r, b.creatures[2])
        ],
        [0, 0, 0],
        "no creature damaged"
    );
}
