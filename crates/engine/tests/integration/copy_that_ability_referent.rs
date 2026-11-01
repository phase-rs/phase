//! CR 707.10 + CR 113.7a + CR 608.2k: "copy that ability" names the activated
//! ability on the stack, so it survives its source leaving and never binds the
//! permanent the copier is attached to. A countered original is still copied
//! as it last existed, and nothing else on the stack ever stands in for it.
//!
//! CR 707.10: to copy an activated ability means to put a copy of it onto the
//! stack. CR 113.7a: once activated, an ability exists on the stack
//! independently of its source. CR 608.2k: an effect that refers to an
//! untargeted object named by the trigger condition still affects that object.
//! CR 701.6a + CR 608.2h: a countered ability is removed from the stack, so
//! nothing left on the stack is "that ability"; the copy is made from its
//! last-known state (Rings of Brighthearth ruling).

use engine::types::game_state::CastPaymentMode;
use std::collections::VecDeque;

use engine::game::effects::attach::attach_to;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, EffectKind, PtValue, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntry, StackEntryKind, WaitingFor};
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
    targets: VecDeque<ObjectId>,
) -> Vec<GameEvent> {
    drive_ordering(runner, stop, targets, None)
}

/// [`drive`], additionally answering a trigger-ordering prompt by placing the
/// trigger whose source is `copier_last` last — on top of the others (CR
/// 603.3b; index 0 is placed first). Without a copier the prompt is unexpected.
fn drive_ordering(
    runner: &mut GameRunner,
    stop: impl Fn(&GameState) -> bool,
    mut targets: VecDeque<ObjectId>,
    copier_last: Option<ObjectId>,
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
            WaitingFor::OrderTriggers { triggers, .. } if copier_last.is_some() => {
                let mut order: Vec<usize> = (0..triggers.len()).collect();
                order.sort_by_key(|&index| Some(triggers[index].source_id) == copier_last);
                GameAction::OrderTriggers { order }
            }
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
                payment_mode: CastPaymentMode::Auto,
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
                payment_mode: CastPaymentMode::Auto,
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
                payment_mode: CastPaymentMode::Auto,
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
/// Stifles the activated ability while the copier's trigger waits. Asserts that
/// the trigger's resolution puts a copy of Host's countered ability on the
/// stack, then empties the stack. Returns the events of the trigger's
/// resolution, P1's life delta and P0's draws once the stack is empty.
fn run_with_the_original_countered(
    runner: &mut GameRunner,
    host: ObjectId,
    copier: ObjectId,
    bolt: ObjectId,
    stifle: ObjectId,
) -> (Vec<GameEvent>, i32, usize) {
    let life_before = runner.state().players[1].life;
    let library_before = library_size(runner);
    runner.cast(bolt).target_player(P1).commit();
    drive(runner, priority_with_stack(1), VecDeque::new());
    runner
        .act(GameAction::ActivateAbility {
            source_id: host,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Host's ability activates");
    drive(runner, priority_with_stack(3), VecDeque::new());
    let activated = runner
        .state()
        .stack
        .iter()
        .find(|entry| matches!(entry.kind, StackEntryKind::ActivatedAbility { .. }))
        .map(|entry| entry.id)
        .expect("reach: the activated ability is on the stack");
    runner.cast(stifle).target_object(activated).commit();
    drive(runner, priority_with_stack(2), VecDeque::new());
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
    let trigger_events = drive(runner, copier_trigger_resolved(copier), VecDeque::new());
    assert_copy_put_on_stack(runner, &trigger_events, host);
    drive(runner, priority_with_empty_stack, VecDeque::new());
    (
        trigger_events,
        runner.state().players[1].life - life_before,
        library_before - library_size(runner),
    )
}

/// T6: Rings of Brighthearth copy the countered ability it names (ruling:
/// "They resolve even if that ability is countered"), never Lightning Bolt.
#[test]
fn rings_copy_the_countered_ability_and_never_an_unrelated_spell() {
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
    let mut runner = scenario.build();

    let (events, life_delta, draws) =
        run_with_the_original_countered(&mut runner, host, rings, bolt, stifle);
    assert!(
        copy_effect_resolved(&events, rings),
        "reach: the Rings' copy effect resolved"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "reach: the optional {{2}} was paid"
    );
    assert_eq!(draws, 1, "only the copy draws: the original was countered");
    assert_eq!(life_delta, -3, "Lightning Bolt was not copied");
}

/// T7: Illusionist's Bracers copy the countered ability they name, never
/// Lightning Bolt, and the copy effect still resolves.
#[test]
fn bracers_copy_the_countered_ability_and_never_an_unrelated_spell() {
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

    let (events, life_delta, draws) =
        run_with_the_original_countered(&mut runner, host, bracers, bolt, stifle);
    assert!(
        copy_effect_resolved(&events, bracers),
        "the Bracers' copy effect resolves"
    );
    assert_eq!(draws, 1, "only the copy draws: the original was countered");
    assert_eq!(life_delta, -3, "Lightning Bolt was not copied");
}

const CHROME_COMPANION: &str = "Whenever this creature becomes tapped, you gain 1 life.\n{2}, {T}: Put target card from a graveyard on the bottom of its owner's library.";
const STONE_KAVU: &str =
    "{R}: This creature gets +1/+0 until end of turn.\n{W}: This creature gets +0/+1 until end of turn.";
/// The quoted text of Rowan Kenrith's −8 emblem, carried by a test artifact.
const ROWAN_KENRITH_EMBLEM: &str = "Whenever you activate an ability that isn't a mana ability, copy it. You may choose new targets for the copy.";

/// R-ev: the activation's `AbilityActivated` event names the stack object it
/// created (CR 602.2a).
fn assert_activation_names_entry(events: &[GameEvent], source: ObjectId, entry: ObjectId) {
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::AbilityActivated { source_id, stack_entry: Some(id), .. }
                if *source_id == source && *id == entry
        )),
        "reach: the activation event names the stack entry it created"
    );
}

/// The copier's trigger has resolved: P0 holds priority and nothing on the
/// stack is sourced by `copier`.
fn copier_trigger_resolved(copier: ObjectId) -> impl Fn(&GameState) -> bool {
    move |state: &GameState| {
        matches!(state.waiting_for, WaitingFor::Priority { .. })
            && !state.stack.iter().any(|entry| entry.source_id == copier)
    }
}

/// Chrome Companion, whose `{2}, {T}` activation taps it and so triggers its
/// own "becomes tapped" ability alongside the copier's trigger, plus a card in
/// P1's graveyard for the activation to target and a zero-cost Stifle.
struct TapTriggerBoard {
    runner: GameRunner,
    chrome: ObjectId,
    copier: ObjectId,
    card: ObjectId,
    stifle: ObjectId,
    life_before: i32,
}

impl TapTriggerBoard {
    /// `equipment` attaches the copier to Chrome Companion as an Equipment;
    /// otherwise it is a plain artifact.
    fn new(copier_name: &str, copier_oracle: &str, equipment: bool, colorless: usize) -> Self {
        let mut scenario = main_phase();
        let copier = if equipment {
            scenario
                .add_creature(P0, copier_name, 0, 0)
                .as_artifact()
                .with_subtypes(vec!["Equipment"])
                .from_oracle_text(copier_oracle)
                .id()
        } else {
            scenario
                .add_artifact_from_oracle(P0, copier_name, copier_oracle)
                .id()
        };
        let chrome = scenario
            .add_creature(P0, "Chrome Companion", 2, 1)
            .as_artifact_creature()
            .from_oracle_text(CHROME_COMPANION)
            .id();
        let card = scenario.add_creature_to_graveyard(P1, "Bear", 2, 2).id();
        let stifle = add_zero_cost_instant(&mut scenario, "Stifle", STIFLE);
        add_colorless(&mut scenario, colorless);
        let mut runner = scenario.build();
        if equipment {
            attach_to(runner.state_mut(), copier, chrome);
        }
        let life_before = runner.state().players[0].life;
        Self {
            runner,
            chrome,
            copier,
            card,
            stifle,
            life_before,
        }
    }

    /// Activates Chrome Companion with the copier's trigger ordered on top,
    /// asserts R-ev and R-stack, and returns the activated ability's id.
    fn activate_with_copier_on_top(&mut self) -> ObjectId {
        let mut events = self
            .runner
            .act(GameAction::ActivateAbility {
                source_id: self.chrome,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("Chrome Companion's ability activates")
            .events;
        let copier = self.copier;
        events.extend(drive_ordering(
            &mut self.runner,
            move |state: &GameState| {
                matches!(state.waiting_for, WaitingFor::Priority { .. })
                    && state.stack.iter().any(|entry| entry.source_id == copier)
            },
            VecDeque::from([self.card]),
            Some(self.copier),
        ));
        let stack: Vec<_> = self.runner.state().stack.iter().collect();
        // R-stack, CR 603.3b: the tap trigger and the copier's trigger go on the
        // stack together, above the activated ability, the copier's on top.
        assert!(
            matches!(
                stack.as_slice(),
                [activated, tapped, copy_trigger]
                    if activated.source_id == self.chrome
                        && matches!(activated.kind, StackEntryKind::ActivatedAbility { .. })
                        && tapped.source_id == self.chrome
                        && matches!(tapped.kind, StackEntryKind::TriggeredAbility { .. })
                        && copy_trigger.source_id == self.copier
                        && matches!(copy_trigger.kind, StackEntryKind::TriggeredAbility { .. })
            ),
            "reach: [activation, its tap trigger, the copier's trigger] bottom to top"
        );
        let activated = stack[0].id;
        assert_activation_names_entry(&events, self.chrome, activated);
        activated
    }

    /// Resolves the copier's trigger and returns the events of that resolution.
    fn resolve_copier_trigger(&mut self) -> Vec<GameEvent> {
        drive(
            &mut self.runner,
            copier_trigger_resolved(self.copier),
            VecDeque::new(),
        )
    }

    fn life_delta_after_stack_empties(&mut self) -> i32 {
        drive(&mut self.runner, priority_with_empty_stack, VecDeque::new());
        self.runner.state().players[0].life - self.life_before
    }
}

/// J1: the Bracers copy Chrome Companion's activated ability, never the
/// "becomes tapped" trigger that activation caused, though it has the same
/// source and waits between them (CR 602.2a + CR 707.10b).
#[test]
fn bracers_copy_the_activation_not_a_trigger_it_caused() {
    let mut board = TapTriggerBoard::new("Illusionist's Bracers", ILLUSIONISTS_BRACERS, true, 2);
    board.activate_with_copier_on_top();
    let events = board.resolve_copier_trigger();
    assert_copy_put_on_stack(&board.runner, &events, board.chrome);
    assert_eq!(
        board.life_delta_after_stack_empties(),
        1,
        "the tap trigger resolves once and is never copied"
    );
}

/// J2: the same through Rings of Brighthearth's `PayCost` root → `CopySpell`
/// sub node.
#[test]
fn rings_copy_the_activation_not_a_trigger_it_caused() {
    let mut board = TapTriggerBoard::new("Rings of Brighthearth", RINGS_OF_BRIGHTHEARTH, false, 4);
    board.activate_with_copier_on_top();
    let events = board.resolve_copier_trigger();
    assert_eq!(
        board.runner.state().players[0].mana_pool.total(),
        0,
        "the optional {{2}} was paid"
    );
    assert_copy_put_on_stack(&board.runner, &events, board.chrome);
    assert_eq!(
        board.life_delta_after_stack_empties(),
        1,
        "the tap trigger resolves once and is never copied"
    );
}

/// J3: "copy it" (`TriggeringSource`) names the activated ability too.
#[test]
fn an_activation_copy_it_trigger_copies_the_activation_not_a_trigger_it_caused() {
    let mut board = TapTriggerBoard::new("Emblem Carrier", ROWAN_KENRITH_EMBLEM, false, 2);
    board.activate_with_copier_on_top();
    let events = board.resolve_copier_trigger();
    assert_copy_put_on_stack(&board.runner, &events, board.chrome);
    assert_eq!(
        board.life_delta_after_stack_empties(),
        1,
        "the tap trigger resolves once and is never copied"
    );
}

/// J4: with the activated ability countered (CR 701.6a), the Bracers do not
/// copy the tap trigger it caused. The countered activation itself is copied
/// (CR 113.7a + 608.2h), never its tap trigger.
#[test]
fn bracers_copy_the_countered_activation_not_a_trigger_it_caused() {
    let mut board = TapTriggerBoard::new("Illusionist's Bracers", ILLUSIONISTS_BRACERS, true, 2);
    let activated = board.activate_with_copier_on_top();
    board
        .runner
        .cast(board.stifle)
        .target_object(activated)
        .commit();
    drive(&mut board.runner, priority_with_stack(2), VecDeque::new());
    let stack = &board.runner.state().stack;
    assert!(
        !stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::ActivatedAbility { .. }))
            && stack
                .front()
                .is_some_and(|entry| entry.source_id == board.chrome)
            && stack
                .back()
                .is_some_and(|entry| entry.source_id == board.copier),
        "reach: the activation was countered while its tap trigger and the copy trigger remain"
    );
    let events = board.resolve_copier_trigger();
    assert_copy_put_on_stack(&board.runner, &events, board.chrome);
    assert_eq!(
        board.life_delta_after_stack_empties(),
        1,
        "the tap trigger resolves once and is never copied"
    );
    assert_eq!(
        board.runner.state().objects[&board.card].zone,
        Zone::Library,
        "the copy put the card on the bottom of its owner's library"
    );
}

/// J5: two activations of one source (CR 117.1b). Each Bracers trigger names
/// its own activation; with the second countered, its trigger never copies the
/// first one still waiting below. With the second countered, its trigger copies
/// that second activation from its last-known state.
#[test]
fn bracers_copy_each_activation_not_an_earlier_one_of_the_same_source() {
    let mut scenario = main_phase();
    let bracers = scenario
        .add_creature(P0, "Illusionist's Bracers", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(ILLUSIONISTS_BRACERS)
        .id();
    let kavu = scenario
        .add_creature(P0, "Stone Kavu", 3, 3)
        .from_oracle_text(STONE_KAVU)
        .id();
    let stifle = add_zero_cost_instant(&mut scenario, "Stifle", STIFLE);
    scenario.with_mana_pool(
        P0,
        [ManaType::Red, ManaType::White]
            .into_iter()
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), bracers, kavu);
    let pump_index = |runner: &GameRunner, power: i32, toughness: i32| {
        runner.state().objects[&kavu]
            .abilities
            .iter()
            .position(|ability| {
                matches!(
                    &*ability.effect,
                    Effect::Pump {
                        power: PtValue::Fixed(p),
                        toughness: PtValue::Fixed(t),
                        ..
                    } if *p == power && *t == toughness
                )
            })
            .expect("Stone Kavu's pump ability")
    };
    let red_index = pump_index(&runner, 1, 0);
    let white_index = pump_index(&runner, 0, 1);

    let mut events = Vec::new();
    for (ability_index, stack_len) in [(red_index, 2), (white_index, 4)] {
        events.extend(
            runner
                .act(GameAction::ActivateAbility {
                    source_id: kavu,
                    ability_index,
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("Stone Kavu's ability activates")
                .events,
        );
        events.extend(drive(
            &mut runner,
            priority_with_stack(stack_len),
            VecDeque::new(),
        ));
    }
    let stack: Vec<_> = runner.state().stack.iter().collect();
    let activation_index = |entry: &StackEntry| match &entry.kind {
        StackEntryKind::ActivatedAbility { source_id, ability } if *source_id == kavu => {
            ability.ability_index
        }
        _ => None,
    };
    assert!(
        matches!(
            stack.as_slice(),
            [first, first_trigger, second, second_trigger]
                if activation_index(first) == Some(red_index)
                    && first_trigger.source_id == bracers
                    && activation_index(second) == Some(white_index)
                    && second_trigger.source_id == bracers
        ),
        "reach: [A1, T1, A2, T2] bottom to top"
    );
    let (first, second, second_trigger) = (stack[0].id, stack[2].id, stack[3].id);
    assert_activation_names_entry(&events, kavu, first);
    assert_activation_names_entry(&events, kavu, second);

    runner.cast(stifle).target_object(second).commit();
    drive(&mut runner, priority_with_stack(3), VecDeque::new());
    assert!(
        runner.state().stack.iter().all(|entry| entry.id != second),
        "reach: the second activation was countered"
    );
    let events = drive(
        &mut runner,
        move |state: &GameState| {
            matches!(state.waiting_for, WaitingFor::Priority { .. })
                && state.stack.iter().all(|entry| entry.id != second_trigger)
        },
        VecDeque::new(),
    );
    assert!(
        copy_effect_resolved(&events, bracers),
        "reach: the second trigger's copy effect resolved"
    );
    drive(&mut runner, priority_with_empty_stack, VecDeque::new());
    assert_eq!(
        runner.state().objects[&kavu].power,
        Some(5),
        "the first activation resolves twice (original + its own copy); the second trigger copies nothing of it"
    );
    assert_eq!(
        runner.state().objects[&kavu].toughness,
        Some(4),
        "the countered white activation is copied once, by its own trigger"
    );
}
