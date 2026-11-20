//! Trigger-event mana-value gates ("that spell has mana value 5 or greater"): Herald of Ilharg (cast trigger, the
//! spell may be countered before the trigger resolves, CR 608.2h) and Prismari Apprentice (magecraft: cast OR copy,
//! CR 707.10). Deekah and Zaffai (supported) are controls for the `SpellCopied` event subject.

use super::fixture::*;
use super::oracle::*;
use engine::game::scenario::{GameRunner, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CopyChoiceMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::zones::Zone;

// ---------------------------------------------------------------- Herald of Ilharg (printed 3/3)

fn add_herald(sc: &mut engine::game::scenario::GameScenario) -> ObjectId {
    add_creature_on_battlefield(sc, P0, &HERALD_OF_ILHARG)
}

/// P0 casts a textless filler creature spell of mana value `mv`; nothing responds.
fn herald_plain(mv: u32) -> (GameRunner, ObjectId, ObjectId) {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[], mv as usize));
    let herald = add_herald(&mut sc);
    let spell = sc
        .add_creature_to_hand(P0, "Filler Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(mv))
        .id();
    let mut r = sc.build();
    r.cast(spell).resolve();
    (r, herald, spell)
}

#[test]
fn p3s2a_herald_mv5_puts_two_counters_then_damages_each_opponent() {
    let (r, herald, spell) = herald_plain(5);
    assert_eq!(
        zone(r.state(), spell),
        Zone::Battlefield,
        "reach guard: the spell resolved"
    );
    assert_eq!(
        plus_counters(r.state(), herald),
        2,
        "two +1/+1 counters, unconditional"
    );
    assert_eq!(
        life(r.state(), P1),
        20 - 2,
        "MV 5 >= 5: damage equal to the 2 counters on Herald to each opponent"
    );
}

#[test]
fn p3s2a_herald_mv4_puts_two_counters_and_no_damage() {
    let (r, herald, spell) = herald_plain(4);
    assert_eq!(
        zone(r.state(), spell),
        Zone::Battlefield,
        "reach guard: the spell resolved"
    );
    assert_eq!(
        plus_counters(r.state(), herald),
        2,
        "the two counters are unconditional"
    );
    assert_eq!(life(r.state(), P1), 20, "MV 4 < 5: no damage");
}

/// P0 casts a creature spell (Herald triggers, the trigger is above the spell), P1 answers the TRIGGER with the
/// real Essence Scatter at the spell: the spell is countered first (ruling 2024-02-02: the trigger resolves anyway).
/// `x`: cast Walking Ballista with that X instead of a filler of mana value `mv`.
fn herald_countered(mv: u32, x: Option<u32>) -> (GameRunner, ObjectId) {
    let mut sc = scenario();
    let extra = x.map_or(mv as usize, |x| 2 * x as usize);
    sc.with_mana_pool(P0, mana_for(&[], extra));
    sc.with_mana_pool(P1, mana_for(&[ESSENCE_SCATTER.cost], 0));
    let herald = add_herald(&mut sc);
    let spell = match x {
        Some(_) => add_creature_in_hand(&mut sc, P0, &WALKING_BALLISTA),
        None => sc
            .add_creature_to_hand(P0, "Filler Bear", 2, 2)
            .with_mana_cost(ManaCost::generic(mv))
            .id(),
    };
    let scatter = add_spell(&mut sc, P1, &ESSENCE_SCATTER);
    let mut r = sc.build();
    if let Some(x) = x {
        assert_eq!(
            cast_ballista_at_x(&mut r, spell, x),
            2 * x,
            "CR 202.3e: X counts twice for {{X}}{{X}} on the stack"
        );
    } else {
        cast_spell(&mut r, spell);
    }
    assert_eq!(
        r.state().stack.len(),
        2,
        "the spell and Herald's trigger are on the stack"
    );
    r.act(GameAction::PassPriority).expect("P0 passes");
    cast_spell(&mut r, scatter);
    let w = drain(&mut r);
    assert!(
        matches!(w, WaitingFor::Priority { .. }),
        "resolution completed, got {w:?}"
    );
    assert_eq!(
        zone(r.state(), spell),
        Zone::Graveyard,
        "reach guard: the spell was countered"
    );
    if x.is_some() {
        assert_eq!(
            r.state().objects[&spell].effective_mana_value(),
            0,
            "X is zero off the stack"
        );
    }
    (r, herald)
}

#[test]
fn p3s2a_herald_mv5_spell_countered_first_still_damages() {
    let (r, herald, ..) = herald_countered(5, None);
    assert_eq!(
        plus_counters(r.state(), herald),
        2,
        "reach guard: the trigger resolved"
    );
    assert_eq!(
        life(r.state(), P1),
        18,
        "ruling: MV 5 as the spell last existed on the stack: damage still happens"
    );
}

#[test]
fn p3s2a_herald_mv4_spell_countered_first_no_damage() {
    let (r, herald, ..) = herald_countered(4, None);
    assert_eq!(
        plus_counters(r.state(), herald),
        2,
        "reach guard: the trigger resolved"
    );
    assert_eq!(life(r.state(), P1), 20, "MV 4: no damage");
}

#[test]
fn p3s2a_herald_ballista_x3_countered_first_reads_stack_value() {
    let (r, herald) = herald_countered(0, Some(3));
    assert_eq!(
        plus_counters(r.state(), herald),
        2,
        "reach guard: the trigger resolved"
    );
    assert_eq!(
        life(r.state(), P1),
        18,
        "CR 202.3e + CR 608.2h: MV 6 as it last existed on the stack: damage"
    );
}

#[test]
fn p3s2a_herald_ballista_x1_countered_first_no_damage() {
    let (r, herald, ..) = herald_countered(0, Some(1));
    assert_eq!(
        plus_counters(r.state(), herald),
        2,
        "reach guard: the trigger resolved"
    );
    assert_eq!(life(r.state(), P1), 20, "X=1: MV 2 on the stack: no damage");
}

// ---------------------------------------------------------------- Prismari Apprentice (printed 2/2)

fn cant_be_blocked(state: &GameState, id: ObjectId) -> bool {
    use engine::game::functioning_abilities::active_static_definitions;
    use engine::types::statics::StaticMode;
    active_static_definitions(state, &state.objects[&id])
        .any(|d| d.mode == StaticMode::CantBeBlocked)
}

fn add_prismari(sc: &mut engine::game::scenario::GameScenario) -> ObjectId {
    add_creature_on_battlefield(sc, P0, &PRISMARI_APPRENTICE)
}

/// P0 casts a real sorcery with no target (Overrun MV 5 / Harmonize MV 4): the magecraft trigger fires on the CAST.
fn prismari_cast(card: &Printed) -> (GameRunner, ObjectId) {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[card.cost], 0));
    sc.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    let p = add_prismari(&mut sc);
    let spell = add_spell(&mut sc, P0, card);
    let mut r = sc.build();
    let out = r.cast(spell).resolve();
    assert_eq!(
        zone(r.state(), spell),
        Zone::Graveyard,
        "reach guard: the sorcery resolved"
    );
    assert!(
        matches!(out.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution completed"
    );
    engine::game::layers::evaluate_layers(r.state_mut());
    (r, p)
}

#[test]
fn p3s2a_prismari_mv5_cast_is_unblockable_and_gets_a_counter() {
    let (r, p) = prismari_cast(&OVERRUN);
    assert!(cant_be_blocked(r.state(), p), "can't be blocked this turn");
    assert_eq!(
        plus_counters(r.state(), p),
        1,
        "MV 5 >= 5: one +1/+1 counter"
    );
}

#[test]
fn p3s2a_prismari_mv4_cast_is_unblockable_with_no_counter() {
    let (r, p) = prismari_cast(&HARMONIZE);
    assert!(
        cant_be_blocked(r.state(), p),
        "reach guard: the unblockable sentence is unconditional"
    );
    assert_eq!(plus_counters(r.state(), p), 0, "MV 4 < 5: no counter");
}

/// CR 715.3b + CR 608.2h: both readers see Fertile Footsteps (MV 3) after
/// Reject Imperfection counters it, not the Beanstalk Giant (MV 7) in the graveyard.
#[test]
fn countered_adventure_keeps_its_spell_face_for_both_mana_value_gates() {
    let db = crate::support::shared_card_db().expect("integration card database");
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&["{2}{G}"], 0));
    sc.with_mana_pool(P1, mana_for(&[REJECT_IMPERFECTION.cost], 0));
    let apprentice = add_prismari(&mut sc);
    add_counter_holder(&mut sc);
    let giant = sc.add_real_card(P0, "Beanstalk Giant", Zone::Hand, db);
    let reject = add_spell(&mut sc, P1, &REJECT_IMPERFECTION);
    let mut r = sc.build();

    r.cast(giant).adventure_face(false).commit();
    assert_eq!(r.state().stack.len(), 2, "Adventure and magecraft trigger");
    let spell = &r.state().objects[&giant];
    assert_eq!(spell.name, "Fertile Footsteps");
    assert_eq!(spell.card_types.core_types, [CoreType::Sorcery]);
    assert_eq!(spell.card_types.subtypes, ["Adventure"]);
    assert_eq!(spell.effective_mana_value(), 3);

    r.act(GameAction::PassPriority).expect("P0 passes");
    r.cast(reject).target_object(giant).commit();
    let offered = is_proliferate(&drain(&mut r));
    if offered {
        r.act(GameAction::SelectTargets { targets: vec![] })
            .expect("proliferate zero objects");
        drain(&mut r);
    }
    let card = &r.state().objects[&giant];
    assert_eq!(card.zone, Zone::Graveyard, "Adventure was countered");
    assert_eq!(card.name, "Beanstalk Giant");
    assert_eq!(card.card_types.core_types, [CoreType::Creature]);
    assert_eq!(card.card_types.subtypes, ["Giant"]);
    assert_eq!(card.effective_mana_value(), 7);
    assert!(r.state().stack.is_empty(), "magecraft resolved");
    assert!(cant_be_blocked(r.state(), apprentice), "magecraft applied");
    assert_eq!(
        (offered, plus_counters(r.state(), apprentice)),
        (true, 0),
        "MV 3 offers proliferate but earns no magecraft counter"
    );
}

/// Pass only until the named top entry leaves, retaining its public action events.
fn finish_top(r: &mut GameRunner, id: ObjectId) -> Vec<GameEvent> {
    assert_eq!(r.state().stack.back().map(|entry| entry.id), Some(id));
    let mut events = Vec::new();
    for _ in 0..r.state().players.len() {
        assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
        events.extend(r.act(GameAction::PassPriority).expect("pass").events);
        if r.state().stack.iter().all(|entry| entry.id != id) {
            return events;
        }
    }
    panic!("{id:?} did not finish after every player passed");
}

fn prismari_trigger(
    r: &GameRunner,
    apprentice: ObjectId,
    cause: impl FnOnce(&GameEvent) -> bool,
) -> ObjectId {
    let entry = r.state().stack.back().expect("magecraft trigger");
    let StackEntryKind::TriggeredAbility {
        source_id,
        trigger_event: Some(event),
        ..
    } = &entry.kind
    else {
        panic!("expected a magecraft trigger at the top, got {entry:?}");
    };
    assert_eq!(*source_id, apprentice);
    assert!(
        cause(event),
        "unexpected magecraft trigger event: {event:?}"
    );
    entry.id
}

fn prismari_cast_trigger(r: &GameRunner, apprentice: ObjectId, cast: ObjectId) -> ObjectId {
    prismari_trigger(r, apprentice, |event| {
        matches!(
            event,
            GameEvent::SpellCast { card_id: _, controller: P0, object_id, cast_mana_value: _ }
                if *object_id == cast
        )
    })
}

fn prismari_copy_trigger(
    r: &GameRunner,
    apprentice: ObjectId,
    copy: ObjectId,
    original: ObjectId,
) -> ObjectId {
    prismari_trigger(r, apprentice, |event| {
        matches!(
            event,
            GameEvent::SpellCopied { card_id: _, controller: P0, object_id, original_id }
                if *object_id == copy && *original_id == original
        )
    })
}

fn copied_spell(r: &GameRunner, events: &[GameEvent], original: ObjectId) -> ObjectId {
    let mut copies = events.iter().filter_map(|event| {
        let GameEvent::SpellCopied {
            card_id,
            controller,
            object_id,
            original_id,
        } = event
        else {
            return None;
        };
        assert_eq!(*controller, P0);
        assert_eq!(*original_id, original);
        assert_eq!(*card_id, r.state().objects[&original].card_id);
        Some(*object_id)
    });
    let copy = copies.next().expect("a SpellCopied event");
    assert!(copies.next().is_none(), "Twincast copied the spell once");
    assert_ne!(copy, original);
    assert!(
        r.state().stack.iter().any(|entry| {
            entry.id == copy
                && entry.controller == P0
                && matches!(entry.kind, StackEntryKind::Spell { .. })
        }),
        "the event names P0's new spell on the stack"
    );
    copy
}

/// CR 608.2h + CR 707.10: read the copied spell on the stack, or its stack characteristics
/// if it was countered and ceased to exist before its own magecraft trigger resolved.
#[test]
fn p3s2a_prismari_copy_magecraft_reads_live_and_countered_overrun() {
    for (card, response, expected_mv, expected_gain) in [
        (&OVERRUN, None, 5, 1),
        (&OVERRUN, Some(&COUNTERSPELL), 5, 1),
        (&HARMONIZE, Some(&COUNTERSPELL), 4, 0),
    ] {
        let mut sc = scenario();
        sc.with_mana_pool(P0, mana_for(&[card.cost, TWINCAST.cost], 0));
        let counter = response.map(|response| {
            sc.with_mana_pool(P1, mana_for(&[response.cost], 0));
            add_spell(&mut sc, P1, response)
        });
        let apprentice = add_prismari(&mut sc);
        let original = add_spell(&mut sc, P0, card);
        let twincast = add_spell(&mut sc, P0, &TWINCAST);
        let mut r = sc.build();

        r.cast(original).commit();
        let cast_trigger = prismari_cast_trigger(&r, apprentice, original);
        finish_top(&mut r, cast_trigger);
        assert_eq!(r.state().stack.back().map(|entry| entry.id), Some(original));
        let before_copy = plus_counters(r.state(), apprentice);
        assert_eq!(before_copy, expected_gain, "the original spell's magecraft");

        r.cast(twincast).target_object(original).commit();
        let cast_trigger = prismari_cast_trigger(&r, apprentice, twincast);
        finish_top(&mut r, cast_trigger);
        let events = finish_top(&mut r, twincast);
        let copy = copied_spell(&r, &events, original);
        let copy_trigger = prismari_copy_trigger(&r, apprentice, copy, original);
        assert_eq!(
            r.state().stack.len(),
            3,
            "original, copy, and one copy trigger"
        );
        assert_eq!(r.state().objects[&copy].effective_mana_value(), expected_mv);
        assert_eq!(plus_counters(r.state(), apprentice), before_copy);

        if let Some(counter) = counter {
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::Priority { player: P0 }
            ));
            r.act(GameAction::PassPriority).expect("P0 passes to P1");
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::Priority { player: P1 }
            ));
            r.cast(counter).target_object(copy).commit();
            let events = finish_top(&mut r, counter);
            assert!(events.iter().any(|event| matches!(
                event,
                GameEvent::SpellCountered { object_id, countered_by, countered_by_controller: P1 }
                    if *object_id == copy && *countered_by == counter
            )), "Counterspell countered the specific copy");
            assert!(
                !r.state().objects.contains_key(&copy),
                "CR 704.5e: the copy ceased to exist"
            );
            assert_eq!(
                r.state().stack.len(),
                2,
                "original and the pending copy trigger"
            );
            assert_eq!(
                prismari_copy_trigger(&r, apprentice, copy, original),
                copy_trigger
            );
            assert_eq!(plus_counters(r.state(), apprentice), before_copy);
        }

        finish_top(&mut r, copy_trigger);
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        assert_eq!(r.state().stack.len(), if counter.is_some() { 1 } else { 2 });
        assert_eq!(
            zone(r.state(), original),
            Zone::Stack,
            "original remains on the stack"
        );
        if counter.is_none() {
            assert_eq!(
                zone(r.state(), copy),
                Zone::Stack,
                "live leg did not resolve the copy"
            );
        }
        assert_eq!(
            plus_counters(r.state(), apprentice),
            before_copy + expected_gain,
            "{} copy magecraft, countered: {}",
            card.name,
            counter.is_some()
        );
    }
}

// ---------------------------------------------------------------- Deekah / Zaffai controls (supported at base)

/// Deekah: "create a 0/0 Fractal token. Put X +1/+1 counters on it, where X is that spell's mana value."
/// P0 casts Overrun (MV 5), then Twincast copying it: the cast and the copy each make a token with 5 counters
/// (ruling 2021-04-16: magecraft triggers on copies); Twincast's own cast makes one with 2.
#[test]
fn p3s2a_deekah_tokens_carry_the_triggering_spells_mana_value() {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[OVERRUN.cost, TWINCAST.cost], 0));
    add_creature_on_battlefield(&mut sc, P0, &DEEKAH_FRACTAL_THEORIST);
    let overrun = add_spell(&mut sc, P0, &OVERRUN);
    let twincast = add_spell(&mut sc, P0, &TWINCAST);
    let mut r = sc.build();
    let mut c = r.cast(overrun).commit();
    let c2 = c.cast(twincast).target_object(overrun).commit();
    c2.resolve();
    let mut tokens: Vec<u32> = r
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.is_token && o.controller == P0)
        .map(|o| {
            o.counters
                .get(&engine::types::counter::CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        })
        .collect();
    tokens.sort();
    assert_eq!(
        tokens,
        vec![2, 5, 5],
        "Fractal tokens: Twincast cast (MV 2), Overrun cast (MV 5), copy of Overrun (MV 5)"
    );
}

// ---------------------------------------------------------------- copy observers collected exactly once

/// Pass priority one step at a time and count the distinct Prismari triggers that reach the stack: the cast of
/// Overrun, the cast of Twincast and the copy of Overrun make three, each collected once.
#[test]
fn p3s2a_prismari_twincast_trigger_events() {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[OVERRUN.cost, TWINCAST.cost], 0));
    let p = add_prismari(&mut sc);
    let overrun = add_spell(&mut sc, P0, &OVERRUN);
    let twincast = add_spell(&mut sc, P0, &TWINCAST);
    let mut r = sc.build();
    cast_spell(&mut r, overrun);
    cast_spell(&mut r, twincast);
    let mut seen: Vec<ObjectId> = Vec::new();
    for _ in 0..40 {
        for e in r.state().stack.iter() {
            if let StackEntryKind::TriggeredAbility { source_id, .. } = &e.kind {
                if *source_id == p && !seen.contains(&e.id) {
                    seen.push(e.id);
                }
            }
        }
        if r.state().stack.is_empty() {
            break;
        }
        match r.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected decision {other:?}"),
        }
    }
    assert_eq!(seen.len(), 3, "printed: magecraft triggers for the cast of Overrun, the cast of Twincast and the copy of Overrun");
}

/// CR 702.50a + CR 707.10: Endless Swarm's Epic copy is put on the stack at P0's upkeep and is not cast, so the
/// only event Prismari Apprentice ("whenever you cast or copy an instant or sorcery spell") can see is
/// `SpellCopied`, which Epic's resolver pushes without collecting its observers inline. The copy of the
/// MV 8 sorcery earns the printed +1/+1 counter. Guards the duplicate-collection fix: the pass must still
/// collect a `SpellCopied` that no effect collected itself.
#[test]
fn p3s2a_epic_copy_triggers_prismari_apprentice_once() {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[ENDLESS_SWARM.cost], 0));
    sc.with_library_top(
        P0,
        &["Forest", "Forest", "Forest", "Forest", "Forest", "Forest"],
    );
    sc.with_library_top(
        P1,
        &["Island", "Island", "Island", "Island", "Island", "Island"],
    );
    let p = add_prismari(&mut sc);
    let swarm = add_spell(&mut sc, P0, &ENDLESS_SWARM);
    let mut r = sc.build();
    cast_spell(&mut r, swarm);
    // Magecraft on the CAST of the MV 8 sorcery.
    let w = drain(&mut r);
    assert!(
        matches!(w, WaitingFor::Priority { .. }),
        "Swarm resolved, got {w:?}"
    );
    assert_eq!(
        zone(r.state(), swarm),
        Zone::Graveyard,
        "Endless Swarm resolved"
    );
    let after_cast = plus_counters(r.state(), p);
    assert_eq!(after_cast, 1, "one counter for casting the MV 8 sorcery");
    // Next upkeep of P0: the Epic copy goes on the stack, then resolves.
    for _ in 0..80 {
        if r.state().phase == engine::types::phase::Phase::Upkeep
            && r.state().active_player == P0
            && r.state().turn_number > 1
        {
            break;
        }
        match r.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                r.declare_attackers(&[]).expect("declare no attackers");
            }
            other => panic!("unexpected decision on the way to P0's upkeep: {other:?}"),
        }
    }
    assert_eq!(r.state().active_player, P0, "P0's next upkeep reached");
    let w = drain(&mut r);
    assert!(
        matches!(w, WaitingFor::Priority { .. }),
        "Epic copy resolved, got {w:?}"
    );
    assert_eq!(
        plus_counters(r.state(), p),
        2,
        "the Epic copy of the MV 8 sorcery triggers magecraft exactly once"
    );
}

// ---------------------------------------------------------------- copy of a targeted spell (CopyRetarget walk)

/// CR 603.2c + CR 707.10c: Twincast copying a targeted Bolt opens the real retarget
/// prompt. Keeping the targets must resume Twincast without enrolling magecraft twice.
#[test]
fn p3s2a_prismari_twincast_on_targeted_spell_triggers_three_times() {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[LIGHTNING_BOLT.cost, TWINCAST.cost], 0));
    let p = add_prismari(&mut sc);
    let bolt = add_spell(&mut sc, P0, &LIGHTNING_BOLT);
    let twincast = add_spell(&mut sc, P0, &TWINCAST);
    let mut r = sc.build();
    r.cast(bolt).target_player(P1).commit();
    assert_eq!(r.state().stack.len(), 2, "Bolt and one magecraft trigger");
    let cast_trigger = prismari_cast_trigger(&r, p, bolt);
    finish_top(&mut r, cast_trigger);
    assert_eq!(r.state().stack.len(), 1);

    r.cast(twincast).target_object(bolt).commit();
    assert_eq!(
        r.state().stack.len(),
        3,
        "Bolt, Twincast, and one magecraft trigger"
    );
    let cast_trigger = prismari_cast_trigger(&r, p, twincast);
    finish_top(&mut r, cast_trigger);
    let events = finish_top(&mut r, twincast);
    let copy = copied_spell(&r, &events, bolt);
    let WaitingFor::CopyRetarget {
        player,
        copy_id,
        mode,
        target_slots,
        can_keep_rest,
        ..
    } = &r.state().waiting_for
    else {
        panic!(
            "the targeted copy must offer retargeting, got {:?}",
            r.state().waiting_for
        );
    };
    assert_eq!(
        (*player, *copy_id, *mode),
        (P0, copy, Some(CopyChoiceMode::Retarget))
    );
    assert_eq!(target_slots.len(), 1);
    assert_eq!(target_slots[0].current, Some(TargetRef::Player(P1)));
    assert!(*can_keep_rest);

    let resume = r
        .act(GameAction::KeepAllCopyTargets)
        .expect("keep copy targets");
    assert!(
        resume.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { kind: EffectKind::CopySpell, source_id, subject: _ }
                if *source_id == copy
        )),
        "keeping the target completed this copy effect"
    );
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(
        r.state().stack.len(),
        3,
        "Bolt, copy, and exactly one copy-caused trigger"
    );
    let copy_trigger = prismari_copy_trigger(&r, p, copy, bolt);
    finish_top(&mut r, copy_trigger);
    assert_eq!(
        r.state().stack.len(),
        2,
        "the one copy-caused trigger is finished"
    );
    let w = drain(&mut r);
    assert!(r.state().stack.is_empty());
    assert!(
        matches!(w, WaitingFor::Priority { player: P0 }),
        "finished, got {w:?}"
    );
    assert_eq!(life(r.state(), P1), 14, "both Bolts resolved on P1");
    assert_eq!(
        plus_counters(r.state(), p),
        0,
        "all three spells have mana value below five"
    );
}

/// CR 608.2h + CR 400.7: Prismari Apprentice's trigger for the FIRST cast of a spell reads that cast's last known mana value even
/// after the same card was recast (a new object, CR 400.7) before the trigger resolved. P0 casts Stroke of Genius with X=3
/// (MV 6; trigger T1), P1 Remands it, P0 recasts it with X=0 (MV 3; trigger T2). T1 sees MV 6 and puts the counter; T2 sees MV 3.
#[test]
fn p3s2a_prismari_trigger_reads_its_own_cast_after_a_recast() {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[STROKE_OF_GENIUS.cost, "{2}{U}"], 3));
    sc.with_mana_pool(P1, mana_for(&[REMAND.cost], 0));
    sc.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    sc.with_library_top(P1, &["Island", "Island", "Island", "Island"]);
    let p = add_prismari(&mut sc);
    let stroke = add_spell(&mut sc, P0, &STROKE_OF_GENIUS);
    let remand = add_spell(&mut sc, P1, &REMAND);
    let mut r = sc.build();
    let _ = r.cast(stroke).x(3).target_player(P0).commit();
    assert_eq!(
        r.state().stack.len(),
        2,
        "Stroke and Prismari trigger T1; waiting {:?}",
        r.state().waiting_for
    );
    r.act(GameAction::PassPriority).expect("P0 passes");
    let _ = r.cast(remand).target_object(stroke).commit();
    for _ in 0..10 {
        if r.state().stack.len() == 1 {
            break;
        }
        r.act(GameAction::PassPriority).expect("pass");
    }
    assert_eq!(r.state().stack.len(), 1, "only T1 is left");
    assert_eq!(zone(r.state(), stroke), Zone::Hand, "Remanded");
    let _ = r.cast(stroke).x(0).target_player(P0).commit();
    drain(&mut r);
    assert_eq!(
        plus_counters(r.state(), p),
        1,
        "T1 (the MV 6 cast) puts the counter; T2 (the MV 3 recast) does not"
    );
}

// ---------------------------------------------------------------- two observers of one event (ordering)

/// Count the magecraft/cast triggers `sources` have on the stack, one per source, all from `cast`'s cast event.
fn triggers_from_cast(r: &GameRunner, sources: &[ObjectId], cast: ObjectId) -> usize {
    r.state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility {
                    source_id,
                    trigger_event: Some(GameEvent::SpellCast { object_id, .. }),
                    ..
                } if sources.contains(source_id) && *object_id == cast
            )
        })
        .count()
}

/// CR 603.3b + CR 608.2h: two Prismari Apprentices observe ONE cast of Overrun (mana value 5). Each trigger's
/// condition reads the cast spell's mana value and writes only counters on its own source, so neither result
/// depends on the order and the controller is not asked to order them. A "board P/T read" profile of the mana
/// value gate would see the sibling's own-counter write as a feed and prompt (the ordering-parity regression).
#[test]
fn p3s2a_two_prismari_apprentices_observe_one_cast_without_an_ordering_prompt() {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[OVERRUN.cost], 0));
    sc.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    let first = add_prismari(&mut sc);
    let second = add_prismari(&mut sc);
    let spell = add_spell(&mut sc, P0, &OVERRUN);
    let mut r = sc.build();
    cast_spell(&mut r, spell);
    assert_eq!(
        triggers_from_cast(&r, &[first, second], spell),
        2,
        "reach guard: both apprentices triggered on the cast; waiting {:?}",
        r.state().waiting_for
    );
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "no ordering prompt for two identical independent observers, got {:?}",
        r.state().waiting_for
    );
    let w = drain(&mut r);
    assert!(
        matches!(w, WaitingFor::Priority { .. }),
        "resolved, got {w:?}"
    );
    assert_eq!(zone(r.state(), spell), Zone::Graveyard, "Overrun resolved");
    for apprentice in [first, second] {
        assert_eq!(
            plus_counters(r.state(), apprentice),
            1,
            "MV 5 >= 5: each apprentice gets its own counter"
        );
    }
}

/// The same two-observer discriminator for Herald of Ilharg: a mana value 5 creature spell (Serra Angel) is
/// cast, both heralds trigger on that one event, each gets its two counters and damages the opponent for the
/// counters on it. No ordering prompt; the opponent loses 4 life whichever resolves first.
#[test]
fn p3s2a_two_heralds_observe_one_cast_without_an_ordering_prompt() {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[SERRA_ANGEL.cost], 0));
    let first = add_herald(&mut sc);
    let second = add_herald(&mut sc);
    let spell = add_creature_in_hand(&mut sc, P0, &SERRA_ANGEL);
    let mut r = sc.build();
    let life_before = life(r.state(), P1);
    cast_spell(&mut r, spell);
    assert_eq!(
        triggers_from_cast(&r, &[first, second], spell),
        2,
        "reach guard: both heralds triggered on the cast; waiting {:?}",
        r.state().waiting_for
    );
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "no ordering prompt for two identical independent observers, got {:?}",
        r.state().waiting_for
    );
    let w = drain(&mut r);
    assert!(
        matches!(w, WaitingFor::Priority { .. }),
        "resolved, got {w:?}"
    );
    assert_eq!(
        zone(r.state(), spell),
        Zone::Battlefield,
        "Serra Angel resolved"
    );
    for herald in [first, second] {
        assert_eq!(
            plus_counters(r.state(), herald),
            2,
            "each herald puts two counters on itself"
        );
    }
    assert_eq!(
        life_before - life(r.state(), P1),
        4,
        "MV 5 >= 5: each herald deals damage equal to its two counters"
    );
}
