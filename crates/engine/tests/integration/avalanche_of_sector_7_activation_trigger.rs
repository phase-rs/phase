//! Activation triggers: who activated, what the activated source was, and mana
//! abilities.
//!
//! Avalanche of Sector 7: "Whenever an opponent activates an ability of an
//! artifact they control, Avalanche of Sector 7 deals 1 damage to that player."
//!
//! * CR 602.2a: "they" is the activating player; the source's controller must
//!   be that player.
//! * CR 605.3: activating a mana ability follows CR 602.2, so a trigger with no
//!   "that isn't a mana ability" exclusion sees mana abilities (Elrond,
//!   Moon-Reader's ruling).
//! * CR 113.7: a source a cost moved off the battlefield (a sacrificed
//!   Treasure) answers from its last known information.
//! * CR 603.10: a mana ability's activation is observed immediately after the
//!   event, before the ability's own resolution can change its source.

use engine::game::effects::attach::attach_to;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    ControllerRef, Effect, FilterProp, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::{ActivatedAbilityKind, GameEvent};
use engine::types::game_state::{ManaChoice, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const AVALANCHE: &str = "Menace\nAvalanche of Sector 7's power is equal to the number of artifacts your opponents control.\nWhenever an opponent activates an ability of an artifact they control, Avalanche of Sector 7 deals 1 damage to that player.";
const ELROND: &str = "Whenever you activate an ability of a creature, draw a card. This ability triggers only once each turn.";
const HARSH_MENTOR: &str = "Whenever an opponent activates an ability of an artifact, creature, or land on the battlefield, if it isn't a mana ability, this creature deals 2 damage to that player.";
const SEARBLADES: &str =
    "Whenever you activate an ability of an Elemental, this creature gets +1/+0 until end of turn.";
const ILLUSIONISTS_BRACERS: &str = "Whenever an ability of equipped creature is activated, if it isn't a mana ability, copy that ability. You may choose new targets for the copy.\nEquip {3}";

const DRAW: &str = "{T}: Draw a card.";
const MANA: &str = "{T}: Add {C}.";
const TREASURE_C: &str = "{T}, Sacrifice this artifact: Add {C}.";
const TREASURE: &str = "{T}, Sacrifice this artifact: Add one mana of any color.";
const MANA_THEN_SACRIFICE: &str = "{T}: Add {C}. Sacrifice this creature.";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest"]);
    scenario
}

fn add_avalanche(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Avalanche of Sector 7", 0, 3)
        .from_oracle_text(AVALANCHE)
        .id()
}

fn add_artifact(
    scenario: &mut GameScenario,
    owner: PlayerId,
    name: &str,
    oracle: &str,
) -> ObjectId {
    scenario
        .add_creature(owner, name, 1, 1)
        .as_artifact()
        .from_oracle_text(oracle)
        .id()
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn hand(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn pool(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].mana_pool.total()
}

fn activations_of_kind(events: &[GameEvent], wanted: ActivatedAbilityKind) -> usize {
    events
        .iter()
        .filter(
            |event| matches!(event, GameEvent::AbilityActivated { kind, .. } if *kind == wanted),
        )
        .count()
}

/// Triggered abilities currently on the stack whose source is `source`.
fn triggers_on_stack_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == source
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count()
}

/// CR 603.3b: answer a simultaneous-trigger ordering prompt in listed order.
fn settle_trigger_order(runner: &mut GameRunner) {
    if let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
        let order = (0..triggers.len()).collect();
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("order accepted");
    }
}

/// Pass priority until the stack is empty (each pair of passes resolves the top).
fn resolve_stack(runner: &mut GameRunner) {
    for _ in 0..40 {
        settle_trigger_order(runner);
        if runner.state().stack.is_empty() {
            return;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!("stack did not empty");
}

/// P0 is active; pass so P1 holds priority.
fn give_p1_priority(runner: &mut GameRunner) {
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to P1");
}

fn activate(runner: &mut GameRunner, source: ObjectId, index: usize) -> Vec<GameEvent> {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
        })
        .expect("activation accepted")
        .events
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// CR 602.2a + CR 109.4: "an artifact they control" binds the source's
/// controller to the activating player.
#[test]
fn avalanche_trigger_parses_activator_relative_source_filter() {
    let parsed = parse_oracle_text(
        AVALANCHE,
        "Avalanche of Sector 7",
        &["Menace".to_string()],
        &["Creature".to_string()],
        &["Human".to_string(), "Rebel".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::AbilityActivated)
        .expect("the activation trigger parses as AbilityActivated");
    assert_eq!(
        trigger.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    assert_eq!(
        trigger.valid_card,
        Some(TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Artifact],
            controller: Some(ControllerRef::TriggeringPlayer),
            properties: vec![FilterProp::InZone {
                zone: Zone::Battlefield
            }],
        }))
    );
    assert_eq!(
        trigger.condition, None,
        "no mana-ability exclusion is printed"
    );
    let execute = trigger.execute.as_deref().expect("execute");
    assert!(
        matches!(
            &*execute.effect,
            Effect::DealDamage {
                target: TargetFilter::TriggeringPlayer,
                ..
            }
        ),
        "deals damage to that player, got {:?}",
        execute.effect
    );
}

/// "they" has no antecedent after "you activate", so that shape is not accepted.
#[test]
fn they_control_requires_a_third_person_activator() {
    let parsed = parse_oracle_text(
        "Whenever you activate an ability of an artifact they control, draw a card.",
        "Probe",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert!(
        parsed
            .triggers
            .iter()
            .all(|t| t.mode != TriggerMode::AbilityActivated),
        "a second-person subject is never the antecedent of \"they\""
    );
}

fn activation_trigger_execute(text: &str) -> Effect {
    let parsed = parse_oracle_text(text, "Probe", &[], &["Creature".to_string()], &[]);
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::AbilityActivated)
        .unwrap_or_else(|| panic!("{text:?} must parse as AbilityActivated"));
    (*trigger.execute.as_deref().expect("execute").effect).clone()
}

fn is_activation_mana_gap(effect: &Effect) -> bool {
    matches!(effect, Effect::Unimplemented { name, .. } if name == "activation_triggered_mana_ability")
}

/// CR 605.1b + CR 605.4a: an untargeted activation-triggered body that could add
/// mana is a triggered mana ability, which the engine does not route inline; it
/// stays an honest gap whatever else the body does, in any order.
#[test]
fn activation_triggered_mana_bodies_are_strict_failed() {
    for text in [
        "Whenever you activate an ability, add {G}.",
        "Whenever you activate an ability, add {G}. You gain 1 life.",
        "Whenever you activate an ability, you gain 1 life. Add {G}.",
        "Whenever you activate an ability, add {G}. Draw a card.",
    ] {
        let effect = activation_trigger_execute(text);
        assert!(
            is_activation_mana_gap(&effect),
            "{text:?} must be a gap, got {effect:?}"
        );
    }
}

/// Controls: a body that adds no mana, a targeted mana-producing body (not a
/// mana ability, CR 605.1b), a non-mana mixed body, and a trigger that excludes
/// mana activations all stay ordinary triggers.
#[test]
fn ordinary_activation_trigger_bodies_are_not_strict_failed() {
    for text in [
        "Whenever you activate an ability, draw a card.",
        "Whenever you activate an ability, add {G}. This creature deals 1 damage to any target.",
        "Whenever you activate an ability, you gain 1 life and draw a card.",
        "Whenever you activate an ability that isn't a mana ability, add {G}.",
    ] {
        let effect = activation_trigger_execute(text);
        assert!(
            !matches!(effect, Effect::Unimplemented { .. }),
            "{text:?} must stay an ordinary trigger, got {effect:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Avalanche of Sector 7
// ---------------------------------------------------------------------------

/// The opponent's own artifact, a stack-using ability.
#[test]
fn avalanche_punishes_an_opponents_artifact_activation() {
    let mut scenario = main_phase();
    add_avalanche(&mut scenario);
    let source = add_artifact(&mut scenario, P1, "Jar", DRAW);
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let (life0, hand0) = (life(&runner, P1), hand(&runner, P1));
    activate(&mut runner, source, 0);
    resolve_stack(&mut runner);
    assert_eq!(hand(&runner, P1), hand0 + 1, "reach: the ability resolved");
    assert_eq!(life(&runner, P1), life0 - 1);
}

/// CR 605.3: a mana ability of the opponent's artifact triggers it too.
#[test]
fn avalanche_punishes_an_opponents_artifact_mana_ability() {
    let mut scenario = main_phase();
    let avalanche = add_avalanche(&mut scenario);
    let source = add_artifact(&mut scenario, P1, "Mind Stone", MANA);
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let life0 = life(&runner, P1);
    let events = activate(&mut runner, source, 0);
    assert_eq!(pool(&runner, P1), 1, "reach: the mana ability produced");
    assert_eq!(activations_of_kind(&events, ActivatedAbilityKind::Mana), 1);
    assert_eq!(triggers_on_stack_from(&runner, avalanche), 1);
    resolve_stack(&mut runner);
    assert_eq!(life(&runner, P1), life0 - 1);
}

/// CR 113.7: a Treasure sacrificed for mana answers from last known information.
#[test]
fn avalanche_sees_a_treasure_sacrificed_for_mana() {
    let mut scenario = main_phase();
    add_avalanche(&mut scenario);
    let treasure = add_artifact(&mut scenario, P1, "Treasure", TREASURE_C);
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let life0 = life(&runner, P1);
    activate(&mut runner, treasure, 0);
    assert_ne!(
        runner.state().objects.get(&treasure).map(|o| o.zone),
        Some(Zone::Battlefield),
        "reach: the cost moved the Treasure"
    );
    assert_eq!(pool(&runner, P1), 1, "reach: mana produced");
    resolve_stack(&mut runner);
    assert_eq!(life(&runner, P1), life0 - 1);
}

/// Hostile (controller): P1 controls an artifact P0 owns and sacrifices it for
/// mana. It goes to its OWNER's graveyard, so only its last known controller
/// is P1 — the trigger must read that, not the owner.
#[test]
fn avalanche_reads_the_last_known_controller_of_a_stolen_artifact() {
    let mut scenario = main_phase();
    add_avalanche(&mut scenario);
    let stolen = scenario
        .add_creature(P0, "Stolen Treasure", 1, 1)
        .as_artifact()
        .from_oracle_text(TREASURE_C)
        .controlled_by(P1)
        .id();
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let (p0_life, p1_life) = (life(&runner, P0), life(&runner, P1));
    activate(&mut runner, stolen, 0);
    assert_eq!(
        pool(&runner, P1),
        1,
        "reach: P1 activated the stolen artifact"
    );
    resolve_stack(&mut runner);
    assert_eq!(life(&runner, P1), p1_life - 1, "they (P1) controlled it");
    assert_eq!(life(&runner, P0), p0_life);
}

const ANY_PLAYER_DRAW: &str = "{T}: Draw a card. Any player may activate this ability.";

/// Hostile (relation): the opponent activates an artifact Avalanche's
/// controller controls. "they control" is false, so no trigger.
#[test]
fn avalanche_ignores_an_artifact_the_activator_does_not_control() {
    let mut scenario = main_phase();
    let avalanche = add_avalanche(&mut scenario);
    let shared = add_artifact(&mut scenario, P0, "Shared Jar", ANY_PLAYER_DRAW);
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let (life0, hand0) = (life(&runner, P1), hand(&runner, P1));
    activate(&mut runner, shared, 0);
    assert_eq!(triggers_on_stack_from(&runner, avalanche), 0);
    resolve_stack(&mut runner);
    assert_eq!(hand(&runner, P1), hand0 + 1, "reach: P1 activated and drew");
    assert_eq!(life(&runner, P1), life0);
}

/// Hostile (multiplayer relation): P1 activates another opponent's (P2's)
/// artifact. P1 is an opponent, but does not control it.
#[test]
fn avalanche_ignores_another_opponents_artifact_in_multiplayer() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, p2] {
        scenario.with_library_top(player, &["Forest", "Forest"]);
    }
    let avalanche = add_avalanche(&mut scenario);
    let shared = add_artifact(&mut scenario, p2, "Shared Jar", ANY_PLAYER_DRAW);
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let (life0, hand0) = (life(&runner, P1), hand(&runner, P1));
    activate(&mut runner, shared, 0);
    assert_eq!(triggers_on_stack_from(&runner, avalanche), 0);
    resolve_stack(&mut runner);
    assert_eq!(hand(&runner, P1), hand0 + 1, "reach: P1 activated and drew");
    assert_eq!(life(&runner, P1), life0);
}

/// Control: Avalanche's controller activating its own artifact is not an opponent.
#[test]
fn avalanche_ignores_its_controllers_activation() {
    let mut scenario = main_phase();
    let avalanche = add_avalanche(&mut scenario);
    let jar = add_artifact(&mut scenario, P0, "Jar", DRAW);
    let mut runner = scenario.build();
    let (life0, hand0) = (life(&runner, P0), hand(&runner, P0));
    activate(&mut runner, jar, 0);
    assert_eq!(triggers_on_stack_from(&runner, avalanche), 0);
    resolve_stack(&mut runner);
    assert_eq!(hand(&runner, P0), hand0 + 1, "reach: P0 drew");
    assert_eq!(life(&runner, P0), life0);
}

fn add_instant(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    scenario
        .add_spell_to_hand(player, name, true)
        .from_oracle_text("Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        })
        .id()
}

/// CR 603.3 + CR 605.3: an artifact mana ability activated while paying for a
/// spell (the automatic payment path) triggers Avalanche; the trigger waits
/// until the spell is cast and goes on the stack above it.
#[test]
fn avalanche_trigger_from_auto_payment_goes_above_the_spell() {
    let mut scenario = main_phase();
    let avalanche = add_avalanche(&mut scenario);
    add_artifact(&mut scenario, P1, "Mind Stone", MANA);
    let spell = add_instant(&mut scenario, P1, "Opt");
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let life0 = life(&runner, P1);
    let mut commit = runner.cast(spell).commit();
    let state = commit.state_mut().clone();
    let stack: Vec<_> = state.stack.iter().collect();
    assert_eq!(
        stack.len(),
        2,
        "the spell and Avalanche's trigger: {stack:?}"
    );
    assert_eq!(stack[0].source_id, spell, "the spell is below");
    assert_eq!(stack[1].source_id, avalanche, "the trigger is on top");
    drop(commit);
    // Both players pass: the trigger (top) resolves first.
    runner.act(GameAction::PassPriority).expect("P1 passes");
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert_eq!(life(&runner, P1), life0 - 1);
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == spell),
        "the spell is still waiting on the stack"
    );
}

/// Hostile (trigger source leaves): Avalanche leaves after triggering; its
/// ability still resolves from last known information (CR 113.7a).
#[test]
fn avalanche_trigger_resolves_after_avalanche_leaves() {
    let mut scenario = main_phase();
    let avalanche = add_avalanche(&mut scenario);
    let stone = add_artifact(&mut scenario, P1, "Mind Stone", MANA);
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let life0 = life(&runner, P1);
    activate(&mut runner, stone, 0);
    assert_eq!(
        triggers_on_stack_from(&runner, avalanche),
        1,
        "reach: triggered"
    );
    move_to_zone(
        runner.state_mut(),
        avalanche,
        Zone::Graveyard,
        &mut Vec::new(),
    );
    resolve_stack(&mut runner);
    assert_eq!(life(&runner, P1), life0 - 1);
}

// ---------------------------------------------------------------------------
// Exactly once across pauses
// ---------------------------------------------------------------------------

/// EO1 + EO2: a colour prompt sits between activation and production. The
/// activation event is published once, its trigger queued once; a rejected
/// colour answer adds neither.
#[test]
fn colour_choice_pause_publishes_one_activation_and_one_trigger() {
    let mut scenario = main_phase();
    let avalanche = add_avalanche(&mut scenario);
    let treasure = add_artifact(&mut scenario, P1, "Treasure", TREASURE);
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let life0 = life(&runner, P1);
    let mut events = activate(&mut runner, treasure, 0);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "reach: paused on the colour, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::Combination(vec![ManaType::Red, ManaType::Red]),
                count: 1,
            })
            .is_err(),
        "a mismatched answer is rejected"
    );
    events.extend(
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            })
            .expect("colour accepted")
            .events,
    );
    assert_eq!(pool(&runner, P1), 1, "reach: mana produced");
    assert_eq!(activations_of_kind(&events, ActivatedAbilityKind::Mana), 1);
    assert_eq!(triggers_on_stack_from(&runner, avalanche), 1);
    resolve_stack(&mut runner);
    assert_eq!(life(&runner, P1), life0 - 1);
}

/// EO3: bulk sibling activation (one colour answer for two Treasures) is two
/// activations and two triggers.
#[test]
fn bulk_sibling_treasures_are_each_one_activation_and_one_trigger() {
    let mut scenario = main_phase();
    let avalanche = add_avalanche(&mut scenario);
    let first = add_artifact(&mut scenario, P1, "Treasure", TREASURE);
    add_artifact(&mut scenario, P1, "Treasure", TREASURE);
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let life0 = life(&runner, P1);
    let mut events = activate(&mut runner, first, 0);
    events.extend(
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 2,
            })
            .expect("bulk colour accepted")
            .events,
    );
    assert_eq!(pool(&runner, P1), 2, "reach: both Treasures produced");
    assert_eq!(activations_of_kind(&events, ActivatedAbilityKind::Mana), 2);
    settle_trigger_order(&mut runner);
    assert_eq!(triggers_on_stack_from(&runner, avalanche), 2);
    resolve_stack(&mut runner);
    assert_eq!(life(&runner, P1), life0 - 2);
}

// ---------------------------------------------------------------------------
// Elrond, Moon-Reader / Ceaseless Searblades (no mana exclusion)
// ---------------------------------------------------------------------------

fn elrond_board() -> (GameScenario, ObjectId) {
    let mut scenario = main_phase();
    let elrond = scenario
        .add_creature(P0, "Elrond, Moon-Reader", 3, 3)
        .from_oracle_text(ELROND)
        .id();
    (scenario, elrond)
}

/// Elrond's ruling: a creature's mana ability triggers it.
#[test]
fn elrond_draws_for_a_creature_mana_ability() {
    let (mut scenario, _) = elrond_board();
    let dork = scenario
        .add_creature(P0, "Dork", 1, 1)
        .from_oracle_text(MANA)
        .id();
    let mut runner = scenario.build();
    let hand0 = hand(&runner, P0);
    activate(&mut runner, dork, 0);
    assert_eq!(pool(&runner, P0), 1, "reach: mana produced");
    resolve_stack(&mut runner);
    assert_eq!(hand(&runner, P0), hand0 + 1);
}

/// "This ability triggers only once each turn" across a mana ability and a
/// later stack ability.
#[test]
fn elrond_triggers_once_per_turn_across_mana_and_other_abilities() {
    let (mut scenario, _) = elrond_board();
    let dork = scenario
        .add_creature(P0, "Dork", 1, 1)
        .from_oracle_text(MANA)
        .id();
    let looter = scenario
        .add_creature(P0, "Looter", 1, 1)
        .from_oracle_text(DRAW)
        .id();
    let mut runner = scenario.build();
    let hand0 = hand(&runner, P0);
    activate(&mut runner, dork, 0);
    resolve_stack(&mut runner);
    activate(&mut runner, looter, 0);
    resolve_stack(&mut runner);
    // One Elrond draw plus the looter's own draw.
    assert_eq!(hand(&runner, P0), hand0 + 2);
}

/// The automatic payment path (auto-tap of a creature mana source while
/// casting) triggers Elrond too.
#[test]
fn elrond_draws_for_an_auto_paid_creature_mana_ability() {
    let (mut scenario, _) = elrond_board();
    let dork = scenario
        .add_creature(P0, "Dork", 1, 1)
        .from_oracle_text(MANA)
        .id();
    let spell = add_instant(&mut scenario, P0, "Opt");
    let mut runner = scenario.build();
    let hand0 = hand(&runner, P0);
    runner.cast(spell).resolve();
    assert!(
        runner.state().objects[&dork].tapped,
        "reach: the auto payment tapped the dork"
    );
    resolve_stack(&mut runner);
    // The spell left the hand (-1), drew (+1), and Elrond drew (+1).
    assert_eq!(hand(&runner, P0), hand0 + 1);
}

/// Mixed automatic then manual activations still draw once.
#[test]
fn elrond_once_per_turn_holds_across_auto_and_manual_activations() {
    let (mut scenario, _) = elrond_board();
    let first = scenario
        .add_creature(P0, "Dork A", 1, 1)
        .from_oracle_text(MANA)
        .id();
    let second = scenario
        .add_creature(P0, "Dork B", 1, 1)
        .from_oracle_text(MANA)
        .id();
    let spell = add_instant(&mut scenario, P0, "Opt");
    let mut runner = scenario.build();
    let hand0 = hand(&runner, P0);
    runner.cast(spell).resolve();
    resolve_stack(&mut runner);
    let auto_tapped = [first, second]
        .into_iter()
        .filter(|dork| runner.state().objects[dork].tapped)
        .collect::<Vec<_>>();
    assert_eq!(auto_tapped.len(), 1, "reach: auto payment tapped one dork");
    // -1 spell, +1 spell's draw, +1 Elrond.
    assert_eq!(hand(&runner, P0), hand0 + 1);
    let untapped = if auto_tapped[0] == first {
        second
    } else {
        first
    };
    activate(&mut runner, untapped, 0);
    assert_eq!(
        pool(&runner, P0),
        1,
        "reach: the manual activation produced"
    );
    resolve_stack(&mut runner);
    assert_eq!(
        hand(&runner, P0),
        hand0 + 1,
        "no second Elrond draw this turn"
    );
}

/// A2a: the mana ability's own resolution sacrifices its source. The
/// activation was observed before that (CR 603.10), so Elrond still draws.
#[test]
fn elrond_draws_when_the_mana_ability_sacrifices_its_source_on_resolution() {
    for oracle in [MANA_THEN_SACRIFICE, MANA] {
        let (mut scenario, _) = elrond_board();
        let dork = scenario
            .add_creature(P0, "Dork", 1, 1)
            .from_oracle_text(oracle)
            .id();
        let mut runner = scenario.build();
        let hand0 = hand(&runner, P0);
        activate(&mut runner, dork, 0);
        assert_eq!(pool(&runner, P0), 1, "reach: mana produced ({oracle})");
        if oracle == MANA_THEN_SACRIFICE {
            assert_ne!(
                runner.state().objects.get(&dork).map(|o| o.zone),
                Some(Zone::Battlefield),
                "reach: the effect sacrificed it"
            );
        }
        resolve_stack(&mut runner);
        assert_eq!(hand(&runner, P0), hand0 + 1, "{oracle}");
    }
}

/// A2b: the trigger source IS the mana source, and its effect sacrifices it.
/// Its trigger was collected while it was on the battlefield, so it draws.
#[test]
fn self_sacrificing_mana_source_keeps_its_own_activation_trigger() {
    let mut scenario = main_phase();
    let elrond = scenario
        .add_creature(P0, "Self Elrond", 1, 1)
        .from_oracle_text(&format!("{ELROND}\n{MANA_THEN_SACRIFICE}"))
        .id();
    let mut runner = scenario.build();
    let hand0 = hand(&runner, P0);
    let index = runner.state().objects[&elrond]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::Mana { .. }))
        .expect("mana ability");
    activate(&mut runner, elrond, index);
    resolve_stack(&mut runner);
    assert_eq!(hand(&runner, P0), hand0 + 1);
}

/// T11 (Kurkesh-class): the trigger source sacrificed as the activation's
/// COST is gone when the ability is considered activated — no trigger. The
/// sibling (another creature's mana ability) does trigger it.
#[test]
fn trigger_source_sacrificed_as_the_cost_does_not_trigger() {
    let mut scenario = main_phase();
    let elrond = scenario
        .add_creature(P0, "Self Elrond", 1, 1)
        .from_oracle_text(&format!("{ELROND}\nSacrifice this creature: Add {{C}}."))
        .id();
    let mut runner = scenario.build();
    let hand0 = hand(&runner, P0);
    let index = runner.state().objects[&elrond]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::Mana { .. }))
        .expect("mana ability");
    activate(&mut runner, elrond, index);
    assert_eq!(pool(&runner, P0), 1, "reach: mana produced");
    resolve_stack(&mut runner);
    assert_eq!(
        hand(&runner, P0),
        hand0,
        "the trigger source was gone at activation"
    );
}

fn searblades_power_after(oracles: &[(&str, bool)]) -> i32 {
    let mut scenario = main_phase();
    let searblades = scenario
        .add_creature(P0, "Ceaseless Searblades", 2, 4)
        .from_oracle_text(SEARBLADES)
        .id();
    let sources: Vec<ObjectId> = oracles
        .iter()
        .map(|(oracle, elemental)| {
            let mut builder = scenario.add_creature(P0, "Source", 1, 1);
            if *elemental {
                builder.with_subtypes(vec!["Elemental"]);
            }
            builder.from_oracle_text(oracle).id()
        })
        .collect();
    let mut runner = scenario.build();
    for source in sources {
        activate(&mut runner, source, 0);
        resolve_stack(&mut runner);
    }
    runner.state().objects[&searblades].power.expect("power")
}

#[test]
fn ceaseless_searblades_pumps_for_an_elemental_mana_ability() {
    assert_eq!(searblades_power_after(&[(MANA, true)]), 3);
}

#[test]
fn ceaseless_searblades_ignores_a_non_elemental_mana_ability() {
    assert_eq!(searblades_power_after(&[(MANA, false)]), 2);
}

#[test]
fn ceaseless_searblades_pumps_once_per_elemental_activation() {
    assert_eq!(searblades_power_after(&[(MANA, true), (MANA, true)]), 4);
}

// ---------------------------------------------------------------------------
// "that isn't a mana ability" stays a real exclusion; cost-moved sources
// ---------------------------------------------------------------------------

fn harsh_mentor_life_delta(oracle: &str, source_is_artifact: bool) -> i32 {
    let mut scenario = main_phase();
    scenario
        .add_creature(P0, "Harsh Mentor", 2, 2)
        .from_oracle_text(HARSH_MENTOR);
    let source = if source_is_artifact {
        add_artifact(&mut scenario, P1, "Source", oracle)
    } else {
        scenario
            .add_creature(P1, "Source", 1, 1)
            .from_oracle_text(oracle)
            .id()
    };
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let life0 = life(&runner, P1);
    activate(&mut runner, source, 0);
    resolve_stack(&mut runner);
    life(&runner, P1) - life0
}

#[test]
fn harsh_mentor_still_excludes_mana_abilities() {
    assert_eq!(harsh_mentor_life_delta(MANA, false), 0);
    assert_eq!(harsh_mentor_life_delta(TREASURE_C, true), 0);
    assert_eq!(
        harsh_mentor_life_delta(DRAW, false),
        -2,
        "reach: it observes P1"
    );
}

/// CR 113.7: a Clue-like artifact sacrificed as the cost of a non-mana ability.
#[test]
fn harsh_mentor_sees_a_source_sacrificed_as_the_cost() {
    assert_eq!(
        harsh_mentor_life_delta("Sacrifice this artifact: Draw a card.", true),
        -2
    );
}

/// Hostile (zone guard): a creature that died earlier this step is activated
/// from the graveyard. Its stale battlefield last known information must not
/// make it "a creature on the battlefield" (Harsh Mentor ruling).
#[test]
fn a_graveyard_activation_never_reads_stale_battlefield_lki() {
    let mut scenario = main_phase();
    scenario
        .add_creature(P0, "Harsh Mentor", 2, 2)
        .from_oracle_text(HARSH_MENTOR);
    let card = scenario
        .add_creature(P1, "Bone Thing", 1, 1)
        .from_oracle_text("Exile this card from your graveyard: Draw a card.")
        .id();
    let mut runner = scenario.build();
    let activation_zone = runner.state().objects[&card].abilities[0].activation_zone;
    assert_eq!(
        activation_zone,
        Some(Zone::Graveyard),
        "reach: a graveyard ability"
    );
    move_to_zone(runner.state_mut(), card, Zone::Graveyard, &mut Vec::new());
    assert!(
        runner.state().lki_cache.contains_key(&card),
        "reach: it left the battlefield this step"
    );
    give_p1_priority(&mut runner);
    let (life0, hand0) = (life(&runner, P1), hand(&runner, P1));
    activate(&mut runner, card, 0);
    resolve_stack(&mut runner);
    assert_eq!(hand(&runner, P1), hand0 + 1, "reach: the ability resolved");
    assert_eq!(life(&runner, P1), life0);
}

// ---------------------------------------------------------------------------
// Illusionist's Bracers: the attachment relation is read after costs
// ---------------------------------------------------------------------------

fn bracers_draws(creature_oracle: &str) -> usize {
    bracers_run(creature_oracle).0
}

fn bracers_run(creature_oracle: &str) -> (usize, Vec<GameEvent>, ObjectId) {
    let mut scenario = main_phase();
    let bracers = scenario
        .add_creature(P0, "Illusionist's Bracers", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(ILLUSIONISTS_BRACERS)
        .id();
    let creature = scenario
        .add_creature(P0, "Host", 1, 1)
        .from_oracle_text(creature_oracle)
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), bracers, creature);
    let hand0 = hand(&runner, P0);
    let mut events = activate(&mut runner, creature, 0);
    if let WaitingFor::PayCost { choices, .. } = &runner.state().waiting_for {
        assert_eq!(
            choices,
            &vec![bracers],
            "reach: the Bracers are the only artifact"
        );
        events.extend(
            runner
                .act(GameAction::SelectCards {
                    cards: vec![bracers],
                })
                .expect("sacrifice the Bracers")
                .events,
        );
    }
    resolve_stack(&mut runner);
    (hand(&runner, P0) - hand0, events, bracers)
}

/// B1: live-equipped — the ability is copied.
#[test]
fn bracers_copy_a_live_equipped_creatures_ability() {
    assert_eq!(bracers_draws(DRAW), 2);
}

/// B2: the equipped creature sacrificed as the cost is no longer equipped when
/// the ability is activated (Illusionist's Bracers ruling) — no copy.
#[test]
fn bracers_do_not_copy_when_the_equipped_creature_is_the_cost() {
    let (draws, events, bracers) = bracers_run("Sacrifice this creature: Draw a card.");
    assert_eq!(draws, 1);
    // The matcher cleared attachments on a COPY only: the event's last known
    // information still records the Bracers on the departed creature.
    let attachments = events
        .iter()
        .find_map(|event| match event {
            GameEvent::AbilityActivated {
                departed_source_lki: Some(lki),
                ..
            } => Some(lki.attachments.clone()),
            _ => None,
        })
        .expect("reach: the activation carries the departed creature's LKI");
    assert!(
        attachments
            .iter()
            .any(|attachment| attachment.object_id == bracers),
        "the event's attachment snapshot is intact: {attachments:?}"
    );
}

/// B3: the Bracers themselves sacrificed as the cost — the trigger source is
/// gone, so no copy.
#[test]
fn bracers_do_not_copy_when_they_are_the_cost() {
    assert_eq!(bracers_draws("Sacrifice an artifact: Draw a card."), 1);
}

// ---------------------------------------------------------------------------
// Intrinsic basic-land mana abilities (CR 305.6)
// ---------------------------------------------------------------------------

/// An artifact with the Forest land type. Layers make its intrinsic CR 305.6
/// "{T}: Add {G}" ability explicit, so tapping it is an ordinary mana-ability
/// activation of a land.
fn make_artifact(runner: &mut GameRunner, land: ObjectId) {
    let object = runner.state_mut().objects.get_mut(&land).expect("land");
    for types in [&mut object.card_types, &mut object.base_card_types] {
        types
            .core_types
            .push(engine::types::card_type::CoreType::Artifact);
    }
}

fn add_artifact_forest(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    scenario.add_basic_land(owner, engine::types::mana::ManaColor::Green)
}

fn tap_land_selection(runner: &GameRunner, land: ObjectId) -> GameAction {
    let (_, _, grouped) = engine::ai_support::legal_actions_full(runner.state());
    grouped
        .get(&land)
        .into_iter()
        .flatten()
        .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
        .cloned()
        .expect("the engine authors the land's mana selection")
}

/// The manual tap of an intrinsic mana ability is a mana activation.
#[test]
fn avalanche_sees_an_intrinsic_land_mana_ability_tapped_manually() {
    let mut scenario = main_phase();
    let avalanche = add_avalanche(&mut scenario);
    let land = add_artifact_forest(&mut scenario, P1);
    let mut runner = scenario.build();
    make_artifact(&mut runner, land);
    give_p1_priority(&mut runner);
    let life0 = life(&runner, P1);
    let tap = tap_land_selection(&runner, land);
    let events = runner.act(tap).expect("tap the land").events;
    assert_eq!(pool(&runner, P1), 1, "reach: mana produced");
    assert_eq!(activations_of_kind(&events, ActivatedAbilityKind::Mana), 1);
    assert_eq!(triggers_on_stack_from(&runner, avalanche), 1);
    resolve_stack(&mut runner);
    assert_eq!(life(&runner, P1), life0 - 1);
}

/// The automatic payment tapping a land's intrinsic mana ability is a mana
/// activation too.
#[test]
fn avalanche_sees_an_intrinsic_land_mana_ability_auto_tapped() {
    let mut scenario = main_phase();
    let avalanche = add_avalanche(&mut scenario);
    let land = add_artifact_forest(&mut scenario, P1);
    let spell = add_instant(&mut scenario, P1, "Opt");
    let mut runner = scenario.build();
    make_artifact(&mut runner, land);
    give_p1_priority(&mut runner);
    let life0 = life(&runner, P1);
    let mut commit = runner.cast(spell).commit();
    let state = commit.state_mut().clone();
    drop(commit);
    assert!(
        state.objects[&land].tapped,
        "reach: the auto payment tapped the land"
    );
    assert!(
        state.stack.iter().any(|entry| entry.source_id == avalanche),
        "Avalanche triggered: {:?}",
        state.stack
    );
    resolve_stack(&mut runner);
    assert_eq!(life(&runner, P1), life0 - 1);
}

// ---------------------------------------------------------------------------
// The activation's kind comes from the definition the player activated
// ---------------------------------------------------------------------------

const GRANT_TREASURE_ABILITY: &str =
    "Artifacts you control have \"{T}, Sacrifice this artifact: Add {C}.\"";
const REPLACE_WITH_TREASURE_ABILITY: &str =
    "Artifacts you control lose all abilities and have \"{T}, Sacrifice this artifact: Add {C}.\"";

fn granted_mana_ability_index(runner: &GameRunner, artifact: ObjectId) -> usize {
    runner.state().objects[&artifact]
        .abilities
        .iter()
        .position(|ability| matches!(*ability.effect, Effect::Mana { .. }))
        .expect("reach: the granted mana ability is live")
}

/// A3c: a vanilla artifact granted a self-sacrificing mana ability. The
/// activation is a mana activation, classified from the definition the player
/// activated (not rediscovered from the departed source): Avalanche triggers,
/// Harsh Mentor ("if it isn't a mana ability") does not.
#[test]
fn granted_self_sacrificing_mana_ability_is_a_mana_activation() {
    let mut scenario = main_phase();
    let avalanche = add_avalanche(&mut scenario);
    let mentor = scenario
        .add_creature(P0, "Harsh Mentor", 2, 2)
        .from_oracle_text(HARSH_MENTOR)
        .id();
    scenario
        .add_creature(P1, "Grantor", 1, 1)
        .from_oracle_text(GRANT_TREASURE_ABILITY);
    let artifact = add_artifact(&mut scenario, P1, "Bauble", "");
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let life0 = life(&runner, P1);
    let index = granted_mana_ability_index(&runner, artifact);
    let events = activate(&mut runner, artifact, index);
    assert_eq!(pool(&runner, P1), 1, "reach: mana from the granted ability");
    assert_eq!(activations_of_kind(&events, ActivatedAbilityKind::Mana), 1);
    assert_eq!(
        activations_of_kind(&events, ActivatedAbilityKind::Normal),
        0
    );
    assert_eq!(triggers_on_stack_from(&runner, avalanche), 1);
    assert_eq!(triggers_on_stack_from(&runner, mentor), 0);
    resolve_stack(&mut runner);
    assert_eq!(life(&runner, P1), life0 - 1);
}

/// A3c2: the granted mana ability replaced the printed ability at the same
/// index. Once the cost moves the artifact, that index names its printed,
/// NON-mana ability again; the activation must still be the mana ability the
/// player activated (production and kind both from the bound definition).
#[test]
fn a_departed_source_never_rebinds_its_mana_ability_to_the_printed_occupant() {
    let mut scenario = main_phase();
    let avalanche = add_avalanche(&mut scenario);
    let mentor = scenario
        .add_creature(P0, "Harsh Mentor", 2, 2)
        .from_oracle_text(HARSH_MENTOR)
        .id();
    scenario
        .add_creature(P1, "Grantor", 1, 1)
        .from_oracle_text(REPLACE_WITH_TREASURE_ABILITY);
    let artifact = add_artifact(&mut scenario, P1, "Jar", "{T}: You gain 3 life.");
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let index = granted_mana_ability_index(&runner, artifact);
    assert_eq!(
        index, 0,
        "reach: the grant occupies the printed ability's index"
    );
    let life0 = life(&runner, P1);
    let events = activate(&mut runner, artifact, index);
    assert_eq!(pool(&runner, P1), 1, "mana from the elected definition");
    assert_eq!(activations_of_kind(&events, ActivatedAbilityKind::Mana), 1);
    assert_eq!(triggers_on_stack_from(&runner, mentor), 0);
    assert_eq!(triggers_on_stack_from(&runner, avalanche), 1);
    resolve_stack(&mut runner);
    assert_eq!(
        life(&runner, P1),
        life0 - 1,
        "Avalanche only; no printed life gain"
    );
}

// ---------------------------------------------------------------------------
// CantBeActivated's kind axis keeps its "non-loyalty" meaning
// ---------------------------------------------------------------------------

fn mana_activation_allowed(
    kind: Option<ActivatedAbilityKind>,
    exemption: engine::types::statics::ActivationExemption,
    oracle: &str,
) -> bool {
    let mut scenario = main_phase();
    scenario.add_creature(P1, "Prohibitor", 1, 1).with_static(
        engine::types::statics::StaticMode::CantBeActivated {
            who: engine::types::statics::ProhibitionScope::AllPlayers,
            source_filter: TargetFilter::Any,
            exemption,
            kind,
        },
    );
    let dork = scenario
        .add_creature(P0, "Dork", 1, 1)
        .from_oracle_text(oracle)
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: dork,
            ability_index: 0,
        })
        .is_ok()
}

#[test]
fn cant_be_activated_kind_axis_reads_normal_as_non_loyalty() {
    use engine::types::statics::ActivationExemption::{ManaAbilities, None as NoExemption};
    // `Some(Normal)` predates the Mana kind and means "non-loyalty".
    assert!(!mana_activation_allowed(
        Some(ActivatedAbilityKind::Normal),
        NoExemption,
        MANA
    ));
    assert!(mana_activation_allowed(
        Some(ActivatedAbilityKind::Normal),
        ManaAbilities,
        MANA
    ));
    assert!(!mana_activation_allowed(None, NoExemption, MANA));
    assert!(mana_activation_allowed(None, ManaAbilities, MANA));
    assert!(mana_activation_allowed(
        Some(ActivatedAbilityKind::Loyalty),
        NoExemption,
        MANA
    ));
    assert!(!mana_activation_allowed(
        Some(ActivatedAbilityKind::Mana),
        NoExemption,
        MANA
    ));
    // `Some(Mana)` blocks only mana abilities; an ordinary ability is allowed.
    assert!(mana_activation_allowed(
        Some(ActivatedAbilityKind::Mana),
        NoExemption,
        DRAW
    ));
    assert!(!mana_activation_allowed(
        Some(ActivatedAbilityKind::Normal),
        NoExemption,
        DRAW
    ));
}

// ---------------------------------------------------------------------------
// Exactly once across a replacement-paused cost
// ---------------------------------------------------------------------------

/// EO4: the Treasure's sacrifice pauses on a replacement-order choice (two
/// graveyard replacements). The activation is published once, after the pause
/// resolves, and triggers once.
#[test]
fn replacement_paused_cost_publishes_one_activation_and_one_trigger() {
    let mut scenario = main_phase();
    let avalanche = add_avalanche(&mut scenario);
    scenario
        .add_creature(P0, "Rest in Peace", 0, 0)
        .as_enchantment()
        .from_oracle_text(
            "If a card or token would be put into a graveyard from anywhere, exile it instead.",
        );
    scenario
        .add_creature(P0, "Leyline of the Void", 0, 0)
        .as_enchantment()
        .from_oracle_text("If a card or token would be put into an opponent's graveyard from anywhere, exile it instead.");
    let treasure = add_artifact(&mut scenario, P1, "Treasure", TREASURE_C);
    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let life0 = life(&runner, P1);
    let mut events = activate(&mut runner, treasure, 0);
    let paused = matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    );
    assert!(
        paused,
        "reach: the cost paused on a replacement choice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        activations_of_kind(&events, ActivatedAbilityKind::Mana),
        0,
        "nothing is published while the cost is unpaid"
    );
    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("replacement chosen")
            .events,
    );
    assert_eq!(pool(&runner, P1), 1, "reach: mana produced after the pause");
    assert_eq!(activations_of_kind(&events, ActivatedAbilityKind::Mana), 1);
    settle_trigger_order(&mut runner);
    assert_eq!(triggers_on_stack_from(&runner, avalanche), 1);
    resolve_stack(&mut runner);
    assert_eq!(life(&runner, P1), life0 - 1);
}

// ---------------------------------------------------------------------------
// Copy guard: a mana activation has no stack entry to copy
// ---------------------------------------------------------------------------

/// A no-exclusion "copy that ability" trigger observing a mana activation must
/// not fall back to an unrelated stack entry of the same source.
#[test]
fn a_mana_activation_never_copies_an_unrelated_same_source_entry() {
    let mut scenario = main_phase();
    scenario
        .add_creature(P0, "Copier", 1, 1)
        .from_oracle_text("Whenever you activate an ability, copy that ability. You may choose new targets for the copy.");
    let source = scenario
        .add_creature(P0, "Source", 1, 1)
        .from_oracle_text(&format!("{MANA}\n{{0}}: You gain 1 life."))
        .id();
    let mut runner = scenario.build();
    let life0 = life(&runner, P0);
    // The non-mana ability: original + its copy (positive control).
    activate(&mut runner, source, 1);
    // Resolve only the copy trigger, leaving the original (and copy) on the stack.
    runner.act(GameAction::PassPriority).expect("pass");
    runner.act(GameAction::PassPriority).expect("pass");
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == source),
        "reach: a same-source activation is still on the stack"
    );
    // The mana activation triggers the copier, which must copy nothing.
    activate(&mut runner, source, 0);
    resolve_stack(&mut runner);
    assert_eq!(
        life(&runner, P0),
        life0 + 2,
        "original + one copy, nothing more"
    );
}
