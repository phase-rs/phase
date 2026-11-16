//! Declared-target mana-value gates: Reject Imperfection and Sound the Trumpets (countered spell, CR 608.2h last
//! known information of an object that moved stack -> graveyard), Carnivorous Canopy (destroyed permanent),
//! Vindictive Triumph (exiled permanent). Each negative is paired with a reach guard.

use super::fixture::*;
use super::oracle::*;
use engine::game::scenario::{GameRunner, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::zones::Zone;

/// P0 casts a textless filler creature spell of mana value `spell_mv`; P1 answers with the real counterspell
/// `counter_name` (printed text `counter_oracle`, printed cost `counter_cost`) through the production cast path.
fn counter_flow(
    counter_name: &str,
    counter_oracle: &str,
    counter_cost: &str,
    spell_mv: u32,
    hand_for_p1: impl Fn(&mut engine::game::scenario::GameScenario),
) -> (GameRunner, WaitingFor, ObjectId) {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[], spell_mv as usize));
    sc.with_mana_pool(P1, mana_for(&[counter_cost], 0));
    add_counter_holder(&mut sc);
    sc.with_library_top(P1, &["Island", "Island", "Island"]);
    let target = {
        let mut b = sc.add_creature_to_hand(P0, "Filler Spell", 2, 2);
        b.with_mana_cost(ManaCost::generic(spell_mv));
        b.id()
    };
    hand_for_p1(&mut sc);
    let counter = sc
        .add_spell_to_hand_from_oracle(P1, counter_name, true, counter_oracle)
        .with_mana_cost(cost(counter_cost))
        .id();
    let mut runner = sc.build();
    cast_spell(&mut runner, target);
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    cast_spell(&mut runner, counter);
    let w = drain(&mut runner);
    (runner, w, target)
}

// ---- Reject Imperfection: "If that spell's mana value was 3 or less, proliferate." (CR 701.34a, CR 608.2h) ----

fn ri(mv: u32) -> (GameRunner, WaitingFor, ObjectId) {
    counter_flow(
        "Reject Imperfection",
        REJECT_IMPERFECTION,
        REJECT_IMPERFECTION_COST,
        mv,
        |_| {},
    )
}

#[test]
fn p3s2a_reject_imperfection_mv2_proliferates() {
    let (r, w, target) = ri(2);
    assert_eq!(
        zone(r.state(), target),
        Zone::Graveyard,
        "reach guard: the spell was countered"
    );
    assert!(
        is_proliferate(&w),
        "MV 2 <= 3: the caster is offered the proliferate choice, got {w:?}"
    );
}

#[test]
fn p3s2a_reject_imperfection_mv5_does_not_proliferate() {
    let (r, w, target) = ri(5);
    assert_eq!(
        zone(r.state(), target),
        Zone::Graveyard,
        "reach guard: the spell was countered"
    );
    assert_eq!(
        plus_counters(
            r.state(),
            r.state()
                .objects
                .values()
                .find(|o| o.name == COUNTER_HOLDER)
                .unwrap()
                .id
        ),
        1,
        "reach guard: a permanent with a counter exists for proliferate to offer"
    );
    assert!(!is_proliferate(&w), "MV 5 > 3: no proliferate, got {w:?}");
    assert!(
        matches!(w, WaitingFor::Priority { .. }),
        "resolution completed, got {w:?}"
    );
}

/// Walking Ballista ({X}{X}) cast with X=`x` and countered by Reject Imperfection (CR 202.3e: X counts on the
/// stack, 0 elsewhere). Returns (mana value on the stack, mana value in the graveyard, final waiting state).
fn ri_ballista(x: u32) -> (u32, u32, WaitingFor) {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[WALKING_BALLISTA_COST], 2 * x as usize));
    sc.with_mana_pool(P1, mana_for(&[REJECT_IMPERFECTION_COST], 0));
    add_counter_holder(&mut sc);
    let ballista = sc
        .add_creature_to_hand_from_oracle(P0, "Walking Ballista", 0, 0, WALKING_BALLISTA)
        .with_mana_cost(cost(WALKING_BALLISTA_COST))
        .id();
    let ri = sc
        .add_spell_to_hand_from_oracle(P1, "Reject Imperfection", true, REJECT_IMPERFECTION)
        .with_mana_cost(cost(REJECT_IMPERFECTION_COST))
        .id();
    let mut r = sc.build();
    cast_spell(&mut r, ballista);
    choose_x_if_asked(&mut r, x);
    assert_eq!(
        zone(r.state(), ballista),
        Zone::Stack,
        "the X spell is on the stack"
    );
    let stack_mv = r.state().objects[&ballista]
        .mana_cost
        .mana_value_with_x(Zone::Stack, Some(x));
    r.act(GameAction::PassPriority).expect("P0 passes");
    cast_spell(&mut r, ri);
    let w = drain(&mut r);
    assert_eq!(
        zone(r.state(), ballista),
        Zone::Graveyard,
        "reach guard: Ballista was countered"
    );
    let gy_mv = r.state().objects[&ballista]
        .mana_cost
        .mana_value_with_x(Zone::Graveyard, None);
    (stack_mv, gy_mv, w)
}

#[test]
fn p3s2a_reject_imperfection_ballista_x2_reads_stack_value() {
    let (stack_mv, gy_mv, w) = ri_ballista(2);
    assert_eq!(
        (stack_mv, gy_mv),
        (4, 0),
        "operands differ: stack MV 4 (X=2), graveyard MV 0"
    );
    assert!(
        !is_proliferate(&w),
        "stack MV 4 > 3: no proliferate (a live graveyard read of 0 would prompt), got {w:?}"
    );
}

#[test]
fn p3s2a_reject_imperfection_ballista_x1_proliferates() {
    let (stack_mv, gy_mv, w) = ri_ballista(1);
    assert_eq!(
        (stack_mv, gy_mv),
        (2, 0),
        "stack MV 2 (X=1), graveyard MV 0"
    );
    assert!(
        is_proliferate(&w),
        "stack MV 2 <= 3: proliferate, got {w:?}"
    );
}

// ---- Sound the Trumpets: "If that spell's mana value was 2 or less, recruit." (CR 701.70a) ----

fn trumpets(mv: u32) -> (GameRunner, WaitingFor, ObjectId) {
    counter_flow(
        "Sound the Trumpets",
        SOUND_THE_TRUMPETS,
        SOUND_THE_TRUMPETS_COST,
        mv,
        |_| {},
    )
}

#[test]
fn p3s2a_sound_the_trumpets_mv2_recruits() {
    let (r, w, target) = trumpets(2);
    assert_eq!(
        zone(r.state(), target),
        Zone::Graveyard,
        "reach guard: countered"
    );
    assert_eq!(
        r.state().players[1].library.len(),
        2,
        "recruit draws a card (library 3 -> 2); waiting {w:?}"
    );
    // CR 701.70a: draw, then discard; P1's hand is the one drawn Island, so the discard is forced and automatic.
    assert_eq!(
        r.state().players[1].graveyard.len(),
        2,
        "Sound the Trumpets and the discarded Island; waiting {w:?}"
    );
}

#[test]
fn p3s2a_sound_the_trumpets_mv3_suppresses_the_whole_recruit() {
    let (r, w, target) = trumpets(3);
    assert_eq!(
        zone(r.state(), target),
        Zone::Graveyard,
        "reach guard: countered"
    );
    assert_eq!(
        r.state().players[1].library.len(),
        3,
        "no draw: the recruit head is gated"
    );
    assert!(
        matches!(w, WaitingFor::Priority { .. }),
        "no discard decision, got {w:?}"
    );
}

/// CR 701.70a: recruit = draw, discard, then a 1/1 white Human Soldier token iff a nonland card was discarded.
fn recruit_discarding(land: bool) -> usize {
    let (mut r, mut w, _t) = counter_flow(
        "Sound the Trumpets",
        SOUND_THE_TRUMPETS,
        SOUND_THE_TRUMPETS_COST,
        2,
        |sc| {
            if land {
                sc.add_land_to_hand(P1, "Held Land");
            } else {
                sc.add_creature_to_hand(P1, "Held Bear", 2, 2);
            }
        },
    );
    assert!(
        matches!(w, WaitingFor::DiscardChoice { .. }),
        "reach guard: discard decision offered, got {w:?}"
    );
    let held = r
        .state()
        .objects
        .values()
        .find(|o| o.name == if land { "Held Land" } else { "Held Bear" })
        .expect("held card")
        .id;
    r.act(GameAction::SelectCards { cards: vec![held] })
        .expect("discard the held card");
    w = drain(&mut r);
    assert!(
        matches!(w, WaitingFor::Priority { .. }),
        "resolution completed, got {w:?}"
    );
    r.state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.controller == P1 && o.is_token)
        .count()
}

#[test]
fn p3s2a_sound_the_trumpets_nonland_discard_makes_a_token() {
    assert_eq!(
        recruit_discarding(false),
        1,
        "nonland discarded: one Human Soldier token"
    );
}

#[test]
fn p3s2a_sound_the_trumpets_land_discard_makes_no_token() {
    assert_eq!(
        recruit_discarding(true),
        0,
        "land discarded: no token (reach: the discard happened)"
    );
}

// ---- Carnivorous Canopy / Vindictive Triumph: destroy / exile a battlefield permanent ----

fn bf_flow(
    name: &str,
    oracle: &str,
    printed_cost: &str,
    mvn: u32,
    configure: impl Fn(&mut engine::game::scenario::CardBuilder<'_>),
) -> (GameRunner, ObjectId, WaitingFor) {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[printed_cost], 0));
    add_counter_holder(&mut sc);
    let victim = {
        let mut b = sc.add_creature(P1, "Filler Victim", 2, 2);
        b.with_mana_cost(ManaCost::generic(mvn));
        configure(&mut b);
        b.id()
    };
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, name, name == "Vindictive Triumph", oracle)
        .with_mana_cost(cost(printed_cost))
        .id();
    let mut runner = sc.build();
    let out = runner.cast(spell).target_object(victim).resolve();
    let w = out.final_waiting_for().clone();
    (runner, victim, w)
}

fn canopy(mvn: u32, indestructible: bool) -> (GameRunner, ObjectId, WaitingFor) {
    bf_flow(
        "Carnivorous Canopy",
        CARNIVOROUS_CANOPY,
        CARNIVOROUS_CANOPY_COST,
        mvn,
        |b| {
            b.flying();
            if indestructible {
                b.indestructible();
            }
        },
    )
}

#[test]
fn p3s2a_canopy_mv2_destroys_then_proliferates() {
    let (r, v, w) = canopy(2, false);
    assert_eq!(
        zone(r.state(), v),
        Zone::Graveyard,
        "reach guard: destroyed"
    );
    assert!(
        is_proliferate(&w),
        "MV 2 <= 3: proliferate after the destroy, got {w:?}"
    );
}

#[test]
fn p3s2a_canopy_mv5_destroys_without_proliferate() {
    let (r, v, w) = canopy(5, false);
    assert_eq!(
        zone(r.state(), v),
        Zone::Graveyard,
        "reach guard: destroyed"
    );
    assert!(!is_proliferate(&w), "MV 5 > 3: no proliferate, got {w:?}");
    assert!(
        matches!(w, WaitingFor::Priority { .. }),
        "resolution completed, got {w:?}"
    );
}

/// CR 608.2h: the destroy does not happen (indestructible), the permanent is still where it was expected to be,
/// so its current information (MV 2) answers "was 3 or less": the proliferate still happens.
#[test]
fn p3s2a_canopy_indestructible_target_is_read_live() {
    let (r, v, w) = canopy(2, true);
    assert_eq!(
        zone(r.state(), v),
        Zone::Battlefield,
        "indestructible: not destroyed"
    );
    assert!(
        is_proliferate(&w),
        "current MV 2 <= 3: proliferate, got {w:?}"
    );
}

fn triumph(mvn: u32) -> (GameRunner, ObjectId, WaitingFor) {
    bf_flow(
        "Vindictive Triumph",
        VINDICTIVE_TRIUMPH,
        VINDICTIVE_TRIUMPH_COST,
        mvn,
        |_| {},
    )
}

#[test]
fn p3s2a_triumph_mv2_returns_tapped_under_casters_control() {
    let (r, v, _w) = triumph(2);
    let o = &r.state().objects[&v];
    assert_eq!(
        o.zone,
        Zone::Battlefield,
        "MV 2 <= 3: returned to the battlefield"
    );
    assert_eq!(o.controller, P0, "under the caster's control");
    assert!(o.tapped, "tapped");
}

#[test]
fn p3s2a_triumph_mv5_stays_exiled() {
    let (r, v, _w) = triumph(5);
    assert_eq!(
        zone(r.state(), v),
        Zone::Exile,
        "MV 5 > 3: stays in exile (reach: it was exiled)"
    );
}

/// Ruling 2026-08-21 (Vindictive Triumph): a permanent with {X} in its cost has X = 0 for its mana value on the
/// battlefield (CR 202.3e). Walking Ballista cast with X=2 (MV 4 on the stack, 0 on the battlefield) is exiled
/// by Triumph and RETURNS. It enters with X=0 counters and dies to state-based actions afterwards, so the
/// return is observed by its zone-change event to the battlefield under P0's control, not by its final zone.
#[test]
fn p3s2a_triumph_ballista_x_is_zero_on_the_battlefield_so_it_returns() {
    let mut sc = scenario();
    sc.with_mana_pool(
        P0,
        mana_for(&[WALKING_BALLISTA_COST, VINDICTIVE_TRIUMPH_COST], 4),
    );
    let ballista = sc
        .add_creature_to_hand_from_oracle(P0, "Walking Ballista", 0, 0, WALKING_BALLISTA)
        .with_mana_cost(cost(WALKING_BALLISTA_COST))
        .id();
    let triumph = sc
        .add_spell_to_hand_from_oracle(P0, "Vindictive Triumph", true, VINDICTIVE_TRIUMPH)
        .with_mana_cost(cost(VINDICTIVE_TRIUMPH_COST))
        .id();
    let mut r = sc.build();
    cast_spell(&mut r, ballista);
    choose_x_if_asked(&mut r, 2);
    let w = drain(&mut r);
    assert!(matches!(w, WaitingFor::Priority { .. }));
    assert_eq!(
        zone(r.state(), ballista),
        Zone::Battlefield,
        "precondition: Ballista resolved with X=2"
    );
    let on_stack_mv = r
        .state()
        .lki_by_incarnation
        .get(&ballista)
        .map(|h| h.values().map(|s| s.mana_value).collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(
        on_stack_mv.contains(&4),
        "the stack exit recorded MV 4 for the first incarnation: {on_stack_mv:?}"
    );
    let out = r.cast(triumph).target_object(ballista).resolve();
    let returned = out.events().iter().any(|e| {
        matches!(e, GameEvent::ZoneChanged { object_id, from: Some(Zone::Exile), to: Zone::Battlefield, .. } if *object_id == ballista)
    });
    assert!(returned, "MV 0 on the battlefield <= 3: Triumph returns it (a stack-MV 4 read would leave it exiled)");
}
