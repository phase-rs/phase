//! Runtime witnesses for the `confirmed-misparses.toml` rows released in batch 4: the per-opponent
//! target-slot class, "for each opponent, <verb> up to one target <object> that player controls".
//! Every released card is the printed card (printed cost, type line, P/T and verbatim Oracle text,
//! loaded from the card database through `add_real_card`) and is driven through the public
//! cast / pass-priority / choose-target actions of `GameRunner` on a three-player table, so that the
//! "for each opponent" fan-out has two opponents to serve and a caster whose own permanents must
//! never be offered. Only textless fillers (vanilla creatures, one with a Knight subtype, textless
//! artifacts and enchantments) and plain lands are synthetic; the support cards that carry an
//! ability (Murder, Tropical Island) are printed cards.
//!
//! Each shape asserts the announced slot structure first (CR 601.2c, CR 603.3d: one player slot and
//! one object slot per opponent, the object slot offering exactly that opponent's eligible
//! permanents and nothing of the caster's or of the other opponent), then the resolved effect for
//! each opponent (CR 608.2c), then the printed duration or the rider that follows the fan-out.
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);

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
        kinds.extend([kind; 6]);
    }
    kinds.extend([ManaType::Colorless; 4]);
    kinds
        .into_iter()
        .enumerate()
        .map(|(i, kind)| ManaUnit::new(kind, ObjectId(9900 + i as u64), false, vec![]))
        .collect()
}

/// Three players (P0 caster, P1 and P2 opponents), P0 in its precombat main phase with `pool()`;
/// `setup` adds the released card and the permanents around it and returns the ids it created.
fn table<T>(setup: impl FnOnce(&mut GameScenario) -> T) -> (GameRunner, T) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        scenario.with_library_top(
            player,
            &[
                "Draw 1", "Draw 2", "Draw 3", "Draw 4", "Draw 5", "Draw 6", "Draw 7", "Draw 8",
            ],
        );
    }
    scenario.with_mana_pool(P0, pool());
    let made = setup(&mut scenario);
    (scenario.build(), made)
}

fn db() -> &'static engine::database::card_db::CardDatabase {
    shared_card_db().expect("the committed integration card fixture is present")
}

/// One announced target slot as the engine showed it: whether it is optional and what it offered.
type Slot = (bool, Vec<TargetRef>);

/// Answer the target prompt the engine is showing, choosing from each object slot the first of
/// `picks` it offers (and nothing when none is offered, which an "up to one" slot allows). Records
/// the slot structure of the first prompt in `offered`.
fn answer_prompt(
    r: &mut GameRunner,
    picks: &[ObjectId],
    offered: &mut Option<Vec<Slot>>,
    target_slots: Vec<engine::types::game_state::TargetSelectionSlot>,
    current_legal_targets: Vec<TargetRef>,
) {
    offered.get_or_insert_with(|| {
        target_slots
            .iter()
            .map(|s| (s.optional, s.legal_targets.clone()))
            .collect::<Vec<Slot>>()
    });
    let target = if current_legal_targets
        .iter()
        .all(|t| matches!(t, TargetRef::Player(_)))
    {
        current_legal_targets.first().cloned()
    } else {
        current_legal_targets
            .iter()
            .find(|t| matches!(t, TargetRef::Object(o) if picks.contains(o)))
            .cloned()
    };
    r.act(GameAction::ChooseTarget { target }).expect("choose");
}

/// Answer the target prompts of a spell cast just now until the engine next gives a player
/// priority (CR 601.2c: a spell's targets are announced as it is cast). Returns the slot
/// structure of the first prompt, `None` when the spell announced no targets.
fn announce(r: &mut GameRunner, picks: &[ObjectId]) -> Option<Vec<Slot>> {
    let mut offered = None;
    for _ in 0..40 {
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
            } => answer_prompt(
                r,
                picks,
                &mut offered,
                target_slots,
                selection.current_legal_targets,
            ),
            WaitingFor::Priority { .. } => return offered,
            o => panic!("announce: unexpected prompt {o:?}"),
        }
    }
    panic!("announce: never reached priority");
}

/// Pass priority until the stack is empty (every player passing in turn resolves the top object),
/// answering any target prompt a triggered ability puts up on the way (CR 603.3d: a trigger's
/// targets are chosen as it is put on the stack, after the event). Returns the first target
/// prompt's slot structure, if there was one.
fn settle(r: &mut GameRunner, picks: &[ObjectId]) -> Option<Vec<Slot>> {
    let mut offered = None;
    for _ in 0..80 {
        match r.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if r.state().stack.is_empty() => return offered,
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => answer_prompt(
                r,
                picks,
                &mut offered,
                target_slots,
                selection.current_legal_targets,
            ),
            o => panic!("settle: unexpected prompt {o:?}"),
        }
    }
    panic!("settle: the stack never emptied");
}

/// Cast `spell` from P0's hand at its printed cost, announce its targets from `picks` (at the cast
/// for a spell's own targets, as the trigger goes on the stack for a trigger's), and resolve
/// everything that is put on the stack. Returns the first target prompt's slot structure.
fn cast_and_resolve(r: &mut GameRunner, spell: ObjectId, picks: &[ObjectId]) -> Option<Vec<Slot>> {
    let at_cast = cast_and_announce(r, spell, picks);
    let later = settle(r, picks);
    at_cast.or(later)
}

fn cast_and_announce(r: &mut GameRunner, spell: ObjectId, picks: &[ObjectId]) -> Option<Vec<Slot>> {
    let card_id = r.state().objects[&spell].card_id;
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast");
    announce(r, picks)
}

/// Play on with every default answer (pass priority, no attackers, no blockers) until `done`.
fn drive_until(r: &mut GameRunner, label: &str, done: impl Fn(&GameRunner) -> bool) {
    for _ in 0..600 {
        if done(r) {
            return;
        }
        let action = match r.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            ref o => panic!("{label}: unexpected prompt {o:?}"),
        };
        r.act(action).expect(label);
    }
    panic!("{label}: never reached");
}

/// The next precombat main phase of `player`, at the point where that player first has priority.
fn to_next_main_of(r: &mut GameRunner, player: PlayerId) {
    let turn = r.state().turn_number;
    drive_until(r, "next main phase", |r| {
        let s = r.state();
        s.turn_number > turn
            && s.active_player == player
            && s.phase == Phase::PreCombatMain
            && matches!(s.waiting_for, WaitingFor::Priority { .. })
    });
}

/// Assert the announced structure of a per-opponent fan-out: for each `(opponent, eligible)` one
/// non-optional player slot holding exactly that opponent, then one optional ("up to one") object
/// slot offering exactly `eligible`.
fn assert_offers(card: &str, offered: &[Slot], opponents: &[(PlayerId, Vec<ObjectId>)]) {
    assert_eq!(
        offered.len(),
        opponents.len() * 2,
        "{card}: one player slot and one object slot per opponent, nothing for the caster"
    );
    for (i, (opponent, eligible)) in opponents.iter().enumerate() {
        let (header_optional, header) = &offered[2 * i];
        assert!(
            !header_optional,
            "{card}: the opponent slot is not optional"
        );
        assert_eq!(
            header,
            &vec![TargetRef::Player(*opponent)],
            "{card}: slot {} pins the opponent seat",
            2 * i
        );
        let (optional, objects) = &offered[2 * i + 1];
        assert!(
            optional,
            "{card}: \"up to one\" slot {} is optional",
            2 * i + 1
        );
        let mut got: Vec<u64> = objects
            .iter()
            .map(|t| match t {
                TargetRef::Object(o) => o.0,
                TargetRef::Player(p) => panic!("{card}: an object slot offered player {p:?}"),
            })
            .collect();
        let mut want: Vec<u64> = eligible.iter().map(|o| o.0).collect();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(
            got, want,
            "{card}: opponent {opponent:?}'s slot offers exactly that opponent's eligible permanents"
        );
    }
}

fn controller(r: &GameRunner, id: ObjectId) -> PlayerId {
    r.state().objects[&id].controller
}

fn zone_of(r: &GameRunner, id: ObjectId) -> Zone {
    r.state().objects[&id].zone
}

fn power_toughness(r: &GameRunner, id: ObjectId) -> (i32, i32) {
    let o = &r.state().objects[&id];
    (o.power.unwrap_or(0), o.toughness.unwrap_or(0))
}

/// The exact keyword (variant and payload), not merely its kind: `Annihilator(1)` is not
/// `Annihilator(2)` and protection from Assassins is not protection from red.
fn has(r: &GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    r.state().objects[&id].keywords.contains(keyword)
}

/// Every object on the battlefield with `name` (copies and tokens included), in id order.
fn on_battlefield(r: &GameRunner, name: &str) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = r
        .state()
        .objects
        .values()
        .filter(|o| o.name == name && o.zone == Zone::Battlefield)
        .map(|o| o.id)
        .collect();
    ids.sort_by_key(|o| o.0);
    ids
}

fn is_a_creature(r: &GameRunner, id: ObjectId) -> bool {
    r.state().objects[&id]
        .card_types
        .core_types
        .contains(&CoreType::Creature)
}

/// The shared board of the steal / exile / copy shapes: each opponent has two eligible creatures, an
/// artifact creature, a plain artifact and an enchantment; the caster has creatures of its own.
struct Seats {
    mine: ObjectId,
    p1_a: ObjectId,
    p1_b: ObjectId,
    p2_a: ObjectId,
    p2_b: ObjectId,
}

fn seats(s: &mut GameScenario) -> Seats {
    Seats {
        mine: s.add_creature(P0, "My Bear", 2, 2).id(),
        p1_a: s.add_creature(P1, "P1 Bear A", 2, 2).id(),
        p1_b: s.add_creature(P1, "P1 Bear B", 3, 3).id(),
        p2_a: s.add_creature(P2, "P2 Bear A", 4, 4).id(),
        p2_b: s.add_creature(P2, "P2 Bear B", 5, 5).id(),
    }
}

// ---- Shape 1: a one-shot spell, "For each opponent, gain control of up to one target artifact that
// player controls." (Bilbo's Burglaring). The control change has no duration (CR 611.2a). ----
#[test]
fn bilbos_burglaring_takes_one_artifact_from_each_opponent() {
    let card = "Bilbo's Burglaring";
    let (mut r, (spell, mine, p1_trinket, p1_gadget, p1_bear, p2_trinket, p2_bear)) = table(|s| {
        (
            s.add_real_card(P0, card, Zone::Hand, db()),
            s.add_artifact_from_oracle(P0, "My Trinket", "").id(),
            s.add_artifact_from_oracle(P1, "P1 Trinket", "").id(),
            s.add_creature(P1, "P1 Gadget", 3, 3)
                .as_artifact_creature()
                .id(),
            s.add_creature(P1, "P1 Bear", 2, 2).id(),
            s.add_artifact_from_oracle(P2, "P2 Trinket", "").id(),
            s.add_creature(P2, "P2 Bear", 2, 2).id(),
        )
    });
    // The artifact creature is an artifact (eligible); the nonartifact creatures, the caster's own
    // artifact and the other opponent's artifact are not.
    let offered = cast_and_resolve(&mut r, spell, &[p1_gadget, p2_trinket]).expect("targets");
    assert_offers(
        card,
        &offered,
        &[(P1, vec![p1_trinket, p1_gadget]), (P2, vec![p2_trinket])],
    );
    assert_eq!(
        controller(&r, p1_gadget),
        P0,
        "P1's chosen artifact is now P0's"
    );
    assert_eq!(
        controller(&r, p2_trinket),
        P0,
        "P2's chosen artifact is now P0's"
    );
    assert_eq!(controller(&r, p1_trinket), P1, "P1's other artifact stays");
    assert_eq!(controller(&r, p1_bear), P1);
    assert_eq!(controller(&r, p2_bear), P2);
    assert_eq!(controller(&r, mine), P0);
    assert_eq!(zone_of(&r, spell), Zone::Graveyard, "the sorcery is spent");
    // No duration: the artifacts are still P0's on P0's next turn (CR 611.2a).
    to_next_main_of(&mut r, P0);
    assert_eq!(controller(&r, p1_gadget), P0);
    assert_eq!(controller(&r, p2_trinket), P0);
}

/// "Up to one": an opponent whose slot is left empty loses nothing; the other still does.
#[test]
fn bilbos_burglaring_may_skip_an_opponent() {
    let (mut r, (spell, p1_trinket, p2_trinket)) = table(|s| {
        (
            s.add_real_card(P0, "Bilbo's Burglaring", Zone::Hand, db()),
            s.add_artifact_from_oracle(P1, "P1 Trinket", "").id(),
            s.add_artifact_from_oracle(P2, "P2 Trinket", "").id(),
        )
    });
    cast_and_resolve(&mut r, spell, &[p2_trinket]);
    assert_eq!(controller(&r, p1_trinket), P1, "P1's slot was left empty");
    assert_eq!(controller(&r, p2_trinket), P0);
}

// ---- Shape 2: "For each opponent, you create a token that's a copy of up to one target creature that
// player controls." (Elminster's Simulacrum). The copies are P0's; the originals are untouched. ----
#[test]
fn elminsters_simulacrum_copies_one_creature_of_each_opponent() {
    let card = "Elminster's Simulacrum";
    let (mut r, (spell, seat)) = table(|s| (s.add_real_card(P0, card, Zone::Hand, db()), seats(s)));
    let offered = cast_and_resolve(&mut r, spell, &[seat.p1_b, seat.p2_a]).expect("targets");
    assert_offers(
        card,
        &offered,
        &[
            (P1, vec![seat.p1_a, seat.p1_b]),
            (P2, vec![seat.p2_a, seat.p2_b]),
        ],
    );
    let tokens: Vec<_> = r
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield)
        .map(|o| (o.name.clone(), o.controller, o.power, o.toughness))
        .collect();
    let mut tokens = tokens;
    tokens.sort();
    assert_eq!(
        tokens,
        vec![
            ("P1 Bear B".to_string(), P0, Some(3), Some(3)),
            ("P2 Bear A".to_string(), P0, Some(4), Some(4)),
        ],
        "exactly one copy per chosen creature, under P0, copying name and P/T"
    );
    for original in [seat.p1_a, seat.p1_b, seat.p2_a, seat.p2_b] {
        assert_ne!(controller(&r, original), P0, "the originals are not stolen");
        assert_eq!(zone_of(&r, original), Zone::Battlefield);
    }
    assert_eq!(
        on_battlefield(&r, "My Bear").len(),
        1,
        "nothing of P0's was copied"
    );
}

#[test]
fn elminsters_simulacrum_may_skip_an_opponent() {
    let (mut r, (spell, seat)) = table(|s| {
        (
            s.add_real_card(P0, "Elminster's Simulacrum", Zone::Hand, db()),
            seats(s),
        )
    });
    cast_and_resolve(&mut r, spell, &[seat.p2_b]);
    let tokens = r
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield)
        .map(|o| o.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        tokens,
        vec!["P2 Bear B".to_string()],
        "only P2's slot was filled"
    );
}

// ---- Shape 3: an enters trigger, "When ~ enters, for each opponent, tap up to one target nonland
// permanent that opponent controls. Put X stun counters on each of those permanents and you gain X
// life, where X is the number of nonbasic lands you control." (Omega, Heartless Evolution). ----
struct OmegaTable {
    omega: ObjectId,
    p1_bear: ObjectId,
    p1_charm: ObjectId,
    p1_land: ObjectId,
    p2_bear: ObjectId,
    p2_trinket: ObjectId,
    my_bear: ObjectId,
}

fn omega_table() -> (GameRunner, OmegaTable) {
    table(|s| {
        // X counts P0's nonbasic lands only: two Tropical Islands for P0, one for P1 (not counted),
        // and a basic Forest for P0 (not counted).
        s.add_real_card(P0, "Tropical Island", Zone::Battlefield, db());
        s.add_real_card(P0, "Tropical Island", Zone::Battlefield, db());
        s.add_real_card(P1, "Tropical Island", Zone::Battlefield, db());
        s.add_basic_land(P0, ManaColor::Green);
        OmegaTable {
            omega: s.add_real_card(P0, "Omega, Heartless Evolution", Zone::Hand, db()),
            p1_bear: s.add_creature(P1, "P1 Bear", 2, 2).id(),
            p1_charm: s.add_enchantment_from_oracle(P1, "P1 Charm", "").id(),
            p1_land: s.add_basic_land(P1, ManaColor::Red),
            p2_bear: s.add_creature(P2, "P2 Bear", 2, 2).id(),
            p2_trinket: s.add_artifact_from_oracle(P2, "P2 Trinket", "").id(),
            my_bear: s.add_creature(P0, "My Bear", 2, 2).id(),
        }
    })
}

#[test]
fn omega_taps_and_stuns_one_nonland_permanent_of_each_opponent_and_gains_x_once() {
    let card = "Omega, Heartless Evolution";
    let (mut r, t) = omega_table();
    let offered = cast_and_resolve(&mut r, t.omega, &[t.p1_bear, t.p2_trinket]).expect("targets");
    // Nonland only: the lands are not offered; the caster's own Bear is not offered.
    assert_offers(
        card,
        &offered,
        &[
            (P1, vec![t.p1_bear, t.p1_charm]),
            (P2, vec![t.p2_bear, t.p2_trinket]),
        ],
    );
    for chosen in [t.p1_bear, t.p2_trinket] {
        let o = &r.state().objects[&chosen];
        assert!(o.tapped, "{}: tapped", o.name);
        assert_eq!(
            o.counters.get(&CounterType::Stun).copied(),
            Some(2),
            "{}: X = 2 stun counters (two nonbasic lands)",
            o.name
        );
    }
    for untouched in [t.p1_charm, t.p1_land, t.p2_bear, t.my_bear] {
        let o = &r.state().objects[&untouched];
        assert!(!o.counters.contains_key(&CounterType::Stun), "{}", o.name);
    }
    assert!(!r.state().objects[&t.p2_bear].tapped);
    assert!(!r.state().objects[&t.my_bear].tapped);
    // X life exactly once for the whole trigger, not once per opponent (the printed text computes
    // X once and says "you gain X life" once).
    assert_eq!(r.state().players[0].life, 22, "P0 gains X = 2 once");
    assert_eq!(r.state().players[1].life, 20);
    assert_eq!(r.state().players[2].life, 20);
    assert_eq!(zone_of(&r, t.omega), Zone::Battlefield);
    assert_eq!(power_toughness(&r, t.omega), (8, 8));
}

/// "Up to one" again, and "each of those permanents": with P2's slot left empty only P1's permanent
/// is tapped and stunned, while the life gain does not depend on how many permanents were chosen.
#[test]
fn omega_stuns_only_the_chosen_permanents_and_still_gains_life() {
    let (mut r, t) = omega_table();
    cast_and_resolve(&mut r, t.omega, &[t.p1_charm]);
    assert_eq!(
        r.state().objects[&t.p1_charm]
            .counters
            .get(&CounterType::Stun)
            .copied(),
        Some(2)
    );
    for untouched in [t.p1_bear, t.p2_bear, t.p2_trinket] {
        let o = &r.state().objects[&untouched];
        assert!(
            !o.tapped && !o.counters.contains_key(&CounterType::Stun),
            "{}",
            o.name
        );
    }
    assert_eq!(r.state().players[0].life, 22);
}

/// A stun counter replaces the untap with its removal (CR 122.1d): each stunned permanent stays
/// tapped through its controller's next untap step, one counter lighter.
#[test]
fn omega_stun_counters_hold_the_permanents_through_the_next_untap_step() {
    let (mut r, t) = omega_table();
    cast_and_resolve(&mut r, t.omega, &[t.p1_bear, t.p2_trinket]);
    to_next_main_of(&mut r, P1);
    let bear = &r.state().objects[&t.p1_bear];
    assert!(
        bear.tapped,
        "P1's Bear stays tapped through P1's untap step"
    );
    assert_eq!(bear.counters.get(&CounterType::Stun).copied(), Some(1));
    to_next_main_of(&mut r, P2);
    let trinket = &r.state().objects[&t.p2_trinket];
    assert!(
        trinket.tapped,
        "P2's Trinket stays tapped through P2's untap step"
    );
    assert_eq!(trinket.counters.get(&CounterType::Stun).copied(), Some(1));
}

// ---- Shape 4: an enters trigger with a duration that ends with the source, "When Haytham Kenway
// enters, for each opponent, exile up to one target creature that player controls until Haytham
// Kenway leaves the battlefield." ----
#[test]
fn haytham_exiles_one_creature_per_opponent_until_he_leaves_the_battlefield() {
    let card = "Haytham Kenway";
    let (mut r, (haytham, seat, murder, p1_charm)) = table(|s| {
        // Lands, not pool mana, pay for Murder on a later turn (the pool empties between steps,
        // CR 106.4); they untap with P0's untap step.
        for _ in 0..3 {
            s.add_basic_land(P0, ManaColor::Black);
        }
        (
            s.add_real_card(P0, card, Zone::Hand, db()),
            seats(s),
            s.add_real_card(P0, "Murder", Zone::Hand, db()),
            s.add_enchantment_from_oracle(P1, "P1 Charm", "").id(),
        )
    });
    let offered = cast_and_resolve(&mut r, haytham, &[seat.p1_a, seat.p2_b]).expect("targets");
    // Creatures only, and only each opponent's own.
    assert_offers(
        card,
        &offered,
        &[
            (P1, vec![seat.p1_a, seat.p1_b]),
            (P2, vec![seat.p2_a, seat.p2_b]),
        ],
    );
    assert_eq!(zone_of(&r, haytham), Zone::Battlefield);
    assert_eq!(
        zone_of(&r, seat.p1_a),
        Zone::Exile,
        "P1's chosen creature is exiled"
    );
    assert_eq!(
        zone_of(&r, seat.p2_b),
        Zone::Exile,
        "P2's chosen creature is exiled"
    );
    assert_eq!(zone_of(&r, seat.p1_b), Zone::Battlefield);
    assert_eq!(zone_of(&r, seat.p2_a), Zone::Battlefield);
    assert_eq!(zone_of(&r, seat.mine), Zone::Battlefield);
    assert_eq!(zone_of(&r, p1_charm), Zone::Battlefield);

    let incarnations: Vec<u64> = [seat.p1_a, seat.p2_b]
        .iter()
        .map(|id| r.state().objects[id].incarnation)
        .collect();
    // The exile lasts while Haytham stays: a full turn cycle later they are still exiled.
    to_next_main_of(&mut r, P0);
    assert_eq!(zone_of(&r, seat.p1_a), Zone::Exile);
    assert_eq!(zone_of(&r, seat.p2_b), Zone::Exile);

    // Haytham leaves the battlefield (Murder, a printed instant): both creatures come back under
    // their owners' control (CR 610.3c) as new objects (CR 400.7).
    cast_and_resolve(&mut r, murder, &[haytham]);
    assert_eq!(zone_of(&r, haytham), Zone::Graveyard);
    for (i, (name, owner)) in [("P1 Bear A", P1), ("P2 Bear B", P2)]
        .into_iter()
        .enumerate()
    {
        let back = on_battlefield(&r, name);
        assert_eq!(back.len(), 1, "{name} returns to the battlefield");
        assert_ne!(
            r.state().objects[&back[0]].incarnation,
            incarnations[i],
            "{name} returns as a new object"
        );
        assert_eq!(
            controller(&r, back[0]),
            owner,
            "{name} returns under its owner"
        );
        assert!(is_a_creature(&r, back[0]));
    }
}

/// The other printed sentences of the card: "Other Knights you control get +2/+2 and have
/// protection from Assassins", and Haytham's own protection from Assassins.
#[test]
fn haytham_static_ability_pumps_only_other_knights_you_control() {
    let (mut r, (haytham, my_knight, their_knight, my_bear)) = table(|s| {
        (
            s.add_real_card(P0, "Haytham Kenway", Zone::Hand, db()),
            s.add_creature(P0, "My Knight", 2, 2)
                .with_subtypes(vec!["Knight"])
                .id(),
            s.add_creature(P1, "Their Knight", 2, 2)
                .with_subtypes(vec!["Knight"])
                .id(),
            s.add_creature(P0, "My Bear", 2, 2).id(),
        )
    });
    cast_and_resolve(&mut r, haytham, &[]);
    let protection = Keyword::Protection(ProtectionTarget::CardType("assassins".to_string()));
    assert_eq!(
        power_toughness(&r, haytham),
        (3, 3),
        "Haytham is not an \"other\" Knight"
    );
    assert!(
        has(&r, haytham, &protection),
        "Haytham has protection from Assassins"
    );
    assert_eq!(power_toughness(&r, my_knight), (4, 4));
    assert!(has(&r, my_knight, &protection));
    assert_eq!(
        power_toughness(&r, their_knight),
        (2, 2),
        "only Knights you control"
    );
    assert!(!has(&r, their_knight, &protection));
    assert_eq!(power_toughness(&r, my_bear), (2, 2), "only Knights");
}

// ---- Shape 5: a gain-control trigger with a duration and a rider, "gain control of up to one target
// creature that player controls until end of turn. Untap those creatures. They gain haste until
// end of turn." (Molten Primordial; the same sentence on an enters trigger). ----
fn tap(r: &mut GameRunner, ids: &[ObjectId]) {
    for id in ids {
        r.state_mut().objects.get_mut(id).unwrap().tapped = true;
    }
}

#[test]
fn molten_primordial_steals_untaps_and_hastes_one_creature_per_opponent_until_end_of_turn() {
    let card = "Molten Primordial";
    let (mut r, (primordial, seat, p1_artifact)) = table(|s| {
        (
            s.add_real_card(P0, card, Zone::Hand, db()),
            seats(s),
            s.add_artifact_from_oracle(P1, "P1 Trinket", "").id(),
        )
    });
    tap(&mut r, &[seat.p1_b, seat.p2_a]);
    let offered = cast_and_resolve(&mut r, primordial, &[seat.p1_b, seat.p2_a]).expect("targets");
    // Creatures only: the artifact that is not a creature is not offered.
    assert_offers(
        card,
        &offered,
        &[
            (P1, vec![seat.p1_a, seat.p1_b]),
            (P2, vec![seat.p2_a, seat.p2_b]),
        ],
    );
    for stolen in [seat.p1_b, seat.p2_a] {
        assert_eq!(controller(&r, stolen), P0, "stolen");
        assert!(
            !r.state().objects[&stolen].tapped,
            "untapped by the second sentence"
        );
        assert!(
            has(&r, stolen, &Keyword::Haste),
            "haste from the third sentence"
        );
    }
    for kept in [seat.p1_a, seat.p2_b] {
        assert_ne!(controller(&r, kept), P0, "not chosen, not stolen");
    }
    assert_eq!(controller(&r, p1_artifact), P1);
    assert!(
        has(&r, primordial, &Keyword::Haste),
        "Molten Primordial has haste itself"
    );
    assert_eq!(power_toughness(&r, primordial), (6, 4));

    // "Until end of turn": at cleanup the creatures go home (CR 514.2), losing the haste.
    let turn = r.state().turn_number;
    drive_until(&mut r, "next turn", |r| r.state().turn_number > turn);
    for (stolen, owner) in [(seat.p1_b, P1), (seat.p2_a, P2)] {
        assert_eq!(
            controller(&r, stolen),
            owner,
            "control returns at end of turn"
        );
        assert!(!has(&r, stolen, &Keyword::Haste));
    }
}

// ---- Shape 6: the same sentence on a "when you cast this spell" trigger, with a longer rider:
// "...gain control of up to one target creature that player controls until end of turn. Untap
// those creatures. They gain trample, haste, and annihilator 1 until end of turn." (Hideous
// Taskmaster). The trigger resolves before the creature spell does (CR 603.3, CR 608.1). ----
#[test]
fn hideous_taskmaster_cast_trigger_steals_and_empowers_one_creature_per_opponent() {
    let card = "Hideous Taskmaster";
    let (mut r, (taskmaster, seat, p1_artifact)) = table(|s| {
        (
            s.add_real_card(P0, card, Zone::Hand, db()),
            seats(s),
            s.add_artifact_from_oracle(P1, "P1 Trinket", "").id(),
        )
    });
    tap(&mut r, &[seat.p1_a, seat.p2_b]);
    let offered = cast_and_announce(&mut r, taskmaster, &[seat.p1_a, seat.p2_b]).expect("targets");
    assert_offers(
        card,
        &offered,
        &[
            (P1, vec![seat.p1_a, seat.p1_b]),
            (P2, vec![seat.p2_a, seat.p2_b]),
        ],
    );
    // The creature spell is on the stack under its own cast trigger.
    assert_eq!(
        r.state().stack.len(),
        2,
        "the creature spell and its cast trigger"
    );
    assert_eq!(zone_of(&r, taskmaster), Zone::Stack);
    for chosen in [seat.p1_a, seat.p2_b] {
        assert_ne!(
            controller(&r, chosen),
            P0,
            "nothing happens before the trigger resolves"
        );
    }
    // One resolution: the trigger. The creature spell is still on the stack afterwards.
    while r.state().stack.len() == 2 {
        r.act(GameAction::PassPriority).expect("pass");
    }
    assert_eq!(
        zone_of(&r, taskmaster),
        Zone::Stack,
        "the trigger resolved first"
    );
    let annihilator = Keyword::Annihilator(1);
    for stolen in [seat.p1_a, seat.p2_b] {
        assert_eq!(controller(&r, stolen), P0, "stolen");
        assert!(!r.state().objects[&stolen].tapped, "untapped");
        assert!(has(&r, stolen, &Keyword::Trample), "trample");
        assert!(has(&r, stolen, &Keyword::Haste), "haste");
        assert!(has(&r, stolen, &annihilator), "annihilator 1");
    }
    for kept in [seat.p1_b, seat.p2_a] {
        assert_ne!(controller(&r, kept), P0);
        assert!(!has(&r, kept, &Keyword::Trample));
    }
    assert_eq!(controller(&r, p1_artifact), P1);
    settle(&mut r, &[]);
    assert_eq!(zone_of(&r, taskmaster), Zone::Battlefield);
    assert_eq!(power_toughness(&r, taskmaster), (7, 2));
    assert!(
        r.state().objects[&taskmaster].color.is_empty(),
        "Devoid: colorless"
    );
    for printed in [Keyword::Trample, Keyword::Haste, annihilator.clone()] {
        assert!(
            has(&r, taskmaster, &printed),
            "Hideous Taskmaster has {printed:?} itself"
        );
    }

    // Until end of turn.
    let turn = r.state().turn_number;
    drive_until(&mut r, "next turn", |r| r.state().turn_number > turn);
    for (stolen, owner) in [(seat.p1_a, P1), (seat.p2_b, P2)] {
        assert_eq!(
            controller(&r, stolen),
            owner,
            "control returns at end of turn"
        );
        assert!(!has(&r, stolen, &Keyword::Trample));
        assert!(!has(&r, stolen, &Keyword::Haste));
        assert!(!has(&r, stolen, &annihilator));
    }
}
