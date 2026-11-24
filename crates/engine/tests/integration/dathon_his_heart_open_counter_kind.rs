//! Dathon, His Heart Open (TRK):
//!
//! "Whenever you cast an enchantment spell, draw a card. If it's a Saga spell,
//!  Dathon deals 2 damage to each opponent.
//!  {2}: Choose a counter on target enchantment you control. Put another
//!  counter of that kind on it. Activate only as a sorcery."
//!
//! CR 608.2d + CR 122.1: the controller chooses one kind among those already
//! on the target (no choice with one kind, none with zero), then CR 122.6 one
//! more counter of that kind is put on it. The cast trigger (draw; two damage
//! when the spell is a Saga) and the sorcery-speed restriction have their own
//! whole-card witnesses at the end of the file.

use crate::support::shared_card_db;
use engine::game::coverage::card_face_gaps;
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn counters(runner: &GameRunner, id: ObjectId, kind: CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&kind)
        .copied()
        .unwrap_or(0)
}

fn contains_unimplemented(def: &AbilityDefinition) -> bool {
    matches!(def.effect.as_ref(), Effect::Unimplemented { .. })
        || def
            .sub_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
}

fn setup(counters: &[(CounterType, u32)]) -> (GameRunner, ObjectId, ObjectId) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dathon = scenario.add_real_card(P0, "Dathon, His Heart Open", Zone::Battlefield, db);
    // Blank enchantment filler: Dathon's ability needs a target enchantment.
    let ench = scenario
        .add_enchantment_from_oracle(P0, "Test Enchantment", "")
        .id();
    for (kind, n) in counters {
        scenario.with_counter(ench, kind.clone(), *n);
    }
    // Dathon's printed {2} activation cost.
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]); 2],
    );
    let runner = scenario.build();
    let obj = &runner.state().objects[&dathon];
    assert!(
        !obj.abilities.iter().any(contains_unimplemented),
        "Dathon's activated ability must parse with no Unimplemented residue: {:?}",
        obj.abilities
    );
    (runner, dathon, ench)
}

fn activated_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| matches!(a.kind, AbilityKind::Activated))
        .expect("Dathon's activated ability")
}

#[test]
fn single_counter_kind_gets_one_more_of_that_kind() {
    let (mut runner, dathon, ench) = setup(&[(CounterType::Stun, 1)]);
    let idx = activated_index(&runner, dathon);
    runner.activate(dathon, idx).target_object(ench).resolve();
    assert_eq!(counters(&runner, ench, CounterType::Stun), 2);
}

/// Settle the stack after the counter-kind answer.
fn settle(runner: &mut GameRunner) {
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
}

/// CR 608.2d + CR 122.6: with two kinds present, Dathon's controller is asked
/// which one, and exactly the chosen kind grows — checked for each kind, so an
/// implementation that ignored the answer cannot pass both.
#[test]
fn two_kinds_prompt_and_only_the_chosen_kind_grows() {
    for (chosen, other) in [
        (CounterType::Stun, CounterType::Shield),
        (CounterType::Shield, CounterType::Stun),
    ] {
        let (mut runner, dathon, ench) = setup(&[(CounterType::Stun, 1), (CounterType::Shield, 1)]);
        let idx = activated_index(&runner, dathon);
        runner.activate(dathon, idx).target_object(ench).resolve();

        let WaitingFor::NamedChoice {
            player, options, ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "two counter kinds must prompt, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(player, P0, "Dathon's controller chooses the kind");
        assert_eq!(options.len(), 2, "one option per kind on the target");
        let choice = options
            .iter()
            .find(|option| option.eq_ignore_ascii_case(chosen.as_str().as_ref()))
            .cloned()
            .unwrap_or_else(|| panic!("{chosen:?} is an option: {options:?}"));
        runner
            .act(GameAction::ChooseOption { choice })
            .expect("answer the counter-kind choice");
        settle(&mut runner);

        assert_eq!(
            counters(&runner, ench, chosen.clone()),
            2,
            "{chosen:?} grew"
        );
        assert_eq!(
            counters(&runner, ench, other.clone()),
            1,
            "{other:?} unchanged"
        );
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

/// Clockspinning shares the "choose a counter on target ..." clause: "Choose a
/// counter on target permanent or suspended card. Remove that counter from
/// that permanent or card or put another of those counters on it." CR 608.2c:
/// "that counter" is the kind just chosen, so after choosing Stun only the two
/// Stun adjustments are offered.
#[test]
fn clockspinning_offers_only_the_chosen_kinds_adjustments() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ench = scenario
        .add_enchantment_from_oracle(P0, "Test Enchantment", "")
        .id();
    scenario.with_counter(ench, CounterType::Stun, 1);
    scenario.with_counter(ench, CounterType::Shield, 1);
    let spell = scenario.add_real_card(P0, "Clockspinning", Zone::Hand, db);
    // Clockspinning's printed {U}; its Buyback {3} stays declined (the cast
    // driver's default optional policy).
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(ench).commit().resolve();
    let WaitingFor::NamedChoice { options, .. } = outcome.final_waiting_for().clone() else {
        panic!(
            "the counter is chosen first, got {:?}",
            outcome.final_waiting_for()
        );
    };
    let choice = options
        .iter()
        .find(|option| option.eq_ignore_ascii_case("stun"))
        .cloned()
        .expect("Stun is an option");
    runner
        .act(GameAction::ChooseOption { choice })
        .expect("choose the Stun counter");
    let WaitingFor::ChooseOneOfBranch { branches, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the adjustment is offered next, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        branches.len(),
        2,
        "remove or add a Stun counter only: {branches:?}"
    );
    runner
        .act(GameAction::ChooseBranch { index: 1 })
        .expect("put another Stun counter");
    settle(&mut runner);
    assert_eq!(counters(&runner, ench, CounterType::Stun), 2);
    assert_eq!(counters(&runner, ench, CounterType::Shield), 1);
}

#[test]
fn no_counters_adds_none() {
    let (mut runner, dathon, ench) = setup(&[]);
    let idx = activated_index(&runner, dathon);
    runner.activate(dathon, idx).target_object(ench).resolve();
    let obj = &runner.state().objects[&ench];
    assert!(obj.counters.values().all(|n| *n == 0), "nothing to copy");
}

/// The object ids currently in `player`'s hand, in hand order.
fn hand_cards(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .iter()
        .copied()
        .collect()
}

/// Units remaining in `player`'s mana pool.
fn mana_total(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .mana_pool
        .total()
}

/// P0 controls Dathon (printed card, `add_real_card`) with the printed
/// enchantment `spell_name` in hand, a mana pool seeded with exactly `mana`
/// (the printed cost), and "Island" staged on top of the library — the
/// `with_library_top(P0, &["Island"])` convention the Quark tests use, so the
/// card the cast trigger draws is known in advance. P0's precombat main with
/// an empty stack: a legal cast window (CR 307.1; CR 505.1 + CR 505.6a).
fn dathon_cast_setup(spell_name: &str, mana: Vec<ManaUnit>) -> (GameRunner, ObjectId) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Dathon, His Heart Open", Zone::Battlefield, db);
    let spell = scenario.add_real_card(P0, spell_name, Zone::Hand, db);
    scenario.with_mana_pool(P0, mana);
    scenario.with_library_top(P0, &["Island"]);
    let runner = scenario.build();
    (runner, spell)
}

/// Pass priority until the stack drains; any prompt fails loudly, so an
/// unexpected choice cannot be answered blindly.
fn settle_without_prompts(runner: &mut GameRunner) {
    for _ in 0..20 {
        let WaitingFor::Priority { .. } = runner.state().waiting_for else {
            panic!(
                "unexpected prompt while settling: {:?}",
                runner.state().waiting_for
            );
        };
        if runner.state().stack.is_empty() {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass so the top of the stack resolves");
    }
    panic!("the stack never drained: {:?}", runner.state().stack);
}

/// CR 603.1 + CR 121.1: casting a non-Saga enchantment makes Dathon's
/// "Whenever you cast an enchantment spell, draw a card" trigger fire and
/// resolve — P0's hand ends holding exactly the staged library-top card.
/// The Saga rider does not fire: P1's life is untouched.
#[test]
fn casting_a_non_saga_enchantment_draws_without_damage() {
    // Printed Bad Moon cost, read from the fixture: {1}{B}.
    let (mut runner, moon) = dathon_cast_setup(
        "Bad Moon",
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ],
    );
    assert_eq!(hand_cards(&runner, P0).len(), 1, "only Bad Moon in hand");
    assert_eq!(hand_cards(&runner, P1).len(), 0);
    assert_eq!(runner.life(P1), 20, "precondition: P1 at 20");

    // Production path: announce, pay the printed cost, resolve (CR 601.2a-h),
    // then let the cast trigger resolve off the stack.
    runner.cast(moon).resolve();

    // Control: this fails if the trigger did not draw — casting moved the
    // Moon out of hand, so only the trigger's draw can put the known card
    // there.
    let hand = hand_cards(&runner, P0);
    assert_eq!(hand.len(), 1, "drew exactly one card");
    assert_eq!(
        runner.state().objects[&hand[0]].name,
        "Island",
        "the drawn card is the staged library top"
    );
    assert_eq!(
        runner.state().objects[&moon].zone,
        Zone::Battlefield,
        "Bad Moon resolved"
    );
    assert_eq!(mana_total(&runner, P0), 0, "the printed cost was paid");
    assert_eq!(hand_cards(&runner, P1).len(), 0, "P1 drew nothing");
    assert_eq!(runner.life(P1), 20, "not a Saga — no damage to opponents");
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 603.1 + CR 121.1 + the Saga rider: casting a Saga draws the card AND
/// has Dathon deal 2 damage to each opponent (P1: 20 -> 18). CR 714.3a: the Saga
/// enters with a lore counter and Chapter I resolves (a 2/2 white Vigilance
/// Knight token), driven to rest without any prompt.
#[test]
fn casting_a_saga_also_deals_two_damage_to_each_opponent() {
    // Printed History of Benalia cost, read from the fixture: {1}{W}{W}.
    let (mut runner, saga) = dathon_cast_setup(
        "History of Benalia",
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
        ],
    );
    assert_eq!(runner.life(P1), 20, "precondition: P1 at 20");
    assert_eq!(runner.life(P0), 20, "precondition: P0 at 20");

    runner.cast(saga).resolve();
    // Chapter I (token creation) is choice-free; any prompt fails the settle.
    settle_without_prompts(&mut runner);

    let hand = hand_cards(&runner, P0);
    assert_eq!(hand.len(), 1, "drew exactly one card");
    assert_eq!(
        runner.state().objects[&hand[0]].name,
        "Island",
        "the drawn card is the staged library top"
    );
    assert_eq!(
        runner.state().objects[&saga].zone,
        Zone::Battlefield,
        "the Saga resolved"
    );
    assert_eq!(mana_total(&runner, P0), 0, "the printed cost was paid");
    assert_eq!(runner.life(P1), 18, "2 damage to each opponent (P1)");
    assert_eq!(runner.life(P0), 20, "P0 is not their own opponent");
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 603.2 + CR 109.5: Dathon's "Whenever YOU cast an enchantment spell" is its
/// controller's cast only. P1 casting the same Saga (History of Benalia, as in
/// `casting_a_saga_also_deals_two_damage_to_each_opponent`) on P1's own turn
/// neither draws P0 a card nor deals Dathon's 2 damage: the paired positive is
/// that test, where P0 casts the identical Saga and both effects happen.
#[test]
fn an_opponents_saga_cast_does_not_trigger_dathon() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Dathon, His Heart Open", Zone::Battlefield, db);
    let saga = scenario.add_real_card(P1, "History of Benalia", Zone::Hand, db);
    scenario.with_library_top(P0, &["Island"]);
    let mut runner = scenario.build();
    // CR 117.1a: a Saga is cast at sorcery speed, so it is P1's turn with P1
    // holding priority; fund the printed {1}{W}{W}.
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
    for color in [ManaType::Colorless, ManaType::White, ManaType::White] {
        runner.state_mut().players[1].mana_pool.add(ManaUnit::new(
            color,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    assert_eq!(runner.life(P1), 20, "precondition: P1 at 20");

    runner.cast(saga).resolve();
    settle_without_prompts(&mut runner);

    // REACH: the cast went through, so the absence below is Dathon's scope.
    assert_eq!(
        runner.state().objects[&saga].zone,
        Zone::Battlefield,
        "P1's Saga resolved"
    );
    assert_eq!(
        hand_cards(&runner, P0).len(),
        0,
        "Dathon's draw did not fire on an opponent's cast"
    );
    assert_eq!(runner.life(P1), 20, "no Dathon damage to the caster");
    assert_eq!(runner.life(P0), 20);
}

/// Dathon plus the blank enchantment filler carrying one Stun counter — the
/// existing `setup` board — but staged at `phase`, so the same printed {2}
/// activation can be attempted in an illegal and a legal window.
fn dathon_board_at(phase: Phase) -> (GameRunner, ObjectId, ObjectId) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    let dathon = scenario.add_real_card(P0, "Dathon, His Heart Open", Zone::Battlefield, db);
    // Blank enchantment filler: the {2} ability needs a target enchantment
    // (same exemption as the existing file's "Test Enchantment").
    let ench = scenario
        .add_enchantment_from_oracle(P0, "Test Enchantment", "")
        .id();
    scenario.with_counter(ench, CounterType::Stun, 1);
    // Dathon's printed {2} activation cost, funded exactly.
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]); 2],
    );
    let runner = scenario.build();
    let obj = &runner.state().objects[&dathon];
    assert!(
        !obj.abilities.iter().any(contains_unimplemented),
        "Dathon's activated ability must parse with no Unimplemented residue: {:?}",
        obj.abilities
    );
    (runner, dathon, ench)
}

/// CR 307.1 + CR 602.5d ("Activate only as a sorcery"): the printed {2}
/// counter ability is rejected outside a main phase with an empty stack —
/// here P0's own beginning-of-combat step (the scenario API stages it
/// directly via `at_phase(Phase::BeginCombat)`; P1's turn is the equivalent
/// negative window but needs raw state mutation to stage) — while the same
/// activation on the same board at P0's precombat main succeeds. Both sides
/// live in one test so an engine that always rejected OR always allowed
/// cannot pass.
#[test]
fn dathon_counter_ability_is_sorcery_speed_only() {
    // Negative: P0's combat step  — priority is P0's, the stack is empty, the
    // pool covers {2}: everything is legal except the timing, so the
    // rejection must come from the printed sorcery-speed restriction.
    let (mut runner, dathon, ench) = dathon_board_at(Phase::BeginCombat);
    let idx = activated_index(&runner, dathon);
    assert_eq!(counters(&runner, ench, CounterType::Stun), 1);
    assert_eq!(mana_total(&runner, P0), 2);

    let Err(rejection) = runner.act(GameAction::ActivateAbility {
        source_id: dathon,
        ability_index: idx,
        payment_mode: CastPaymentMode::Auto,
    }) else {
        panic!("the {{2}} ability must be rejected in the combat step");
    };
    assert!(
        matches!(&rejection, EngineError::ActionNotAllowed(msg) if msg.contains("AsSorcery")),
        "rejected by the printed sorcery-timing restriction, got {rejection:?}"
    );
    // Nothing was spent and nothing moved.
    assert_eq!(
        counters(&runner, ench, CounterType::Stun),
        1,
        "no counter added"
    );
    assert_eq!(mana_total(&runner, P0), 2, "the {{2}} was not spent");
    assert!(
        runner.state().stack.is_empty(),
        "nothing was put on the stack"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    // Positive control: identical board at P0's precombat main (empty stack)
    // — the same announcement is accepted, the cost is paid, and CR 608.2d +
    // CR 122.6 put one more counter of the only kind present.
    let (mut runner, dathon, ench) = dathon_board_at(Phase::PreCombatMain);
    let idx = activated_index(&runner, dathon);
    runner.activate(dathon, idx).target_object(ench).resolve();
    assert_eq!(counters(&runner, ench, CounterType::Stun), 2);
    assert_eq!(mana_total(&runner, P0), 0, "the {{2}} was spent");
    assert!(runner.state().stack.is_empty());
}

/// CR 608.2d + CR 122.1: Dathon and Clockspinning are supported only when every
/// line parses (Dathon's cast trigger and its "Put another counter of that kind"
/// sentence; Clockspinning's chosen-kind adjustment). Read from the
/// production-written fixture entries; the runtime witnesses above prove the
/// behavior.
#[test]
fn dathon_and_clockspinning_whole_cards_have_no_coverage_gap() {
    let db = shared_card_db().expect("integration card fixture must load");
    for name in ["Dathon, His Heart Open", "Clockspinning"] {
        let face = db
            .get_face_by_name(name)
            .unwrap_or_else(|| panic!("{name} is in the integration fixture"));
        assert!(
            !face.abilities.is_empty() || !face.triggers.is_empty(),
            "REACH: {name} parsed into abilities or triggers"
        );
        assert!(
            card_face_gaps(face).is_empty(),
            "{name} must have no coverage gaps: {:?}",
            card_face_gaps(face)
        );
    }
}
