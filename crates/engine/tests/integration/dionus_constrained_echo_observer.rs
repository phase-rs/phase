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
const SELESNYA_ORACLE: &str = "If an effect would create one or more tokens, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent, it puts twice that many of those counters on that permanent instead.\nWhenever chaos ensues, until end of turn, whenever you tap a land for mana, add one mana of any type that land produced.";
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

/// CR 311.2 + CR 311.4 + CR 701.31b: both real planes stay in Command.
/// Chaos creates the delayed echo; planeswalking turns Selesnya face down so
/// its counter doubler stops while the duration-bound echo persists (CR 603.7b).
fn install_delayed_echo(runner: &mut GameRunner) {
    fn plane(state: &mut engine::types::game_state::GameState, name: &str, text: &str) -> ObjectId {
        let parsed =
            engine::parser::oracle::parse_oracle_text(text, name, &[], &["Plane".into()], &[]);
        let mut face = engine::types::card::CardFace {
            name: name.into(),
            oracle_text: Some(text.into()),
            abilities: parsed.abilities,
            triggers: parsed.triggers,
            static_abilities: parsed.statics,
            replacements: parsed.replacements,
            ..Default::default()
        };
        face.card_type
            .core_types
            .push(engine::types::card_type::CoreType::Plane);
        engine::database::synthesis::synthesize_all(&mut face);
        let card_id = engine::types::identifiers::CardId(state.next_object_id);
        let id = engine::game::zones::create_object(
            state,
            card_id,
            P0,
            name.into(),
            engine::types::zones::Zone::Command,
        );
        engine::game::printed_cards::apply_card_face_to_object(
            state.objects.get_mut(&id).unwrap(),
            &face,
        );
        engine::game::trigger_index::reindex_object_triggers(state, id);
        id
    }
    let state = runner.state_mut();
    state.format_config.format = engine::types::format::GameFormat::Planechase;
    state.planar_controller = Some(P0);
    let selesnya = plane(state, "Selesnya Loft Gardens", SELESNYA_ORACLE);
    let naya = plane(state, "Naya", "You may play any number of lands on each of your turns.\nWhenever chaos ensues, target red, green, or white creature you control gets +1/+1 until end of turn for each land you control.");
    // allow-raw-zone: initial planar-deck fixture, still in Command (CR 901.4).
    state.command_zone.retain(|id| *id != naya);
    state.planar_deck.push_back(naya);
    state.objects.get_mut(&naya).unwrap().face_down = true;
    let mut events = Vec::new();
    engine::game::planechase::chaos_ensues(state, &mut events);
    engine::game::triggers::process_triggers(state, &events);
    for _ in 0..8 {
        if !runner.state().delayed_triggers.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("resolve the real chaos trigger");
    }
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    assert!(runner.state().stack.is_empty());
    let mut departure = Vec::new();
    engine::game::planechase::planeswalk(runner.state_mut(), P0, &mut departure);
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert_eq!(
        engine::game::planechase::active_plane(runner.state()),
        Some(naya)
    );
    assert_eq!(
        runner.state().objects[&selesnya].zone,
        engine::types::zones::Zone::Command
    );
    assert!(runner.state().objects[&selesnya].face_down);
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 603.3 + CR 605.4a + CR 608.2d: the same constrained observer when the
/// payment selects a mixed-type echo inline. Board: Mirari's Wake +
/// Simic Growth Chamber ({G}{U}) + Dionus + Llanowar Elves, two floating
/// colorless, casting {4}{G}{U} (2 + 1 + 1 + 1 + 1 = 6 with exactly one chosen
/// bonus). Inline settlement must not drop the admitted Dionus context.
#[test]
fn dionus_trigger_survives_selected_echo_bonus_dependent_payment() {
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
        .expect("bonus-dependent payment must settle its selected echo inline");
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "the selected echo completes payment to Priority, got {:?}",
        runner.state().waiting_for
    );

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

/// CR 603.3 + CR 605.4a + CR 608.2d: the same constrained observer when the
/// payment also pauses for a mixed-type echo choice. Board: Selesnya's delayed echo +
/// Simic Growth Chamber ({G}{U}) + Dionus + Llanowar Elves, two floating
/// colorless, casting {4}{G}{U} (2 + 1 + 1 + 1 + 1 = 6 with exactly one chosen
/// bonus). The echo pause must not drop the admitted Dionus context.
#[test]
fn dionus_trigger_survives_echo_paused_bonus_dependent_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
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
    install_delayed_echo(&mut runner);
    let threat_card = runner.state().objects[&threat_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: threat_id,
            card_id: threat_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("bonus-dependent payment must pause for choice, not fail Cannot pay");
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: engine::types::game_state::ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(options.len(), 2);
            assert!(options.contains(&ManaType::Green) && options.contains(&ManaType::Blue));
        }
        other => panic!("the unselected delayed echo must require its exact GU choice: {other:?}"),
    }

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
