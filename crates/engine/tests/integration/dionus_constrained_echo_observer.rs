//! Dionus, Elvish Archdruid (J25, {3}{G}, Legendary Creature — Elf Druid 3/3):
//! "Elves you control have 'Whenever this creature becomes tapped during your
//! turn, untap it and put a +1/+1 counter on it. This ability triggers only
//! once each turn.'"
//!
//! Structural regressions for pre-spend trigger settlement (CR 603.3 +
//! CR 605.4a): the synchronous auto-tap settlement must preserve admitted
//! ordinary contexts — including constrained (once-per-turn) ones — instead of
//! consuming their admission and depending on later rediscovery. Collection
//! records constrained admissions at match time; a discarded context can never
//! be rediscovered once its ledger marks it fired.
//!
//! Both rows drive real automatic payments through `GameAction::CastSpell`
//! with verbatim Oracle text: no hand-installed definitions. The Elf subtype is
//! supplied through the scenario builder's `with_subtypes` (the same path the
//! printed "Creature — Elf Druid" back face would take), so the granted static's
//! `Elves you control` filter sees a base characteristic that survives layer
//! evaluation.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, ManaChoice, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const DIONUS_ORACLE: &str = "Elves you control have \"Whenever this creature becomes tapped during your turn, untap it and put a +1/+1 counter on it. This ability triggers only once each turn.\"";
const LLANOWAR_ELVES_ORACLE: &str = "{T}: Add {G}.";
const MIRARI_ORACLE: &str = "Creatures you control get +1/+1.\nWhenever you tap a land for mana, add one mana of any type that land produced.";
const CHAMBER_ORACLE: &str =
    "This land enters tapped.\nWhen this land enters, return a land you control to its owner's hand.\n{T}: Add {G}{U}.";

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn floating_colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// One-line census of every carrier the deferred-trigger machinery reads.
fn carriers(runner: &GameRunner) -> String {
    let state = runner.state();
    format!(
        "stack={:?} waiting={:?} deferred={} pending_cast={} prc={} pcmr={} pdlcr={} \
         res_stack={} res_entry={} repl={} consumed={}",
        state
            .stack
            .iter()
            .map(|entry| format!("{:?}", entry.kind))
            .collect::<Vec<_>>(),
        state.waiting_for,
        state.deferred_triggers.len(),
        state.pending_cast.is_some(),
        state.pending_resolution_completion.is_some(),
        state.pending_cost_move_resume.is_some(),
        state.pending_deferred_life_cost_resume.is_some(),
        state.resolution_stack.len(),
        state.resolving_stack_entry.is_some(),
        state.pending_replacement.is_some(),
        state.consumed_before_priority_trigger_events.len(),
    )
}

/// Index of the object's first root-`Effect::Mana` activated ability.
fn mana_ability_index(runner: &GameRunner, id: ObjectId) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(|def| matches!(*def.effect, Effect::Mana { .. }))
        .unwrap_or_else(|| panic!("object {id:?} has no mana ability"))
}

/// CR 603.3 + CR 603.2c: an automatic green payment tapping Llanowar Elves
/// under Dionus (no echo live) must preserve the admitted once-per-turn tap
/// trigger. Discarding the admitted ordinary context consumes its ledger
/// without queueing it, so the trigger is lost; deferring queues it for the
/// owner's post-payment boundary instead.
#[test]
fn dionus_once_per_turn_tap_trigger_survives_automatic_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Dionus, Elvish Archdruid", 3, 3, DIONUS_ORACLE);
    let elves = scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf"])
        .from_oracle_text(LLANOWAR_ELVES_ORACLE)
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    let threat_id = scenario
        .add_creature_to_hand(P0, "Small Threat", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    let threat_card = runner.state().objects[&threat_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: threat_id,
            card_id: threat_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Elves plus Forest must pay {1}{G}");

    // The paid spell reached the stack; the Dionus trigger was admitted during
    // the pre-spend settlement and must be queued, not stranded in a consumed
    // ledger. Both players passing resolves the spell and then the trigger.
    let mut trace: Vec<String> = Vec::new();
    let post_cast_stack = runner.state().stack.len();
    trace.push(format!("post-cast: {}", carriers(&runner)));
    for pass in 0..6 {
        if runner.act(GameAction::PassPriority).is_err() {
            trace.push(format!("pass{pass}: pass rejected"));
            break;
        }
        trace.push(format!(
            "pass{pass}: {} elf_tapped={} counters={}",
            carriers(&runner),
            runner.state().objects[&elves].tapped,
            plus_one_counters(&runner, elves),
        ));
    }
    let diag = trace.join("\n");
    assert_eq!(
        post_cast_stack, 2,
        "the paid spell plus the preserved Dionus trigger must be on the stack\n{diag}"
    );
    assert!(
        !runner.state().objects[&elves].tapped,
        "Dionus must have untapped the Elves\n{diag}"
    );
    assert_eq!(
        plus_one_counters(&runner, elves),
        1,
        "Dionus must have put exactly one +1/+1 counter on the Elves\n{diag}"
    );
}

/// CR 603.3 + CR 605.4a + CR 608.2d: the same constrained observer when the
/// payment also pauses for a mixed-type echo choice. Board: Mirari's Wake +
/// Simic Growth Chamber ({G}{U}) + Dionus + Llanowar Elves, two floating
/// colorless, casting {4}{G}{U} (2 + 1 + 1 + 1 + 1 = 6 with exactly one chosen
/// bonus). The echo pause must not drop the admitted Dionus context.
#[test]
fn dionus_trigger_survives_echo_paused_bonus_dependent_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ORACLE);
    scenario.add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE);
    scenario.add_creature_from_oracle(P0, "Dionus, Elvish Archdruid", 3, 3, DIONUS_ORACLE);
    let elves = scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf"])
        .from_oracle_text(LLANOWAR_ELVES_ORACLE)
        .id();
    scenario.with_mana_pool(P0, floating_colorless(2));
    let threat_id = scenario
        .add_creature_to_hand(P0, "Big Threat", 6, 6)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
            generic: 4,
        })
        .id();
    let mut runner = scenario.build();
    let threat_card = runner.state().objects[&threat_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: threat_id,
            card_id: threat_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("bonus-dependent payment must pause for choice, not fail Cannot pay");
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "the echo choice must be pending pre-spend, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("answering Green must resume payment");
    // Payment completed with the bonus (2C + G + GU + bonus G for 4GU):
    // the spell plus the preserved Dionus trigger are on the stack.
    let mut trace: Vec<String> = Vec::new();
    let post_choice_stack = runner.state().stack.len();
    trace.push(format!("post-choice: {}", carriers(&runner)));
    for pass in 0..6 {
        if runner.act(GameAction::PassPriority).is_err() {
            trace.push(format!("pass{pass}: pass rejected"));
            break;
        }
        trace.push(format!(
            "pass{pass}: {} elf_tapped={} counters={}",
            carriers(&runner),
            runner.state().objects[&elves].tapped,
            plus_one_counters(&runner, elves),
        ));
    }
    let diag = trace.join("\n");
    assert_eq!(
        post_choice_stack, 2,
        "the paid spell plus the preserved Dionus trigger must be on the stack\n{diag}"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "bonus-dependent payment spends everything\n{diag}"
    );
    assert!(
        !runner.state().objects[&elves].tapped,
        "Dionus must have untapped the Elves\n{diag}"
    );
    assert_eq!(
        plus_one_counters(&runner, elves),
        1,
        "Dionus must have put exactly one +1/+1 counter on the Elves\n{diag}"
    );
}

/// Grant liveness pin: with no cast and no echo anywhere, tapping the Elves for
/// mana through their own {T} ability must surface Dionus's granted
/// once-per-turn tap trigger at the next priority window (untap + counter).
/// This isolates the granted trigger from the payment settlement: the grant
/// installs on the typed subject and the tap event matches through the ordinary
/// collection pipeline, so any miss inside a payment is a settlement-collection
/// defect rather than a card-shape gap.
#[test]
fn dionus_grant_fires_for_a_direct_mana_ability_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Dionus, Elvish Archdruid", 3, 3, DIONUS_ORACLE);
    let elves = scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf"])
        .from_oracle_text(LLANOWAR_ELVES_ORACLE)
        .id();
    let mut runner = scenario.build();

    let idx = mana_ability_index(&runner, elves);
    runner
        .act(GameAction::ActivateAbility {
            source_id: elves,
            ability_index: idx,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating the Elves' mana ability must succeed");
    assert!(
        runner.state().objects[&elves].tapped,
        "the activation's tap cost must tap the Elves"
    );

    let mut trace = vec![format!("post-activation: {}", carriers(&runner))];
    for pass in 0..4 {
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority must succeed");
        trace.push(format!(
            "pass{pass}: {} elf_tapped={} counters={}",
            carriers(&runner),
            runner.state().objects[&elves].tapped,
            plus_one_counters(&runner, elves),
        ));
    }
    let diag = trace.join("\n");
    assert!(
        !runner.state().objects[&elves].tapped,
        "Dionus must have untapped the Elves\n{diag}"
    );
    assert_eq!(
        plus_one_counters(&runner, elves),
        1,
        "Dionus must have put exactly one +1/+1 counter on the Elves\n{diag}"
    );
}
