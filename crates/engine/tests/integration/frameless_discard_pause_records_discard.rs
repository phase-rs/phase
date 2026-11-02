//! A frameless single-card effect discard (connive, learn rummage) whose move is paused by a
//! CR 616.1 replacement choice must still be recorded exactly once on the resume.
//!
//! CR 701.9a: "To discard a card, move it from its owner's hand to that player's graveyard."
//! CR 614.6: "If an event is replaced, it never happens. A modified event occurs instead, which may
//! in turn trigger abilities." CR 616.1: the affected player chooses which replacement to apply.
//!
//! Real cards, verbatim Oracle text:
//! - Hypnotic Grifter: "{3}: This creature connives. (Draw a card, then discard a card. If you
//!   discarded a nonland card, put a +1/+1 counter on this creature.)"
//! - Library of Leng: "You have no maximum hand size.\nIf an effect causes you to discard a card,
//!   discard it, but you may put it on top of your library instead of into your graveyard."
//! - Rest in Peace: "When this enchantment enters, exile all graveyards.\nIf a card or token would
//!   be put into a graveyard from anywhere, exile it instead."
//! - Geth's Grimoire: "Whenever an opponent discards a card, you may draw a card."
//! - Mind Rot: "Target player discards two cards."
//! - First Day of Class: "Whenever a creature you control enters this turn, put a +1/+1 counter on
//!   it and it gains haste until end of turn.\nLearn. (You may reveal a Lesson card you own from
//!   outside the game and put it into your hand, or discard a card to draw a card.)"

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::Effect;
use engine::types::actions::{GameAction, LearnOption};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

#[derive(Clone, Copy, Debug)]
enum Entry {
    Connive,
    Learn,
}

/// What one `ChooseReplacement` answer left behind.
struct Step {
    discarded_in_action: usize,
    carrier_present: bool,
    prompting: bool,
    card_zone: Option<Zone>,
}

struct Run {
    prompts: usize,
    steps: Vec<Step>,
    /// `Discarded` events for P0, accumulated over the whole run: `(card, source_id)`.
    discarded: Vec<(ObjectId, Option<ObjectId>)>,
    recorded: u32,
    grimoire_triggers: usize,
    card_zone: Zone,
    carrier_present: bool,
    resolved_to_priority: bool,
    drew_extra_card: bool,
}

fn discarded_for(events: &[GameEvent], player: PlayerId) -> Vec<(ObjectId, Option<ObjectId>)> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::Discarded {
                player_id,
                object_id,
                source_id,
            } if *player_id == player => Some((*object_id, *source_id)),
            _ => None,
        })
        .collect()
}

fn run(entry: Entry, rip: usize, leng: bool, picks: &[&str]) -> Run {
    let db = shared_card_db().expect("card database");
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for seat in [P0, P1] {
        for _ in 0..3 {
            s.add_real_card(seat, "Mind Rot", Zone::Library, db);
        }
    }
    let grimoire = s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
    for _ in 0..rip {
        s.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db);
    }
    if leng {
        s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
    }
    let grifter = s.add_real_card(P0, "Hypnotic Grifter", Zone::Battlefield, db);
    let (fdoc, rummaged) = match entry {
        Entry::Learn => (
            Some(s.add_real_card(P0, "First Day of Class", Zone::Hand, db)),
            Some(s.add_real_card(P0, "Mind Rot", Zone::Hand, db)),
        ),
        Entry::Connive => (None, None),
    };
    let (mana, count) = match entry {
        Entry::Connive => (ManaType::Colorless, 3),
        Entry::Learn => (ManaType::Red, 2),
    };
    s.with_mana_pool(
        P0,
        (0..count)
            .map(|_| ManaUnit::new(mana, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner: GameRunner = s.build();
    let mut events: Vec<GameEvent>;
    match entry {
        Entry::Connive => {
            let idx = runner.state().objects[&grifter]
                .abilities
                .iter()
                .position(|a| matches!(a.effect.as_ref(), Effect::Connive { .. }))
                .expect("Hypnotic Grifter has a connive ability");
            events = runner.activate(grifter, idx).resolve().events().to_vec();
        }
        Entry::Learn => {
            events = runner.cast(fdoc.unwrap()).resolve().events().to_vec();
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::LearnChoice { .. }),
                "reach guard: Learn must offer its choice, got {:?}",
                runner.state().waiting_for
            );
            events.extend(
                runner
                    .act(GameAction::LearnDecision {
                        choice: LearnOption::Rummage {
                            card_id: rummaged.unwrap(),
                        },
                    })
                    .expect("rummage")
                    .events,
            );
        }
    }
    // The card being discarded: the learn rummage target, or the connive draw still in hand at the
    // first prompt (connive draws one card from an empty hand).
    let mut card = rummaged.unwrap_or_else(|| {
        runner.state().players[0]
            .hand
            .iter()
            .next()
            .copied()
            .unwrap_or(ObjectId(0))
    });

    let mut steps = Vec::new();
    let mut prompts = 0;
    while let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    {
        let descs: Vec<String> = candidates.iter().map(|c| c.description.clone()).collect();
        let want = picks.get(prompts).copied().unwrap_or("");
        let index = descs.iter().position(|d| d.contains(want)).unwrap_or(0);
        prompts += 1;
        assert!(prompts <= 5, "runaway replacement prompts");
        let answer_events = runner
            .act(GameAction::ChooseReplacement { index })
            .expect("answer replacement choice")
            .events;
        let st = runner.state();
        steps.push(Step {
            discarded_in_action: discarded_for(&answer_events, P0).len(),
            carrier_present: st.pending_single_discard.is_some(),
            prompting: matches!(st.waiting_for, WaitingFor::ReplacementChoice { .. }),
            card_zone: st.objects.get(&card).map(|o| o.zone),
        });
        events.extend(answer_events);
    }

    let discarded = discarded_for(&events, P0);
    if let Some((id, _)) = discarded.first() {
        card = *id;
    }
    let st = runner.state();
    Run {
        prompts,
        steps,
        recorded: st
            .cards_discarded_this_turn_by_player
            .get(&P0)
            .copied()
            .unwrap_or(0),
        grimoire_triggers: st.stack.iter().filter(|e| e.source_id == grimoire).count(),
        card_zone: st.objects[&card].zone,
        carrier_present: st.pending_single_discard.is_some(),
        resolved_to_priority: matches!(st.waiting_for, WaitingFor::Priority { .. }),
        drew_extra_card: matches!(entry, Entry::Learn)
            && st.players[0]
                .hand
                .iter()
                .any(|id| *id != card && st.objects[id].zone == Zone::Hand),
        discarded,
    }
}

#[test]
fn connive_two_rest_in_peace_records_the_paused_discard() {
    let r = run(Entry::Connive, 2, false, &[]);
    assert_eq!(
        r.prompts, 1,
        "reach guard: two RIP raise one CR 616.1 choice"
    );
    assert_eq!(r.card_zone, Zone::Exile);
    assert_eq!(r.discarded.len(), 1, "exactly one Discarded event");
    assert_eq!(r.discarded[0].1, None, "connive passes no discard source");
    assert_eq!(r.recorded, 1, "cards_discarded_this_turn_by_player");
    assert_eq!(r.grimoire_triggers, 1, "Geth's Grimoire triggers once");
    assert!(!r.carrier_present);
    assert!(r.resolved_to_priority);
}

#[test]
fn connive_library_of_leng_accept_records_the_paused_discard() {
    let r = run(Entry::Connive, 0, true, &["Accept"]);
    assert_eq!(r.prompts, 1);
    assert_eq!(r.card_zone, Zone::Library);
    assert_eq!(r.discarded.len(), 1);
    assert_eq!(r.recorded, 1);
    assert_eq!(r.grimoire_triggers, 1);
    assert!(!r.carrier_present);
}

#[test]
fn connive_library_of_leng_decline_records_the_paused_discard() {
    let r = run(Entry::Connive, 0, true, &["Decline"]);
    assert_eq!(r.prompts, 1);
    assert_eq!(r.card_zone, Zone::Graveyard);
    assert_eq!(r.discarded.len(), 1);
    assert_eq!(r.recorded, 1);
    assert_eq!(r.grimoire_triggers, 1);
    assert!(!r.carrier_present);
}

#[test]
fn connive_nested_leng_decline_then_rip_records_once_at_the_delivering_answer() {
    let r = run(Entry::Connive, 2, true, &["Decline"]);
    assert_eq!(r.prompts, 2, "reach guard: Leng, then the RIP ordering");
    // CR 616.1f: the first answer re-parks a fresh choice; the discard has not been delivered, so
    // nothing is recorded yet and the carrier must survive the re-park.
    assert!(r.steps[0].prompting);
    assert_eq!(r.steps[0].discarded_in_action, 0);
    assert!(r.steps[0].carrier_present);
    assert_eq!(r.steps[0].card_zone, Some(Zone::Hand));
    assert!(!r.steps[1].prompting);
    assert_eq!(r.card_zone, Zone::Exile);
    assert_eq!(r.discarded.len(), 1);
    assert_eq!(r.recorded, 1);
    assert_eq!(r.grimoire_triggers, 1);
    assert!(!r.carrier_present);
}

#[test]
fn learn_rummage_two_rest_in_peace_records_the_paused_discard() {
    let r = run(Entry::Learn, 2, false, &[]);
    assert!(r.prompts >= 1, "reach guard: the rummage discard pauses");
    assert_eq!(r.card_zone, Zone::Exile);
    assert_eq!(r.discarded.len(), 1);
    assert_eq!(r.recorded, 1);
    assert_eq!(r.grimoire_triggers, 1);
    assert!(r.drew_extra_card, "rummage draws a card");
    assert!(!r.carrier_present);
}

#[test]
fn learn_rummage_leng_accept_same_id_round_trip_records_once() {
    // CR 400.7: the rummaged card goes to the library top and the rummage draw returns it to
    // hand under the same ObjectId in the same action; it is still recorded exactly once.
    let r = run(Entry::Learn, 0, true, &["Accept"]);
    assert_eq!(r.prompts, 1);
    assert_eq!(r.card_zone, Zone::Hand);
    assert_eq!(r.discarded.len(), 1);
    assert_eq!(r.recorded, 1);
    assert!(!r.carrier_present);
}

#[test]
fn connive_unpaused_discard_is_not_double_counted() {
    let r = run(Entry::Connive, 1, false, &[]);
    assert_eq!(r.prompts, 0, "positive guard: one RIP raises no choice");
    assert_eq!(r.card_zone, Zone::Exile);
    assert_eq!(r.discarded.len(), 1);
    assert_eq!(r.recorded, 1);
    assert!(!r.carrier_present);
}

#[test]
fn mind_rot_batch_is_not_double_counted() {
    let db = shared_card_db().expect("card database");
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let grimoire = s.add_real_card(P0, "Geth's Grimoire", Zone::Battlefield, db);
    for _ in 0..2 {
        s.add_real_card(P0, "Rest in Peace", Zone::Battlefield, db);
    }
    let a = s.add_real_card(P1, "Mind Rot", Zone::Hand, db);
    let b = s.add_real_card(P1, "Swamp", Zone::Hand, db);
    let rot = s.add_real_card(P0, "Mind Rot", Zone::Hand, db);
    s.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = s.build();
    let mut events = runner
        .cast(rot)
        .target_player(P1)
        .resolve()
        .events()
        .to_vec();
    let mut prompts = 0;
    while let WaitingFor::ReplacementChoice { .. } = runner.state().waiting_for.clone() {
        prompts += 1;
        assert!(prompts < 6, "runaway replacement prompts");
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect("answer")
                .events,
        );
        assert!(
            runner.state().pending_single_discard.is_none(),
            "a batch never installs the single-discard carrier"
        );
    }
    assert!(prompts >= 1, "reach guard: the batch paused");
    let st = runner.state();
    assert_eq!(discarded_for(&events, P1).len(), 2);
    assert_eq!(st.cards_discarded_this_turn_by_player[&P1], 2);
    assert_eq!(
        st.stack.iter().filter(|e| e.source_id == grimoire).count(),
        2
    );
    assert_eq!(st.objects[&a].zone, Zone::Exile);
    assert_eq!(st.objects[&b].zone, Zone::Exile);
}
