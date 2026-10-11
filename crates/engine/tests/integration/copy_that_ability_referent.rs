//! CR 707.10 + CR 113.7a + CR 608.2k: "copy that ability" names the activated
//! ability on the stack, so it survives its source leaving and never binds the
//! permanent the copier is attached to. Countered originals copy nothing
//! unrelated.
//!
//! CR 707.10: to copy an activated ability means to put a copy of it onto the
//! stack. CR 113.7a: once activated, an ability exists on the stack
//! independently of its source. CR 608.2k: an effect that refers to an
//! untargeted object named by the trigger condition still affects that object.
//! CR 701.6a: a countered ability is removed from the stack, so nothing left on
//! the stack is "that ability".

use std::collections::VecDeque;

use engine::game::effects::attach::attach_to;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ILLUSIONISTS_BRACERS: &str = "Whenever an ability of equipped creature is activated, if it isn't a mana ability, copy that ability. You may choose new targets for the copy.\nEquip {3}";
const BATTLEMAGES_BRACERS: &str = "Equipped creature has haste.\nWhenever an ability of equipped creature is activated, if it isn't a mana ability, you may pay {1}. If you do, copy that ability. You may choose new targets for the copy.\nEquip {2}";
const ROWANS_TALENT: &str = "Enchant planeswalker\nEnchanted planeswalker has \"[+1]: Up to one target creature gets +2/+0 and gains first strike and trample until end of turn.\"\nWhenever you activate a loyalty ability of enchanted planeswalker, copy that ability. You may choose new targets for the copy.";
const RINGS_OF_BRIGHTHEARTH: &str = "Whenever you activate an ability, if it isn't a mana ability, you may pay {2}. If you do, copy that ability. You may choose new targets for the copy.";
const BRASS_SQUIRE: &str =
    "{T}: Attach target Equipment you control to target creature you control.";
const BOOMERANG: &str = "Return target permanent to its owner's hand.";
const STIFLE: &str =
    "Counter target activated or triggered ability. (Mana abilities can't be targeted.)";
const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const DRAW: &str = "{T}: Draw a card.";

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest", "Forest"]);
    scenario
}

fn add_zero_cost_instant(scenario: &mut GameScenario, name: &str, oracle: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, true, oracle)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn add_colorless(scenario: &mut GameScenario, amount: usize) {
    scenario.with_mana_pool(
        P0,
        std::iter::repeat_n(ManaType::Colorless, amount)
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn library_size(runner: &GameRunner) -> usize {
    runner.state().players[0].library.len()
}

/// Answers prompts (target prompts from `targets`, in order) until `stop`
/// holds, and returns every event emitted on the way.
fn drive(
    runner: &mut GameRunner,
    stop: impl Fn(&GameState) -> bool,
    mut targets: VecDeque<ObjectId>,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..64 {
        if stop(runner.state()) {
            return events;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                GameAction::PassPriority
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::CopyRetarget { .. } => GameAction::KeepAllCopyTargets,
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Object(
                    targets
                        .pop_front()
                        .expect("a declared target for this prompt"),
                )),
            },
            other => panic!("unexpected prompt: {other:?}"),
        };
        events.extend(
            runner
                .act(action)
                .expect("the engine accepts the action")
                .events,
        );
    }
    panic!(
        "stop condition never reached; waiting for {:?}",
        runner.state().waiting_for
    );
}

/// P0 holds priority with exactly `len` objects on the stack.
fn priority_with_stack(len: usize) -> impl Fn(&GameState) -> bool {
    move |state: &GameState| {
        matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.len() == len
    }
}

fn priority_with_no_trigger_waiting(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { .. })
        && !state
            .stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
}

fn priority_with_empty_stack(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty()
}

/// R1: the activated ability waits at the bottom with the copier's trigger
/// above it.
fn assert_trigger_waits_above_activation(
    runner: &GameRunner,
    activated: ObjectId,
    copier: ObjectId,
) {
    let stack = &runner.state().stack;
    assert_eq!(stack.len(), 2, "reach: activation and trigger only");
    assert!(
        stack
            .front()
            .is_some_and(|entry| entry.source_id == activated
                && matches!(entry.kind, StackEntryKind::ActivatedAbility { .. })),
        "reach: the activated ability waits at the bottom"
    );
    assert!(
        stack.back().is_some_and(|entry| entry.source_id == copier
            && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })),
        "reach: the copy trigger waits above the activation"
    );
}

/// R3 + CR 707.10b: the trigger's resolution put a copy of the activated
/// ability (same source as the original) onto the stack.
fn assert_copy_put_on_stack(runner: &GameRunner, events: &[GameEvent], activated: ObjectId) {
    let copied = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::StackPushed { object_id } => Some(*object_id),
            _ => None,
        })
        .any(|id| {
            runner.state().stack.iter().any(|entry| {
                entry.id == id
                    && entry.source_id == activated
                    && matches!(entry.kind, StackEntryKind::ActivatedAbility { .. })
            })
        });
    assert!(
        copied,
        "the trigger's resolution puts a copy of the activated ability on the stack"
    );
}

fn copy_effect_resolved(events: &[GameEvent], copier: ObjectId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved { kind: EffectKind::CopySpell, source_id, .. }
                if *source_id == copier
        )
    })
}

/// An Equipment with a copy trigger on Host (`{T}: Draw a card.`), plus Other
/// (`{T}: Draw a card.`), Brass Squire and a zero-cost Boomerang.
struct EquippedBoard {
    runner: GameRunner,
    bracers: ObjectId,
    host: ObjectId,
    other: ObjectId,
    squire: ObjectId,
    boomerang: ObjectId,
    library_before: usize,
}

impl EquippedBoard {
    fn new(bracers_name: &str, bracers_oracle: &str, colorless: usize) -> Self {
        let mut scenario = main_phase();
        let bracers = scenario
            .add_creature(P0, bracers_name, 0, 0)
            .as_artifact()
            .with_subtypes(vec!["Equipment"])
            .from_oracle_text(bracers_oracle)
            .id();
        let host = scenario
            .add_creature(P0, "Host", 1, 1)
            .from_oracle_text(DRAW)
            .id();
        let other = scenario
            .add_creature(P0, "Other", 1, 1)
            .from_oracle_text(DRAW)
            .id();
        let squire = scenario
            .add_creature(P0, "Brass Squire", 1, 3)
            .from_oracle_text(BRASS_SQUIRE)
            .id();
        let boomerang = add_zero_cost_instant(&mut scenario, "Boomerang", BOOMERANG);
        add_colorless(&mut scenario, colorless);
        let mut runner = scenario.build();
        attach_to(runner.state_mut(), bracers, host);
        let library_before = library_size(&runner);
        Self {
            runner,
            bracers,
            host,
            other,
            squire,
            boomerang,
            library_before,
        }
    }

    fn activate_host(&mut self) {
        self.runner
            .act(GameAction::ActivateAbility {
                source_id: self.host,
                ability_index: 0,
            })
            .expect("Host's ability activates");
        drive(&mut self.runner, priority_with_stack(2), VecDeque::new());
        assert_trigger_waits_above_activation(&self.runner, self.host, self.bracers);
    }

    fn bounce_host(&mut self) {
        self.runner
            .cast(self.boomerang)
            .target_object(self.host)
            .commit();
        drive(&mut self.runner, priority_with_stack(2), VecDeque::new());
        assert_eq!(
            self.runner.state().objects[&self.host].zone,
            Zone::Hand,
            "reach: Host left before the trigger resolves"
        );
    }

    fn move_bracers_to_other(&mut self) {
        self.runner
            .act(GameAction::ActivateAbility {
                source_id: self.squire,
                ability_index: 0,
            })
            .expect("Brass Squire's ability activates");
        drive(
            &mut self.runner,
            priority_with_stack(2),
            VecDeque::from([self.bracers, self.other]),
        );
        assert_eq!(
            self.runner.state().objects[&self.bracers].attached_to,
            Some(AttachTarget::Object(self.other)),
            "reach: the Equipment moved to Other before the trigger resolves"
        );
    }

    /// Resolves the copy trigger and asserts R3.
    fn resolve_trigger(&mut self) {
        let events = drive(
            &mut self.runner,
            priority_with_no_trigger_waiting,
            VecDeque::new(),
        );
        assert_copy_put_on_stack(&self.runner, &events, self.host);
    }

    fn colorless_left(&self) -> usize {
        self.runner.state().players[0].mana_pool.total()
    }

    fn draws_after_stack_empties(&mut self) -> usize {
        drive(&mut self.runner, priority_with_empty_stack, VecDeque::new());
        self.library_before - library_size(&self.runner)
    }
}

/// T1: the equipped creature left while the trigger waited; the copy is still
/// made from the activated ability (CR 113.7a).
#[test]
fn bracers_copy_the_ability_after_the_equipped_creature_left() {
    let mut board = EquippedBoard::new("Illusionist's Bracers", ILLUSIONISTS_BRACERS, 0);
    board.activate_host();
    board.bounce_host();
    board.resolve_trigger();
    assert_eq!(board.draws_after_stack_empties(), 2);
}

/// T4: the Equipment moved to another creature while the trigger waited; the
/// referent is still the ability the original host activated (CR 608.2k).
#[test]
fn bracers_copy_the_original_ability_after_moving_to_another_creature() {
    let mut board = EquippedBoard::new("Illusionist's Bracers", ILLUSIONISTS_BRACERS, 0);
    board.activate_host();
    board.move_bracers_to_other();
    assert_eq!(
        board.runner.state().objects[&board.host].zone,
        Zone::Battlefield,
        "reach: Host is still on the battlefield"
    );
    board.resolve_trigger();
    assert_eq!(board.draws_after_stack_empties(), 2);
    assert!(
        !board.runner.state().objects[&board.other].tapped,
        "Other's ability was never activated"
    );
}

/// T5: Host left, then the Equipment moved to Other.
#[test]
fn bracers_copy_the_original_ability_after_the_host_left_and_they_moved() {
    let mut board = EquippedBoard::new("Illusionist's Bracers", ILLUSIONISTS_BRACERS, 0);
    board.activate_host();
    board.bounce_host();
    board.move_bracers_to_other();
    board.resolve_trigger();
    assert_eq!(board.draws_after_stack_empties(), 2);
}

/// T8: Battlemage's Bracers copy from a `PayCost` root's `CopySpell` sub node;
/// moving the Equipment to Other must not change the referent (CR 608.2k).
#[test]
fn battlemages_bracers_copy_the_original_ability_after_moving_to_another_creature() {
    let mut board = EquippedBoard::new("Battlemage's Bracers", BATTLEMAGES_BRACERS, 1);
    board.activate_host();
    board.move_bracers_to_other();
    assert_eq!(
        board.runner.state().objects[&board.host].zone,
        Zone::Battlefield,
        "reach: Host is still on the battlefield"
    );
    board.resolve_trigger();
    assert_eq!(board.colorless_left(), 0, "the optional {{1}} was paid");
    assert_eq!(board.draws_after_stack_empties(), 2);
    assert!(
        !board.runner.state().objects[&board.other].tapped,
        "Other's ability was never activated"
    );
}

/// T9: live positive control for T8.
#[test]
fn battlemages_bracers_copy_a_live_equipped_creatures_ability() {
    let mut board = EquippedBoard::new("Battlemage's Bracers", BATTLEMAGES_BRACERS, 1);
    board.activate_host();
    board.resolve_trigger();
    assert_eq!(board.colorless_left(), 0, "the optional {{1}} was paid");
    assert_eq!(board.draws_after_stack_empties(), 2);
}

/// Rowan's Talent attached to a planeswalker whose printed loyalty ability is
/// `pw_oracle`, plus a zero-cost Boomerang.
struct RowanBoard {
    runner: GameRunner,
    pw: ObjectId,
    talent: ObjectId,
    boomerang: ObjectId,
    library_before: usize,
}

impl RowanBoard {
    fn new(loyalty: u32, pw_oracle: &str) -> Self {
        let mut scenario = main_phase();
        let pw = scenario
            .add_planeswalker_from_oracle(P0, "Host Walker", "Rowan", loyalty, pw_oracle)
            .id();
        let talent = scenario
            .add_enchantment_from_oracle(P0, "Rowan's Talent", ROWANS_TALENT)
            .with_subtypes(vec!["Aura"])
            .id();
        let boomerang = add_zero_cost_instant(&mut scenario, "Boomerang", BOOMERANG);
        let mut runner = scenario.build();
        attach_to(runner.state_mut(), talent, pw);
        let library_before = library_size(&runner);
        Self {
            runner,
            pw,
            talent,
            boomerang,
            library_before,
        }
    }

    /// Activates the printed loyalty ability, located by its `Draw` effect —
    /// the Talent grants the planeswalker a second loyalty ability.
    fn activate_printed_ability(&mut self) {
        let ability_index = self.runner.state().objects[&self.pw]
            .abilities
            .iter()
            .position(|ability| matches!(*ability.effect, Effect::Draw { .. }))
            .expect("the printed draw ability");
        self.runner
            .act(GameAction::ActivateAbility {
                source_id: self.pw,
                ability_index,
            })
            .expect("the loyalty ability activates");
        drive(&mut self.runner, priority_with_stack(2), VecDeque::new());
        assert_trigger_waits_above_activation(&self.runner, self.pw, self.talent);
    }

    fn resolve_trigger(&mut self) {
        let events = drive(
            &mut self.runner,
            priority_with_no_trigger_waiting,
            VecDeque::new(),
        );
        assert_copy_put_on_stack(&self.runner, &events, self.pw);
    }

    fn draws_after_stack_empties(&mut self) -> usize {
        drive(&mut self.runner, priority_with_empty_stack, VecDeque::new());
        self.library_before - library_size(&self.runner)
    }
}

/// Live positive control for T2/T3.
#[test]
fn rowans_talent_copies_a_live_planeswalkers_loyalty_ability() {
    let mut board = RowanBoard::new(3, "[+1]: Draw a card.");
    board.activate_printed_ability();
    board.resolve_trigger();
    assert_eq!(board.draws_after_stack_empties(), 2);
}

/// T2: the enchanted planeswalker left while the trigger waited.
#[test]
fn rowans_talent_copies_the_loyalty_ability_after_the_planeswalker_left() {
    let mut board = RowanBoard::new(3, "[+1]: Draw a card.");
    board.activate_printed_ability();
    board
        .runner
        .cast(board.boomerang)
        .target_object(board.pw)
        .commit();
    drive(&mut board.runner, priority_with_stack(2), VecDeque::new());
    assert_eq!(
        board.runner.state().objects[&board.pw].zone,
        Zone::Hand,
        "reach: the planeswalker left before the trigger resolves"
    );
    board.resolve_trigger();
    assert_eq!(board.draws_after_stack_empties(), 2);
}

/// T3: the loyalty cost took the planeswalker to 0 (CR 704.5i) and the Talent
/// then went to the graveyard (CR 704.5m) before the trigger resolved.
#[test]
fn rowans_talent_copies_a_loyalty_ability_that_took_the_planeswalker_to_zero() {
    let mut board = RowanBoard::new(1, "[−1]: Draw a card.");
    board.activate_printed_ability();
    let objects = &board.runner.state().objects;
    assert_eq!(
        (objects[&board.pw].zone, objects[&board.talent].zone),
        (Zone::Graveyard, Zone::Graveyard),
        "reach: the planeswalker and the Talent left before the trigger resolves"
    );
    board.resolve_trigger();
    assert_eq!(board.draws_after_stack_empties(), 2);
}

/// P0 casts Lightning Bolt at P1 and holds priority, activates Host, then
/// Stifles the activated ability while the copier's trigger waits. Returns the
/// events of the trigger's resolution and P1's life delta once the stack is
/// empty.
fn run_with_the_original_countered(
    mut runner: GameRunner,
    host: ObjectId,
    copier: ObjectId,
    bolt: ObjectId,
    stifle: ObjectId,
) -> (Vec<GameEvent>, i32) {
    let life_before = runner.state().players[1].life;
    runner.cast(bolt).target_player(P1).commit();
    drive(&mut runner, priority_with_stack(1), VecDeque::new());
    runner
        .act(GameAction::ActivateAbility {
            source_id: host,
            ability_index: 0,
        })
        .expect("Host's ability activates");
    drive(&mut runner, priority_with_stack(3), VecDeque::new());
    let activated = runner
        .state()
        .stack
        .iter()
        .find(|entry| matches!(entry.kind, StackEntryKind::ActivatedAbility { .. }))
        .map(|entry| entry.id)
        .expect("reach: the activated ability is on the stack");
    runner.cast(stifle).target_object(activated).commit();
    drive(&mut runner, priority_with_stack(2), VecDeque::new());
    let stack = &runner.state().stack;
    assert!(
        !stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::ActivatedAbility { .. }))
            && stack.front().is_some_and(|entry| entry.source_id == bolt)
            && stack.back().is_some_and(|entry| entry.source_id == copier
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })),
        "reach: the ability was countered while Bolt and the copy trigger remain"
    );
    let trigger_events = drive(&mut runner, priority_with_stack(1), VecDeque::new());
    drive(&mut runner, priority_with_empty_stack, VecDeque::new());
    (trigger_events, runner.state().players[1].life - life_before)
}

/// T6: Rings of Brighthearth must not copy Lightning Bolt when the ability it
/// names was countered.
#[test]
fn rings_do_not_copy_an_unrelated_spell_when_the_ability_was_countered() {
    let mut scenario = main_phase();
    let rings = scenario
        .add_artifact_from_oracle(P0, "Rings of Brighthearth", RINGS_OF_BRIGHTHEARTH)
        .id();
    let host = scenario
        .add_creature(P0, "Host", 1, 1)
        .from_oracle_text(DRAW)
        .id();
    let bolt = add_zero_cost_instant(&mut scenario, "Lightning Bolt", LIGHTNING_BOLT);
    let stifle = add_zero_cost_instant(&mut scenario, "Stifle", STIFLE);
    add_colorless(&mut scenario, 2);
    let runner = scenario.build();

    let (events, life_delta) = run_with_the_original_countered(runner, host, rings, bolt, stifle);
    assert_eq!(life_delta, -3, "Lightning Bolt was not copied");
    assert!(
        copy_effect_resolved(&events, rings),
        "reach: the Rings' copy effect resolved"
    );
}

/// T7: Illusionist's Bracers must not copy Lightning Bolt when the ability
/// they name was countered, and the copy effect still resolves.
#[test]
fn bracers_do_not_copy_an_unrelated_spell_when_the_ability_was_countered() {
    let mut scenario = main_phase();
    let bracers = scenario
        .add_creature(P0, "Illusionist's Bracers", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(ILLUSIONISTS_BRACERS)
        .id();
    let host = scenario
        .add_creature(P0, "Host", 1, 1)
        .from_oracle_text(DRAW)
        .id();
    let bolt = add_zero_cost_instant(&mut scenario, "Lightning Bolt", LIGHTNING_BOLT);
    let stifle = add_zero_cost_instant(&mut scenario, "Stifle", STIFLE);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), bracers, host);

    let (events, life_delta) = run_with_the_original_countered(runner, host, bracers, bolt, stifle);
    assert_eq!(life_delta, -3, "Lightning Bolt was not copied");
    assert!(
        copy_effect_resolved(&events, bracers),
        "the Bracers' copy effect resolves"
    );
}
