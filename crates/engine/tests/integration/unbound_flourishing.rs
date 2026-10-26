//! Unbound Flourishing — {2}{G} Enchantment (MH1), Oracle text verbatim from MTGJSON:
//!
//! > Whenever you cast a permanent spell with a mana cost that contains {X},
//! > double the value of X.
//! > Whenever you cast an instant or sorcery spell or activate an ability, if
//! > that spell's mana cost or that ability's activation cost contains {X}, copy
//! > that spell or ability. You may choose new targets for the copy.
//!
//! These rows drive the real cast/activate pipeline. They cover line 1's
//! `{X}`-in-mana-cost qualifier and line 2's copy, which lowers to a spell-cast
//! trigger and an activation trigger, each answering its own half of the
//! intervening-if from its event (CR 603.4). Rulings exercised: the copy is made
//! even if the original was countered; activated abilities are copied (also
//! after Stifle counters one, U-24); X
//! without the mana symbol {X} does not count; X = 0 is still
//! copied; two Unbounds copy twice; the copy has the same X; the copy is not
//! cast or activated; you may choose new targets; any {X} instant, sorcery or
//! activated ability is copied, targeted or not.
//!
//! Line 1's "double the value of X" is covered from U-16 on: Walking Ballista
//! X=2 entering with 4 counters, two Unbounds giving 8, X = 0, Hydroid
//! Krasis's order choice, and a spell-cast trigger reading the doubled mana
//! value.
//!
//! `X Draw Engine` ("{X}: Draw X cards.\n{1}: Draw a card.") is the one
//! synthetic fixture: it isolates an `{X}` activation and a non-`{X}`
//! activation of the same source with clean draw counts, which no printed card
//! gives (the Magus Lucea Kane rows use the same shape).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, EffectKind, SacrificeRequirement, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card::PrintedLoyalty;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, ConvokeMode, GameState, PayCostKind, PayableResource, StackEntryKind,
    WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const UNBOUND: &str = "Whenever you cast a permanent spell with a mana cost that contains {X}, double the value of X.\nWhenever you cast an instant or sorcery spell or activate an ability, if that spell's mana cost or that ability's activation cost contains {X}, copy that spell or ability. You may choose new targets for the copy.";
const WALKING_BALLISTA: &str = "This creature enters with X +1/+1 counters on it.\n{4}: Put a +1/+1 counter on this creature.\nRemove a +1/+1 counter from this creature: It deals 1 damage to any target.";
const SPHINXS_REVELATION: &str = "You gain X life and draw X cards.";
const STROKE_OF_GENIUS: &str = "Target player draws X cards.";
const OPT: &str = "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.";
const CANCEL: &str = "Counter target spell.";
const PERNICIOUS_DEED: &str = "{X}, Sacrifice this enchantment: Destroy each artifact, creature, and enchantment with mana value X or less.";
const KATARA: &str = "Vigilance\nWhen Katara enters, create a 1/1 white Ally creature token.\nWaterbend {X}: Creatures you control have base power and toughness X/X until end of turn. X can't be 0. Activate only during your turn. (While paying a waterbend cost, you can tap your artifacts and creatures to help. Each one pays for {1}.)";
const WIZARDS_ROCKETS: &str = "This artifact enters tapped.\n{X}, {T}, Sacrifice this artifact: Add X mana in any combination of colors.\nWhen this artifact is put into a graveyard from the battlefield, draw a card.";
const LAZAV: &str = "When Lazav enters, surveil 1. (Look at the top card of your library. You may put it into your graveyard.)\n{X}: Lazav becomes a copy of target creature card in your graveyard with mana value X, except its name is Lazav, the Multifarious, it's legendary in addition to its other types, and it has this ability.";
const BOTTLE_GNOMES: &str = "Sacrifice this creature: You gain 3 life.";
/// Krav, the Unredeemed's activated line, verbatim.
const KRAV_ACTIVATED: &str =
    "{B}, Sacrifice X creatures: Target player draws X cards and gains X life. Put X +1/+1 counters on Krav.";
/// SYNTHETIC: an `{X}` and a non-`{X}` activation of one source.
const X_DRAW_ENGINE: &str = "{X}: Draw X cards.\n{1}: Draw a card.";
const KRASIS: &str = "When you cast this spell, you gain half X life and draw half X cards. Round down each time.\nFlying, trample\nThis creature enters with X +1/+1 counters on it.";
const NISSA: &str = "[+2]: Scry 2.\n[0]: Look at the top card of your library. If it's a land card or a creature card with mana value less than or equal to the number of loyalty counters on Nissa, you may put that card onto the battlefield.\n[\u{2212}6]: Untap up to two target lands you control. They become 5/5 Elemental creatures with flying and haste until end of turn. They're still lands.";
const TEVAL: &str = "Flying, lifelink\nSpells you cast have delve. (Each card you exile from your graveyard while casting those spells pays for {1}.)\nWhenever you cast a spell, you lose life equal to its mana value.";
const ENDLESS_ONE: &str = "This creature enters with X +1/+1 counters on it.";
const FATED_FIREPOWER: &str = "Flash\nThis enchantment enters with X fire counters on it.\nIf a source you control would deal damage to an opponent or a permanent an opponent controls, it deals that much damage plus an amount of damage equal to the number of fire counters on this enchantment instead.";
/// Helix Pinnacle, verbatim (MTGJSON), reminder text included.
const HELIX_PINNACLE: &str = "Shroud (This enchantment can't be the target of spells or abilities.)\n{X}: Put X tower counters on this enchantment.\nAt the beginning of your upkeep, if there are 100 or more tower counters on this enchantment, you win the game.";
const STIFLE: &str =
    "Counter target activated or triggered ability. (Mana abilities can't be targeted.)";

/// A stack entry, summarized for exact stack assertions (bottom first).
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Spell(ObjectId),
    Activated(ObjectId, Option<u32>),
    Triggered(ObjectId),
    Other,
}

fn stack_entries(state: &GameState) -> Vec<Entry> {
    state
        .stack
        .iter()
        .map(|entry| match &entry.kind {
            StackEntryKind::Spell { .. } => Entry::Spell(entry.source_id),
            StackEntryKind::ActivatedAbility { source_id, ability } => {
                Entry::Activated(*source_id, ability.chosen_x)
            }
            StackEntryKind::TriggeredAbility { source_id, .. } => Entry::Triggered(*source_id),
            _ => Entry::Other,
        })
        .collect()
}

fn mana(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

fn add_player_mana(runner: &mut GameRunner, player: PlayerId, mana_type: ManaType, count: usize) {
    for _ in 0..count {
        runner.state_mut().players[usize::from(player.0)]
            .mana_pool
            .add(ManaUnit::new(mana_type, ObjectId(0), false, vec![]));
    }
}

fn add_mana(runner: &mut GameRunner, mana_type: ManaType, count: usize) {
    add_player_mana(runner, P0, mana_type, count);
}

fn unbound_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Card One",
            "Card Two",
            "Card Three",
            "Card Four",
            "Card Five",
            "Card Six",
            "Card Seven",
            "Card Eight",
            "Card Nine",
            "Card Ten",
            "Card Eleven",
            "Card Twelve",
        ],
    );
    scenario.with_library_top(
        P1,
        &[
            "Other One",
            "Other Two",
            "Other Three",
            "Other Four",
            "Other Five",
            "Other Six",
            "Other Seven",
            "Other Eight",
            "Other Nine",
            "Other Ten",
        ],
    );
    scenario
}

/// Unbound Flourishing on P0's battlefield.
fn unbound(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(P0, "Unbound Flourishing", UNBOUND)
        .with_mana_cost(mana(&[ManaCostShard::Green], 2))
        .id()
}

/// Walking Ballista ({X}{X}) in P0's hand.
fn add_ballista(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Walking Ballista", 0, 0, WALKING_BALLISTA)
        .as_artifact_creature()
        .with_mana_cost(mana(&[ManaCostShard::X, ManaCostShard::X], 0))
        .id()
}

fn add_revelation(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Sphinx's Revelation", true, SPHINXS_REVELATION)
        .with_mana_cost(mana(
            &[
                ManaCostShard::X,
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
            ],
            0,
        ))
        .id()
}

/// Mana for Sphinx's Revelation with the given X.
fn add_revelation_mana(runner: &mut GameRunner, x: usize) {
    add_mana(runner, ManaType::White, 1);
    add_mana(runner, ManaType::Blue, 2);
    add_mana(runner, ManaType::Colorless, x);
}

fn add_x_draw_engine(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_artifact_from_oracle(P0, "X Draw Engine", X_DRAW_ENGINE)
        .with_mana_cost(ManaCost::generic(2))
        .id()
}

fn library_len(runner: &GameRunner, player: usize) -> usize {
    runner.state().players[player].library.len()
}

fn trigger_count(state: &GameState, sources: &[ObjectId]) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| {
            matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                && sources.contains(&entry.source_id)
        })
        .count()
}

fn activated_entries_of(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::ActivatedAbility { source_id, .. } if *source_id == source
            )
        })
        .count()
}

fn spell_copies_of(events: &[GameEvent], original: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event, GameEvent::SpellCopied { original_id, .. } if *original_id == original)
        })
        .count()
}

/// Submit `action`, which must be accepted, and return the events it produced.
fn act_events(runner: &mut GameRunner, action: GameAction) -> Vec<GameEvent> {
    let label = format!("{action:?}");
    runner
        .act(action)
        .unwrap_or_else(|error| panic!("{label} must be accepted: {error:?}"))
        .events
}

fn act(runner: &mut GameRunner, action: GameAction) {
    act_events(runner, action);
}

fn pay_until_priority(runner: &mut GameRunner) {
    for _ in 0..4 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        act(runner, GameAction::PassPriority);
    }
    panic!(
        "the activation must finish paying: {:?}",
        runner.state().waiting_for
    );
}

/// Announce an `{X}` activation with the given X and pay it, ending at the
/// activator's priority with the ability on the stack.
fn activate_with_x(runner: &mut GameRunner, source: ObjectId, ability_index: usize, x: u32) {
    act(
        runner,
        GameAction::ActivateAbility {
            source_id: source,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
        "the activation announces X: {:?}",
        runner.state().waiting_for
    );
    act(runner, GameAction::ChooseX { value: x });
    pay_until_priority(runner);
}

/// Both players pass once, resolving the top of the stack; returns the events.
fn pass_both_events(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = act_events(runner, GameAction::PassPriority);
    events.extend(act_events(runner, GameAction::PassPriority));
    events
}

/// Both players pass once, resolving the top of the stack.
fn pass_both(runner: &mut GameRunner) {
    pass_both_events(runner);
}

/// Pass priority until the stack is empty, answering trigger ordering with the
/// printed order, and return every event produced on the way. Any other prompt
/// is a failure: a row that needs one answered drives it explicitly.
fn drain_stack_events(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..48 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return events;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            other => panic!("the stack drains through priority passes: {other:?}"),
        };
        events.extend(act_events(runner, action));
    }
    panic!(
        "the stack must drain: {:?} / {:?}",
        stack_entries(runner.state()),
        runner.state().waiting_for
    );
}

/// Pass priority until the stack is empty (see [`drain_stack_events`]).
fn drain_stack(runner: &mut GameRunner) {
    drain_stack_events(runner);
}

/// Cast `spell` through the public actions with the given X, answering the X
/// announcement, the target prompt (with `targets`) and the payment window, and
/// stop at the first prompt after payment (`Priority` or `OrderTriggers`).
fn cast_with_x_until_prompt(
    runner: &mut GameRunner,
    spell: ObjectId,
    x: u32,
    targets: &[ObjectId],
) {
    let card_id = runner.state().objects[&spell].card_id;
    act(
        runner,
        GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    );
    let mut targets_chosen = targets.is_empty();
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::ChooseXValue { .. } => act(runner, GameAction::ChooseX { value: x }),
            WaitingFor::TargetSelection { .. } if !targets_chosen => {
                targets_chosen = true;
                act(
                    runner,
                    GameAction::SelectTargets {
                        targets: targets.iter().copied().map(TargetRef::Object).collect(),
                    },
                );
            }
            WaitingFor::ManaPayment { .. } => act(runner, GameAction::PassPriority),
            _ => {
                assert!(targets_chosen, "the cast never asked for its targets");
                return;
            }
        }
    }
    panic!("the cast must finish: {:?}", runner.state().waiting_for);
}

/// `EffectResolved { ModifyAnnouncedX }` events emitted by `source`.
fn modify_x_events(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ModifyAnnouncedX,
                    source_id,
                    ..
                } if *source_id == source
            )
        })
        .count()
}

fn counters(runner: &GameRunner, object: ObjectId, kind: CounterType) -> u32 {
    runner.state().objects[&object]
        .counters
        .get(&kind)
        .copied()
        .unwrap_or(0)
}

fn pool_total(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[usize::from(player.0)]
        .mana_pool
        .total()
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[usize::from(player.0)].life
}

/// The sources of the triggers an `OrderTriggers` prompt for `player` offers,
/// in prompt order. Panics unless that prompt is pending.
fn order_prompt_sources(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    let WaitingFor::OrderTriggers {
        player: chooser,
        triggers,
    } = &runner.state().waiting_for
    else {
        panic!(
            "the triggers wait for an order choice: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*chooser, player);
    triggers.iter().map(|trigger| trigger.source_id).collect()
}

/// Assert P0's pending order prompt offers exactly the triggers of `below` and
/// `on_top` (in either prompt order), then answer it placing `on_top`'s trigger
/// last, so it is on top and resolves first (CR 603.3b + CR 405.2).
fn order_with_on_top(runner: &mut GameRunner, below: ObjectId, on_top: ObjectId) {
    let sources = order_prompt_sources(runner, P0);
    let mut offered = sources.clone();
    offered.sort();
    let mut expected = vec![below, on_top];
    expected.sort();
    assert_eq!(
        offered, expected,
        "the prompt offers exactly these two triggers"
    );
    let index = |source: ObjectId| {
        sources
            .iter()
            .position(|candidate| *candidate == source)
            .expect("the source is offered")
    };
    let order = vec![index(below), index(on_top)];
    act(runner, GameAction::OrderTriggers { order });
}

fn assert_settled(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty(),
        "{:?}",
        runner.state().stack
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
}

/// U-1: line 1 watches only permanent spells whose mana cost contains {X}.
/// Grizzly Bears ({1}{G}) does not trigger it; Walking Ballista ({X}{X}) does.
/// Line 1's doubling is asserted by the doubling rows (U-16 onward); this row
/// asserts only the trigger filter.
#[test]
fn line_one_ignores_a_permanent_spell_without_x_in_its_mana_cost() {
    let mut scenario = unbound_scenario();
    let unbound = unbound(&mut scenario);
    let bears = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(mana(&[ManaCostShard::Green], 1))
        .id();
    let ballista = add_ballista(&mut scenario);
    let mut runner = scenario.build();

    add_mana(&mut runner, ManaType::Green, 1);
    add_mana(&mut runner, ManaType::Colorless, 1);
    let bears_cast = runner.cast(bears).commit();
    assert_eq!(
        stack_entries(bears_cast.state()),
        vec![Entry::Spell(bears)],
        "a permanent spell without {{X}} in its mana cost does not trigger line 1"
    );
    let resolved = bears_cast.resolve();
    assert_eq!(resolved.zone_of(bears), Zone::Battlefield);

    // Reach guard: an {X} permanent spell does trigger line 1.
    add_mana(&mut runner, ManaType::Colorless, 2);
    let ballista_cast = runner.cast(ballista).x(1).commit();
    assert_eq!(
        stack_entries(ballista_cast.state()),
        vec![Entry::Spell(ballista), Entry::Triggered(unbound)]
    );
}

/// U-2 (rulings: the copy has the same X; any {X} instant is copied): Sphinx's
/// Revelation X=2 and its copy each gain 2 life and draw 2 cards.
#[test]
fn copies_an_x_instant_with_the_same_x() {
    let mut scenario = unbound_scenario();
    let _unbound = unbound(&mut scenario);
    let revelation = add_revelation(&mut scenario);
    let mut runner = scenario.build();
    let library_before = library_len(&runner, 0);
    add_revelation_mana(&mut runner, 2);

    let outcome = runner.cast(revelation).x(2).resolve();

    assert_eq!(
        outcome.life_delta(P0),
        4,
        "CR 707.10: the copy has the same X"
    );
    assert_eq!(library_before - outcome.state().players[0].library.len(), 4);
    assert_eq!(spell_copies_of(outcome.events(), revelation), 1);
    assert!(outcome.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// U-3: an instant whose mana cost has no {X} is not copied. Opt puts no
/// Unbound trigger on the stack; Sphinx's Revelation (reach guard) does.
#[test]
fn does_not_copy_an_instant_without_x_in_its_mana_cost() {
    let mut scenario = unbound_scenario();
    let unbound = unbound(&mut scenario);
    let opt = scenario
        .add_spell_to_hand_from_oracle(P0, "Opt", true, OPT)
        .with_mana_cost(mana(&[ManaCostShard::Blue], 0))
        .id();
    let revelation = add_revelation(&mut scenario);
    let mut runner = scenario.build();

    add_mana(&mut runner, ManaType::Blue, 1);
    let opt_cast = runner.cast(opt).commit();
    assert_eq!(
        stack_entries(opt_cast.state()),
        vec![Entry::Spell(opt)],
        "\"that spell's mana cost contains {{X}}\" is false for Opt"
    );
    let resolved = opt_cast.resolve();
    assert_eq!(resolved.zone_of(opt), Zone::Graveyard);

    add_revelation_mana(&mut runner, 1);
    let revelation_cast = runner.cast(revelation).x(1).commit();
    assert_eq!(
        stack_entries(revelation_cast.state()),
        vec![Entry::Spell(revelation), Entry::Triggered(unbound)],
        "reach: an {{X}} instant triggers the copy half"
    );
}

/// U-4 (ruling; CR 707.10c): the copy of Stroke of Genius may take a new
/// target. The original targets P0; the copy is retargeted to P1.
#[test]
fn the_copy_of_an_x_instant_may_take_a_new_target() {
    let mut scenario = unbound_scenario();
    let _unbound = unbound(&mut scenario);
    let stroke = scenario
        .add_spell_to_hand_from_oracle(P0, "Stroke of Genius", true, STROKE_OF_GENIUS)
        .with_mana_cost(mana(&[ManaCostShard::X, ManaCostShard::Blue], 2))
        .id();
    let mut runner = scenario.build();
    let (p0_before, p1_before) = (library_len(&runner, 0), library_len(&runner, 1));
    add_mana(&mut runner, ManaType::Blue, 1);
    add_mana(&mut runner, ManaType::Colorless, 4);

    drop(runner.cast(stroke).x(2).target_player(P0).commit());
    for _ in 0..6 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        act(&mut runner, GameAction::PassPriority);
    }
    let WaitingFor::CopyRetarget {
        player,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the copy offers new targets: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(target_slots.len(), 1);
    assert_eq!(
        target_slots[0].legal_alternatives,
        vec![TargetRef::Player(P1)]
    );
    act(
        &mut runner,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        },
    );
    drain_stack(&mut runner);

    assert_eq!(p0_before - library_len(&runner, 0), 2, "the original");
    assert_eq!(
        p1_before - library_len(&runner, 1),
        2,
        "the retargeted copy"
    );
    assert_settled(&runner);
}

/// U-5 (ruling: a copy is created even if the original was countered; CR 701.6a
/// + CR 608.2h): Cancel counters Sphinx's Revelation before Unbound's trigger
///   resolves; the trigger still copies it.
#[test]
fn copies_an_x_instant_countered_before_the_trigger_resolves() {
    let mut scenario = unbound_scenario();
    let _unbound = unbound(&mut scenario);
    let revelation = add_revelation(&mut scenario);
    let cancel = scenario
        .add_spell_to_hand_from_oracle(P0, "Cancel", true, CANCEL)
        .with_mana_cost(mana(&[ManaCostShard::Blue, ManaCostShard::Blue], 1))
        .id();
    let mut runner = scenario.build();
    let library_before = library_len(&runner, 0);
    let life_before = runner.state().players[0].life;
    add_revelation_mana(&mut runner, 2);
    add_mana(&mut runner, ManaType::Blue, 2);
    add_mana(&mut runner, ManaType::Colorless, 1);

    let mut revelation_cast = runner.cast(revelation).x(2).commit();
    let outcome = revelation_cast
        .cast(cancel)
        .target_objects(&[revelation])
        .resolve();

    assert_eq!(outcome.zone_of(revelation), Zone::Graveyard, "countered");
    assert_eq!(
        outcome.state().players[0].life - life_before,
        2,
        "only the copy resolves"
    );
    assert_eq!(library_before - outcome.state().players[0].library.len(), 2);
    assert_eq!(spell_copies_of(outcome.events(), revelation), 1);
    assert!(outcome.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// U-6 (ruling: X = 0 is still copied): the condition reads the mana symbol in
/// the printed cost, not the announced value.
#[test]
fn copies_an_x_spell_announced_as_zero() {
    let mut scenario = unbound_scenario();
    let _unbound = unbound(&mut scenario);
    let revelation = add_revelation(&mut scenario);
    let mut runner = scenario.build();
    add_revelation_mana(&mut runner, 0);

    let outcome = runner.cast(revelation).x(0).resolve();

    assert_eq!(spell_copies_of(outcome.events(), revelation), 1);
    assert!(outcome.state().stack.is_empty());
}

/// U-7 (rulings: two Unbounds copy twice; a copy is not cast, CR 707.10): each
/// Unbound copies Sphinx's Revelation once, and the copies trigger nothing.
#[test]
fn two_unbounds_copy_an_x_instant_twice() {
    let mut scenario = unbound_scenario();
    let _first = unbound(&mut scenario);
    let _second = unbound(&mut scenario);
    let revelation = add_revelation(&mut scenario);
    let mut runner = scenario.build();
    let library_before = library_len(&runner, 0);
    add_revelation_mana(&mut runner, 2);

    let outcome = runner.cast(revelation).x(2).resolve();

    assert_eq!(outcome.life_delta(P0), 6);
    assert_eq!(library_before - outcome.state().players[0].library.len(), 6);
    assert_eq!(spell_copies_of(outcome.events(), revelation), 2);
    assert!(outcome.state().stack.is_empty());
}

/// U-8 (rulings: the copy has the same X; X without {X} is not copied;
/// CR 107.3k): the `{X}` activation of a source is copied with its X; the same
/// source's `{1}` activation is not.
#[test]
fn copies_an_x_activated_ability_but_not_a_non_x_ability_of_the_same_source() {
    let mut scenario = unbound_scenario();
    let unbound = unbound(&mut scenario);
    let engine = add_x_draw_engine(&mut scenario);
    let mut runner = scenario.build();
    let library_before = library_len(&runner, 0);
    add_mana(&mut runner, ManaType::Colorless, 2);

    activate_with_x(&mut runner, engine, 0, 2);
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Activated(engine, Some(2)), Entry::Triggered(unbound)]
    );
    drain_stack(&mut runner);
    assert_eq!(
        library_before - library_len(&runner, 0),
        4,
        "the {{X}} ability and its copy each draw 2"
    );

    let library_before = library_len(&runner, 0);
    add_mana(&mut runner, ManaType::Colorless, 1);
    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: engine,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    pay_until_priority(&mut runner);
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Activated(engine, None)],
        "\"that ability's activation cost contains {{X}}\" is false for {{1}}"
    );
    drain_stack(&mut runner);
    assert_eq!(library_before - library_len(&runner, 0), 1);
    assert_settled(&runner);
}

/// U-9/U-10: Pernicious Deed's "{X}, Sacrifice this enchantment" moves its source
/// as a cost; the activation's own cost still answers, so Unbound copies it
/// (ruling: X = 0 still counts).
fn deed_activation_is_copied(x: u32) {
    let mut scenario = unbound_scenario();
    let unbound = unbound(&mut scenario);
    let deed = scenario
        .add_enchantment_from_oracle(P0, "Pernicious Deed", PERNICIOUS_DEED)
        .with_mana_cost(mana(&[ManaCostShard::Black, ManaCostShard::Green], 1))
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Colorless, x as usize);

    activate_with_x(&mut runner, deed, 0, x);

    assert_eq!(
        runner.state().objects[&deed].zone,
        Zone::Graveyard,
        "reach: the cost sacrificed Deed"
    );
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Activated(deed, Some(x)), Entry::Triggered(unbound)]
    );
    pass_both(&mut runner);
    assert_eq!(
        activated_entries_of(&runner, deed),
        2,
        "CR 707.10: Unbound copied Deed's ability"
    );
    drain_stack(&mut runner);
}

#[test]
fn copies_an_x_sacrifice_ability_after_its_cost_moved_the_source() {
    deed_activation_is_copied(1);
}

#[test]
fn copies_an_x_sacrifice_ability_announced_as_zero() {
    deed_activation_is_copied(0);
}

/// U-11: Katara's "Waterbend {X}" means "Pay {X}" (CR 701.67a), so the
/// activation has {X} in its activation cost and is copied.
#[test]
fn copies_katara_waterbend_x_activation() {
    let mut scenario = unbound_scenario();
    let unbound = unbound(&mut scenario);
    let katara = scenario
        .add_creature_from_oracle(P0, "Katara, Water Tribe's Hope", 3, 3, KATARA)
        .with_mana_cost(mana(
            &[
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
            ],
            2,
        ))
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Colorless, 2);

    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: katara,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseXValue { min: 1, .. }
        ),
        "X can't be 0: {:?}",
        runner.state().waiting_for
    );
    act(&mut runner, GameAction::ChooseX { value: 2 });
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment {
                convoke_mode: Some(ConvokeMode::Waterbend),
                ..
            }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    act(&mut runner, GameAction::PassPriority);
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Activated(katara, Some(2)), Entry::Triggered(unbound)]
    );

    pass_both(&mut runner);
    assert_eq!(
        activated_entries_of(&runner, katara),
        2,
        "CR 707.10: Unbound copied the waterbend ability"
    );
    drain_stack(&mut runner);
    let katara_object = &runner.state().objects[&katara];
    assert_eq!(
        (katara_object.power, katara_object.toughness),
        (Some(2), Some(2))
    );
}

/// U-12: Wizard's Rockets' "{X}, {T}, Sacrifice this artifact: Add X mana …" is
/// a mana ability with {X} in its activation cost. Its activation triggers the
/// copy half (CR 605.3: it follows CR 602.2), and the copy copies nothing: a
/// mana ability never uses the stack (CR 605.3b).
#[test]
fn unbound_triggers_on_an_x_mana_ability_and_copies_nothing() {
    let mut scenario = unbound_scenario();
    let unbound = unbound(&mut scenario);
    let rockets = scenario
        .add_artifact_from_oracle(P0, "Wizard's Rockets", WIZARDS_ROCKETS)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();
    let library_before = library_len(&runner, 0);

    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: rockets,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    // CR 107.3a + CR 605.3: X in the mana ability's activation cost is announced first.
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::PayAmountChoice {
                resource: PayableResource::ManaGeneric { .. },
                source_id,
                pending_mana_ability: Some(_),
                ..
            } if *source_id == rockets
        ),
        "{:?}",
        runner.state().waiting_for
    );
    act(&mut runner, GameAction::SubmitPayAmount { amount: 0 });
    let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for else {
        panic!(
            "Rockets' dies trigger and Unbound's trigger wait together: {:?}",
            runner.state().waiting_for
        );
    };
    let mut sources: Vec<ObjectId> = triggers.iter().map(|trigger| trigger.source_id).collect();
    sources.sort();
    let mut expected = vec![unbound, rockets];
    expected.sort();
    assert_eq!(sources, expected);
    assert_eq!(runner.state().objects[&rockets].zone, Zone::Graveyard);

    act(&mut runner, GameAction::OrderTriggers { order: vec![0, 1] });
    assert_eq!(trigger_count(runner.state(), &[unbound]), 1);

    for _ in 0..32 {
        assert_eq!(
            activated_entries_of(&runner, rockets),
            0,
            "CR 605.3b: there is no Rockets ability on the stack to copy"
        );
        if runner.state().stack.is_empty() {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "{:?}",
            runner.state().waiting_for
        );
        act(&mut runner, GameAction::PassPriority);
    }
    assert_eq!(
        library_before - library_len(&runner, 0),
        1,
        "Rockets' dies trigger draws a card"
    );
    assert_settled(&runner);
}

/// Lazav, the Multifarious becomes a copy of the creature card `copied` puts
/// in the graveyard (mana value `x`), keeping its own `{X}` ability second.
/// Unbound Flourishing is then cast, so Lazav's own `{X}` activation is not
/// copied; an `X Draw Engine` waits for the reach guard.
fn lazav_copying(
    copied: impl FnOnce(&mut GameScenario) -> ObjectId,
    x: u32,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = unbound_scenario();
    let lazav = scenario
        .add_creature_from_oracle(P0, "Lazav, the Multifarious", 1, 3, LAZAV)
        .with_mana_cost(mana(&[ManaCostShard::Blue, ManaCostShard::Black], 0))
        .id();
    let copied = copied(&mut scenario);
    let unbound = scenario
        .add_spell_to_hand_from_oracle(P0, "Unbound Flourishing", false, UNBOUND)
        .as_enchantment()
        .with_mana_cost(mana(&[ManaCostShard::Green], 2))
        .id();
    let engine = add_x_draw_engine(&mut scenario);
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Colorless, x as usize);
    runner
        .activate(lazav, 0)
        .x(x)
        .target_object(copied)
        .resolve();

    add_mana(&mut runner, ManaType::Green, 1);
    add_mana(&mut runner, ManaType::Colorless, 2);
    runner.cast(unbound).resolve();
    assert_eq!(runner.state().objects[&unbound].zone, Zone::Battlefield);
    assert_settled(&runner);
    (runner, lazav, unbound, engine)
}

fn lazav_costs(runner: &GameRunner, lazav: ObjectId) -> Vec<Option<AbilityCost>> {
    runner.state().objects[&lazav]
        .abilities
        .iter()
        .map(|ability| ability.cost.clone())
        .collect()
}

fn is_x_mana(cost: &Option<AbilityCost>) -> bool {
    matches!(
        cost,
        Some(AbilityCost::Mana { cost: ManaCost::Cost { shards, .. } })
            if shards.contains(&ManaCostShard::X)
    )
}

/// Reach guard shared by U-13/U-14: an `{X}` activation in the same game puts
/// exactly one Unbound trigger above it.
fn assert_x_activation_still_triggers(
    runner: &mut GameRunner,
    engine: ObjectId,
    unbound: ObjectId,
) {
    add_mana(runner, ManaType::Colorless, 1);
    activate_with_x(runner, engine, 0, 1);
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Activated(engine, Some(1)), Entry::Triggered(unbound)],
        "reach: the copy half watches activations in this game"
    );
    drain_stack(runner);
}

/// U-13: Lazav as a copy of Bottle Gnomes sacrifices itself. "Sacrifice this
/// creature" has no {X}; after the move the activated index names Lazav's own
/// `{X}` ability, which must not answer for it.
#[test]
fn a_copied_sacrifice_ability_whose_move_reveals_an_x_ability_is_not_copied() {
    let (mut runner, lazav, unbound, engine) = lazav_copying(
        |scenario| {
            scenario
                .add_creature_to_graveyard(P0, "Bottle Gnomes", 1, 3)
                .with_mana_cost(ManaCost::generic(3))
                .from_oracle_text(BOTTLE_GNOMES)
                .id()
        },
        3,
    );
    let costs = lazav_costs(&runner, lazav);
    assert!(
        matches!(
            costs.first(),
            Some(Some(AbilityCost::Sacrifice(sacrifice)))
                if sacrifice.target == TargetFilter::SelfRef
        ) && costs.get(1).is_some_and(is_x_mana),
        "fixture: Lazav is Bottle Gnomes with its own {{X}} ability second: {costs:?}"
    );
    let life_before = runner.state().players[0].life;

    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: lazav,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    pay_until_priority(&mut runner);

    assert_eq!(
        runner.state().objects[&lazav].zone,
        Zone::Graveyard,
        "reach: the cost moved Lazav"
    );
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Activated(lazav, None)],
        "\"Sacrifice this creature\" has no {{X}} in its activation cost"
    );
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].life - life_before, 3);

    assert_x_activation_still_triggers(&mut runner, engine, unbound);
}

/// U-14 (ruling: a cost that includes X but not the mana symbol {X} is not
/// copied): Lazav as a copy of Krav, the Unredeemed, sacrifices itself as
/// X = 1 for "{B}, Sacrifice X creatures".
#[test]
fn an_ability_announcing_x_without_the_x_symbol_is_not_copied() {
    let (mut runner, lazav, unbound, engine) = lazav_copying(
        |scenario| {
            scenario
                .add_creature_to_graveyard(P0, "Krav, the Unredeemed", 3, 3)
                .with_mana_cost(mana(&[ManaCostShard::Black], 4))
                .from_oracle_text(KRAV_ACTIVATED)
                .id()
        },
        5,
    );
    let costs = lazav_costs(&runner, lazav);
    assert!(
        matches!(
            costs.first(),
            Some(Some(AbilityCost::Composite { costs: krav }))
                if krav.iter().any(|cost| matches!(
                    cost,
                    AbilityCost::Sacrifice(sacrifice)
                        if sacrifice.requirement == SacrificeRequirement::Count { count: u32::MAX }
                ))
        ) && costs.get(1).is_some_and(is_x_mana),
        "fixture: Lazav is Krav with its own {{X}} ability second: {costs:?}"
    );
    let life_before = runner.state().players[0].life;
    add_mana(&mut runner, ManaType::Black, 1);

    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: lazav,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    act(
        &mut runner,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P0)],
        },
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    act(&mut runner, GameAction::SelectCards { cards: vec![lazav] });
    pay_until_priority(&mut runner);

    assert_eq!(
        runner.state().objects[&lazav].zone,
        Zone::Graveyard,
        "reach: Lazav sacrificed itself as X = 1"
    );
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Activated(lazav, Some(1))],
        "X was announced, but the activation cost has no mana symbol {{X}}"
    );
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].life - life_before, 1);

    assert_x_activation_still_triggers(&mut runner, engine, unbound);
}

/// U-15 (rulings: two Unbounds copy twice; a copied ability is not activated,
/// CR 707.10): an `{X}` activation is copied once per Unbound.
#[test]
fn two_unbounds_copy_an_x_activated_ability_twice() {
    let mut scenario = unbound_scenario();
    let first = unbound(&mut scenario);
    let second = unbound(&mut scenario);
    let engine = add_x_draw_engine(&mut scenario);
    let mut runner = scenario.build();
    let library_before = library_len(&runner, 0);
    add_mana(&mut runner, ManaType::Colorless, 1);

    activate_with_x(&mut runner, engine, 0, 1);
    assert_eq!(trigger_count(runner.state(), &[first, second]), 2);
    assert_eq!(activated_entries_of(&runner, engine), 1);
    drain_stack(&mut runner);

    assert_eq!(
        library_before - library_len(&runner, 0),
        3,
        "the original and two copies each draw 1"
    );
    assert_settled(&runner);
}

// ---------------------------------------------------------------------------
// Line 1: "double the value of X" (CR 107.3e; CR 107.3c's note: the value of X
// may change while the spell is on the stack).
// ---------------------------------------------------------------------------

/// Walking Ballista cast with the given X from 8 colorless mana, with or
/// without Unbound; returns the runner at the post-cast `Priority`.
fn ballista_cast(with_unbound: bool, x: u32) -> (GameRunner, Option<ObjectId>, ObjectId) {
    let mut scenario = unbound_scenario();
    let unbound = with_unbound.then(|| unbound(&mut scenario));
    let ballista = add_ballista(&mut scenario);
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Colorless, 8);
    cast_with_x_until_prompt(&mut runner, ballista, x, &[]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    (runner, unbound, ballista)
}

/// U-16 (rulings: Unbound raises X for permanent spells, but you don't pay any
/// more mana; CR 107.3m + CR 601.2h): Walking Ballista X=2 enters with 4
/// counters, and only the 4 mana of X=2 is spent.
#[test]
fn unbound_doubles_walking_ballistas_x_without_more_mana() {
    let (mut runner, unbound, ballista) = ballista_cast(true, 2);
    let unbound = unbound.unwrap();
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Spell(ballista), Entry::Triggered(unbound)]
    );

    pass_both(&mut runner);
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Spell(ballista)],
        "Unbound's trigger resolved above Ballista"
    );
    let state = runner.state();
    assert_eq!(state.objects[&ballista].cost_x_paid, Some(4));
    let paid = &state.stack_paid_facts[&ballista];
    assert_eq!(
        (paid.x_value, paid.actual_mana_spent),
        (Some(4), 4),
        "the paid snapshot carries the doubled X and the mana actually spent"
    );
    assert_eq!(pool_total(&runner, P0), 4);

    drain_stack(&mut runner);
    assert_settled(&runner);
    assert_eq!(runner.state().objects[&ballista].zone, Zone::Battlefield);
    assert_eq!(counters(&runner, ballista, CounterType::Plus1Plus1), 4);
    assert_eq!(pool_total(&runner, P0), 4, "doubling X costs no more mana");
}

/// U-16 control: without Unbound, Walking Ballista X=2 enters with 2 counters.
#[test]
fn walking_ballista_without_unbound_enters_with_x_counters() {
    let (mut runner, _, ballista) = ballista_cast(false, 2);
    assert_eq!(stack_entries(runner.state()), vec![Entry::Spell(ballista)]);
    drain_stack(&mut runner);
    assert_settled(&runner);
    assert_eq!(counters(&runner, ballista, CounterType::Plus1Plus1), 2);
    assert_eq!(pool_total(&runner, P0), 4);
}

/// U-17 (ruling: two Unbounds multiply X by four): each doubling reads the X
/// as it is when it resolves. The two identical doublings commute, so they go
/// on the stack without an order prompt (CR 603.3b).
#[test]
fn two_unbounds_quadruple_walking_ballistas_x() {
    let mut scenario = unbound_scenario();
    let first = unbound(&mut scenario);
    let second = unbound(&mut scenario);
    let ballista = add_ballista(&mut scenario);
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Colorless, 8);

    cast_with_x_until_prompt(&mut runner, ballista, 2, &[]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "identical doublings need no order choice: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        stack_entries(runner.state()),
        vec![
            Entry::Spell(ballista),
            Entry::Triggered(first),
            Entry::Triggered(second)
        ]
    );

    let events = drain_stack_events(&mut runner);
    assert_settled(&runner);
    assert_eq!(modify_x_events(&events, first), 1);
    assert_eq!(modify_x_events(&events, second), 1);
    assert_eq!(counters(&runner, ballista, CounterType::Plus1Plus1), 8);
    assert_eq!(pool_total(&runner, P0), 4);
}

/// U-18 (ruling: X = 0 doubles to 0 for permanent spells): Walking Ballista
/// X=0 stays at X=0 and dies as a 0/0 (CR 704.5f).
#[test]
fn unbound_doubles_x_of_zero_to_zero() {
    let (mut runner, unbound, ballista) = ballista_cast(true, 0);
    let unbound = unbound.unwrap();
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Spell(ballista), Entry::Triggered(unbound)]
    );

    let events = pass_both_events(&mut runner);
    assert_eq!(
        modify_x_events(&events, unbound),
        1,
        "the doubling resolved"
    );
    assert_eq!(runner.state().objects[&ballista].cost_x_paid, Some(0));
    assert_eq!(runner.state().stack_paid_facts[&ballista].x_value, Some(0));

    drain_stack(&mut runner);
    assert_settled(&runner);
    assert_eq!(runner.state().objects[&ballista].zone, Zone::Graveyard);
    assert_eq!(counters(&runner, ballista, CounterType::Plus1Plus1), 0);
}

/// Hydroid Krasis ({X}{G}{U}) cast with X=4 under Unbound: the cast trigger and
/// the doubling wait for P0's order choice.
fn krasis_cast_with_unbound() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = unbound_scenario();
    let unbound = unbound(&mut scenario);
    let krasis = scenario
        .add_creature_to_hand_from_oracle(P0, "Hydroid Krasis", 0, 0, KRASIS)
        .with_mana_cost(mana(
            &[ManaCostShard::X, ManaCostShard::Green, ManaCostShard::Blue],
            0,
        ))
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Green, 1);
    add_mana(&mut runner, ManaType::Blue, 1);
    add_mana(&mut runner, ManaType::Colorless, 8);
    cast_with_x_until_prompt(&mut runner, krasis, 4, &[]);
    (runner, unbound, krasis)
}

/// U-19a (ruling: you choose whether to double X before or after a cast
/// trigger that reads X; CR 603.3b + CR 202.3e): with the doubling on top,
/// Krasis's cast trigger sees X=8.
#[test]
fn unbound_doubling_before_krasis_cast_trigger_feeds_it() {
    let (mut runner, unbound, krasis) = krasis_cast_with_unbound();
    let life_before = life(&runner, P0);
    let library_before = library_len(&runner, 0);
    order_with_on_top(&mut runner, krasis, unbound);

    pass_both(&mut runner);
    let state = runner.state();
    assert_eq!(state.objects[&krasis].zone, Zone::Stack);
    assert_eq!(state.objects[&krasis].cost_x_paid, Some(8));
    assert_eq!(
        state.objects[&krasis].spell_mana_value(),
        10,
        "CR 202.3e: X counts as its doubled value on the stack"
    );

    drain_stack(&mut runner);
    assert_settled(&runner);
    assert_eq!(life(&runner, P0) - life_before, 4);
    assert_eq!(library_before - library_len(&runner, 0), 4);
    assert_eq!(counters(&runner, krasis, CounterType::Plus1Plus1), 8);
    assert_eq!(pool_total(&runner, P0), 4);
}

/// U-19b: with Krasis's cast trigger on top, it resolves before the doubling
/// and sees X=4; Krasis still enters with the doubled 8 counters.
#[test]
fn krasis_cast_trigger_before_unbound_reads_announced_x() {
    let (mut runner, unbound, krasis) = krasis_cast_with_unbound();
    let life_before = life(&runner, P0);
    let library_before = library_len(&runner, 0);
    order_with_on_top(&mut runner, unbound, krasis);

    drain_stack(&mut runner);
    assert_settled(&runner);
    assert_eq!(life(&runner, P0) - life_before, 2);
    assert_eq!(library_before - library_len(&runner, 0), 2);
    assert_eq!(counters(&runner, krasis, CounterType::Plus1Plus1), 8);
}

/// Nissa, Steward of Elements (loyalty X) cast with X=2; returns its loyalty
/// counters once the stack has drained.
fn nissa_loyalty(with_unbound: bool) -> u32 {
    let mut scenario = unbound_scenario();
    if with_unbound {
        unbound(&mut scenario);
    }
    let nissa = scenario
        .add_spell_to_hand_from_oracle(P0, "Nissa, Steward of Elements", false, NISSA)
        .as_planeswalker_with_loyalty("Nissa", 0)
        .with_mana_cost(mana(
            &[ManaCostShard::X, ManaCostShard::Green, ManaCostShard::Blue],
            0,
        ))
        .id();
    let mut runner = scenario.build();
    // The printed loyalty "X" (MTGJSON `loyalty: "X"`), which the builder lacks.
    runner
        .state_mut()
        .objects
        .get_mut(&nissa)
        .unwrap()
        .printed_loyalty = Some(PrintedLoyalty::X);
    add_mana(&mut runner, ManaType::Green, 1);
    add_mana(&mut runner, ManaType::Blue, 1);
    add_mana(&mut runner, ManaType::Colorless, 2);

    cast_with_x_until_prompt(&mut runner, nissa, 2, &[]);
    drain_stack(&mut runner);
    assert_settled(&runner);
    assert_eq!(runner.state().objects[&nissa].zone, Zone::Battlefield);
    counters(&runner, nissa, CounterType::Loyalty)
}

/// U-20 (CR 306.5b + CR 107.3m): a planeswalker whose printed loyalty is X
/// enters with the doubled X, read from the paid snapshot at resolution.
#[test]
fn unbound_doubles_x_loyalty_planeswalker() {
    assert_eq!(nissa_loyalty(true), 4);
}

/// U-20 control: without Unbound, Nissa X=2 enters with loyalty 2.
#[test]
fn x_loyalty_planeswalker_without_unbound_enters_with_x_loyalty() {
    assert_eq!(nissa_loyalty(false), 2);
}

/// U-21 (CR 107.3e + CR 400.7 + CR 107.3g; CR 701.6a): each doubling binds the
/// spell its own cast event names. Walking Ballista X=1 and then Fated
/// Firepower X=1 (flash) each put an Unbound trigger above themselves; Cancel
/// counters Fated Firepower. Its doubling finds its spell gone and changes
/// nothing, and Ballista's doubling doubles Ballista once.
#[test]
fn unbound_doubles_only_the_spell_its_own_trigger_names() {
    let mut scenario = unbound_scenario();
    let unbound = unbound(&mut scenario);
    let ballista = add_ballista(&mut scenario);
    let firepower = scenario
        .add_spell_to_hand_from_oracle(P0, "Fated Firepower", true, FATED_FIREPOWER)
        .as_enchantment()
        .with_mana_cost(mana(
            &[
                ManaCostShard::X,
                ManaCostShard::Red,
                ManaCostShard::Red,
                ManaCostShard::Red,
            ],
            0,
        ))
        .id();
    let cancel = scenario
        .add_spell_to_hand_from_oracle(P0, "Cancel", true, CANCEL)
        .with_mana_cost(mana(&[ManaCostShard::Blue, ManaCostShard::Blue], 1))
        .id();
    let mut runner = scenario.build();

    add_mana(&mut runner, ManaType::Colorless, 2);
    cast_with_x_until_prompt(&mut runner, ballista, 1, &[]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Spell(ballista), Entry::Triggered(unbound)]
    );

    add_mana(&mut runner, ManaType::Red, 3);
    add_mana(&mut runner, ManaType::Colorless, 1);
    cast_with_x_until_prompt(&mut runner, firepower, 1, &[]);
    assert_eq!(
        stack_entries(runner.state()),
        vec![
            Entry::Spell(ballista),
            Entry::Triggered(unbound),
            Entry::Spell(firepower),
            Entry::Triggered(unbound)
        ]
    );

    add_mana(&mut runner, ManaType::Blue, 2);
    add_mana(&mut runner, ManaType::Colorless, 1);
    cast_with_x_until_prompt(&mut runner, cancel, 0, &[firepower]);
    assert_eq!(
        stack_entries(runner.state()),
        vec![
            Entry::Spell(ballista),
            Entry::Triggered(unbound),
            Entry::Spell(firepower),
            Entry::Triggered(unbound),
            Entry::Spell(cancel)
        ]
    );

    let events = drain_stack_events(&mut runner);
    assert_settled(&runner);
    assert_eq!(
        modify_x_events(&events, unbound),
        2,
        "reach: both doublings resolved, the second after Fated Firepower left the stack"
    );
    assert_eq!(runner.state().objects[&firepower].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&cancel].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&ballista].zone, Zone::Battlefield);
    assert_eq!(
        counters(&runner, ballista, CounterType::Plus1Plus1),
        2,
        "Ballista is doubled once, by its own trigger"
    );
    // Internal channel: the countered spell's stored X was not rewritten.
    assert_eq!(runner.state().objects[&firepower].cost_x_paid, Some(1));
}

/// Teval, Arbiter of Virtue on P0's battlefield, Endless One ({X}) in P0's
/// hand with 2 colorless mana, Cancel in P1's hand, and Unbound if asked.
struct TevalBoard {
    runner: GameRunner,
    unbound: Option<ObjectId>,
    teval: ObjectId,
    endless_one: ObjectId,
    cancel: ObjectId,
}

fn teval_scenario(with_unbound: bool) -> TevalBoard {
    let mut scenario = unbound_scenario();
    let unbound = with_unbound.then(|| unbound(&mut scenario));
    let teval = scenario
        .add_creature_from_oracle(P0, "Teval, Arbiter of Virtue", 6, 6, TEVAL)
        .with_mana_cost(mana(
            &[
                ManaCostShard::Black,
                ManaCostShard::Green,
                ManaCostShard::Blue,
            ],
            2,
        ))
        .id();
    let endless_one = scenario
        .add_creature_to_hand_from_oracle(P0, "Endless One", 0, 0, ENDLESS_ONE)
        .with_mana_cost(mana(&[ManaCostShard::X], 0))
        .id();
    let cancel = scenario
        .add_spell_to_hand_from_oracle(P1, "Cancel", true, CANCEL)
        .with_mana_cost(mana(&[ManaCostShard::Blue, ManaCostShard::Blue], 1))
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Colorless, 2);
    TevalBoard {
        runner,
        unbound,
        teval,
        endless_one,
        cancel,
    }
}

/// U-22 (CR 608.2h + CR 202.3e; CR 603.3b): Teval's "you lose life equal to
/// its mana value" reads Endless One as it is on the stack when the trigger
/// resolves, so with the doubling on top P0 loses the doubled 4.
#[test]
fn teval_ordered_after_unbound_loses_the_doubled_mana_value() {
    let TevalBoard {
        mut runner,
        unbound,
        teval,
        endless_one,
        ..
    } = teval_scenario(true);
    let life_before = life(&runner, P0);
    cast_with_x_until_prompt(&mut runner, endless_one, 2, &[]);
    order_with_on_top(&mut runner, teval, unbound.unwrap());

    drain_stack(&mut runner);
    assert_settled(&runner);
    assert_eq!(life_before - life(&runner, P0), 4);
    assert_eq!(counters(&runner, endless_one, CounterType::Plus1Plus1), 4);
}

/// U-22b: with Teval's trigger on top, it resolves while X is still 2.
#[test]
fn teval_ordered_before_unbound_loses_the_announced_mana_value() {
    let TevalBoard {
        mut runner,
        unbound,
        teval,
        endless_one,
        ..
    } = teval_scenario(true);
    let life_before = life(&runner, P0);
    cast_with_x_until_prompt(&mut runner, endless_one, 2, &[]);
    order_with_on_top(&mut runner, unbound.unwrap(), teval);

    drain_stack(&mut runner);
    assert_settled(&runner);
    assert_eq!(life_before - life(&runner, P0), 2);
    assert_eq!(counters(&runner, endless_one, CounterType::Plus1Plus1), 4);
}

/// U-22 control: without Unbound there is no order choice, and P0 loses 2.
#[test]
fn teval_without_unbound_loses_the_announced_mana_value() {
    let TevalBoard {
        mut runner,
        teval,
        endless_one,
        ..
    } = teval_scenario(false);
    let life_before = life(&runner, P0);
    cast_with_x_until_prompt(&mut runner, endless_one, 2, &[]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Spell(endless_one), Entry::Triggered(teval)]
    );

    drain_stack(&mut runner);
    assert_settled(&runner);
    assert_eq!(life_before - life(&runner, P0), 2);
    assert_eq!(counters(&runner, endless_one, CounterType::Plus1Plus1), 2);
}

/// From P0's priority with stack `[Endless One, Teval's trigger]`: P0 passes,
/// P1 counters Endless One with Cancel, and Cancel resolves (CR 701.6a),
/// leaving Teval's trigger alone on the stack.
fn p1_cancels_endless_one(
    runner: &mut GameRunner,
    teval: ObjectId,
    endless_one: ObjectId,
    cancel: ObjectId,
) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Spell(endless_one), Entry::Triggered(teval)]
    );
    act(runner, GameAction::PassPriority);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    add_player_mana(runner, P1, ManaType::Blue, 2);
    add_player_mana(runner, P1, ManaType::Colorless, 1);
    // Endless One is Cancel's only legal target (Teval's trigger is not a
    // spell), so the cast selects it without a target prompt
    // (`auto_select_targets`); the stack entry below pins that target.
    cast_with_x_until_prompt(runner, cancel, 0, &[]);
    assert_eq!(
        stack_entries(runner.state()),
        vec![
            Entry::Spell(endless_one),
            Entry::Triggered(teval),
            Entry::Spell(cancel)
        ]
    );
    let cancel_targets = runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::Spell {
                ability: Some(ability),
                ..
            } if entry.source_id == cancel => Some(ability.targets.clone()),
            _ => None,
        })
        .expect("Cancel is on the stack with its resolved ability");
    assert_eq!(cancel_targets, vec![TargetRef::Object(endless_one)]);
    pass_both(runner);
    assert_eq!(runner.state().objects[&endless_one].zone, Zone::Graveyard);
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Triggered(teval)],
        "reach: Teval's trigger resolves after Endless One left the stack"
    );
}

/// U-23 (CR 608.2h + CR 202.3e; CR 701.6a): Endless One X=2 is doubled to
/// X=4, then countered before Teval's trigger resolves. "Its mana value" is
/// then last-known information: Endless One as it last existed on the stack,
/// with X=4, so P0 loses 4.
#[test]
fn teval_reads_the_doubled_mana_value_of_a_spell_countered_after_the_doubling() {
    let TevalBoard {
        mut runner,
        unbound,
        teval,
        endless_one,
        cancel,
    } = teval_scenario(true);
    let life_before = life(&runner, P0);
    cast_with_x_until_prompt(&mut runner, endless_one, 2, &[]);
    order_with_on_top(&mut runner, teval, unbound.unwrap());
    pass_both(&mut runner);
    assert_eq!(runner.state().objects[&endless_one].cost_x_paid, Some(4));

    p1_cancels_endless_one(&mut runner, teval, endless_one, cancel);
    assert_eq!(
        life(&runner, P0),
        life_before,
        "Teval's trigger has not resolved"
    );

    drain_stack(&mut runner);
    assert_settled(&runner);
    assert_eq!(life_before - life(&runner, P0), 4);
}

/// U-23 control: without Unbound, the countered Endless One last existed on
/// the stack with X=2, so P0 loses 2.
#[test]
fn teval_reads_the_announced_mana_value_of_a_spell_countered_without_unbound() {
    let TevalBoard {
        mut runner,
        teval,
        endless_one,
        cancel,
        ..
    } = teval_scenario(false);
    let life_before = life(&runner, P0);
    cast_with_x_until_prompt(&mut runner, endless_one, 2, &[]);

    p1_cancels_endless_one(&mut runner, teval, endless_one, cancel);
    assert_eq!(
        life(&runner, P0),
        life_before,
        "Teval's trigger has not resolved"
    );

    drain_stack(&mut runner);
    assert_settled(&runner);
    assert_eq!(life_before - life(&runner, P0), 2);
}

// ---------------------------------------------------------------------------
// Line 2 after a counter: "copy that spell or ability" names the activated
// ability as it last existed on the stack (CR 113.7a + CR 608.2h).
// ---------------------------------------------------------------------------

/// Stifle, at no cost, in P0's hand.
fn add_stifle(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Stifle", true, STIFLE)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// The topmost activated-ability entry of `source` on the stack whose
/// announced X is `x`.
fn activated_entry(runner: &GameRunner, source: ObjectId, x: Option<u32>) -> ObjectId {
    runner
        .state()
        .stack
        .iter()
        .rev()
        .find(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::ActivatedAbility { source_id, ability }
                    if *source_id == source && ability.chosen_x == x
            )
        })
        .map(|entry| entry.id)
        .expect("reach: the activation is on the stack")
}

/// U-24 (rulings: a copy is created even if the original was countered; the
/// copy has the same X; CR 701.6a + CR 113.7a + CR 608.2h + CR 707.10): Stifle
/// counters Helix Pinnacle's `{X}` activation (X = 2) before Unbound's trigger
/// resolves; the trigger still copies it, and only the copy puts counters.
#[test]
fn copies_an_x_activated_ability_countered_before_the_trigger_resolves() {
    let mut scenario = unbound_scenario();
    let unbound = unbound(&mut scenario);
    let pinnacle = scenario
        .add_enchantment_from_oracle(P0, "Helix Pinnacle", HELIX_PINNACLE)
        .from_oracle_text_with_keywords(&["Shroud"], HELIX_PINNACLE)
        .with_mana_cost(mana(&[ManaCostShard::Green], 0))
        .id();
    let stifle = add_stifle(&mut scenario);
    let mut runner = scenario.build();
    let pinnacle_x = runner.state().objects[&pinnacle]
        .abilities
        .iter()
        .position(|ability| is_x_mana(&ability.cost))
        .expect("reach: Helix Pinnacle has its {X} ability");
    let towers = || CounterType::Generic("tower".to_string());
    add_mana(&mut runner, ManaType::Colorless, 2);

    activate_with_x(&mut runner, pinnacle, pinnacle_x, 2);
    assert_eq!(
        stack_entries(runner.state()),
        vec![
            Entry::Activated(pinnacle, Some(2)),
            Entry::Triggered(unbound)
        ]
    );
    let activation = activated_entry(&runner, pinnacle, Some(2));

    drop(runner.cast(stifle).target_objects(&[activation]).commit());
    pass_both(&mut runner);
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Triggered(unbound)],
        "reach: Stifle countered the activation; Unbound's trigger still waits"
    );

    pass_both(&mut runner);
    assert_eq!(
        stack_entries(runner.state()),
        vec![Entry::Activated(pinnacle, Some(2))],
        "CR 707.10: the countered activation is copied with its X"
    );
    assert!(
        runner
            .state()
            .departed_activated_abilities
            .contains_key(&activation),
        "the countered activation is recorded while its step lasts"
    );

    drain_stack(&mut runner);
    assert_settled(&runner);
    assert_eq!(
        counters(&runner, pinnacle, towers()),
        2,
        "only the copy resolves"
    );

    // CR 500.2: the step ends on an empty stack, and its departed records end
    // with it.
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    pass_both(&mut runner);
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert!(
        runner.state().departed_activated_abilities.is_empty(),
        "the departed record is cleared when the step ends"
    );
}

/// U-25 (CR 602.2a + CR 707.10b; hostile): X Draw Engine's first `{X}`
/// activation (X = 1) is copied while it waits; a second activation (X = 2) is
/// then Stifled. Its own trigger copies the countered second activation
/// (X = 2), never the earlier activation or its copy, both live below with the
/// same source.
#[test]
fn a_countered_x_activation_is_copied_not_an_earlier_one_or_its_copy() {
    let mut scenario = unbound_scenario();
    let unbound = unbound(&mut scenario);
    let engine = add_x_draw_engine(&mut scenario);
    let stifle = add_stifle(&mut scenario);
    let mut runner = scenario.build();
    let library_before = library_len(&runner, 0);
    add_mana(&mut runner, ManaType::Colorless, 3);

    activate_with_x(&mut runner, engine, 0, 1);
    pass_both(&mut runner);
    assert_eq!(
        stack_entries(runner.state()),
        vec![
            Entry::Activated(engine, Some(1)),
            Entry::Activated(engine, Some(1))
        ],
        "reach: the first activation and its copy wait"
    );

    activate_with_x(&mut runner, engine, 0, 2);
    assert_eq!(
        stack_entries(runner.state()),
        vec![
            Entry::Activated(engine, Some(1)),
            Entry::Activated(engine, Some(1)),
            Entry::Activated(engine, Some(2)),
            Entry::Triggered(unbound)
        ]
    );
    let second = activated_entry(&runner, engine, Some(2));

    drop(runner.cast(stifle).target_objects(&[second]).commit());
    pass_both(&mut runner);
    assert!(
        runner.state().stack.iter().all(|entry| entry.id != second),
        "reach: Stifle countered the second activation"
    );
    assert_eq!(
        stack_entries(runner.state()),
        vec![
            Entry::Activated(engine, Some(1)),
            Entry::Activated(engine, Some(1)),
            Entry::Triggered(unbound)
        ]
    );

    pass_both(&mut runner);
    assert_eq!(
        stack_entries(runner.state()),
        vec![
            Entry::Activated(engine, Some(1)),
            Entry::Activated(engine, Some(1)),
            Entry::Activated(engine, Some(2))
        ],
        "the trigger copies the countered second activation (X = 2), not the first or its copy"
    );

    drain_stack(&mut runner);
    assert_settled(&runner);
    assert_eq!(
        library_before - library_len(&runner, 0),
        4,
        "the first activation and its copy draw 1 each; the copy of the second draws 2"
    );
}
