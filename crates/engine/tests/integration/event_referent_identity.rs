//! CR 400.7: an event-context object referent ("it", "that creature") names the
//! object the trigger event named, and nothing once that object has become a new
//! object. Every hostile row changes the referent's zone (or phases it out)
//! between the event and the resolution; its positive control is the same board
//! without the response. All cards are the printed fixture cards.

use engine::game::combat::AttackTarget;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::TargetRef;

fn db() -> &'static engine::database::card_db::CardDatabase {
    crate::support::shared_card_db().expect("integration card fixture must load")
}

/// Passes priority, orders triggers, picks the first legal trigger target and
/// accepts optional effects.
fn step(r: &mut GameRunner) {
    match r.state().waiting_for.clone() {
        WaitingFor::Priority { .. } => {
            r.act(GameAction::PassPriority).expect("pass priority");
        }
        WaitingFor::OrderTriggers { triggers, .. } => {
            r.act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            })
            .expect("order triggers");
        }
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => {
            let target = selection
                .current_legal_targets
                .first()
                .cloned()
                .or_else(|| {
                    target_slots
                        .get(selection.current_slot)
                        .and_then(|slot| slot.legal_targets.first().cloned())
                });
            r.act(GameAction::ChooseTarget { target })
                .expect("choose trigger target");
        }
        WaitingFor::OptionalEffectChoice { .. } => {
            r.act(GameAction::DecideOptionalEffect { accept: true })
                .expect("accept optional");
        }
        other => panic!("unexpected prompt {other:?}"),
    }
}

fn drive(r: &mut GameRunner, stop: impl Fn(&GameRunner) -> bool) {
    for _ in 0..400 {
        if stop(r) {
            return;
        }
        step(r);
    }
    panic!("the game never reached the expected point");
}

fn priority(r: &GameRunner, player: PlayerId) -> bool {
    matches!(r.state().waiting_for, WaitingFor::Priority { player: p } if p == player)
}

fn settled(r: &GameRunner) -> bool {
    r.state().stack.is_empty() && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
}

fn pay(r: &mut GameRunner, player: PlayerId, mana: &[ManaType]) {
    for kind in mana {
        r.state_mut()
            .add_mana_to_pool(player, ManaUnit::new(*kind, ObjectId(0), false, vec![]));
    }
}

fn triggers_of(r: &GameRunner, source: ObjectId) -> Vec<ObjectId> {
    r.state()
        .stack
        .iter()
        .filter(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == source)
        })
        .map(|e| e.id)
        .collect()
}

fn incarnation(r: &GameRunner, id: ObjectId) -> u64 {
    r.state().objects[&id].incarnation
}

/// Casts `spell` (paid with `mana`) and resolves it, stopping at the next priority.
fn cast_and_resolve(
    r: &mut GameRunner,
    player: PlayerId,
    spell: ObjectId,
    mana: &[ManaType],
    targets: &[ObjectId],
) {
    pay(r, player, mana);
    r.cast(spell).target_objects(targets).commit();
    drive(r, |r| {
        r.state().objects[&spell].zone != Zone::Stack
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    });
}

/// Activates `source`'s ability `index`, answering a target or card prompt with `choose`.
fn activate(r: &mut GameRunner, source: ObjectId, index: usize, choose: &[ObjectId]) {
    r.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("activate");
    for _ in 0..10 {
        match r.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => return,
            WaitingFor::TargetSelection { .. } => {
                r.act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(choose[0])),
                })
                .expect("target");
            }
            WaitingFor::DiscardChoice { .. } | WaitingFor::PayCost { .. } => {
                r.act(GameAction::SelectCards {
                    cards: choose.to_vec(),
                })
                .expect("select cards");
            }
            other => panic!("activation prompt {other:?}"),
        }
    }
}

fn zone(r: &GameRunner, id: ObjectId) -> Zone {
    r.state().objects[&id].zone
}

// ---------------------------------------------------------------------------
// Destroy: Dinosaur Hunter ("Whenever this creature deals damage to a Dinosaur,
// destroy that creature."), #8554 / #8574.
// ---------------------------------------------------------------------------

/// Ram Through: Dinosaur Hunter deals damage to Colossal Dreadmaw; P1 holds
/// priority with Hunter's trigger on the stack. `extra` cards go to P1; then
/// P0's second Ram Through and its `p0` battlefield cards, in that order.
fn hunter_hit(
    extra: &[(&str, Zone)],
    p0: &[&str],
) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let hunter = s.add_real_card(P0, "Dinosaur Hunter", Zone::Battlefield, db());
    let dino = s.add_real_card(P1, "Colossal Dreadmaw", Zone::Battlefield, db());
    let ram = s.add_real_card(P0, "Ram Through", Zone::Hand, db());
    let mut extra: Vec<ObjectId> = extra
        .iter()
        .map(|(name, zone)| s.add_real_card(P1, name, *zone, db()))
        .collect();
    extra.push(s.add_real_card(P0, "Ram Through", Zone::Hand, db()));
    for name in p0 {
        extra.push(s.add_real_card(P0, name, Zone::Battlefield, db()));
    }
    let mut r = s.build();
    pay(&mut r, P0, &[ManaType::Green, ManaType::Colorless]);
    r.cast(ram).target_objects(&[hunter, dino]).commit();
    // With P0 permanents to activate, stop while P0 (the active player) holds priority.
    let holder = if p0.is_empty() { P1 } else { P0 };
    drive(&mut r, |r| {
        triggers_of(r, hunter).len() == 1 && priority(r, holder)
    });
    (r, hunter, dino, extra)
}

#[test]
fn destroy_control() {
    let (mut r, _, dino, _) = hunter_hit(&[], &[]);
    drive(&mut r, settled);
    assert_eq!(zone(&r, dino), Zone::Graveyard);
}

#[test]
fn destroy_spares_blinked_dinosaur() {
    let (mut r, _, dino, extra) = hunter_hit(&[("Ephemerate", Zone::Hand)], &[]);
    let before = incarnation(&r, dino);
    cast_and_resolve(&mut r, P1, extra[0], &[ManaType::White], &[dino]);
    assert_ne!(incarnation(&r, dino), before, "reach: blinked");
    drive(&mut r, settled);
    assert_eq!(zone(&r, dino), Zone::Battlefield, "CR 400.7: a new object");
}

/// Probe A: the same dealer deals an equal hit to the returned Dreadmaw; that
/// trigger is countered; the first trigger still names the first Dreadmaw.
#[test]
fn destroy_equal_hit_after_blink() {
    let (mut r, hunter, dino, extra) =
        hunter_hit(&[("Ephemerate", Zone::Hand), ("Stifle", Zone::Hand)], &[]);
    let t1 = triggers_of(&r, hunter)[0];
    let before = incarnation(&r, dino);
    cast_and_resolve(&mut r, P1, extra[0], &[ManaType::White], &[dino]);
    assert_ne!(incarnation(&r, dino), before, "reach: blinked");
    drive(&mut r, |r| priority(r, P0));
    pay(&mut r, P0, &[ManaType::Green, ManaType::Colorless]);
    r.cast(extra[2]).target_objects(&[hunter, dino]).commit();
    drive(&mut r, |r| {
        triggers_of(r, hunter).len() == 2 && priority(r, P1)
    });
    let t2 = *triggers_of(&r, hunter)
        .iter()
        .find(|id| **id != t1)
        .unwrap();
    cast_and_resolve(&mut r, P1, extra[1], &[ManaType::Blue], &[t2]);
    assert_eq!(triggers_of(&r, hunter), [t1], "reach: only T1 remains");
    drive(&mut r, settled);
    assert_eq!(
        zone(&r, dino),
        Zone::Battlefield,
        "T1 named the pre-blink Dreadmaw"
    );
}
#[test]
fn destroy_skips_phased_out_dinosaur() {
    let (mut r, _, dino, extra) = hunter_hit(&[("Slip Out the Back", Zone::Hand)], &[]);
    let before = incarnation(&r, dino);
    cast_and_resolve(&mut r, P1, extra[0], &[ManaType::Blue], &[dino]);
    assert!(
        !r.state().objects[&dino].is_phased_in(),
        "reach: phased out"
    );
    drive(&mut r, settled);
    let o = &r.state().objects[&dino];
    assert_eq!(o.zone, Zone::Battlefield, "CR 702.26b: not affected");
    assert_eq!(o.incarnation, before, "phasing keeps the object");
}

/// CR 608.2k: a referent whose characteristics change is still the same object.
#[test]
fn destroy_follows_characteristic_change() {
    let (mut r, _, dino, extra) = hunter_hit(&[("Amoeboid Changeling", Zone::Battlefield)], &[]);
    // Deck loading populates the creature-type universe; a GameScenario does not.
    r.state_mut().all_creature_types = vec!["Dinosaur".into(), "Shapeshifter".into()];
    activate(&mut r, extra[0], 1, &[dino]);
    drive(&mut r, |r| {
        r.state().stack.len() == 1 && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    });
    assert!(
        r.state().objects[&dino].card_types.subtypes.is_empty(),
        "reach: no longer a Dinosaur"
    );
    drive(&mut r, settled);
    assert_eq!(zone(&r, dino), Zone::Graveyard);
}

// ---------------------------------------------------------------------------
// Destroy the damage source: No Mercy ("Whenever a creature deals damage to
// you, destroy it.").
// ---------------------------------------------------------------------------

/// Steps until a trigger asks for its target, then answers `target`.
fn aim_trigger(r: &mut GameRunner, target: TargetRef) {
    for _ in 0..50 {
        if matches!(
            r.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            r.act(GameAction::ChooseTarget {
                target: Some(target),
            })
            .expect("aim trigger");
            return;
        }
        step(r);
    }
    panic!("no trigger target prompt");
}

/// P0 casts Sonic Shrieker ("When this creature enters, it deals 2 damage to any
/// target and you gain 2 life. If a player is dealt damage this way, they discard
/// a card."); its trigger T1 aims at P1, who controls No Mercy. With `blink`, P0
/// Ephemerates the Shrieker in response and aims the new trigger at itself, so
/// only T1, dealt by the departed Shrieker, damages P1. Hands are empty, so no
/// discard prompt. Returns the Shrieker's zone once settled.
fn shrieker(blink: bool) -> Zone {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let shrieker = s.add_real_card(P0, "Sonic Shrieker", Zone::Hand, db());
    let eph = s.add_real_card(P0, "Ephemerate", Zone::Hand, db());
    let mercy = s.add_real_card(P1, "No Mercy", Zone::Battlefield, db());
    let mut r = s.build();
    pay(
        &mut r,
        P0,
        &[
            ManaType::Red,
            ManaType::White,
            ManaType::Black,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    r.cast(shrieker).commit();
    aim_trigger(&mut r, TargetRef::Player(P1));
    assert_eq!(triggers_of(&r, shrieker).len(), 1, "reach: T1 aims at P1");
    if blink {
        let before = incarnation(&r, shrieker);
        pay(&mut r, P0, &[ManaType::White]);
        r.cast(eph).target_object(shrieker).commit();
        aim_trigger(&mut r, TargetRef::Player(P0));
        assert_ne!(incarnation(&r, shrieker), before, "reach: blinked");
        assert_eq!(triggers_of(&r, shrieker).len(), 2, "reach: T1 and T2");
    }
    drive(&mut r, |r| triggers_of(r, mercy).len() == 1);
    drive(&mut r, settled);
    zone(&r, shrieker)
}

#[test]
fn damage_source_control_is_destroyed() {
    assert_eq!(shrieker(false), Zone::Graveyard);
}

/// CR 400.7 + CR 113.7a: T1's damage was dealt by the departed Shrieker; No
/// Mercy's "it" names that object, not the returned one.
#[test]
fn damage_source_spares_blinked_dealer() {
    assert_eq!(shrieker(true), Zone::Battlefield);
}

// ---------------------------------------------------------------------------
// Return + continuous rider: Dread Slaver ("Whenever a creature dealt damage by
// this creature this turn dies, return it to the battlefield under your
// control. That creature is a black Zombie ..."), #11034.
// ---------------------------------------------------------------------------

/// Dread Slaver attacks; Snapcaster Mage blocks and dies; P1 holds priority
/// with Slaver's trigger on the stack.
fn slaver_kill(p1_hand: &[&str]) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let slaver = s.add_real_card(P0, "Dread Slaver", Zone::Battlefield, db());
    let snap = s.add_real_card(P1, "Snapcaster Mage", Zone::Battlefield, db());
    let hand = p1_hand
        .iter()
        .map(|name| s.add_real_card(P1, name, Zone::Hand, db()))
        .collect();
    let mut r = s.build();
    drive(&mut r, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. })
    });
    r.act(GameAction::DeclareAttackers {
        attacks: vec![(slaver, AttackTarget::Player(P1))],
        bands: vec![],
    })
    .expect("attack");
    drive(&mut r, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. })
    });
    r.act(GameAction::DeclareBlockers {
        assignments: vec![(snap, slaver)],
    })
    .expect("block");
    drive(&mut r, |r| {
        triggers_of(r, slaver).len() == 1 && priority(r, P1)
    });
    assert_eq!(zone(&r, snap), Zone::Graveyard, "reach: died");
    (r, snap, hand)
}

fn black_zombie(r: &GameRunner, id: ObjectId) -> bool {
    let o = &r.state().objects[&id];
    o.color.contains(&ManaColor::Black) || o.card_types.subtypes.iter().any(|t| t == "Zombie")
}

#[test]
fn return_and_rider_control() {
    let (mut r, snap, _) = slaver_kill(&[]);
    drive(&mut r, |r| r.state().stack.is_empty());
    let o = &r.state().objects[&snap];
    assert_eq!((o.zone, o.controller), (Zone::Battlefield, P0));
    assert!(
        o.color.contains(&ManaColor::Black) && o.card_types.subtypes.iter().any(|t| t == "Zombie")
    );
}

#[test]
fn rider_skips_shallow_grave_object() {
    let (mut r, snap, hand) = slaver_kill(&["Shallow Grave"]);
    cast_and_resolve(
        &mut r,
        P1,
        hand[0],
        &[ManaType::Black, ManaType::Colorless],
        &[],
    );
    assert_eq!(
        (zone(&r, snap), r.state().objects[&snap].controller),
        (Zone::Battlefield, P1),
        "reach"
    );
    drive(&mut r, |r| r.state().stack.is_empty());
    assert_eq!(r.state().objects[&snap].controller, P1);
    assert!(
        !black_zombie(&r, snap),
        "CR 400.7: Shallow Grave's Snapcaster is a new object"
    );
}

#[test]
fn return_skips_graveyard_round_trip() {
    let (mut r, snap, hand) = slaver_kill(&["Coffin Purge", "Pull from Eternity"]);
    let before = incarnation(&r, snap);
    cast_and_resolve(&mut r, P1, hand[0], &[ManaType::Black], &[snap]);
    drive(&mut r, |r| priority(r, P1));
    cast_and_resolve(&mut r, P1, hand[1], &[ManaType::White], &[snap]);
    assert_eq!(
        zone(&r, snap),
        Zone::Graveyard,
        "reach: back in the graveyard"
    );
    assert_ne!(incarnation(&r, snap), before, "reach: a new object");
    drive(&mut r, |r| r.state().stack.is_empty());
    assert_eq!(zone(&r, snap), Zone::Graveyard);
    assert!(!black_zombie(&r, snap));
}

// ---------------------------------------------------------------------------
// Attach: Novel Nunchaku ("When this Equipment enters, attach it to target
// creature you control. When you do, equipped creature fights ..."), #9614.
// ---------------------------------------------------------------------------

/// Nunchaku's first enters trigger targets Host A; with `flicker`, Scrollshift
/// returns the Nunchaku, whose new trigger targets Host B. Returns the attach
/// host and the number of fights (damage records to the Victim).
fn nunchaku(flicker: bool) -> (Option<AttachTarget>, usize, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let host_a = s.add_creature(P0, "Host A", 2, 2).id();
    let host_b = s.add_creature(P0, "Host B", 3, 3).id();
    let victim = s.add_creature(P1, "Victim", 1, 9).id();
    let nun = s.add_real_card(P0, "Novel Nunchaku", Zone::Hand, db());
    let shift = s.add_real_card(P0, "Scrollshift", Zone::Hand, db());
    for i in 0..5 {
        s.add_card_to_library_top(P0, &format!("Library {i}"));
    }
    let mut r = s.build();
    pay(
        &mut r,
        P0,
        &[ManaType::Green, ManaType::Colorless, ManaType::Colorless],
    );
    r.cast(nun).commit();
    let choose = |r: &mut GameRunner, want: ObjectId| {
        let WaitingFor::TriggerTargetSelection { selection, .. } = r.state().waiting_for.clone()
        else {
            return false;
        };
        let legal = selection.current_legal_targets;
        let pick = [want, victim]
            .into_iter()
            .map(TargetRef::Object)
            .find(|t| legal.contains(t))
            .or_else(|| legal.first().cloned());
        r.act(GameAction::ChooseTarget { target: pick })
            .expect("choose");
        true
    };
    for _ in 0..100 {
        if triggers_of(&r, nun).len() == 1 && priority(&r, P0) {
            break;
        }
        if !choose(&mut r, host_a) {
            step(&mut r);
        }
    }
    assert_eq!(
        triggers_of(&r, nun).len(),
        1,
        "reach: enters trigger waiting"
    );
    if flicker {
        let before = incarnation(&r, nun);
        pay(
            &mut r,
            P0,
            &[ManaType::White, ManaType::Colorless, ManaType::Colorless],
        );
        r.cast(shift).target_object(nun).commit();
        drive(&mut r, |r| r.state().objects[&shift].zone != Zone::Stack);
        assert_ne!(incarnation(&r, nun), before, "reach: flickered");
    }
    for _ in 0..300 {
        if settled(&r) {
            break;
        }
        if !choose(&mut r, host_b) {
            step(&mut r);
        }
    }
    assert!(settled(&r), "the chain completes");
    let fights = r
        .state()
        .damage_dealt_this_turn
        .iter()
        .filter(|d| d.target == TargetRef::Object(victim))
        .count();
    (r.state().objects[&nun].attached_to, fights, host_a, host_b)
}

#[test]
fn attach_control() {
    let (host, fights, host_a, _) = nunchaku(false);
    assert_eq!((host, fights), (Some(AttachTarget::Object(host_a)), 1));
}

#[test]
fn attach_skips_flickered_equipment() {
    let (host, fights, _, host_b) = nunchaku(true);
    assert_eq!(
        (host, fights),
        (Some(AttachTarget::Object(host_b)), 1),
        "CR 400.7: the first trigger can't move the new Nunchaku, so no second fight"
    );
}

// ---------------------------------------------------------------------------
// Becomes saddled: Kolodin, Triumph Caster ("Whenever a Mount you control
// enters, it becomes saddled until end of turn.").
// ---------------------------------------------------------------------------

fn kolodin(blink: bool) -> bool {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let kolodin = s.add_real_card(P0, "Kolodin, Triumph Caster", Zone::Battlefield, db());
    let arynx = s.add_real_card(P1, "Trained Arynx", Zone::Graveyard, db());
    let reanimate = s.add_real_card(P0, "Reanimate", Zone::Hand, db());
    let eph = s.add_real_card(P0, "Ephemerate", Zone::Hand, db());
    let mut r = s.build();
    pay(&mut r, P0, &[ManaType::Black]);
    r.cast(reanimate).target_object(arynx).commit();
    drive(&mut r, |r| {
        triggers_of(r, kolodin).len() == 1 && priority(r, P0)
    });
    if blink {
        cast_and_resolve(&mut r, P0, eph, &[ManaType::White], &[arynx]);
        assert_eq!(
            r.state().objects[&arynx].controller,
            P1,
            "reach: returned to its owner"
        );
    }
    drive(&mut r, settled);
    r.state().objects[&arynx].is_saddled
}

#[test]
fn saddle_control() {
    assert!(kolodin(false));
}

#[test]
fn saddle_skips_blinked_mount() {
    assert!(
        !kolodin(true),
        "CR 400.7: the returned Arynx is a new object"
    );
}

// ---------------------------------------------------------------------------
// Untargeted "it" on the ability's own source: Mana Bloom, #9651 (fixed by
// #10699; pinned here).
// ---------------------------------------------------------------------------

fn mana_bloom(flicker: bool) -> Zone {
    let mut s = GameScenario::new();
    s.at_phase(Phase::Untap);
    let bloom = s.add_real_card(P0, "Mana Bloom", Zone::Battlefield, db());
    let fof = s.add_real_card(P0, "Flicker of Fate", Zone::Hand, db());
    for i in 0..5 {
        s.add_card_to_library_top(P0, &format!("Library {i}"));
    }
    let mut r = s.build();
    drive(&mut r, |r| {
        triggers_of(r, bloom).len() == 1 && priority(r, P0)
    });
    if flicker {
        let before = incarnation(&r, bloom);
        cast_and_resolve(
            &mut r,
            P0,
            fof,
            &[ManaType::White, ManaType::Colorless],
            &[bloom],
        );
        assert_ne!(incarnation(&r, bloom), before, "reach: flickered");
    }
    drive(&mut r, settled);
    zone(&r, bloom)
}

#[test]
fn mana_bloom_control() {
    assert_eq!(mana_bloom(false), Zone::Hand);
}

#[test]
fn mana_bloom_flicker_stays() {
    assert_eq!(mana_bloom(true), Zone::Battlefield);
}

// ---------------------------------------------------------------------------
// Optional exile of a dead source: Academy Rector (#10708 section 1).
// ---------------------------------------------------------------------------

/// Academy Rector dies to Shock. With `round_trip`, P1 exiles the dead Rector
/// with Coffin Purge and returns it with Pull from Eternity before its trigger
/// resolves. Drives to a settled game, declining any prompt, and returns
/// whether the "you may exile it" prompt was offered, then the Rector's and the
/// enchantment's zones.
fn rector(round_trip: bool) -> (bool, Zone, Zone) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let rector = s.add_real_card(P0, "Academy Rector", Zone::Battlefield, db());
    let shock = s.add_real_card(P0, "Shock", Zone::Hand, db());
    let anthem = s.add_real_card(P0, "Glorious Anthem", Zone::Library, db());
    let purge = s.add_real_card(P1, "Coffin Purge", Zone::Hand, db());
    let pull = s.add_real_card(P1, "Pull from Eternity", Zone::Hand, db());
    let mut r = s.build();
    pay(&mut r, P0, &[ManaType::Red]);
    r.cast(shock).target_object(rector).commit();
    drive(&mut r, |r| {
        triggers_of(r, rector).len() == 1 && priority(r, P1)
    });
    if round_trip {
        let before = incarnation(&r, rector);
        cast_and_resolve(&mut r, P1, purge, &[ManaType::Black], &[rector]);
        drive(&mut r, |r| priority(r, P1));
        cast_and_resolve(&mut r, P1, pull, &[ManaType::White], &[rector]);
        assert_eq!(zone(&r, rector), Zone::Graveyard, "reach: round trip");
        assert_ne!(incarnation(&r, rector), before, "reach: a new object");
    }
    let mut offered = false;
    for _ in 0..400 {
        if settled(&r) {
            break;
        }
        if matches!(
            r.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            offered = true;
            r.act(GameAction::DecideOptionalEffect { accept: false })
                .expect("decline optional");
            continue;
        }
        step(&mut r);
    }
    assert!(settled(&r), "the game never settled");
    (offered, zone(&r, rector), zone(&r, anthem))
}

/// CR 608.2d: an impossible option isn't offered. The round-tripped Rector is a
/// new object, so "exile it" can't be done and the prompt never appears.
#[test]
fn rector_round_trip_offers_no_exile() {
    assert_eq!(rector(true), (false, Zone::Graveyard, Zone::Library));
}

#[test]
fn rector_control_offers_exile() {
    assert_eq!(rector(false), (true, Zone::Graveyard, Zone::Library));
}

// ---------------------------------------------------------------------------
// Perpetual: Volatile Rift (hand arrival), Mephit's Enthusiasm (cast spell).
// ---------------------------------------------------------------------------

/// Volatile Rift's drawn Grizzly Bears, optionally discarded to Psychic Frog and
/// returned with Phyrexian Reclamation before the Rift trigger resolves.
fn rift(round_trip: bool) -> Option<i32> {
    let mut s = GameScenario::new();
    s.at_phase(Phase::Upkeep);
    let rift = s.add_real_card(P0, "Volatile Rift", Zone::Battlefield, db());
    let frog = s.add_real_card(P0, "Psychic Frog", Zone::Battlefield, db());
    let recl = s.add_real_card(P0, "Phyrexian Reclamation", Zone::Battlefield, db());
    let bears = s.add_real_card(P0, "Grizzly Bears", Zone::Library, db());
    let mut r = s.build();
    drive(&mut r, |r| {
        zone(r, bears) == Zone::Hand && triggers_of(r, rift).len() == 1 && priority(r, P0)
    });
    if round_trip {
        let before = incarnation(&r, bears);
        activate(&mut r, frog, 0, &[bears]);
        drive(&mut r, |r| r.state().stack.len() == 1 && priority(r, P0));
        pay(&mut r, P0, &[ManaType::Black, ManaType::Colorless]);
        activate(&mut r, recl, 0, &[bears]);
        drive(&mut r, |r| r.state().stack.len() == 1 && priority(r, P0));
        assert_eq!(zone(&r, bears), Zone::Hand, "reach: back in hand");
        assert_ne!(incarnation(&r, bears), before, "reach: a new object");
        assert_eq!(
            triggers_of(&r, rift).len(),
            1,
            "reach: Rift trigger waiting"
        );
    }
    drive(&mut r, settled);
    r.state().objects[&bears].base_power
}

#[test]
fn perpetual_control() {
    assert_eq!(rift(false), Some(6));
}

#[test]
fn perpetual_skips_round_tripped_card() {
    assert_eq!(
        rift(true),
        Some(2),
        "CR 400.7: the returned Bears are a new object"
    );
}

#[test]
fn mephit_boon_on_cast_creature() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let target = s.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db());
    let mephit = s.add_real_card(P0, "Mephit's Enthusiasm", Zone::Hand, db());
    let bears = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db());
    let mut r = s.build();
    cast_and_resolve(
        &mut r,
        P0,
        mephit,
        &[ManaType::Red, ManaType::Colorless],
        &[target],
    );
    drive(&mut r, |r| settled(r) && priority(r, P0));
    cast_and_resolve(
        &mut r,
        P0,
        bears,
        &[ManaType::Green, ManaType::Colorless],
        &[],
    );
    drive(&mut r, settled);
    assert_eq!(
        r.state().objects[&bears].power,
        Some(4),
        "the boon names the cast spell"
    );
}

// ---------------------------------------------------------------------------
// Information reads keep reading the event referent (CR 608.2h).
// ---------------------------------------------------------------------------

/// Amareth: "If it shares a card type with that permanent" is an information
/// read; it still reads the creature that was blinked in response.
#[test]
fn amareth_information_read_survives_flicker() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let amareth = s.add_real_card(P0, "Amareth, the Lustrous", Zone::Battlefield, db());
    let bears = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db());
    let eph = s.add_real_card(P0, "Ephemerate", Zone::Hand, db());
    let top = s.add_real_card(P0, "Colossal Dreadmaw", Zone::Library, db());
    let second = s.add_real_card(P0, "Grizzly Bears", Zone::Library, db());
    let mut r = s.build();
    cast_and_resolve(
        &mut r,
        P0,
        bears,
        &[ManaType::Green, ManaType::Colorless],
        &[],
    );
    drive(&mut r, |r| {
        triggers_of(r, amareth).len() == 1 && priority(r, P0)
    });
    let before = incarnation(&r, bears);
    cast_and_resolve(&mut r, P0, eph, &[ManaType::White], &[bears]);
    assert_ne!(incarnation(&r, bears), before, "reach: blinked");
    assert_eq!(
        triggers_of(&r, amareth).len(),
        2,
        "reach: the re-entry trigger"
    );
    drive(&mut r, settled);
    // The re-entry trigger resolves first and takes one card; the first trigger
    // still reads the blinked Bears' card type and takes the other.
    assert_eq!(
        (zone(&r, top), zone(&r, second)),
        (Zone::Hand, Zone::Hand),
        "both creature cards are put into hand"
    );
}

/// The Ozolith reads the departed creature's counters (CR 122.8 + 608.2h).
#[test]
fn ozolith_reads_departed_counters() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let ozolith = s.add_real_card(P0, "The Ozolith", Zone::Battlefield, db());
    let frog = s.add_real_card(P0, "Psychic Frog", Zone::Battlefield, db());
    s.with_counter(frog, CounterType::Plus1Plus1, 3);
    let eph = s.add_real_card(P0, "Ephemerate", Zone::Hand, db());
    let mut r = s.build();
    pay(&mut r, P0, &[ManaType::White]);
    r.cast(eph).target_object(frog).commit();
    drive(&mut r, settled);
    let counters = r.state().objects[&ozolith]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied();
    assert_eq!(counters, Some(3));
}

// ---------------------------------------------------------------------------
// Lifecycle: delayed snapshots and copies.
// ---------------------------------------------------------------------------

/// Ohran Viper: "destroy that creature at end of combat" after its blocker is
/// blinked once the delayed trigger exists.
fn ohran(blink: bool) -> Zone {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let viper = s.add_real_card(P0, "Ohran Viper", Zone::Battlefield, db());
    let bears = s.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db());
    let eph = s.add_real_card(P1, "Ephemerate", Zone::Hand, db());
    let mut r = s.build();
    drive(&mut r, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. })
    });
    r.act(GameAction::DeclareAttackers {
        attacks: vec![(viper, AttackTarget::Player(P1))],
        bands: vec![],
    })
    .expect("attack");
    drive(&mut r, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. })
    });
    r.act(GameAction::DeclareBlockers {
        assignments: vec![(bears, viper)],
    })
    .expect("block");
    drive(&mut r, |r| {
        !r.state().delayed_triggers.is_empty() && settled(r) && priority(r, P1)
    });
    if blink {
        let before = incarnation(&r, bears);
        cast_and_resolve(&mut r, P1, eph, &[ManaType::White], &[bears]);
        assert_ne!(incarnation(&r, bears), before, "reach: blinked");
    }
    drive(&mut r, |r| {
        r.state().delayed_triggers.is_empty() && settled(r)
    });
    zone(&r, bears)
}

#[test]
fn delayed_snapshot_control() {
    assert_eq!(ohran(false), Zone::Graveyard);
}

#[test]
fn delayed_snapshot_spares_blinked_creature() {
    assert_eq!(ohran(true), Zone::Battlefield);
}

/// One Hunter hit; P0 copies the trigger with Strionic Resonator (CR 707.10:
/// the copy names the same event referent); optionally the Dreadmaw blinks.
fn resonator(blink: bool) -> Zone {
    let (mut r, hunter, dino, extra) =
        hunter_hit(&[("Ephemerate", Zone::Hand)], &["Strionic Resonator"]);
    let reso = extra[2];
    let t1 = triggers_of(&r, hunter)[0];
    pay(&mut r, P0, &[ManaType::Colorless, ManaType::Colorless]);
    activate(&mut r, reso, 0, &[t1]);
    // CR 117.4: the Resonator's ability resolves first and puts the copy on the stack.
    drive(&mut r, |r| {
        triggers_of(r, hunter).len() == 2 && priority(r, P1)
    });
    let pending = triggers_of(&r, hunter);
    assert_eq!(r.state().stack.len(), 2, "reach: only T1 and its copy");
    assert!(
        pending.contains(&t1) && pending[0] != pending[1],
        "reach: T1 and a distinct copy"
    );
    if blink {
        let before = incarnation(&r, dino);
        cast_and_resolve(&mut r, P1, extra[0], &[ManaType::White], &[dino]);
        assert_ne!(incarnation(&r, dino), before, "reach: blinked");
        assert_eq!(r.state().stack.len(), 2, "reach: T1 and copy pending");
    }
    drive(&mut r, settled);
    zone(&r, dino)
}

#[test]
fn copied_trigger_control() {
    assert_eq!(resonator(false), Zone::Graveyard);
}

#[test]
fn copied_trigger_after_blink() {
    assert_eq!(resonator(true), Zone::Battlefield);
}
