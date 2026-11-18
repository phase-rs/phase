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
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// P0 casts a textless filler creature spell of mana value `spell_mv`; P1 answers with the real counterspell
/// `counter` through the production cast path.
fn counter_flow(
    counter: &Printed,
    spell_mv: u32,
    hand_for_p1: impl Fn(&mut engine::game::scenario::GameScenario),
) -> (GameRunner, WaitingFor, ObjectId) {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[], spell_mv as usize));
    sc.with_mana_pool(P1, mana_for(&[counter.cost], 0));
    add_counter_holder(&mut sc);
    sc.with_library_top(P1, &["Island", "Island", "Island"]);
    let target = {
        let mut b = sc.add_creature_to_hand(P0, "Filler Spell", 2, 2);
        b.with_mana_cost(ManaCost::generic(spell_mv));
        b.id()
    };
    hand_for_p1(&mut sc);
    let counter_id = add_spell(&mut sc, P1, counter);
    let mut runner = sc.build();
    cast_spell(&mut runner, target);
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    cast_spell(&mut runner, counter_id);
    let w = drain(&mut runner);
    (runner, w, target)
}

// ---- Reject Imperfection: "If that spell's mana value was 3 or less, proliferate." (CR 701.34a, CR 608.2h) ----

fn ri(mv: u32) -> (GameRunner, WaitingFor, ObjectId) {
    counter_flow(&REJECT_IMPERFECTION, mv, |_| {})
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
    sc.with_mana_pool(P0, mana_for(&[WALKING_BALLISTA.cost], 2 * x as usize));
    sc.with_mana_pool(P1, mana_for(&[REJECT_IMPERFECTION.cost], 0));
    add_counter_holder(&mut sc);
    let ballista = add_creature_in_hand(&mut sc, P0, &WALKING_BALLISTA);
    let ri = add_spell(&mut sc, P1, &REJECT_IMPERFECTION);
    let mut r = sc.build();
    let stack_mv = cast_ballista_at_x(&mut r, ballista, x);
    r.act(GameAction::PassPriority).expect("P0 passes");
    cast_spell(&mut r, ri);
    let w = drain(&mut r);
    assert_eq!(
        zone(r.state(), ballista),
        Zone::Graveyard,
        "reach guard: Ballista was countered"
    );
    let gy_mv = r.state().objects[&ballista].effective_mana_value();
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
    assert!(
        matches!(w, WaitingFor::Priority { player: P0 }),
        "counter spell finished, got {w:?}"
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
    counter_flow(&SOUND_THE_TRUMPETS, mv, |_| {})
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
    let (mut r, w, target) = counter_flow(&SOUND_THE_TRUMPETS, 2, |sc| {
        if land {
            sc.add_land_to_hand(P1, "Held Land");
        } else {
            sc.add_creature_to_hand(P1, "Held Bear", 2, 2);
        }
    });
    assert_eq!(
        zone(r.state(), target),
        Zone::Graveyard,
        "Trumpets countered the spell"
    );
    let WaitingFor::DiscardChoice {
        player,
        count,
        cards,
        source_id,
        ..
    } = w
    else {
        panic!("recruit must offer a discard choice, got {w:?}");
    };
    assert_eq!(player, P1);
    assert_eq!(count, 1);
    assert_eq!(r.state().objects[&source_id].name, SOUND_THE_TRUMPETS.name);
    assert_eq!(cards.len(), 2, "held card and the freshly drawn card");
    let held = r
        .state()
        .objects
        .values()
        .find(|o| o.name == if land { "Held Land" } else { "Held Bear" })
        .expect("held card")
        .id;
    assert!(cards.contains(&held), "selected card was offered");
    let drawn = cards
        .into_iter()
        .find(|&id| id != held)
        .expect("drawn alternative");
    assert!(
        r.state().players[1].hand.contains(&held),
        "held card in hand before selection"
    );
    let result = r
        .act(GameAction::SelectCards { cards: vec![held] })
        .expect("discard the held card");
    let discards = result.events.iter().filter(|event| {
        matches!(
            event,
            GameEvent::Discarded { player_id: P1, object_id, source_id: Some(source) }
                if *object_id == held && *source == source_id
        )
    });
    assert_eq!(
        discards.count(),
        1,
        "Trumpets discarded the selected card exactly once"
    );
    let w = drain(&mut r);
    assert!(
        matches!(w, WaitingFor::Priority { player: P0 }),
        "resolution completed, got {w:?}"
    );
    assert!(r.state().stack.is_empty(), "recruit resolution finished");
    assert_eq!(
        zone(r.state(), source_id),
        Zone::Graveyard,
        "Trumpets finished"
    );
    assert_eq!(
        zone(r.state(), held),
        Zone::Graveyard,
        "selected card was discarded"
    );
    assert!(r.state().players[1].graveyard.contains(&held));
    assert_eq!(
        zone(r.state(), drawn),
        Zone::Hand,
        "the unchosen card stayed in hand"
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

/// P0 casts the real `spell` at P1's real printed `victim` (a flier, an artifact) through the production cast path.
fn bf_flow(spell: &Printed, victim: &Printed) -> (GameRunner, ObjectId, WaitingFor) {
    let mut sc = scenario();
    sc.at_phase(Phase::PostCombatMain);
    sc.with_mana_pool(P0, mana_for(&[spell.cost], 0));
    add_counter_holder(&mut sc);
    let victim = if victim.type_line.contains("Creature") {
        add_creature_on_battlefield(&mut sc, P1, victim)
    } else {
        add_noncreature_permanent(&mut sc, P1, victim)
    };
    let spell = add_spell(&mut sc, P0, spell);
    let mut runner = sc.build();
    let out = runner.cast(spell).target_object(victim).resolve();
    let w = out.final_waiting_for().clone();
    (runner, victim, w)
}

/// Carnivorous Canopy at a printed target: Angelic Wall (flying, MV 2), Serra Angel (flying, MV 5) or Darksteel
/// Ingot (indestructible artifact, MV 3).
fn canopy(victim: &Printed) -> (GameRunner, ObjectId, WaitingFor) {
    bf_flow(&CARNIVOROUS_CANOPY, victim)
}

#[test]
fn p3s2a_canopy_mv2_destroys_then_proliferates() {
    let (r, v, w) = canopy(&ANGELIC_WALL);
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
    let (r, v, w) = canopy(&SERRA_ANGEL);
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

/// CR 608.2h: the destroy does not happen (Darksteel Ingot is indestructible), the permanent is still where it was
/// expected to be, so it is answered from its current information (MV 3, "3 or less"): the proliferate still happens.
/// The Ingot never moves, so this pins the not-departed read; the departed reads are pinned by the counter and
/// bounce witnesses.
#[test]
fn p3s2a_canopy_indestructible_target_is_read_live() {
    let (r, v, w) = canopy(&DARKSTEEL_INGOT);
    assert_eq!(
        zone(r.state(), v),
        Zone::Battlefield,
        "indestructible: not destroyed"
    );
    assert!(
        is_proliferate(&w),
        "current MV 3 <= 3: proliferate, got {w:?}"
    );
}

fn triumph(victim: &Printed) -> (GameRunner, ObjectId, WaitingFor) {
    bf_flow(&VINDICTIVE_TRIUMPH, victim)
}

#[test]
fn p3s2a_triumph_mv2_returns_tapped_then_exiles_at_next_end_step() {
    let (mut r, v, _w) = triumph(&ANGELIC_WALL);
    let o = &r.state().objects[&v];
    assert_eq!(
        o.zone,
        Zone::Battlefield,
        "MV 2 <= 3: returned to the battlefield"
    );
    assert_eq!(o.controller, P0, "under the caster's control");
    assert!(o.tapped, "tapped");

    let cast_turn = r.state().turn_number;
    assert_eq!(r.state().phase, Phase::PostCombatMain);
    // CR 603.7b + CR 603.3: the delayed ability triggers at the next end step;
    // the Wall is still on the battlefield pending its resolution.
    r.advance_to_end_step();
    assert_eq!(r.state().turn_number, cast_turn);
    assert_eq!(r.state().phase, Phase::End);
    assert_eq!(zone(r.state(), v), Zone::Battlefield);
    r.advance_until_stack_empty();
    assert_eq!(r.state().phase, Phase::End);
    assert_eq!(zone(r.state(), v), Zone::Exile);
}

#[test]
fn p3s2a_triumph_mv5_stays_exiled() {
    let (r, v, _w) = triumph(&SERRA_ANGEL);
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
        mana_for(&[WALKING_BALLISTA.cost, VINDICTIVE_TRIUMPH.cost], 4),
    );
    let ballista = add_creature_in_hand(&mut sc, P0, &WALKING_BALLISTA);
    let triumph = add_spell(&mut sc, P0, &VINDICTIVE_TRIUMPH);
    let mut r = sc.build();
    assert_eq!(cast_ballista_at_x(&mut r, ballista, 2), 4);
    let w = drain(&mut r);
    assert!(matches!(w, WaitingFor::Priority { .. }));
    assert_eq!(
        zone(r.state(), ballista),
        Zone::Battlefield,
        "precondition: Ballista resolved with X=2"
    );
    assert_eq!(r.state().objects[&ballista].effective_mana_value(), 0);
    let out = r.cast(triumph).target_object(ballista).resolve();
    let returned = out.events().iter().any(|e| {
        matches!(e, GameEvent::ZoneChanged { object_id, from: Some(Zone::Exile), to: Zone::Battlefield, .. } if *object_id == ballista)
    });
    assert!(returned, "MV 0 on the battlefield <= 3: Triumph returns it (a stack-MV 4 read would leave it exiled)");
}
