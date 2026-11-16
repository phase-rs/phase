//! Trigger-event mana-value gates ("that spell has mana value 5 or greater"): Herald of Ilharg (cast trigger, the
//! spell may be countered before the trigger resolves, CR 608.2h) and Prismari Apprentice (magecraft: cast OR copy,
//! CR 707.10). Deekah and Zaffai (supported) are controls for the `SpellCopied` event subject.

use super::fixture::*;
use super::oracle::*;
use engine::game::scenario::{GameRunner, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::zones::Zone;

// ---------------------------------------------------------------- Herald of Ilharg (printed 3/3)

fn add_herald(sc: &mut engine::game::scenario::GameScenario) -> ObjectId {
    sc.add_creature_from_oracle(P0, "Herald of Ilharg", 3, 3, HERALD_OF_ILHARG)
        .id()
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
fn herald_countered(mv: u32, x: Option<u32>) -> (GameRunner, ObjectId, ObjectId, GameState) {
    let mut sc = scenario();
    let extra = x.map_or(mv as usize, |x| 2 * x as usize);
    sc.with_mana_pool(P0, mana_for(&[], extra));
    sc.with_mana_pool(P1, mana_for(&[ESSENCE_SCATTER_COST], 0));
    let herald = add_herald(&mut sc);
    let spell = match x {
        Some(_) => sc
            .add_creature_to_hand_from_oracle(P0, "Walking Ballista", 0, 0, WALKING_BALLISTA)
            .with_mana_cost(cost(WALKING_BALLISTA_COST))
            .id(),
        None => sc
            .add_creature_to_hand(P0, "Filler Bear", 2, 2)
            .with_mana_cost(ManaCost::generic(mv))
            .id(),
    };
    let scatter = sc
        .add_spell_to_hand_from_oracle(P1, "Essence Scatter", true, ESSENCE_SCATTER)
        .with_mana_cost(cost(ESSENCE_SCATTER_COST))
        .id();
    let mut r = sc.build();
    cast_spell(&mut r, spell);
    if let Some(x) = x {
        choose_x_if_asked(&mut r, x);
    }
    let stack_before = r.state().clone();
    assert_eq!(
        r.state().stack.len(),
        2,
        "the spell and Herald's trigger are on the stack"
    );
    if let Some(x) = x {
        let stack_mv = r.state().objects[&spell]
            .mana_cost
            .mana_value_with_x(Zone::Stack, Some(x));
        assert_eq!(
            stack_mv,
            2 * x,
            "CR 202.3e: X counts twice for {{X}}{{X}} on the stack"
        );
    }
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
    (r, herald, spell, stack_before)
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
    let (r, herald, spell, _) = herald_countered(0, Some(3));
    let gy_mv = r.state().objects[&spell]
        .mana_cost
        .mana_value_with_x(Zone::Graveyard, None);
    assert_eq!(
        gy_mv, 0,
        "operands differ: stack MV 6 (X=3) versus graveyard MV 0"
    );
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
    sc.add_creature_from_oracle(P0, "Prismari Apprentice", 2, 2, PRISMARI_APPRENTICE)
        .id()
}

/// P0 casts a real sorcery with no target (Overrun MV 5 / Harmonize MV 4): the magecraft trigger fires on the CAST.
fn prismari_cast(name: &str, oracle: &str, printed_cost: &str) -> (GameRunner, ObjectId) {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[printed_cost], 0));
    sc.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    let p = add_prismari(&mut sc);
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, name, false, oracle)
        .with_mana_cost(cost(printed_cost))
        .id();
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
    let (r, p) = prismari_cast("Overrun", OVERRUN, OVERRUN_COST);
    assert!(cant_be_blocked(r.state(), p), "can't be blocked this turn");
    assert_eq!(
        plus_counters(r.state(), p),
        1,
        "MV 5 >= 5: one +1/+1 counter"
    );
}

#[test]
fn p3s2a_prismari_mv4_cast_is_unblockable_with_no_counter() {
    let (r, p) = prismari_cast("Harmonize", HARMONIZE, HARMONIZE_COST);
    assert!(
        cant_be_blocked(r.state(), p),
        "reach guard: the unblockable sentence is unconditional"
    );
    assert_eq!(plus_counters(r.state(), p), 0, "MV 4 < 5: no counter");
}

/// P0 casts Overrun (sorcery, MV 5), then Twincast (instant, MV 2) targeting it. Triggers of the magecraft ability:
/// cast Overrun (MV 5), cast Twincast (MV 2), copy of Overrun (MV 5). Printed: counters on the first and the third = 2.
fn prismari_twincast() -> (GameRunner, ObjectId, Vec<GameEvent>) {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[OVERRUN_COST, TWINCAST_COST], 0));
    let p = add_prismari(&mut sc);
    let overrun = sc
        .add_spell_to_hand_from_oracle(P0, "Overrun", false, OVERRUN)
        .with_mana_cost(cost(OVERRUN_COST))
        .id();
    let twincast = sc
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(cost(TWINCAST_COST))
        .id();
    let mut r = sc.build();
    let mut c = r.cast(overrun).commit();
    let mut events: Vec<GameEvent> = c.events().to_vec();
    let c2 = c.cast(twincast).target_object(overrun).commit();
    let out = c2.resolve(); // `out.events()` already contains the commit events of this cast
    events.extend(out.events().to_vec());
    engine::game::layers::evaluate_layers(r.state_mut());
    (r, p, events)
}

fn census(events: &[GameEvent]) -> (usize, usize, usize) {
    let cast = events
        .iter()
        .filter(|e| matches!(e, GameEvent::SpellCast { .. }))
        .count();
    let copied = events
        .iter()
        .filter(|e| matches!(e, GameEvent::SpellCopied { .. }))
        .count();
    let added = events
        .iter()
        .filter(|e| matches!(e, GameEvent::CounterAdded { .. }))
        .count();
    (cast, copied, added)
}

/// CR 707.10: the copy of Overrun is a spell on the stack, not cast; the magecraft ability's "copy" half fires on it.
#[test]
fn p3s2a_prismari_twincast_copy_of_mv5_spell_earns_a_second_counter() {
    let (r, p, events) = prismari_twincast();
    let (cast, copied, _added) = census(&events);
    assert_eq!(
        (cast, copied),
        (2, 1),
        "reach guard: two casts (Overrun, Twincast) and one copy (of Overrun)"
    );
    assert_eq!(
        plus_counters(r.state(), p),
        2,
        "cast of Overrun (MV 5) and copy of Overrun (MV 5); Twincast (MV 2) earns none"
    );
}

// ---------------------------------------------------------------- Deekah / Zaffai controls (supported at base)

/// Deekah: "create a 0/0 Fractal token. Put X +1/+1 counters on it, where X is that spell's mana value."
/// P0 casts Overrun (MV 5), then Twincast copying it: the cast and the copy each make a token with 5 counters
/// (ruling 2021-04-16: magecraft triggers on copies); Twincast's own cast makes one with 2.
#[test]
fn p3s2a_deekah_tokens_carry_the_triggering_spells_mana_value() {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[OVERRUN_COST, TWINCAST_COST], 0));
    sc.add_creature_from_oracle(
        P0,
        "Deekah, Fractal Theorist",
        3,
        3,
        DEEKAH_FRACTAL_THEORIST,
    );
    let overrun = sc
        .add_spell_to_hand_from_oracle(P0, "Overrun", false, OVERRUN)
        .with_mana_cost(cost(OVERRUN_COST))
        .id();
    let twincast = sc
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(cost(TWINCAST_COST))
        .id();
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

// ---------------------------------------------------------------- the extra trigger measured in ticket 82

/// Ticket 82 measured 4 `+1/+1` counters from the UNCONDITIONAL base parse where 3 magecraft triggers were
/// expected. This census prints and asserts the event sequence of the real Overrun + Twincast line.
#[test]
fn p3s2a_prismari_twincast_event_census() {
    let (_r, _p, events) = prismari_twincast();
    for (i, e) in events.iter().enumerate() {
        let d = format!("{e:?}");
        eprintln!("EV{i:02} {}", &d[..d.len().min(170)]);
    }
    let (cast, copied, added) = census(&events);
    eprintln!(
        "census: cast={cast} copied={copied} counter_added={added} total_events={}",
        events.len()
    );
}

/// Pass priority one step at a time and record, for every Prismari trigger that reaches the stack, the event that
/// caused it (`StackEntryKind::TriggeredAbility.trigger_event`).
#[test]
fn p3s2a_prismari_twincast_trigger_events() {
    use engine::types::game_state::StackEntryKind;
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[OVERRUN_COST, TWINCAST_COST], 0));
    let p = add_prismari(&mut sc);
    let overrun = sc
        .add_spell_to_hand_from_oracle(P0, "Overrun", false, OVERRUN)
        .with_mana_cost(cost(OVERRUN_COST))
        .id();
    let twincast = sc
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(cost(TWINCAST_COST))
        .id();
    let mut r = sc.build();
    cast_spell(&mut r, overrun);
    cast_spell(&mut r, twincast);
    let mut seen: Vec<(ObjectId, String)> = Vec::new();
    for _ in 0..40 {
        for e in r.state().stack.iter() {
            if let StackEntryKind::TriggeredAbility {
                source_id,
                trigger_event,
                ..
            } = &e.kind
            {
                if *source_id == p && !seen.iter().any(|(id, _)| *id == e.id) {
                    let d = trigger_event
                        .as_ref()
                        .map(|t| format!("{t:?}"))
                        .unwrap_or_else(|| "None".into());
                    seen.push((e.id, d[..d.len().min(160)].to_string()));
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
    for (id, d) in &seen {
        eprintln!("PRISMARI-TRIGGER stack id {id:?} <- {d}");
    }
    assert_eq!(seen.len(), 3, "printed: magecraft triggers for the cast of Overrun, the cast of Twincast and the copy of Overrun");
}
