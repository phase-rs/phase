//! Runtime witnesses for the verbless counted counter-placement conjunct on
//! Jean-Luc Picard, Luminary — the whole printed card, loaded from the
//! committed card fixture and driven through public `GameRunner` actions.
//!
//! Oracle text (verbatim, TRC):
//! > Whenever another historic permanent you control with mana value 3 or
//! > greater enters, draw a card. (Artifacts, legendaries, and Sagas are
//! > historic.)
//! > Seek out — Whenever you draw your second card each turn, put a +1/+1
//! > counter on up to one other target creature and two charge counters on up
//! > to one target Spacecraft.
//!
//! The second ability's tail — "and two charge counters on up to one target
//! Spacecraft" — is a verbless conjunct: no verb of its own, just a count, a
//! counter kind and an "on <recipient>" phrase. Before the parser change it
//! left the whole line as `Effect::Unimplemented`; after it, the line lowers
//! to a head `PutCounter(+1/+1, 1)` over "another creature" with a chained
//! sub `PutCounter(charge, 2)` over "Spacecraft", and the trigger announces
//! TWO independent "up to one" slots when it goes on the stack
//! (CR 603.3d + CR 601.2c). Only textless fillers are synthetic: the two
//! support bodies with rules text are printed cards (Exploration Broodship,
//! Reach Through Mists), taken through `add_real_card`.
//!
//! Each row asserts the trigger's target prompt unconditionally — actor P0,
//! two optional slots, exact offered membership per slot — then the resolved
//! counters, and ends settled (empty stack, priority to P0).

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// The card under test, and the two printed support cards. All three come
/// from the committed integration fixture via `add_real_card`.
const PICARD: &str = "Jean-Luc Picard, Luminary";
/// Printed Spacecraft (Artifact — Spacecraft, 4/4): the "up to one target
/// Spacecraft" recipient. An artifact, never a creature at zero charge
/// counters, so it must not leak into the creature slot.
const BROODSHIP: &str = "Exploration Broodship";
/// Printed {U} cantrip whose entire Oracle text is "Draw a card.": the
/// production-path draw that becomes P0's second card of the turn.
const CANTRIP: &str = "Reach Through Mists";

fn db() -> &'static CardDatabase {
    shared_card_db().expect("the committed integration card fixture is present")
}

struct Table {
    runner: GameRunner,
    picard: ObjectId,
    /// P0 filler creature, 3/3.
    engineer: ObjectId,
    /// P0 filler creature, 1/1.
    cadet: ObjectId,
    /// P1 filler creature, 4/2 — a legal "other target creature" of the
    /// opponent's, so the slot-0 membership assertion proves the filter is
    /// not controller-restricted.
    foe: ObjectId,
    broodship: ObjectId,
    cantrips: Vec<ObjectId>,
}

/// P0's board: Picard, two P0 filler creatures, one P1 filler creature and
/// the printed Spacecraft; `cantrip_copies` printed cantrips in hand, with
/// one pooled blue mana each; textless Islands on top of the library to draw.
///
/// `draws_already_this_turn` simulates how many cards P0 has drawn this turn
/// (CR 504.1: the draw step's mandatory draw counts), the same way the
/// existing second-card-each-turn tests stage it.
fn table(draws_already_this_turn: u32, cantrip_copies: usize) -> Table {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let picard = scenario.add_real_card(P0, PICARD, Zone::Battlefield, db());
    let engineer = scenario.add_creature(P0, "Bridge Engineer", 3, 3).id();
    let cadet = scenario.add_creature(P0, "Bridge Cadet", 1, 1).id();
    let foe = scenario.add_creature(P1, "Away Team Foe", 4, 2).id();
    let broodship = scenario.add_real_card(P0, BROODSHIP, Zone::Battlefield, db());
    let cantrips: Vec<ObjectId> = (0..cantrip_copies)
        .map(|_| scenario.add_real_card(P0, CANTRIP, Zone::Hand, db()))
        .collect();
    for _ in 0..4 {
        scenario.add_card_to_library_top(P0, "Island");
    }
    let pool: Vec<ManaUnit> = (0..cantrip_copies as u64)
        .map(|i| ManaUnit::new(ManaType::Blue, ObjectId(9900 + i), false, Vec::new()))
        .collect();
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].cards_drawn_this_turn = draws_already_this_turn;
    Table {
        runner,
        picard,
        engineer,
        cadet,
        foe,
        broodship,
        cantrips,
    }
}

fn plus_one_count(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn charge_count(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| {
            obj.counters
                .get(&CounterType::Generic("charge".to_string()))
                .copied()
        })
        .unwrap_or(0)
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort_by_key(|id| id.0);
    ids
}

/// Cast through the production reducer with no pre-declared targets, so the
/// engine raises its own prompts (CR 601.2c).
fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the printed cantrip is castable from hand with pooled mana");
}

/// Pass priority until the engine either empties the stack (settled) or
/// raises a prompt (the Seek out trigger's target selection). Fails loudly
/// if neither boundary is reached.
fn pass_until_boundary(runner: &mut GameRunner, label: &str) {
    for _ in 0..40 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().stack.is_empty()
        {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .unwrap_or_else(|err| panic!("{label}: pass priority failed: {err:?}"));
    }
    panic!("{label}: neither a prompt nor an empty stack was reached");
}

/// Pass the whole resolution to a settled board and FAIL if any prompt —
/// target selection above all — is raised on the way. Used only for the
/// first-draw rows, where the expected behavior is silence.
fn pass_until_settled_with_no_prompt(runner: &mut GameRunner, label: &str) {
    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            panic!(
                "{label}: no prompt may appear, got {:?}",
                runner.state().waiting_for
            );
        }
        runner
            .act(GameAction::PassPriority)
            .unwrap_or_else(|err| panic!("{label}: pass priority failed: {err:?}"));
    }
    assert_settled_p0(runner, label);
}

fn assert_settled_p0(runner: &GameRunner, label: &str) {
    assert!(
        runner.state().stack.is_empty(),
        "{label}: the stack must end empty"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "{label}: must end settled with priority to P0, got {:?}",
        runner.state().waiting_for
    );
}

/// The Seek out trigger's target prompt, asserted unconditionally: raised at
/// all, announced by P0, exactly two slots, both declinable. Returns the
/// cloned slots so the caller can assert offered membership.
fn require_seek_out_prompt(runner: &GameRunner) -> Vec<TargetSelectionSlot> {
    let WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Seek out trigger must raise its target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, P0,
        "Picard's controller announces the trigger's targets (CR 603.3d)"
    );
    assert_eq!(
        target_slots.len(),
        2,
        "the verbless conjunct is a second, independent \"up to one\" slot (CR 601.2c)"
    );
    assert!(
        target_slots[0].optional,
        "the \"up to one other target creature\" half is declinable (CR 115.6)"
    );
    assert!(
        target_slots[1].optional,
        "the \"up to one target Spacecraft\" half is declinable (CR 115.6)"
    );
    target_slots
}

/// One slot's offered objects, sorted. No player may appear in these slots.
fn slot_objects(slots: &[TargetSelectionSlot], slot: usize) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = slots[slot]
        .legal_targets
        .iter()
        .map(|target| match target {
            TargetRef::Object(id) => *id,
            TargetRef::Player(player) => {
                panic!("slot {slot} must offer objects, offered player {player:?}")
            }
        })
        .collect();
    ids.sort_by_key(|id| id.0);
    ids
}

/// Answer the live prompt's current slot through the production reducer.
/// `None` declines an "up to one" instance (CR 115.6).
fn choose_slot(runner: &mut GameRunner, slot: usize, choice: Option<ObjectId>) {
    runner
        .act(GameAction::ChooseTarget {
            target: choice.map(TargetRef::Object),
        })
        .unwrap_or_else(|err| panic!("slot {slot} must accept {choice:?}: {err:?}"));
}

/// Positive row. The cantrip's draw is P0's SECOND card of the turn (the
/// draw step's card already happened), so the Seek out trigger fires: slot 0
/// offers exactly the three other creatures (P0's two fillers and P1's, never
/// Picard itself, never the Spacecraft), slot 1 exactly the Spacecraft.
/// Both halves are declared, and exactly one +1/+1 counter and two charge
/// counters land on the chosen objects (CR 122.1 / CR 122.1a).
#[test]
fn picard_second_draw_puts_one_plus_one_counter_and_two_charge_counters() {
    let mut t = table(1, 1);

    cast(&mut t.runner, t.cantrips[0]);
    pass_until_boundary(&mut t.runner, "the second-card draw resolves");

    let slots = require_seek_out_prompt(&t.runner);
    assert_eq!(
        slot_objects(&slots, 0),
        sorted(vec![t.engineer, t.cadet, t.foe]),
        "the creature slot offers exactly P0's and P1's creatures other than \
         Picard itself — never Picard, never the Spacecraft"
    );
    assert_eq!(
        slot_objects(&slots, 1),
        vec![t.broodship],
        "the Spacecraft slot offers exactly the printed Spacecraft"
    );

    choose_slot(&mut t.runner, 0, Some(t.engineer));
    choose_slot(&mut t.runner, 1, Some(t.broodship));
    pass_until_boundary(&mut t.runner, "the Seek out trigger resolves");
    assert_settled_p0(&t.runner, "positive row");

    assert_eq!(
        plus_one_count(&t.runner, t.engineer),
        1,
        "the declared creature takes exactly one +1/+1 counter (CR 122.1a)"
    );
    assert_eq!(
        charge_count(&t.runner, t.broodship),
        2,
        "the declared Spacecraft takes exactly two charge counters (CR 122.1)"
    );
    assert_eq!(
        plus_one_count(&t.runner, t.cadet),
        0,
        "the unchosen P0 creature takes nothing"
    );
    assert_eq!(
        plus_one_count(&t.runner, t.foe),
        0,
        "the unchosen P1 creature takes nothing"
    );
    assert_eq!(
        plus_one_count(&t.runner, t.picard),
        0,
        "\"other\" excludes Picard itself"
    );
    assert_eq!(
        plus_one_count(&t.runner, t.broodship),
        0,
        "the Spacecraft takes no +1/+1 counter from the creature half"
    );
    assert_eq!(
        charge_count(&t.runner, t.engineer),
        0,
        "the creature takes no charge counter from the Spacecraft half"
    );
}

/// Zero case (CR 115.6): the creature half is declined, the Spacecraft half
/// declared. The two halves are independent instances of "target", so the
/// chained sub-ability still resolves with its own declaration: only the
/// Spacecraft is charged, nothing anywhere takes a +1/+1 counter. The
/// counters landing on the Spacecraft are the reach guard proving the
/// trigger resolved.
#[test]
fn picard_second_draw_declined_creature_slot_charges_only_the_spacecraft() {
    let mut t = table(1, 1);

    cast(&mut t.runner, t.cantrips[0]);
    pass_until_boundary(&mut t.runner, "the second-card draw resolves");

    let slots = require_seek_out_prompt(&t.runner);
    assert_eq!(
        slot_objects(&slots, 0),
        sorted(vec![t.engineer, t.cadet, t.foe]),
        "the creature half is declinable because it offers other creatures"
    );
    assert_eq!(
        slot_objects(&slots, 1),
        vec![t.broodship],
        "the Spacecraft half still offers the printed Spacecraft"
    );

    choose_slot(&mut t.runner, 0, None);
    choose_slot(&mut t.runner, 1, Some(t.broodship));
    pass_until_boundary(&mut t.runner, "the Seek out trigger resolves");
    assert_settled_p0(&t.runner, "declined creature half");

    assert_eq!(
        charge_count(&t.runner, t.broodship),
        2,
        "reach guard: the declared half places exactly two charge counters"
    );
    for id in [t.picard, t.engineer, t.cadet, t.foe, t.broodship] {
        assert_eq!(
            plus_one_count(&t.runner, id),
            0,
            "no object takes a +1/+1 counter when the creature half is \
             declined, got one on {id:?}"
        );
    }
    assert_eq!(
        charge_count(&t.runner, t.engineer),
        0,
        "no charge counter leaks off the Spacecraft half"
    );
}

/// Hostile row. P0 has drawn nothing this turn, so the cantrip's draw is the
/// FIRST card of the turn: the Seek out trigger must not fire — no target
/// prompt, no counters. The same board then draws its second card of the
/// turn and the trigger fires with the full prompt and both counters, which
/// is the positive reach guard proving the silence above is the ordinal
/// gate and not a dead ability.
#[test]
fn picard_first_draw_of_the_turn_does_not_trigger_seek_out() {
    let mut t = table(0, 2);

    cast(&mut t.runner, t.cantrips[0]);
    pass_until_settled_with_no_prompt(&mut t.runner, "the first draw of the turn");
    for id in [t.picard, t.engineer, t.cadet, t.foe, t.broodship] {
        assert_eq!(
            plus_one_count(&t.runner, id),
            0,
            "the first draw of the turn puts no +1/+1 counter on {id:?}"
        );
        assert_eq!(
            charge_count(&t.runner, id),
            0,
            "the first draw of the turn puts no charge counter on {id:?}"
        );
    }

    // Reach guard: the NEXT draw of the same turn is the second card, so the
    // full trigger must fire on this exact board.
    cast(&mut t.runner, t.cantrips[1]);
    pass_until_boundary(&mut t.runner, "the second-card draw resolves");
    let slots = require_seek_out_prompt(&t.runner);
    assert_eq!(
        slot_objects(&slots, 0),
        sorted(vec![t.engineer, t.cadet, t.foe]),
        "the same board now offers the three other creatures"
    );
    assert_eq!(
        slot_objects(&slots, 1),
        vec![t.broodship],
        "the same board now offers the printed Spacecraft"
    );

    choose_slot(&mut t.runner, 0, Some(t.cadet));
    choose_slot(&mut t.runner, 1, Some(t.broodship));
    pass_until_boundary(&mut t.runner, "the Seek out trigger resolves");
    assert_settled_p0(&t.runner, "second draw after a quiet first draw");

    assert_eq!(
        plus_one_count(&t.runner, t.cadet),
        1,
        "reach guard: the second card of the turn puts the +1/+1 counter on \
         the declared creature"
    );
    assert_eq!(
        charge_count(&t.runner, t.broodship),
        2,
        "reach guard: the second card of the turn puts the two charge \
         counters on the declared Spacecraft"
    );
}
