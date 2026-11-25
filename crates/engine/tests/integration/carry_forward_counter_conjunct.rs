//! Runtime witnesses for the verbless counted-counter conjunct released by
//! `starts_counted_counter_placement` + the carry-forward reparse in
//! `parser/oracle_effect/mod.rs`. CR 608.2c: a conjunct like "and two +1/+1
//! counters on another target creature" has no verb of its own; the primary
//! verb ("put") is prepended so the conjunct parses as a counter placement
//! with its own target slot. Before the fix each such conjunct lowered to
//! `Effect::Unimplemented`, so the whole card no-opped or never prompted.
//!
//! Every card under test is the printed card, staged through `add_real_card`
//! from the committed card-database fixture; every other body is a textless
//! vanilla filler (`add_vanilla`) that carries no ability, so the only rules
//! text exercised is the printed card's. All rows drive the production
//! pipeline (`GameScenario` + `GameRunner` public actions, the real turn
//! structure) and end settled: empty stack and `Priority` for P0.
//!
//! CR references verified against docs/MagicCompRules.txt:
//!   - CR 608.2c: the controller follows the instructions in the order
//!     written; later text ("and …") is part of the same instruction.
//!   - CR 601.2c: each target is announced while the spell/ability goes on
//!     the stack, one slot per instance of the word "target".
//!   - CR 122.1a: a +1/+1 counter adds 1 to power and toughness.
//!   - CR 503.1a + CR 603.3d: upkeep triggers are put on the stack (their
//!     targets announced then) before the active player gets priority.
//!   - CR 306.5b: a planeswalker enters with loyalty counters equal to its
//!     printed loyalty.
//!   - CR 606.4: the cost of a loyalty ability is putting on / removing
//!     loyalty counters, paid at activation.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::EngineError;
use engine::types::ability::{AbilityCost, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn db() -> &'static engine::database::card_db::CardDatabase {
    shared_card_db().expect("the committed integration card fixture is present")
}

/// Stand Together is {3}{G}{G}: two green plus three generic, all payable
/// from five green units in the pool.
fn stand_together_pool() -> Vec<ManaUnit> {
    [ManaType::Green; 5]
        .into_iter()
        .enumerate()
        .map(|(i, kind)| ManaUnit::new(kind, ObjectId(9_600 + i as u64), false, vec![]))
        .collect()
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort_by_key(|o| o.0);
    ids
}

/// The object ids offered for the target slot the engine is asking about
/// (cast-time or trigger-time prompt). Asserts P0 is the chooser.
fn offered_targets(runner: &GameRunner) -> Vec<ObjectId> {
    let (player, selection) = match &runner.state().waiting_for {
        WaitingFor::TargetSelection {
            player, selection, ..
        }
        | WaitingFor::TriggerTargetSelection {
            player, selection, ..
        } => (*player, selection),
        other => panic!("expected a target prompt, got {other:?}"),
    };
    assert_eq!(player, P0, "P0 announces the targets");
    let mut ids: Vec<ObjectId> = selection
        .current_legal_targets
        .iter()
        .map(|t| match t {
            TargetRef::Object(o) => *o,
            TargetRef::Player(p) => panic!("an object prompt offered player {p:?}"),
        })
        .collect();
    ids.sort_by_key(|o| o.0);
    ids
}

fn choose_object(runner: &mut GameRunner, id: ObjectId) {
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id)),
        })
        .expect("choose target");
}

fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
}

fn activate(runner: &mut GameRunner, source: ObjectId, index: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate");
}

/// Pass priority while an object is on the stack; returns the waiting state
/// the engine stopped in (a prompt, or priority with an empty stack).
fn resolve_stack(runner: &mut GameRunner) -> WaitingFor {
    for _ in 0..40 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().stack.is_empty()
        {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    runner.state().waiting_for.clone()
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn loyalty_count(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Loyalty)
        .copied()
        .unwrap_or(0)
}

fn power_toughness(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let object = &runner.state().objects[&id];
    (object.power.unwrap_or(0), object.toughness.unwrap_or(0))
}

/// Scenario-setup loyalty override (the `activation_manual_mana_payment`
/// pattern): keeps `loyalty` and `counters[Loyalty]` in sync (CR 306.5b).
fn set_loyalty(runner: &mut GameRunner, id: ObjectId, loyalty: u32) {
    let object = runner.state_mut().objects.get_mut(&id).expect("the walker");
    object.loyalty = Some(loyalty);
    object.counters.insert(CounterType::Loyalty, loyalty);
}

/// Index of the printed loyalty ability with the given loyalty cost
/// (the parser maps the printed [+N]/[−N] bracket to a signed amount).
fn loyalty_index(runner: &GameRunner, source: ObjectId, amount: i32) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.cost == Some(AbilityCost::Loyalty { amount }))
        .unwrap_or_else(|| {
            panic!("expected a [{amount}] loyalty ability on the printed planeswalker")
        })
}

fn assert_settled(runner: &GameRunner, what: &str) {
    let waiting = &runner.state().waiting_for;
    assert!(
        matches!(waiting, WaitingFor::Priority { player } if *player == P0),
        "{what}: expected P0 priority with an empty stack, got {waiting:?}"
    );
    assert!(
        runner.state().stack.is_empty(),
        "{what}: the stack is empty"
    );
}

// ---------------------------------------------------------------------------------------------
// Stand Together — Instant, {3}{G}{G}. Verbatim Oracle text:
// "Put two +1/+1 counters on target creature and two +1/+1 counters on
// another target creature."
// ---------------------------------------------------------------------------------------------

/// CR 601.2c + CR 122.1a: both target slots are announced in written order;
/// slot 2's "another target creature" must not re-offer slot 1's creature.
/// Each chosen creature ends with exactly two +1/+1 counters (P/T +2/+2) and
/// a third bystander stays empty. With the fix reverted the conjunct is
/// `Unimplemented`, the whole card no-ops and every counter reads zero.
#[test]
fn stand_together_counters_two_creatures_and_spares_the_bystander() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, stand_together_pool());
    let stand_together = scenario.add_real_card(P0, "Stand Together", Zone::Hand, db());
    let mine = scenario.add_vanilla(P0, 2, 2);
    let theirs = scenario.add_vanilla(P1, 3, 3);
    let bystander = scenario.add_vanilla(P0, 4, 4);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db());

    cast(&mut runner, stand_together);
    assert_eq!(
        offered_targets(&runner),
        sorted(vec![mine, theirs, bystander]),
        "slot 1 offers every creature on the battlefield"
    );
    choose_object(&mut runner, mine);
    assert_eq!(
        offered_targets(&runner),
        sorted(vec![theirs, bystander]),
        "slot 2 (\"another target creature\") must not re-offer slot 1's choice"
    );
    choose_object(&mut runner, theirs);
    resolve_stack(&mut runner);

    assert_eq!(
        p1p1(&runner, mine),
        2,
        "slot 1's creature ends with exactly two +1/+1 counters"
    );
    assert_eq!(
        p1p1(&runner, theirs),
        2,
        "slot 2's creature ends with exactly two +1/+1 counters"
    );
    assert_eq!(p1p1(&runner, bystander), 0, "the bystander is not targeted");
    assert_eq!(
        power_toughness(&runner, mine),
        (4, 4),
        "2/2 plus two counters"
    );
    assert_eq!(
        power_toughness(&runner, theirs),
        (5, 5),
        "3/3 plus two counters"
    );
    assert_eq!(
        runner.state().objects[&stand_together].zone,
        Zone::Graveyard,
        "the instant finished resolving"
    );
    assert_settled(&runner, "Stand Together");
}

/// Hostile sibling: CR 601.2c — "another target creature" cannot name the
/// creature already chosen for slot 1. The duplicate must be rejected through
/// the public action path with nothing applied, and the same test then
/// completes the cast legally so a positive reach guard proves the rejection
/// was a targeting gate, not a silent no-op of the whole card.
#[test]
fn stand_together_rejects_the_same_creature_for_both_slots() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, stand_together_pool());
    let stand_together = scenario.add_real_card(P0, "Stand Together", Zone::Hand, db());
    let mine = scenario.add_vanilla(P0, 2, 2);
    let theirs = scenario.add_vanilla(P1, 3, 3);
    let bystander = scenario.add_vanilla(P0, 4, 4);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db());

    cast(&mut runner, stand_together);
    choose_object(&mut runner, mine);

    // Submit the SAME creature for slot 2 through the public action path.
    let rejected = runner.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(mine)),
    });
    assert!(
        matches!(&rejected, Err(EngineError::InvalidAction(message)) if message.contains("Illegal target")),
        "re-submitting slot 1's creature must be rejected as an illegal target, got {rejected:?}"
    );

    // Nothing was applied: the cast is still mid-announcement and the
    // slot-2 prompt (with its exclusion) is unchanged.
    assert_eq!(
        offered_targets(&runner),
        sorted(vec![theirs, bystander]),
        "the rejection leaves the slot-2 prompt intact"
    );
    assert_eq!(
        p1p1(&runner, mine),
        0,
        "no counters were placed by the rejected attempt"
    );
    assert_eq!(p1p1(&runner, theirs), 0, "no counters were placed anywhere");

    // Positive reach guard: complete the cast with legal, distinct creatures
    // and the effect runs on exactly them.
    choose_object(&mut runner, theirs);
    resolve_stack(&mut runner);
    assert_eq!(
        p1p1(&runner, mine),
        2,
        "reach guard: slot 1's creature got its two counters"
    );
    assert_eq!(
        p1p1(&runner, theirs),
        2,
        "reach guard: slot 2's creature got its two counters"
    );
    assert_eq!(
        p1p1(&runner, bystander),
        0,
        "the bystander still got nothing"
    );
    assert_eq!(
        runner.state().objects[&stand_together].zone,
        Zone::Graveyard,
        "reach guard: the spell resolved"
    );
    assert_settled(&runner, "Stand Together duplicate rejection");
}

// ---------------------------------------------------------------------------------------------
// Evolutionary Escalation — Enchantment, {1}{G}. Verbatim Oracle text:
// "At the beginning of your upkeep, put three +1/+1 counters on target
// creature you control and three +1/+1 counters on target creature an
// opponent controls."
// ---------------------------------------------------------------------------------------------

/// CR 503.1a + CR 603.3d + CR 601.2c: the upkeep trigger goes on the stack
/// with P0 (its controller) announcing both slots — slot 1 offers exactly
/// P0's creatures, slot 2 exactly an opponent's. Each side has two candidates
/// so the choice is discriminating: exactly three counters land on each
/// chosen creature and the unchosen candidates stay empty. With the fix
/// reverted the verbless conjunct is `Unimplemented` and the trigger prompt
/// never appears.
#[test]
fn evolutionary_escalation_upkeep_counters_one_creature_each_side() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let _escalation =
        scenario.add_real_card(P0, "Evolutionary Escalation", Zone::Battlefield, db());
    let mine_a = scenario.add_vanilla(P0, 2, 2);
    let mine_b = scenario.add_vanilla(P0, 2, 2);
    let theirs_c = scenario.add_vanilla(P1, 3, 3);
    let theirs_d = scenario.add_vanilla(P1, 3, 3);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db());
    // Pre-existing battlefield: turn 2 is P0's turn, entered from Untap.
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    runner.advance_to_upkeep();

    let mut slots_answered = 0;
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 50, "stuck at {:?}", runner.state().waiting_for);
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { player, .. } => {
                assert_eq!(player, P0, "the trigger's controller announces both slots");
                match slots_answered {
                    0 => {
                        assert_eq!(
                            offered_targets(&runner),
                            sorted(vec![mine_a, mine_b]),
                            "slot 1 offers exactly P0's creatures (never P1's)"
                        );
                        choose_object(&mut runner, mine_a);
                    }
                    1 => {
                        assert_eq!(
                            offered_targets(&runner),
                            sorted(vec![theirs_c, theirs_d]),
                            "slot 2 offers exactly the opponent's creatures (never P0's)"
                        );
                        choose_object(&mut runner, theirs_c);
                    }
                    _ => panic!("the trigger has exactly two target slots"),
                }
                slots_answered += 1;
            }
            WaitingFor::Priority { .. }
                if runner.state().stack.is_empty() && slots_answered == 2 =>
            {
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority in upkeep");
            }
            other => panic!("unexpected waiting state while driving the upkeep: {other:?}"),
        }
    }

    assert_eq!(
        p1p1(&runner, mine_a),
        3,
        "exactly three +1/+1 counters on P0's chosen creature"
    );
    assert_eq!(
        p1p1(&runner, theirs_c),
        3,
        "exactly three +1/+1 counters on the opponent's chosen creature"
    );
    assert_eq!(
        p1p1(&runner, mine_b),
        0,
        "P0's unchosen candidate stays empty"
    );
    assert_eq!(
        p1p1(&runner, theirs_d),
        0,
        "the opponent's unchosen candidate stays empty"
    );
    assert_eq!(
        power_toughness(&runner, mine_a),
        (5, 5),
        "CR 122.1a: 2/2 plus three counters"
    );
    assert_eq!(
        power_toughness(&runner, theirs_c),
        (6, 6),
        "CR 122.1a: 3/3 plus three counters"
    );
    assert_settled(&runner, "Evolutionary Escalation");
}

// ---------------------------------------------------------------------------------------------
// Ajani Unyielding — Legendary Planeswalker Ajani, {4}{G}{W}, printed loyalty
// 4. Verbatim Oracle text:
// "[+2]: Reveal the top three cards of your library. Put all nonland
// permanent cards revealed this way into your hand and the rest on the bottom
// of your library in any order.
// [−2]: Exile target creature. Its controller gains life equal to its power.
// [−9]: Put five +1/+1 counters on each creature you control and five
// loyalty counters on each other planeswalker you control."
// ---------------------------------------------------------------------------------------------

/// Witness for the [−9] only (its verbless conjunct "and five loyalty
/// counters on each other planeswalker you control" previously lowered to
/// `Unimplemented`, so the whole ability no-opped). CR 606.4: the nine
/// loyalty leave at activation, before resolution. Discriminators: the
/// pre-marked creature ends at six counters (one + five) while its plain
/// sibling ends at exactly five; the OTHER planeswalker gains exactly five
/// loyalty (4 → 9); Ajani himself ends at one (10 − 9 — gaining the five
/// himself would read six); and everything P1 controls ("you control" is
/// P0-scoped) stays untouched.
#[test]
fn ajani_unyielding_minus_nine_pumps_creatures_and_other_walkers_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ajani = scenario.add_real_card(P0, "Ajani Unyielding", Zone::Battlefield, db());
    let chandra = scenario.add_real_card(P0, "Chandra, Torch of Defiance", Zone::Battlefield, db());
    let jace = scenario.add_real_card(P1, "Jace, the Mind Sculptor", Zone::Battlefield, db());
    let mine_plain = scenario.add_vanilla(P0, 2, 2);
    let mine_marked = scenario.add_vanilla(P0, 1, 1);
    scenario.with_counter(mine_marked, CounterType::Plus1Plus1, 1);
    let theirs = scenario.add_vanilla(P1, 3, 3);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db());

    // Scenario setup: loyalty 10 so the [−9] is payable (CR 118.3: a cost
    // must be payable in full) and the remainder (1) is a nonzero,
    // discriminating value.
    set_loyalty(&mut runner, ajani, 10);
    // Fixture reach-guards: `add_real_card` applied the printed loyalty
    // (CR 306.5b) to the two untouched walkers.
    assert_eq!(runner.state().objects[&ajani].loyalty, Some(10));
    assert_eq!(
        runner.state().objects[&chandra].loyalty,
        Some(4),
        "Chandra, Torch of Defiance's printed loyalty is 4"
    );
    assert_eq!(
        runner.state().objects[&jace].loyalty,
        Some(3),
        "Jace, the Mind Sculptor's printed loyalty is 3"
    );
    assert_eq!(
        p1p1(&runner, mine_marked),
        1,
        "the pre-existing counter is staged"
    );

    let minus_nine = loyalty_index(&runner, ajani, -9);
    activate(&mut runner, ajani, minus_nine);

    // CR 606.4: the cost leaves at activation; the counters land at resolution.
    assert_eq!(
        runner.state().objects[&ajani].loyalty,
        Some(1),
        "the [−9] cost is paid at activation (10 - 9)"
    );
    assert!(
        !runner.state().stack.is_empty(),
        "the ability is on the stack, not yet resolved"
    );
    assert_eq!(p1p1(&runner, mine_plain), 0, "nothing applied yet");

    resolve_stack(&mut runner);

    assert_eq!(
        p1p1(&runner, mine_plain),
        5,
        "the unmarked creature gains exactly five +1/+1 counters"
    );
    assert_eq!(
        p1p1(&runner, mine_marked),
        6,
        "one pre-existing counter plus exactly five new ones = six"
    );
    assert_eq!(
        power_toughness(&runner, mine_plain),
        (7, 7),
        "CR 122.1a: 2/2 plus five counters"
    );
    assert_eq!(
        runner.state().objects[&chandra].loyalty,
        Some(9),
        "the OTHER planeswalker gains exactly five loyalty counters (4 + 5)"
    );
    assert_eq!(loyalty_count(&runner, chandra), 9);
    assert_eq!(
        runner.state().objects[&ajani].loyalty,
        Some(1),
        "Ajani paid nine and, being the source (\"each OTHER planeswalker\"), gained none of the five"
    );
    assert_eq!(loyalty_count(&runner, ajani), 1);
    // Hostile controls: "each creature you control" / "each other
    // planeswalker you control" exclude everything P1 controls.
    assert_eq!(
        p1p1(&runner, theirs),
        0,
        "the opponent's creature is untouched"
    );
    assert_eq!(
        power_toughness(&runner, theirs),
        (3, 3),
        "no counter modified its P/T"
    );
    assert_eq!(
        runner.state().objects[&jace].loyalty,
        Some(3),
        "the opponent's planeswalker is untouched"
    );
    assert_eq!(loyalty_count(&runner, jace), 3);
    assert_settled(&runner, "Ajani Unyielding [−9]");
}
