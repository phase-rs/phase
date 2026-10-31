//! CR 614.1c + CR 603.6d + CR 706.4: "As this creature enters, roll <dice>. It
//! enters with a number of +1/+1 counters on it equal to the result / the total
//! of those results." (Neverwinter Hydra, Hydradoodle, Hammer Jammer, Jumbo Imp,
//! Temp of the Damned), and the entry-timing rule the class needs: an as-enters
//! continuation is part of its entrant's entry, so the entry is observed and
//! collected for triggers once, after the continuation completes, by whichever
//! collector owns it (CR 603.2 + CR 603.2c + CR 603.6a + CR 614.12a).
//!
//! The roll is made before the permanent is on the battlefield (CR 614.1c,
//! CR 614.12), so its abilities do not function yet (CR 113.6) and it is not among
//! the objects checked after that event (CR 603.10). Hammer Jammer therefore
//! enters with counters equal to its roll and does not trigger on it. A later roll
//! by its controller triggers it (CR 603.2), once per surviving die (CR 603.2c).
//!
//! Oracle text is verbatim (Scryfall / card-data). Barbarian Class and First Day
//! of Class carry only the line under test (the Class level-1 line; First Day of
//! Class's first sentence, without its Learn rider). Beck carries Beck's half.
//! Monoxa, Pixie Guide, Tayam, Rhythm of the Wild, Stolen Identity, Fathom Mage and
//! Storm the Festival carry only the lines under test.
//! Dice-dependent rows choose their RNG seed by search and assert against the
//! emitted `DieRolled` results, never a hard-coded total.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, ChoiceType, Effect, PostReplacementContinuation, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, DrainStatus, GameState, ParkedEntryState, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolution::{FrameKind, ResolutionFrame};
use engine::types::zones::Zone;

const NEVERWINTER_HYDRA: &str = "As this creature enters, roll X d6. It enters with a number of +1/+1 counters on it equal to the total of those results.\nTrample\nWard {4} (Whenever this creature becomes the target of a spell or ability an opponent controls, counter it unless that player pays {4}.)";
const HYDRADOODLE: &str = "As this creature enters, roll X six-sided dice. This creature enters with a number of +1/+1 counters on it equal to the total of those results.\nReach, trample";
const HAMMER_JAMMER: &str = "As this creature enters, roll a six-sided die. This creature enters with a number of +1/+1 counters on it equal to the result.\nWhenever you roll a die, remove all +1/+1 counters from this creature, then put a number of +1/+1 counters on it equal to the result.";
const JUMBO_IMP: &str = "Flying\nAs this creature enters, roll a six-sided die. This creature enters with a number of +1/+1 counters on it equal to the result.\nAt the beginning of your upkeep, roll a six-sided die and put a number of +1/+1 counters on this creature equal to the result.\nAt the beginning of your end step, roll a six-sided die and remove a number of +1/+1 counters from this creature equal to the result.";

const BARBARIAN_CLASS_L1: &str = "If you would roll one or more dice, instead roll that many dice plus one and ignore the lowest roll.";
const KIORA: &str = "Whenever a creature you control with power 4 or greater enters, draw a card.\n[−1]: Untap target permanent.";
const ELEMENTAL_BOND: &str =
    "Whenever a creature you control with power 3 or greater enters, draw a card.";
const SOUL_WARDEN: &str = "Whenever another creature enters, you gain 1 life.";
const BLOOD_ARTIST: &str = "Whenever this creature or another creature dies, target player loses 1 life and you gain 1 life.";
const THUNDER_THRASH_ELDER: &str = "Devour 3 (As this creature enters, you may sacrifice any number of creatures. It enters with three times that many +1/+1 counters on it.)";
const HARDENED_SCALES: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
const BRANCHING_EVOLUTION: &str = "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.";
const ZOMBIFY: &str = "Return target creature card from your graveyard to the battlefield.";
const RISE_OF_THE_DARK_REALMS: &str =
    "Put all creature cards from all graveyards onto the battlefield under your control.";
const WELCOMING_VAMPIRE: &str = "Flying\nWhenever one or more other creatures you control with power 2 or less enter, draw a card. This ability triggers only once each turn.";
const THROUGH_THE_BREACH: &str = "You may put a creature card from your hand onto the battlefield. That creature gains haste. Sacrifice that creature at the beginning of the next end step.";
const BRAZEN_DWARF: &str =
    "Whenever you roll one or more dice, this creature deals 1 damage to each opponent.";
const FIRST_DAY_OF_CLASS: &str = "Whenever a creature you control enters this turn, put a +1/+1 counter on it and it gains haste until end of turn.";
const BECK: &str = "Whenever a creature enters this turn, you may draw a card.";
const ELESH_NORN: &str = "Vigilance\nOther creatures you control get +2/+2.\nCreatures your opponents control get -2/-2.";
const AJANIS_WELCOME: &str = "Whenever a creature you control enters, you gain 1 life.";
const GLUTTONOUS_SLIME: &str = "Flash\nDevour 1 (As this creature enters, you may sacrifice any number of creatures. It enters with that many +1/+1 counters on it.)";
const NIGHT_OF_SOULS_BETRAYAL: &str = "All creatures get -1/-1.";
const OPT: &str = "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.";
const ANGELIC_SHIELD: &str = "Creatures you control get +0/+1.\nSacrifice this enchantment: Return target creature to its owner's hand.";
const BOX_OF_FREE_RANGE_GOBLINS: &str =
    "Roll a six-sided die. Create a number of 1/1 red Goblin creature tokens equal to the result.";
const EUREKA: &str = "Starting with you, each player may put a permanent card from their hand onto the battlefield. Repeat this process until no one puts a card onto the battlefield.";
const FLASH: &str = "You may put a creature card from your hand onto the battlefield. If you do, sacrifice it unless you pay its mana cost reduced by {2}.";
const CLONE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";
const ADAPTIVE_AUTOMATON: &str = "As this creature enters, choose a creature type.\nThis creature is the chosen type in addition to its other types.\nOther creatures you control of the chosen type get +1/+1.";
const FANATIC_OF_XENAGOS: &str = "Trample\nTribute 1 (As this creature enters, an opponent of your choice may put a +1/+1 counter on it.)\nWhen this creature enters, if tribute wasn't paid, it gets +1/+1 and gains haste until end of turn.";
const ANOINTED_PEACEKEEPER: &str = "Vigilance\nAs this creature enters, look at an opponent's hand, then choose any card name.\nSpells your opponents cast with the chosen name cost {2} more to cast.\nActivated abilities of sources with the chosen name cost {2} more to activate unless they're mana abilities.";
const TRISKELION: &str = "This creature enters with three +1/+1 counters on it.\nRemove a +1/+1 counter from this creature: It deals 1 damage to any target.";
const CITY_OF_TRAITORS: &str = "When you play another land, sacrifice this land.\n{T}: Add {C}{C}.";
const LOTUS_VALE: &str = "If this land would enter, sacrifice two untapped lands instead. If you do, put this land onto the battlefield. If you don't, put it into its owner's graveyard.\n{T}: Add three mana of any one color.";
const TEMP_OF_THE_DAMNED: &str = "As this creature enters, roll a six-sided die. This creature enters with a number of funk counters on it equal to the result.\nAt the beginning of your upkeep, remove a funk counter from this creature. If you can't, sacrifice it.";
const MONOXA: &str = "Whenever you roll a 3 or higher, Monoxa gains first strike until end of turn.\n{6}: Roll a six-sided die.";
const PIXIE_GUIDE: &str = "Grant an Advantage — If you would roll one or more dice, instead roll that many dice plus one and ignore the lowest roll.";
const STORM_THE_FESTIVAL: &str = "Look at the top five cards of your library. You may put up to two permanent cards with mana value 5 or less from among them onto the battlefield. Put the rest on the bottom of your library in a random order.";
const STOLEN_IDENTITY: &str = "Create a token that's a copy of target artifact or creature.";
const GRUMGULLY: &str =
    "Each other non-Human creature you control enters with an additional +1/+1 counter on it.";
const VELOCIPEDE: &str = "Trample\nEach other Vehicle and creature you control enters with an additional +1/+1 counter on it if its mana value is 4 or less. Otherwise, it enters with three additional +1/+1 counters on it.\nCrew 3";
const KAVU_TITAN: &str = "Kicker {2}{G} (You may pay an additional {2}{G} as you cast this spell.)\nIf this creature was kicked, it enters with three +1/+1 counters on it and with trample.";
const TAYAM: &str =
    "Each other creature you control enters with an additional vigilance counter on it.";
const EVIL_REAWAKENED: &str = "Return target creature card from your graveyard to the battlefield with two additional +1/+1 counters on it.";
const RHYTHM_OF_THE_WILD: &str =
    "Nontoken creatures you control have riot. (They enter with your choice of a +1/+1 counter or haste.)";
const FATHOM_MAGE: &str = "Whenever a +1/+1 counter is put on this creature, you may draw a card.";
const BREEDING_POOL: &str =
    "({T}: Add {G} or {U}.)\nAs this land enters, you may pay 2 life. If you don't, it enters tapped.";
const YORE_TILLER_NEPHILIM: &str = "Whenever this creature attacks, return target creature card from your graveyard to the battlefield tapped and attacking.";

/// The scenario module exports only `P0` and `P1`.
const P2: PlayerId = PlayerId(2);

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn board(seed: u64) -> GameScenario {
    let mut scenario = GameScenario::new_n_player(2, seed);
    scenario.at_phase(Phase::PreCombatMain);
    let library: Vec<String> = (0..12).map(|i| format!("Library Card {i}")).collect();
    let names: Vec<&str> = library.iter().map(String::as_str).collect();
    scenario.with_library_top(P0, &names);
    scenario.with_library_top(P1, &names);
    scenario
}

/// Three seats, for a survivor's observer of a departing player's entry: P0
/// owns the entrant and is the active player, P1 is the next seat, and P2 is the
/// third player.
fn board3(seed: u64) -> GameScenario {
    let mut scenario = GameScenario::new_n_player(3, seed);
    scenario.at_phase(Phase::PreCombatMain);
    let library: Vec<String> = (0..12).map(|i| format!("Library Card {i}")).collect();
    let names: Vec<&str> = library.iter().map(String::as_str).collect();
    scenario.with_library_top(P0, &names);
    scenario.with_library_top(P1, &names);
    scenario.with_library_top(P2, &names);
    scenario
}

fn green(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
        .collect()
}

/// A `{X}{X}{G}{G}` creature in P0's hand, with mana for `x`.
fn x_creature(scenario: &mut GameScenario, name: &str, oracle: &str, x: usize) -> ObjectId {
    let id = scenario
        .add_creature_to_hand_from_oracle(P0, name, 0, 0, oracle)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![
                ManaCostShard::X,
                ManaCostShard::X,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
        })
        .id();
    scenario.with_mana_pool(P0, green(2 * x + 2));
    id
}

fn free_creature(
    scenario: &mut GameScenario,
    name: &str,
    power: i32,
    toughness: i32,
    oracle: &str,
) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, name, power, toughness, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

/// A free creature whose keyword line needs MTGJSON's keyword hint (the colon
/// form `devour:N`, which `Keyword`'s `FromStr` splits on).
fn free_keyword_creature(
    scenario: &mut GameScenario,
    name: &str,
    (power, toughness): (i32, i32),
    keywords: &[&str],
    oracle: &str,
) -> ObjectId {
    let mut builder = scenario.add_creature_to_hand(P0, name, power, toughness);
    builder.from_oracle_text_with_keywords(keywords, oracle);
    builder.with_mana_cost(ManaCost::generic(0));
    builder.id()
}

fn thunder_thrash_elder(scenario: &mut GameScenario) -> ObjectId {
    free_keyword_creature(
        scenario,
        "Thunder-Thrash Elder",
        (1, 1),
        &["devour:3"],
        THUNDER_THRASH_ELDER,
    )
}

fn gluttonous_slime(scenario: &mut GameScenario) -> ObjectId {
    free_keyword_creature(
        scenario,
        "Gluttonous Slime",
        (2, 2),
        &["flash", "devour:1"],
        GLUTTONOUS_SLIME,
    )
}

fn free_spell(scenario: &mut GameScenario, name: &str, instant: bool, oracle: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, instant, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

/// A printed 0/0 creature card in P0's graveyard.
fn graveyard_creature(scenario: &mut GameScenario, name: &str, oracle: &str) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, name, 0, 0)
        .from_oracle_text(oracle)
        .id()
}

fn act(runner: &mut GameRunner, action: GameAction, events: &mut Vec<GameEvent>) {
    let result = runner
        .act(action.clone())
        .unwrap_or_else(|err| panic!("{action:?} rejected: {err:?}"));
    events.extend(result.events);
    record_stack(runner.state());
}

/// Pass priority while the stack is non-empty and no player is being asked
/// anything else.
fn pass_until_prompt(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..60 {
        record_stack(runner.state());
        let state = runner.state();
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) && !state.stack.is_empty() {
            act(runner, GameAction::PassPriority, events);
        } else {
            return;
        }
    }
    panic!("the stack never paused or emptied");
}

/// Cast `spell` (committing it to the stack) and pass priority until its
/// resolution pauses or the stack is empty.
fn cast_and_pass(
    runner: &mut GameRunner,
    spell: ObjectId,
    x: Option<u32>,
    target: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) {
    let mut cast = runner.cast(spell);
    if let Some(x) = x {
        cast = cast.x(x);
    }
    if let Some(target) = target {
        cast = cast.target_object(target);
    }
    let commit = cast.commit();
    events.extend(commit.events().iter().cloned());
    drop(commit);
    pass_until_prompt(runner, events);
}

/// The default answers this file uses once its own decisions are made.
#[derive(Clone, Copy)]
struct Answers {
    /// A triggered ability's player target.
    player_target: PlayerId,
    /// Accept "you may" offers.
    accept_optional: bool,
}

const DEFAULT_ANSWERS: Answers = Answers {
    player_target: P1,
    accept_optional: true,
};

/// What a run to an empty stack saw.
#[derive(Default)]
struct Settled {
    optional_offers: Vec<ObjectId>,
    /// Counter-ordering prompts answered on the way, when the run was given a
    /// first pick.
    counter_prompts: usize,
}

thread_local! {
    /// Every triggered-ability stack entry this test thread's drivers saw, as
    /// `(entry id, source id)`, so a trigger put on the stack and resolved
    /// inside any driver is still counted once.
    static TRIGGERS_SEEN: std::cell::RefCell<Vec<(ObjectId, ObjectId)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn record_stack(state: &GameState) {
    TRIGGERS_SEEN.with(|seen| {
        let mut seen = seen.borrow_mut();
        for entry in &state.stack {
            if matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                && !seen.iter().any(|(id, _)| *id == entry.id)
            {
                seen.push((entry.id, entry.source_id));
            }
        }
    });
}

fn reset_triggers_seen() {
    TRIGGERS_SEEN.with(|seen| seen.borrow_mut().clear());
}

/// Triggered abilities from `source` the drivers have seen on the stack since
/// the last reset.
fn triggers_seen_from(source: ObjectId) -> usize {
    TRIGGERS_SEEN.with(|seen| {
        seen.borrow()
            .iter()
            .filter(|(_, from)| *from == source)
            .count()
    })
}

/// Triggered abilities from `source` the drivers have seen on the stack since
/// the last call, then forget them.
fn take_triggers_seen_from(source: ObjectId) -> usize {
    TRIGGERS_SEEN.with(|seen| {
        let seen = std::mem::take(&mut *seen.borrow_mut());
        seen.iter().filter(|(_, from)| *from == source).count()
    })
}

/// Drive to Priority on an empty stack, answering every prompt with the
/// default policy, and assert the settled postcondition.
fn settle(runner: &mut GameRunner, events: &mut Vec<GameEvent>, answers: Answers) -> Settled {
    drive_to_settled(runner, events, answers, None)
}

/// `settle`, answering every counter-ordering prompt with `pick`, and counting
/// those prompts.
fn settle_picking(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    pick: FirstPick,
) -> Settled {
    drive_to_settled(runner, events, DEFAULT_ANSWERS, Some(pick))
}

fn drive_to_settled(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    answers: Answers,
    pick: Option<FirstPick>,
) -> Settled {
    let mut settled = Settled::default();
    for _ in 0..120 {
        record_stack(runner.state());
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if resolution_is_settled(runner.state()) => break,
            WaitingFor::ReplacementChoice { .. }
                if pick.is_some() && is_counter_prompt(runner.state()) =>
            {
                settled.counter_prompts += 1;
                GameAction::ChooseReplacement {
                    index: replacement_index(
                        runner.state(),
                        pick.expect("checked by the guard").source_name(),
                    ),
                }
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::DieKeepChoice {
                ignorable_indices,
                ignore_count,
                ..
            } => GameAction::SelectDieRolls {
                ignore_indices: ignorable_indices
                    .iter()
                    .take(*ignore_count)
                    .copied()
                    .collect(),
            },
            WaitingFor::ScryChoice { cards, .. } => GameAction::SelectCards {
                cards: cards.clone(),
            },
            WaitingFor::OptionalEffectChoice { source_id, .. } => {
                settled.optional_offers.push(*source_id);
                GameAction::DecideOptionalEffect {
                    accept: answers.accept_optional,
                }
            }
            WaitingFor::TriggerTargetSelection { .. } => GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Player(
                    answers.player_target,
                )),
            },
            other => panic!("unhandled prompt while settling: {other:?}"),
        };
        act(runner, action, events);
    }
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }) && resolution_is_settled(state),
        "settled: Priority on an empty stack, got {:?} with {} stack entries",
        state.waiting_for,
        state.stack.len()
    );
    settled
}

/// Nothing is on the stack and no resolution is still under way. A logical
/// zone-change owner whose paused member was answered on a counter-ordering
/// choice resumes at the next priority pass, with its spell still resolving.
fn resolution_is_settled(state: &GameState) -> bool {
    state.stack.is_empty()
        && state.resolution_stack.is_empty()
        && state.resolving_stack_entry.is_none()
}

fn die_results(events: &[GameEvent]) -> Vec<u32> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DieRolled { result, .. } => result.map(u32::from),
            _ => None,
        })
        .collect()
}

fn ignored_rolls(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::DieRollIgnored { .. }))
        .count()
}

fn roll_resolved_for(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::RollDie,
                    source_id,
                    ..
                } if *source_id == source
            )
        })
        .count()
}

fn plus_counters(state: &GameState, id: ObjectId) -> u32 {
    state.objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn zone(state: &GameState, id: ObjectId) -> Zone {
    state.objects[&id].zone
}

fn hand(state: &GameState, player: PlayerId) -> usize {
    state.players[player.0 as usize].hand.len()
}

fn life(state: &GameState, player: PlayerId) -> i32 {
    state.players[player.0 as usize].life
}

fn has_unimplemented(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id]
        .abilities
        .iter()
        .any(|a| matches!(*a.effect, Effect::Unimplemented { .. }))
}

fn parked_entry_of(state: &GameState, id: ObjectId) -> bool {
    parked_state_of(state, id).is_some()
}

/// The recorded state of `id`'s parked battlefield entry.
fn parked_state_of(state: &GameState, id: ObjectId) -> Option<ParkedEntryState> {
    state
        .deferred_entry_events
        .iter()
        .find_map(|parked| match parked.event {
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Battlefield,
                ..
            } if object_id == id => Some(parked.state),
            _ => None,
        })
}

fn queued_or_stacked_from(state: &GameState, source: ObjectId) -> usize {
    let stacked = state
        .stack
        .iter()
        .filter(|entry| {
            matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                && entry.source_id == source
        })
        .count();
    let queued = state
        .deferred_triggers
        .iter()
        .filter(|context| context.pending.source_id == source)
        .count();
    stacked + queued
}

fn tie_prompt_results(state: &GameState) -> Option<Vec<u8>> {
    match &state.waiting_for {
        WaitingFor::DieKeepChoice { results, .. } => Some(results.clone()),
        _ => None,
    }
}

/// The first seed in `0..` for which `accept` holds.
fn find_seed(what: &str, accept: impl Fn(u64) -> bool) -> u64 {
    (0..600)
        .find(|seed| accept(*seed))
        .unwrap_or_else(|| panic!("no seed found for {what}"))
}

/// The survivors' total of a Barbarian Class tie (one ignore of the lowest).
fn survivors_total(naturals: &[u8]) -> u32 {
    let sum: u32 = naturals.iter().map(|&n| u32::from(n)).sum();
    sum - u32::from(*naturals.iter().min().expect("dice"))
}

// ---------------------------------------------------------------------------
// U3: the roll's total is the counter count; X is the paid X; X = 0 rolls none
// ---------------------------------------------------------------------------

/// R4. CR 706.4 + CR 107.3m: the counters equal the sum of the emitted results,
/// one die per paid X.
#[test]
fn counters_equal_the_total_of_the_rolled_dice() {
    for (name, oracle) in [
        ("Neverwinter Hydra", NEVERWINTER_HYDRA),
        ("Hydradoodle", HYDRADOODLE),
    ] {
        let mut scenario = board(7);
        let hydra = x_creature(&mut scenario, name, oracle, 3);
        let mut runner = scenario.build();
        let mut events = Vec::new();
        cast_and_pass(&mut runner, hydra, Some(3), None, &mut events);
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let rolls = die_results(&events);
        assert_eq!(rolls.len(), 3, "{name}: {rolls:?}");
        assert_eq!(zone(runner.state(), hydra), Zone::Battlefield, "{name}");
        assert_eq!(roll_resolved_for(&events, hydra), 1, "{name}");
        assert_eq!(
            plus_counters(runner.state(), hydra),
            rolls.iter().sum::<u32>(),
            "{name}"
        );
    }
}

/// R5. CR 706.1 + CR 704.5f (Hydradoodle's ruling): X = 0 rolls no dice and gives no
/// counters, so the 0/0 dies; an anthem keeps it alive with none. A non-cast
/// entry (X = 0 by CR 107.3m) whose producer stamps an effect count still
/// gives none, and a roll-count replacement does not roll a zero-die roll.
#[test]
fn x_zero_rolls_no_dice_and_gives_no_counters() {
    // Cast with X = 0.
    let mut scenario = board(7);
    let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 0);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    cast_and_pass(&mut runner, hydra, Some(0), None, &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert!(!has_unimplemented(runner.state(), hydra));
    assert_eq!(
        roll_resolved_for(&events, hydra),
        1,
        "reach guard: the roll ran"
    );
    assert!(die_results(&events).is_empty());
    assert_eq!(zone(runner.state(), hydra), Zone::Graveyard);

    // The anthem variant: alive with no counters.
    let mut scenario = board(7);
    scenario.add_enchantment_from_oracle(P0, "Angelic Shield", ANGELIC_SHIELD);
    let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 0);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    cast_and_pass(&mut runner, hydra, Some(0), None, &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert_eq!(roll_resolved_for(&events, hydra), 1);
    assert_eq!(zone(runner.state(), hydra), Zone::Battlefield);
    assert_eq!(plus_counters(runner.state(), hydra), 0);

    // Multi-authority: reanimated (no X paid) by a ChangeZone producer.
    let mut scenario = board(7);
    scenario.add_enchantment_from_oracle(P0, "Angelic Shield", ANGELIC_SHIELD);
    let hydra = graveyard_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA);
    let zombify = free_spell(&mut scenario, "Zombify", false, ZOMBIFY);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    cast_and_pass(&mut runner, zombify, None, Some(hydra), &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert_eq!(
        roll_resolved_for(&events, hydra),
        1,
        "reach guard: the roll ran"
    );
    assert_eq!(zone(runner.state(), hydra), Zone::Battlefield);
    assert_eq!(plus_counters(runner.state(), hydra), 0);

    // Barbarian Class does not turn a zero-die roll into a one-die roll.
    let mut scenario = board(7);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 0);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    cast_and_pass(&mut runner, hydra, Some(0), None, &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert_eq!(roll_resolved_for(&events, hydra), 1);
    assert!(die_results(&events).is_empty());
    assert_eq!(ignored_rolls(&events), 0);
}

fn barbarian_hydra(seed: u64, x: usize) -> (GameRunner, ObjectId) {
    let mut scenario = board(seed);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, x);
    (scenario.build(), hydra)
}

fn pauses_on_tie(runner: &mut GameRunner, hydra: ObjectId, x: u32) -> Option<Vec<u8>> {
    let mut events = Vec::new();
    cast_and_pass(runner, hydra, Some(x), None, &mut events);
    tie_prompt_results(runner.state())
}

/// R6. CR 706.6: a roll modifier sees the as-enters roll; the counters are the
/// survivors' total.
#[test]
fn roll_modifiers_apply_to_the_as_enters_roll() {
    let seed = find_seed("no tie", |seed| {
        let (mut runner, hydra) = barbarian_hydra(seed, 2);
        pauses_on_tie(&mut runner, hydra, 2).is_none()
    });
    let (mut runner, hydra) = barbarian_hydra(seed, 2);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, hydra, Some(2), None, &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 2, "{rolls:?}");
    assert_eq!(ignored_rolls(&events), 1);
    assert_eq!(roll_resolved_for(&events, hydra), 1);
    assert_eq!(
        plus_counters(runner.state(), hydra),
        rolls.iter().sum::<u32>()
    );

    // The tie path: the roller's keep choice is reached and answered.
    let seed = find_seed("a tie", |seed| {
        let (mut runner, hydra) = barbarian_hydra(seed, 2);
        pauses_on_tie(&mut runner, hydra, 2).is_some()
    });
    let (mut runner, hydra) = barbarian_hydra(seed, 2);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, hydra, Some(2), None, &mut events);
    let naturals = tie_prompt_results(runner.state()).expect("DieKeepChoice reached");
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let rolls = die_results(&events);
    assert_eq!(rolls.iter().sum::<u32>(), survivors_total(&naturals));
    assert_eq!(
        plus_counters(runner.state(), hydra),
        survivors_total(&naturals)
    );
}

/// R7. CR 706.1: one instruction is one roll event for "whenever you roll one
/// or more dice"; a zero-die instruction rolls nothing.
#[test]
fn a_battlefield_observer_sees_the_as_enters_roll_once() {
    for (x, expected_loss) in [(2, 1), (0, 0)] {
        let mut scenario = board(7);
        scenario.add_creature_from_oracle(P0, "Brazen Dwarf", 1, 3, BRAZEN_DWARF);
        let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, x);
        let mut runner = scenario.build();
        let before = life(runner.state(), P1);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, hydra, Some(x as u32), None, &mut events);
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        assert_eq!(roll_resolved_for(&events, hydra), 1, "x = {x}");
        assert!(!has_unimplemented(runner.state(), hydra));
        assert_eq!(before - life(runner.state(), P1), expected_loss, "x = {x}");
    }
}

/// R10. CR 107.3m: the paid X binds to the entrant, not to another X permanent.
#[test]
fn the_paid_x_binds_to_the_entrant() {
    let mut scenario = board(7);
    let doodle = x_creature(&mut scenario, "Hydradoodle", HYDRADOODLE, 5);
    let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 2);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    cast_and_pass(&mut runner, doodle, Some(5), None, &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert_eq!(die_results(&events).len(), 5);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, hydra, Some(2), None, &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 2, "{rolls:?}");
    assert_eq!(
        plus_counters(runner.state(), hydra),
        rolls.iter().sum::<u32>()
    );
}

// ---------------------------------------------------------------------------
// U4a: an inline as-enters continuation is observed with its counters
// ---------------------------------------------------------------------------

fn kiora(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_planeswalker_from_oracle(P0, "Kiora, Behemoth Beckoner", "Kiora", 7, KIORA)
        .id()
}

fn kiora_hydra(seed: u64) -> (GameRunner, ObjectId) {
    let mut scenario = board(seed);
    kiora(&mut scenario);
    let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 1);
    (scenario.build(), hydra)
}

fn first_roll(seed: u64, build: impl Fn(u64) -> (GameRunner, ObjectId), x: Option<u32>) -> u32 {
    let (mut runner, id) = build(seed);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, id, x, None, &mut events);
    die_results(&events).first().copied().unwrap_or(0)
}

/// R8a. CR 603.6d + CR 614.1c + CR 603.6a: the counters the creature enters
/// with are part of the entry, so Kiora sees a 4+ power entrant (its ruling:
/// "…as well as any +1/+1 counters the creature enters with").
#[test]
fn an_inline_as_enters_roll_is_observed_with_its_counters() {
    for (wants_draw, what) in [(true, "result >= 4"), (false, "result <= 3")] {
        let seed = find_seed(what, |seed| {
            let r = first_roll(seed, kiora_hydra, Some(1));
            (r >= 4) == wants_draw
        });
        let (mut runner, hydra) = kiora_hydra(seed);
        let hand_before = hand(runner.state(), P0);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, hydra, Some(1), None, &mut events);
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let rolls = die_results(&events);
        assert_eq!(
            plus_counters(runner.state(), hydra),
            rolls.iter().sum::<u32>()
        );
        let draws = hand(runner.state(), P0) + 1 - hand_before;
        assert_eq!(draws, usize::from(wants_draw), "{what}: {rolls:?}");
    }
}

fn bond_imp(seed: u64) -> (GameRunner, ObjectId) {
    let mut scenario = board(seed);
    scenario.add_enchantment_from_oracle(P0, "Elemental Bond", ELEMENTAL_BOND);
    let imp = free_creature(&mut scenario, "Jumbo Imp", 0, 0, JUMBO_IMP);
    (scenario.build(), imp)
}

/// R8b. The sibling class card and observer.
#[test]
fn jumbo_imp_is_observed_with_its_counters() {
    for (wants_draw, what) in [(true, "result >= 3"), (false, "result <= 2")] {
        let seed = find_seed(what, |seed| {
            (first_roll(seed, bond_imp, None) >= 3) == wants_draw
        });
        let (mut runner, imp) = bond_imp(seed);
        let hand_before = hand(runner.state(), P0);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, imp, None, None, &mut events);
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let rolls = die_results(&events);
        assert_eq!(rolls.len(), 1);
        assert_eq!(plus_counters(runner.state(), imp), rolls[0]);
        assert_eq!(
            hand(runner.state(), P0) + 1 - hand_before,
            usize::from(wants_draw)
        );
    }
}

// ---------------------------------------------------------------------------
// U4b: a paused continuation's entry is held, then released once
// ---------------------------------------------------------------------------

fn kiora_barbarian_hydra(seed: u64) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = board(seed);
    let kiora = kiora(&mut scenario);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 2);
    (scenario.build(), hydra, kiora)
}

/// R8c. CR 614.12a + CR 603.6d: a paused roll defers the entry until it
/// completes; it is collected in the answering action, against the realized
/// entrant.
#[test]
fn a_paused_roll_defers_the_entry_until_it_completes() {
    let seed = find_seed("a tie whose survivors total >= 4", |seed| {
        let (mut runner, hydra, _) = kiora_barbarian_hydra(seed);
        pauses_on_tie(&mut runner, hydra, 2).is_some_and(|n| survivors_total(&n) >= 4)
    });
    let (mut runner, hydra, kiora) = kiora_barbarian_hydra(seed);
    let hand_before = hand(runner.state(), P0);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, hydra, Some(2), None, &mut events);
    let state = runner.state();
    let naturals = tie_prompt_results(state).expect("DieKeepChoice reached");
    assert!(
        parked_entry_of(state, hydra),
        "the entry is parked at the pause"
    );
    assert_eq!(queued_or_stacked_from(state, kiora), 0, "the hold");
    let WaitingFor::DieKeepChoice {
        ignorable_indices,
        ignore_count,
        ..
    } = state.waiting_for.clone()
    else {
        unreachable!()
    };
    let mut answer_events = Vec::new();
    act(
        &mut runner,
        GameAction::SelectDieRolls {
            ignore_indices: ignorable_indices.into_iter().take(ignore_count).collect(),
        },
        &mut answer_events,
    );
    assert_eq!(
        queued_or_stacked_from(runner.state(), kiora),
        1,
        "collected in the SelectDieRolls action"
    );
    assert!(!parked_entry_of(runner.state(), hydra));
    events.extend(answer_events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert_eq!(
        plus_counters(runner.state(), hydra),
        survivors_total(&naturals)
    );
    assert_eq!(hand(runner.state(), P0) + 1 - hand_before, 1);

    // Non-entry hostile: a plain die prompt parks nothing.
    let seed = find_seed("a Box of Free-Range Goblins tie", |seed| {
        let mut scenario = board(seed);
        scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
        let spell = free_spell(
            &mut scenario,
            "Box of Free-Range Goblins",
            false,
            BOX_OF_FREE_RANGE_GOBLINS,
        );
        let mut runner = scenario.build();
        let mut events = Vec::new();
        cast_and_pass(&mut runner, spell, None, None, &mut events);
        tie_prompt_results(runner.state()).is_some()
    });
    let mut scenario = board(seed);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let spell = free_spell(
        &mut scenario,
        "Box of Free-Range Goblins",
        false,
        BOX_OF_FREE_RANGE_GOBLINS,
    );
    let mut runner = scenario.build();
    let mut events = Vec::new();
    cast_and_pass(&mut runner, spell, None, None, &mut events);
    assert!(tie_prompt_results(runner.state()).is_some());
    assert!(runner.state().deferred_entry_events.is_empty());
}

fn scales_hydra(seed: u64) -> (GameRunner, ObjectId) {
    let mut scenario = board(seed);
    kiora(&mut scenario);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
    let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 1);
    (scenario.build(), hydra)
}

fn replacement_index(state: &GameState, source_name: &str) -> usize {
    match &state.waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|c| c.source_name == source_name)
            .unwrap_or_else(|| panic!("{source_name} among {candidates:?}")),
        other => panic!("expected ReplacementChoice, got {other:?}"),
    }
}

/// R8d. A pause on the counters (not the roll) also defers the entry.
#[test]
fn a_pause_on_the_counters_also_defers_the_entry() {
    for (first, wants_seed, expected) in [
        ("Hardened Scales", None, None),
        ("Branching Evolution", Some(1u32), Some(3u32)),
    ] {
        let seed = match wants_seed {
            None => 7,
            Some(r) => find_seed("r = 1", |seed| first_roll(seed, scales_hydra, Some(1)) == r),
        };
        let (mut runner, hydra) = scales_hydra(seed);
        let hand_before = hand(runner.state(), P0);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, hydra, Some(1), None, &mut events);
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "{:?}",
            runner.state().waiting_for
        );
        assert!(parked_entry_of(runner.state(), hydra));
        let r = die_results(&events)[0];
        let index = replacement_index(runner.state(), first);
        act(
            &mut runner,
            GameAction::ChooseReplacement { index },
            &mut events,
        );
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let counters = plus_counters(runner.state(), hydra);
        let expected = expected.unwrap_or((r + 1) * 2);
        assert_eq!(counters, expected, "{first} first, r = {r}");
        assert_eq!(
            hand(runner.state(), P0) + 1 - hand_before,
            usize::from(counters >= 4),
            "{first} first"
        );
    }
}

/// R9 / V1. CR 614.1c + CR 614.12 + CR 113.6 + CR 603.10: Hammer Jammer rolls
/// before it is on the battlefield, so its die-roll ability does not function yet
/// and it is not among the objects checked after that roll. It enters with
/// counters equal to the result and triggers on nothing.
#[test]
fn hammer_jammer_does_not_see_its_own_as_enters_roll() {
    reset_triggers_seen();
    let mut scenario = board(7);
    let jammer = free_creature(&mut scenario, "Hammer Jammer", 0, 0, HAMMER_JAMMER);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    {
        let commit = runner.cast(jammer).commit();
        events.extend(commit.events().iter().cloned());
    }
    // Pass until the creature spell itself has resolved.
    let mut guard = 0;
    while zone(runner.state(), jammer) == Zone::Stack {
        act(&mut runner, GameAction::PassPriority, &mut events);
        guard += 1;
        assert!(guard < 10);
    }
    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 1, "{rolls:?}");
    assert_eq!(
        queued_or_stacked_from(runner.state(), jammer),
        0,
        "no Hammer Jammer trigger on entry"
    );
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert_eq!(zone(runner.state(), jammer), Zone::Battlefield);
    assert_eq!(plus_counters(runner.state(), jammer), rolls[0]);
    assert_eq!(take_triggers_seen_from(jammer), 0);
}

// ---------------------------------------------------------------------------
// Devour (an existing as-enters continuation): its counters are part of the entry
// ---------------------------------------------------------------------------

fn effect_zone_choice(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. })
}

/// R12. CR 702.82a + CR 614.1c + CR 603.6d: Kiora sees the devoured counters.
#[test]
fn devour_counters_are_part_of_the_entry() {
    for devour in [true, false] {
        let mut scenario = board(7);
        let kiora = kiora(&mut scenario);
        let victim = scenario.add_vanilla(P0, 2, 2);
        let elder = thunder_thrash_elder(&mut scenario);
        let mut runner = scenario.build();
        let hand_before = hand(runner.state(), P0);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, elder, None, None, &mut events);
        assert!(
            effect_zone_choice(runner.state()),
            "{:?}",
            runner.state().waiting_for
        );
        let cards = if devour { vec![victim] } else { vec![] };
        act(&mut runner, GameAction::SelectCards { cards }, &mut events);
        assert_eq!(
            plus_counters(runner.state(), elder),
            if devour { 3 } else { 0 }
        );
        assert_eq!(
            queued_or_stacked_from(runner.state(), kiora)
                + usize::from(matches!(
                    runner.state().waiting_for,
                    WaitingFor::OrderTriggers { .. }
                )),
            usize::from(devour),
            "Kiora's trigger is placed by the answering action"
        );
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        assert_eq!(
            hand(runner.state(), P0) + 1 - hand_before,
            usize::from(devour)
        );
    }
}

/// R14. CR 603.3b: the devoured creature's dies trigger and the Devourer's
/// ETB observer share the sacrifice handler's drain — one ordering window.
#[test]
fn devour_dies_and_enters_triggers_share_one_drain() {
    for devour in [true, false] {
        let mut scenario = board(7);
        let kiora = kiora(&mut scenario);
        let artist = scenario
            .add_creature_from_oracle(P0, "Blood Artist", 0, 1, BLOOD_ARTIST)
            .id();
        let victim = scenario.add_vanilla(P0, 2, 2);
        let elder = thunder_thrash_elder(&mut scenario);
        let mut runner = scenario.build();
        let hand_before = hand(runner.state(), P0);
        let (p0_before, p1_before) = (life(runner.state(), P0), life(runner.state(), P1));
        let mut events = Vec::new();
        cast_and_pass(&mut runner, elder, None, None, &mut events);
        assert!(effect_zone_choice(runner.state()));
        let cards = if devour { vec![victim] } else { vec![] };
        act(&mut runner, GameAction::SelectCards { cards }, &mut events);
        if devour {
            assert_eq!(plus_counters(runner.state(), elder), 3);
            match &runner.state().waiting_for {
                WaitingFor::OrderTriggers { triggers, .. } => {
                    let sources: Vec<ObjectId> = triggers.iter().map(|t| t.source_id).collect();
                    assert!(sources.contains(&artist), "{sources:?}");
                    assert!(sources.contains(&kiora), "{sources:?}");
                }
                other => panic!("one ordering window for both triggers, got {other:?}"),
            }
        } else {
            assert!(!matches!(
                runner.state().waiting_for,
                WaitingFor::OrderTriggers { .. }
            ));
        }
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        assert_eq!(
            hand(runner.state(), P0) + 1 - hand_before,
            usize::from(devour)
        );
        assert_eq!(life(runner.state(), P1), p1_before - i32::from(devour));
        assert_eq!(life(runner.state(), P0), p0_before + i32::from(devour));
    }
}

// ---------------------------------------------------------------------------
// U4c: under a logical zone-change owner, the paused member is collected once
// ---------------------------------------------------------------------------

fn warden_bond_barbarian_imp(seed: u64) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = board(seed);
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
    scenario.add_enchantment_from_oracle(P0, "Elemental Bond", ELEMENTAL_BOND);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let imp = graveyard_creature(&mut scenario, "Jumbo Imp", JUMBO_IMP);
    let zombify = free_spell(&mut scenario, "Zombify", false, ZOMBIFY);
    (scenario.build(), imp, zombify)
}

fn zombify_tie(seed: u64) -> Option<Vec<u8>> {
    let (mut runner, imp, zombify) = warden_bond_barbarian_imp(seed);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, zombify, None, Some(imp), &mut events);
    tie_prompt_results(runner.state())
}

/// R15. CR 603.2c + CR 603.6d: a reanimated paused entry is collected once,
/// at the owner's resume, with its counters.
#[test]
fn a_reanimated_paused_roll_is_collected_once_with_its_counters() {
    for wants_draw in [true, false] {
        let seed = find_seed("an Imp tie", |seed| {
            zombify_tie(seed).is_some_and(|n| (survivors_total(&n) >= 3) == wants_draw)
        });
        let (mut runner, imp, zombify) = warden_bond_barbarian_imp(seed);
        let hand_before = hand(runner.state(), P0);
        let life_before = life(runner.state(), P0);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, zombify, None, Some(imp), &mut events);
        let naturals = tie_prompt_results(runner.state()).expect("DieKeepChoice reached");
        let WaitingFor::DieKeepChoice {
            ignorable_indices,
            ignore_count,
            ..
        } = runner.state().waiting_for.clone()
        else {
            unreachable!()
        };
        act(
            &mut runner,
            GameAction::SelectDieRolls {
                ignore_indices: ignorable_indices.into_iter().take(ignore_count).collect(),
            },
            &mut events,
        );
        assert!(
            runner.state().active_change_zone_frame().is_none(),
            "the owner completed in the answering action"
        );
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        assert_eq!(
            plus_counters(runner.state(), imp),
            survivors_total(&naturals)
        );
        assert_eq!(
            life(runner.state(), P0),
            life_before + 1,
            "Soul Warden once"
        );
        // Zombify left the hand.
        assert_eq!(
            hand(runner.state(), P0) + 1 - hand_before,
            usize::from(wants_draw),
            "Elemental Bond against the realized Imp"
        );
    }
}

fn warden_kiora_scales_imp(seed: u64) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = board(seed);
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
    kiora(&mut scenario);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
    let imp = graveyard_creature(&mut scenario, "Jumbo Imp", JUMBO_IMP);
    let zombify = free_spell(&mut scenario, "Zombify", false, ZOMBIFY);
    (scenario.build(), imp, zombify)
}

/// R16. The same under a counter-ordering pause.
#[test]
fn a_reanimated_counter_order_pause_is_collected_once() {
    for (first, wants_r1) in [("Hardened Scales", false), ("Branching Evolution", true)] {
        let seed = if wants_r1 {
            find_seed("r = 1", |seed| {
                let (mut runner, imp, zombify) = warden_kiora_scales_imp(seed);
                let mut events = Vec::new();
                cast_and_pass(&mut runner, zombify, None, Some(imp), &mut events);
                die_results(&events).first() == Some(&1)
            })
        } else {
            7
        };
        let (mut runner, imp, zombify) = warden_kiora_scales_imp(seed);
        let hand_before = hand(runner.state(), P0);
        let life_before = life(runner.state(), P0);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, zombify, None, Some(imp), &mut events);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert!(parked_entry_of(runner.state(), imp));
        let r = die_results(&events)[0];
        let index = replacement_index(runner.state(), first);
        act(
            &mut runner,
            GameAction::ChooseReplacement { index },
            &mut events,
        );
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let counters = plus_counters(runner.state(), imp);
        let expected = if wants_r1 { 3 } else { (r + 1) * 2 };
        assert_eq!(counters, expected, "{first} first");
        assert_eq!(
            life(runner.state(), P0),
            life_before + 1,
            "Soul Warden once"
        );
        // Zombify left the hand.
        assert_eq!(
            hand(runner.state(), P0) + 1 - hand_before,
            usize::from(counters >= 4),
            "{first} first"
        );
    }
}

/// R17. Devour under a targeted owner.
#[test]
fn a_reanimated_devour_is_collected_once_with_its_counters() {
    for devour in [true, false] {
        let mut scenario = board(7);
        scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
        kiora(&mut scenario);
        let victim = scenario.add_vanilla(P0, 2, 2);
        let elder = {
            let mut builder = scenario.add_creature_to_graveyard(P0, "Thunder-Thrash Elder", 1, 1);
            builder.from_oracle_text_with_keywords(&["devour:3"], THUNDER_THRASH_ELDER);
            builder.id()
        };
        let zombify = free_spell(&mut scenario, "Zombify", false, ZOMBIFY);
        let mut runner = scenario.build();
        let hand_before = hand(runner.state(), P0);
        let life_before = life(runner.state(), P0);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, zombify, None, Some(elder), &mut events);
        assert!(
            effect_zone_choice(runner.state()),
            "{:?}",
            runner.state().waiting_for
        );
        let cards = if devour { vec![victim] } else { vec![] };
        act(&mut runner, GameAction::SelectCards { cards }, &mut events);
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        assert_eq!(
            plus_counters(runner.state(), elder),
            if devour { 3 } else { 0 }
        );
        assert_eq!(
            life(runner.state(), P0),
            life_before + 1,
            "Soul Warden once"
        );
        // Zombify left the hand.
        assert_eq!(
            hand(runner.state(), P0) + 1 - hand_before,
            usize::from(devour)
        );
    }
}

fn mass_board(seed: u64) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = board(seed);
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
    scenario.add_enchantment_from_oracle(P0, "Elemental Bond", ELEMENTAL_BOND);
    scenario.add_creature_from_oracle(P0, "Welcoming Vampire", 2, 3, WELCOMING_VAMPIRE);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let imp = graveyard_creature(&mut scenario, "Jumbo Imp", JUMBO_IMP);
    let bear = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let rise = free_spell(
        &mut scenario,
        "Rise of the Dark Realms",
        false,
        RISE_OF_THE_DARK_REALMS,
    );
    (scenario.build(), imp, bear, rise)
}

/// R18. CR 603.2c + CR 603.6a: a paused member of a mass entry is collected
/// with its co-entrant once; a batched observer fires once.
#[test]
fn a_paused_member_of_a_mass_entry_is_collected_with_its_co_entrant() {
    for imp_qualifies_for_bond in [true, false] {
        let seed = find_seed("a mass Imp tie", |seed| {
            let (mut runner, _, _, rise) = mass_board(seed);
            let mut events = Vec::new();
            cast_and_pass(&mut runner, rise, None, None, &mut events);
            tie_prompt_results(runner.state())
                .is_some_and(|n| (survivors_total(&n) >= 3) == imp_qualifies_for_bond)
        });
        let (mut runner, imp, bear, rise) = mass_board(seed);
        let hand_before = hand(runner.state(), P0);
        let life_before = life(runner.state(), P0);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, rise, None, None, &mut events);
        let naturals = tie_prompt_results(runner.state()).expect("DieKeepChoice reached");
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        assert_eq!(zone(runner.state(), imp), Zone::Battlefield);
        assert_eq!(zone(runner.state(), bear), Zone::Battlefield);
        assert_eq!(
            plus_counters(runner.state(), imp),
            survivors_total(&naturals)
        );
        assert_eq!(life(runner.state(), P0), life_before + 2, "two entrants");
        // Rise of the Dark Realms left the hand.
        assert_eq!(
            hand(runner.state(), P0) + 1 - hand_before,
            1 + usize::from(imp_qualifies_for_bond),
            "one Welcoming Vampire draw, plus Elemental Bond for a 3+ power Imp"
        );
    }
}

fn breach_board(seed: u64) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = board(seed);
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
    scenario.add_enchantment_from_oracle(P0, "Elemental Bond", ELEMENTAL_BOND);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let imp = scenario
        .add_creature_to_hand_from_oracle(P0, "Jumbo Imp", 0, 0, JUMBO_IMP)
        .id();
    let breach = free_spell(
        &mut scenario,
        "Through the Breach",
        true,
        THROUGH_THE_BREACH,
    );
    (scenario.build(), imp, breach)
}

/// Cast a "you may put" `spell` and put `entrant`, up to its entry's first prompt.
fn put_until_roll(runner: &mut GameRunner, spell: ObjectId, entrant: ObjectId) -> Vec<GameEvent> {
    let mut events = Vec::new();
    cast_and_pass(runner, spell, None, None, &mut events);
    for _ in 0..8 {
        let action = match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::EffectZoneChoice { .. } | WaitingFor::ChooseFromZoneChoice { .. } => {
                GameAction::SelectCards {
                    cards: vec![entrant],
                }
            }
            _ => break,
        };
        act(runner, action, &mut events);
    }
    events
}

/// R19. An un-owned non-cast entry (single-pick put) is held and released once.
#[test]
fn an_unowned_put_onto_the_battlefield_is_collected_once() {
    for wants_draw in [true, false] {
        let seed = find_seed("a Breach Imp tie", |seed| {
            let (mut runner, imp, breach) = breach_board(seed);
            put_until_roll(&mut runner, breach, imp);
            tie_prompt_results(runner.state())
                .is_some_and(|n| (survivors_total(&n) >= 3) == wants_draw)
        });
        let (mut runner, imp, breach) = breach_board(seed);
        let hand_before = hand(runner.state(), P0);
        let life_before = life(runner.state(), P0);
        let mut events = put_until_roll(&mut runner, breach, imp);
        let naturals = tie_prompt_results(runner.state()).expect("DieKeepChoice reached");
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        assert_eq!(zone(runner.state(), imp), Zone::Battlefield);
        assert!(runner.state().objects[&imp].has_keyword(&Keyword::Haste));
        assert_eq!(
            plus_counters(runner.state(), imp),
            survivors_total(&naturals)
        );
        assert_eq!(
            life(runner.state(), P0),
            life_before + 1,
            "Soul Warden once"
        );
        // Jumbo Imp and Through the Breach left the hand.
        assert_eq!(
            hand(runner.state(), P0) + 2 - hand_before,
            usize::from(wants_draw)
        );
    }
}

fn bond_zombify_imp(seed: u64) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = board(seed);
    scenario.add_enchantment_from_oracle(P0, "Elemental Bond", ELEMENTAL_BOND);
    let imp = graveyard_creature(&mut scenario, "Jumbo Imp", JUMBO_IMP);
    let zombify = free_spell(&mut scenario, "Zombify", false, ZOMBIFY);
    (scenario.build(), imp, zombify)
}

/// R20. An inline entry under an owner (no pause).
#[test]
fn an_inline_entry_under_an_owner_is_observed_with_its_counters() {
    for wants_draw in [true, false] {
        let seed = find_seed("an owned Imp roll", |seed| {
            let (mut runner, imp, zombify) = bond_zombify_imp(seed);
            let mut events = Vec::new();
            cast_and_pass(&mut runner, zombify, None, Some(imp), &mut events);
            die_results(&events)
                .first()
                .is_some_and(|&r| (r >= 3) == wants_draw)
        });
        let (mut runner, imp, zombify) = bond_zombify_imp(seed);
        let hand_before = hand(runner.state(), P0);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, zombify, None, Some(imp), &mut events);
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let rolls = die_results(&events);
        assert_eq!(plus_counters(runner.state(), imp), rolls[0]);
        // Zombify left the hand.
        assert_eq!(
            hand(runner.state(), P0) + 1 - hand_before,
            usize::from(wants_draw)
        );
    }
}

// ---------------------------------------------------------------------------
// A parked entry records whether its entry happened
// ---------------------------------------------------------------------------

/// Eureka with Jumbo Imp as P0's only permanent card, a card for P1 to decline,
/// and Soul Warden with the third player. Only the first pass of Eureka's process
/// runs: its repeat stays unimplemented, and no row depends on it.
fn eureka_imp_board(seed: u64) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = board3(seed);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let warden = scenario
        .add_creature_from_oracle(P2, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    let imp = free_creature(&mut scenario, "Jumbo Imp", 0, 0, JUMBO_IMP);
    let eureka = free_spell(&mut scenario, "Eureka", false, EUREKA);
    scenario.add_creature_to_hand(P1, "Vanilla Bear", 2, 2);
    (scenario.build(), imp, eureka, warden)
}

fn eureka_imp_tie_seed() -> u64 {
    find_seed("a Eureka Imp tie", |seed| {
        let (mut runner, imp, eureka, _) = eureka_imp_board(seed);
        put_until_roll(&mut runner, eureka, imp);
        tie_prompt_results(runner.state()).is_some()
    })
}

/// Answer the tie, leaving Imp's entry completed and P1's put open.
fn answer_tie(runner: &mut GameRunner, events: &mut Vec<GameEvent>) -> Vec<u8> {
    let WaitingFor::DieKeepChoice {
        ignorable_indices,
        ignore_count,
        results,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected DieKeepChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    let naturals = results.clone();
    let ignore_indices = ignorable_indices
        .iter()
        .take(*ignore_count)
        .copied()
        .collect();
    act(
        runner,
        GameAction::SelectDieRolls { ignore_indices },
        events,
    );
    naturals
}

const DECLINE_ANSWERS: Answers = Answers {
    player_target: P1,
    accept_optional: false,
};

/// V1. The maintainer's regression. CR 603.2 + CR 800.4a: Imp's entry finished
/// before P1 was asked, so P0 leaving does not untrigger a third player's Soul
/// Warden. Eureka's sequence is sequential (Hypergenesis's ruling on the same
/// process), and its ruling holds the trigger until resolution ends.
#[test]
fn a_completed_entry_survives_its_owners_departure() {
    let seed = eureka_imp_tie_seed();
    for owner_leaves in [true, false] {
        reset_triggers_seen();
        let (mut runner, imp, eureka, warden) = eureka_imp_board(seed);
        let before = life(runner.state(), P2);
        let mut events = put_until_roll(&mut runner, eureka, imp);
        let naturals = answer_tie(&mut runner, &mut events);

        // Reach: the entry completed, is still parked, and nothing collected it.
        assert_eq!(
            parked_state_of(runner.state(), imp),
            Some(ParkedEntryState::Completed)
        );
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { player, .. } if player == P1
            ),
            "{:?}",
            runner.state().waiting_for
        );
        assert_eq!(
            plus_counters(runner.state(), imp),
            survivors_total(&naturals)
        );
        assert_eq!(queued_or_stacked_from(runner.state(), warden), 0);

        if owner_leaves {
            act(
                &mut runner,
                GameAction::Concede { player_id: P0 },
                &mut events,
            );
        }
        settle(&mut runner, &mut events, DECLINE_ANSWERS);

        assert_eq!(life(runner.state(), P2), before + 1, "Soul Warden once");
        assert_eq!(take_triggers_seen_from(warden), 1);
        assert_eq!(
            zone(runner.state(), imp) == Zone::Battlefield,
            !owner_leaves
        );
        assert_eq!(runner.state().players[0].is_eliminated, owner_leaves);
    }
}

/// V5. CR 603.6d + CR 614.12a: the binding survives serialization, because a
/// pause persists across actions.
#[test]
fn the_completion_binding_survives_a_round_trip() {
    let (mut runner, imp, eureka, _) = eureka_imp_board(eureka_imp_tie_seed());
    let mut events = put_until_roll(&mut runner, eureka, imp);
    let wire = serde_json::to_value(runner.state()).expect("state serializes");
    let restored: GameState = serde_json::from_value(wire).expect("state deserializes");
    assert_eq!(
        runner.state().deferred_entry_events,
        restored.deferred_entry_events
    );
    assert_eq!(runner.state().resolution_stack, restored.resolution_stack);
    assert!(
        restored.resolution_stack.iter().any(|frame| matches!(
            frame,
            ResolutionFrame::PostReplacement(drains) if drains.resident().is_some_and(|drain| {
                drain.parked_entrants.iter().any(|entrant| entrant.object_id == imp)
            })
        )),
        "the restored dispatch names Imp"
    );
    *runner.state_mut() = restored;
    answer_tie(&mut runner, &mut events);
    assert_eq!(
        parked_state_of(runner.state(), imp),
        Some(ParkedEntryState::Completed)
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player, .. } if player == P1
    ));
}

/// Answer a `ReplacementChoice` by accepting it, as `spark_double_as_enters` does.
fn accept_replacement(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    act(runner, GameAction::ChooseReplacement { index: 0 }, events);
}

fn choose_copy_target(runner: &mut GameRunner, donor: ObjectId, events: &mut Vec<GameEvent>) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CopyTargetChoice { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    act(
        runner,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(donor)),
        },
        events,
    );
}

/// Answer a `NamedChoice` with its first option, and return that option.
fn answer_named_choice(runner: &mut GameRunner, events: &mut Vec<GameEvent>) -> String {
    let WaitingFor::NamedChoice { options, .. } = &runner.state().waiting_for else {
        panic!("expected NamedChoice, got {:?}", runner.state().waiting_for);
    };
    let choice = options
        .first()
        .expect("a named choice offers options")
        .clone();
    act(
        runner,
        GameAction::ChooseOption {
            choice: choice.clone(),
        },
        events,
    );
    choice
}

/// `ChoiceType::CardName` offers no options: the answer is any name in
/// `all_card_names`.
const SEEDED_CARD_NAME: &str = "Llanowar Elves";

fn seed_card_names(runner: &mut GameRunner) {
    runner.state_mut().all_card_names = vec![SEEDED_CARD_NAME.to_string()].into();
}

fn answer_card_name(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    assert!(is_card_name_choice(runner.state()));
    act(
        runner,
        GameAction::ChooseOption {
            choice: SEEDED_CARD_NAME.to_string(),
        },
        events,
    );
}

fn is_card_name_choice(state: &GameState) -> bool {
    matches!(
        state.waiting_for,
        WaitingFor::NamedChoice {
            choice_type: ChoiceType::CardName,
            ..
        }
    )
}

fn is_pending(state: &GameState, id: ObjectId) -> bool {
    matches!(
        parked_state_of(state, id),
        Some(ParkedEntryState::Pending { .. })
    )
}

fn concede(runner: &mut GameRunner, player_id: PlayerId, events: &mut Vec<GameEvent>) {
    act(runner, GameAction::Concede { player_id }, events);
}

fn add_warden(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P2, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id()
}

/// Anointed Peacekeeper as P0's free spell, with a card name to answer with.
fn peacekeeper_board(seed: u64) -> (GameRunner, ObjectId) {
    let mut scenario = board3(seed);
    add_warden(&mut scenario);
    let peacekeeper = free_keyword_creature(
        &mut scenario,
        "Anointed Peacekeeper",
        (3, 3),
        &["vigilance"],
        ANOINTED_PEACEKEEPER,
    );
    let mut runner = scenario.build();
    seed_card_names(&mut runner);
    (runner, peacekeeper)
}

/// V20. A chooser's concession at an as-enters choice whose chain ended at that
/// prompt: nothing is left above the drain, and the carrier abandonment retires
/// it. Base panics at the abandonment's `.expect`.
#[test]
fn conceding_at_a_chain_ending_as_enters_choice_abandons_the_entry() {
    let (mut runner, peacekeeper) = peacekeeper_board(7);
    let before = life(runner.state(), P2);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, peacekeeper, None, None, &mut events);
    answer_named_choice(&mut runner, &mut events);
    assert!(is_card_name_choice(runner.state()));
    assert!(matches!(
        runner.state().resolution_stack.last(),
        Some(ResolutionFrame::PostReplacement(_))
    ));
    assert!(is_pending(runner.state(), peacekeeper));

    concede(&mut runner, P0, &mut events);
    settle(&mut runner, &mut events, DECLINE_ANSWERS);

    assert_eq!(life(runner.state(), P2), before);
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().deferred_entry_events.is_empty());
    assert!(runner.state().players[0].is_eliminated);
    assert_ne!(zone(runner.state(), peacekeeper), Zone::Battlefield);
}

/// V20's paired positive and bystander. Both choices made: the entry completed
/// and Soul Warden triggers (CR 603.2).
#[test]
fn an_as_enters_choice_chain_completes_when_answered() {
    for bystander_leaves in [false, true] {
        let (mut runner, peacekeeper) = peacekeeper_board(7);
        let before = life(runner.state(), P2);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, peacekeeper, None, None, &mut events);
        answer_named_choice(&mut runner, &mut events);
        assert!(is_card_name_choice(runner.state()));
        if bystander_leaves {
            concede(&mut runner, P1, &mut events);
            assert!(is_card_name_choice(runner.state()));
        }
        answer_card_name(&mut runner, &mut events);
        settle(&mut runner, &mut events, DECLINE_ANSWERS);
        assert_eq!(life(runner.state(), P2), before + 1);
        assert_eq!(zone(runner.state(), peacekeeper), Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&peacekeeper].chosen_card_name(),
            Some(SEEDED_CARD_NAME)
        );
    }
}

/// V20's first prompt: `AC` is on top, so the abandonment clears it directly.
#[test]
fn conceding_at_the_first_choice_of_a_chain_abandons_the_entry() {
    let (mut runner, peacekeeper) = peacekeeper_board(7);
    let before = life(runner.state(), P2);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, peacekeeper, None, None, &mut events);
    assert!(is_pending(runner.state(), peacekeeper));
    concede(&mut runner, P0, &mut events);
    settle(&mut runner, &mut events, DECLINE_ANSWERS);
    assert_eq!(life(runner.state(), P2), before);
}

/// Eureka with Jumbo Imp as P0's card and Anointed Peacekeeper as P1's only
/// permanent card.
fn eureka_peacekeeper_board(seed: u64) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = board3(seed);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    add_warden(&mut scenario);
    let imp = free_creature(&mut scenario, "Jumbo Imp", 0, 0, JUMBO_IMP);
    let eureka = free_spell(&mut scenario, "Eureka", false, EUREKA);
    let peacekeeper = {
        let mut builder = scenario.add_creature_to_hand(P1, "Anointed Peacekeeper", 3, 3);
        builder.from_oracle_text_with_keywords(&["vigilance"], ANOINTED_PEACEKEEPER);
        builder.with_mana_cost(ManaCost::generic(0));
        builder.id()
    };
    let mut runner = scenario.build();
    seed_card_names(&mut runner);
    (runner, imp, eureka, peacekeeper)
}

fn eureka_peacekeeper_seed() -> u64 {
    find_seed("a Eureka Imp tie", |seed| {
        let (mut runner, imp, eureka, _) = eureka_peacekeeper_board(seed);
        put_until_roll(&mut runner, eureka, imp);
        tie_prompt_results(runner.state()).is_some()
    })
}

/// Drive `eureka_peacekeeper_board` to P1's first choice, with Imp's entry
/// completed and Peacekeeper's pending.
fn eureka_to_peacekeeper_choice(
    runner: &mut GameRunner,
    imp: ObjectId,
    eureka: ObjectId,
) -> Vec<GameEvent> {
    let mut events = put_until_roll(runner, eureka, imp);
    answer_tie(runner, &mut events);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player, .. } if player == P1
    ));
    act(
        runner,
        GameAction::DecideOptionalEffect { accept: true },
        &mut events,
    );
    events
}

/// V23. CR 603.2 + CR 800.4a: a completed entry survives the carrier abandonment
/// a later seat's concession raises. Imp's entry finished before P1 was asked,
/// and P1 leaving removes only P1's objects.
#[test]
fn a_completed_entry_survives_a_later_seats_carrier_abandonment() {
    let (mut runner, imp, eureka, peacekeeper) =
        eureka_peacekeeper_board(eureka_peacekeeper_seed());
    let before = life(runner.state(), P2);
    let mut events = eureka_to_peacekeeper_choice(&mut runner, imp, eureka);
    let counters = plus_counters(runner.state(), imp);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::NamedChoice { player, .. } if player == P1
    ));
    assert_eq!(
        parked_state_of(runner.state(), imp),
        Some(ParkedEntryState::Completed)
    );
    assert!(is_pending(runner.state(), peacekeeper));

    concede(&mut runner, P1, &mut events);
    settle(&mut runner, &mut events, DECLINE_ANSWERS);

    assert_eq!(life(runner.state(), P2), before + 1);
    assert_eq!(zone(runner.state(), imp), Zone::Battlefield);
    assert_eq!(plus_counters(runner.state(), imp), counters);
    assert!(!runner.state().players[0].is_eliminated);
    assert_ne!(zone(runner.state(), peacekeeper), Zone::Battlefield);
    assert!(runner.state().deferred_entry_events.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

/// V23's paired positive: both entries complete, in the engine's merged order.
#[test]
fn both_seats_entries_complete_on_eureka() {
    let (mut runner, imp, eureka, peacekeeper) =
        eureka_peacekeeper_board(eureka_peacekeeper_seed());
    let before = life(runner.state(), P2);
    let mut events = eureka_to_peacekeeper_choice(&mut runner, imp, eureka);
    answer_named_choice(&mut runner, &mut events);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P2
        ),
        "{:?}",
        runner.state().waiting_for
    );
    act(
        &mut runner,
        GameAction::DecideOptionalEffect { accept: false },
        &mut events,
    );
    answer_card_name(&mut runner, &mut events);
    settle(&mut runner, &mut events, DECLINE_ANSWERS);
    assert_eq!(life(runner.state(), P2), before + 2);
    assert_eq!(zone(runner.state(), peacekeeper), Zone::Battlefield);
}

/// Eureka with Jumbo Imp as P0's card, Anointed Peacekeeper as P1's and Clone as
/// P2's, Adaptive Automaton on the battlefield, and Soul Warden with P0.
fn eureka_three_entrants_board(
    seed: u64,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = board3(seed);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
    let donor = scenario
        .add_creature_from_oracle(P0, "Adaptive Automaton", 2, 1, ADAPTIVE_AUTOMATON)
        .id();
    let imp = free_creature(&mut scenario, "Jumbo Imp", 0, 0, JUMBO_IMP);
    let eureka = free_spell(&mut scenario, "Eureka", false, EUREKA);
    let peacekeeper = {
        let mut builder = scenario.add_creature_to_hand(P1, "Anointed Peacekeeper", 3, 3);
        builder.from_oracle_text_with_keywords(&["vigilance"], ANOINTED_PEACEKEEPER);
        builder.with_mana_cost(ManaCost::generic(0));
        builder.id()
    };
    let clone = scenario
        .add_creature_to_hand_from_oracle(P2, "Clone", 0, 0, CLONE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    seed_card_names(&mut runner);
    (runner, imp, eureka, peacekeeper, clone, donor)
}

fn stack_frames(state: &GameState) -> Vec<FrameKind> {
    state
        .resolution_stack
        .iter()
        .map(ResolutionFrame::kind)
        .collect()
}

fn named_choice_player(state: &GameState) -> Option<PlayerId> {
    match state.waiting_for {
        WaitingFor::NamedChoice { player, .. } => Some(player),
        _ => None,
    }
}

/// V21. CR 603.2 + CR 800.4a: Clone's copied choice resolves under the drain of
/// Peacekeeper's unfinished entry, so it parks its own owner and its entry
/// completes with the answer. P1 leaving at CardName abandons only Peacekeeper's
/// entry, and Soul Warden sees Imp and Clone. P1 answering is the pair.
#[test]
fn a_copied_choice_outlives_another_entrants_abandonment() {
    let seed = find_seed("a Eureka Imp tie", |seed| {
        let (mut runner, imp, eureka, ..) = eureka_three_entrants_board(seed);
        put_until_roll(&mut runner, eureka, imp);
        tie_prompt_results(runner.state()).is_some()
    });
    for p1_concedes in [true, false] {
        let (mut runner, imp, eureka, peacekeeper, clone, donor) =
            eureka_three_entrants_board(seed);
        let before = life(runner.state(), P0);
        let mut events = eureka_to_peacekeeper_choice(&mut runner, imp, eureka);
        answer_named_choice(&mut runner, &mut events);
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { player, .. } if player == P2
            ),
            "{:?}",
            runner.state().waiting_for
        );
        act(
            &mut runner,
            GameAction::DecideOptionalEffect { accept: true },
            &mut events,
        );
        accept_replacement(&mut runner, &mut events);
        choose_copy_target(&mut runner, donor, &mut events);

        // Reach: the copied prompt, with Clone's owner parked above Peacekeeper's
        // drain and P2's continuation.
        assert_eq!(named_choice_player(runner.state()), Some(P2));
        assert_eq!(
            stack_frames(runner.state()),
            vec![
                FrameKind::PostReplacement,
                FrameKind::AbilityContinuation,
                FrameKind::PostReplacement
            ]
        );
        assert_eq!(
            runner
                .state()
                .resolution_stack
                .validate(&runner.state().waiting_for),
            Ok(())
        );
        assert!(is_pending(runner.state(), clone));
        let chosen = answer_named_choice(&mut runner, &mut events);

        // Reach: CardName is open in P1's seat, with only Peacekeeper's drain left.
        assert!(is_card_name_choice(runner.state()));
        assert_eq!(named_choice_player(runner.state()), Some(P1));
        assert_eq!(
            stack_frames(runner.state()),
            vec![FrameKind::PostReplacement]
        );
        assert_eq!(
            runner
                .state()
                .resolution_stack
                .validate(&runner.state().waiting_for),
            Ok(())
        );
        assert_eq!(
            parked_state_of(runner.state(), imp),
            Some(ParkedEntryState::Completed)
        );
        assert_eq!(
            parked_state_of(runner.state(), clone),
            Some(ParkedEntryState::Completed)
        );
        assert!(is_pending(runner.state(), peacekeeper));

        if p1_concedes {
            concede(&mut runner, P1, &mut events);
        } else {
            answer_card_name(&mut runner, &mut events);
        }
        settle(&mut runner, &mut events, DECLINE_ANSWERS);

        assert_eq!(
            life(runner.state(), P0),
            before + if p1_concedes { 2 } else { 3 },
            "p1_concedes = {p1_concedes}"
        );
        assert_eq!(zone(runner.state(), clone), Zone::Battlefield);
        assert!(runner.state().objects[&clone]
            .card_types
            .subtypes
            .contains(&chosen));
        assert_eq!(runner.state().players[1].is_eliminated, p1_concedes);
        assert_eq!(
            zone(runner.state(), peacekeeper) == Zone::Battlefield,
            !p1_concedes
        );
        assert!(runner.state().deferred_entry_events.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
    }
}

/// A donor on P1's battlefield, Clone in P0's hand, Soul Warden with P2.
fn clone_board3(
    seed: u64,
    donor_name: &str,
    donor_oracle: &str,
    donor_keywords: &[&str],
) -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = board3(seed);
    add_warden(&mut scenario);
    let donor = {
        let mut builder = scenario.add_creature(P1, donor_name, 3, 3);
        builder.from_oracle_text_with_keywords(donor_keywords, donor_oracle);
        builder.id()
    };
    let clone = free_creature(&mut scenario, "Clone", 0, 0, CLONE);
    (scenario, clone, donor)
}

/// Cast Clone, accept its optional entry and answer `CopyTargetChoice`.
fn clone_until_copied(
    runner: &mut GameRunner,
    clone: ObjectId,
    donor: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    cast_and_pass(runner, clone, None, None, events);
    accept_replacement(runner, events);
    choose_copy_target(runner, donor, events);
}

fn automaton_board(seed: u64) -> (GameScenario, ObjectId, ObjectId) {
    clone_board3(seed, "Adaptive Automaton", ADAPTIVE_AUTOMATON, &[])
}

/// V17. CR 614.12a + CR 707.9: the copy answer records nothing. The copied
/// choice is part of the entry, so conceding at it leaves the entry pending.
#[test]
fn conceding_at_a_copied_as_enters_choice_abandons_the_entry() {
    let (scenario, clone, donor) = automaton_board(7);
    let mut runner = scenario.build();
    let before = life(runner.state(), P2);
    let mut events = Vec::new();
    clone_until_copied(&mut runner, clone, donor, &mut events);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::NamedChoice { .. }
    ));
    assert!(is_pending(runner.state(), clone));
    concede(&mut runner, P0, &mut events);
    settle(&mut runner, &mut events, DECLINE_ANSWERS);
    assert_eq!(life(runner.state(), P2), before);
    assert!(runner.state().deferred_entry_events.is_empty());
}

/// V17's paired positive and bystander: the copied choice answered completes the
/// entry, and Clone has the chosen type.
#[test]
fn a_copied_as_enters_choice_completes_the_entry_when_answered() {
    for bystander_leaves in [false, true] {
        let (scenario, clone, donor) = automaton_board(7);
        let mut runner = scenario.build();
        let before = life(runner.state(), P2);
        let mut events = Vec::new();
        clone_until_copied(&mut runner, clone, donor, &mut events);
        if bystander_leaves {
            concede(&mut runner, P1, &mut events);
        }
        let chosen = answer_named_choice(&mut runner, &mut events);
        settle(&mut runner, &mut events, DECLINE_ANSWERS);
        assert_eq!(life(runner.state(), P2), before + 1);
        assert!(runner.state().objects[&clone]
            .card_types
            .subtypes
            .contains(&chosen));
    }
}

/// V17's counter-order sibling: a prompt after the copy, with no copied choice.
#[test]
fn conceding_at_a_copied_counter_order_prompt_abandons_the_entry() {
    let (mut scenario, clone, donor) = clone_board3(7, "Triskelion", TRISKELION, &[]);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
    let mut runner = scenario.build();
    let before = life(runner.state(), P2);
    let mut events = Vec::new();
    clone_until_copied(&mut runner, clone, donor, &mut events);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(is_pending(runner.state(), clone));
    concede(&mut runner, P0, &mut events);
    settle(&mut runner, &mut events, DECLINE_ANSWERS);
    assert_eq!(life(runner.state(), P2), before);
}

/// V17's Flash sibling: multi-eligible Flash rests the owner on its `ChangeZone`
/// frame, and the carrier abandonment takes both. Base panics on that top.
#[test]
fn conceding_at_a_copied_choice_under_flashs_tail_does_not_panic() {
    let mut scenario = board3(7);
    add_warden(&mut scenario);
    let donor = {
        let mut builder = scenario.add_creature(P1, "Adaptive Automaton", 3, 3);
        builder.from_oracle_text(ADAPTIVE_AUTOMATON);
        builder.id()
    };
    let clone = free_creature(&mut scenario, "Clone", 0, 0, CLONE);
    free_creature(&mut scenario, "Vanilla Bear", 2, 2, "");
    let flash = free_spell(&mut scenario, "Flash", true, FLASH);
    let mut runner = scenario.build();
    let before = life(runner.state(), P2);
    let mut events = put_until_roll(&mut runner, flash, clone);
    accept_replacement(&mut runner, &mut events);
    choose_copy_target(&mut runner, donor, &mut events);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::NamedChoice { .. }
    ));
    let frames: Vec<_> = runner
        .state()
        .resolution_stack
        .iter()
        .map(ResolutionFrame::kind)
        .collect();
    assert_eq!(
        frames,
        vec![
            FrameKind::AbilityContinuation,
            FrameKind::ChangeZone,
            FrameKind::PostReplacement
        ]
    );
    concede(&mut runner, P0, &mut events);
    settle(&mut runner, &mut events, DECLINE_ANSWERS);
    assert_eq!(life(runner.state(), P2), before);
    assert!(runner.state().players[0].is_eliminated);
}

fn fanatic_board(seed: u64) -> (GameScenario, ObjectId, ObjectId) {
    clone_board3(
        seed,
        "Fanatic of Xenagos",
        FANATIC_OF_XENAGOS,
        &["trample", "tribute:1"],
    )
}

/// Drive Clone copying Fanatic of Xenagos to the tribute decision.
fn clone_to_tribute(runner: &mut GameRunner, clone: ObjectId, donor: ObjectId) -> Vec<GameEvent> {
    let mut events = Vec::new();
    clone_until_copied(runner, clone, donor, &mut events);
    answer_named_choice(runner, &mut events);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TributeChoice { player, .. } if player == P1
        ),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(is_pending(runner.state(), clone));
    let resident = runner
        .state()
        .active_post_replacement_drains()
        .and_then(|drains| drains.resident())
        .expect("the copied choice has an owner");
    assert!(resident
        .parked_entrants
        .iter()
        .any(|entrant| entrant.object_id == clone));
    events
}

/// V18. CR 702.104a + CR 603.2: a copied choice whose tail prompts keeps an
/// owner: the tribute decision is part of the entry, and the drain under the
/// chain completes it, so the take collects it. An opponent paying gives the
/// counter too.
#[test]
fn a_copied_choice_with_a_prompting_tail_completes_through_its_owner() {
    for pay in [false, true] {
        let (scenario, clone, donor) = fanatic_board(7);
        let mut runner = scenario.build();
        let before = life(runner.state(), P2);
        let mut events = clone_to_tribute(&mut runner, clone, donor);
        act(
            &mut runner,
            GameAction::DecideOptionalEffect { accept: pay },
            &mut events,
        );
        settle(&mut runner, &mut events, DECLINE_ANSWERS);
        assert_eq!(life(runner.state(), P2), before + 1, "pay = {pay}");
        assert_eq!(plus_counters(runner.state(), clone), u32::from(pay));
        assert!(runner.state().resolution_stack.is_empty());
        assert!(runner.state().resolving_stack_entry.is_none());
    }
}

/// V18's concession: the tribute decision was not made, and the authority
/// refuses the gone entrant.
#[test]
fn conceding_at_a_copied_tribute_abandons_the_entry() {
    let (scenario, clone, donor) = fanatic_board(7);
    let mut runner = scenario.build();
    let before = life(runner.state(), P2);
    let mut events = clone_to_tribute(&mut runner, clone, donor);
    concede(&mut runner, P0, &mut events);
    // The prompt is P1's, so it stays open for P1.
    act(
        &mut runner,
        GameAction::DecideOptionalEffect { accept: false },
        &mut events,
    );
    settle(&mut runner, &mut events, DECLINE_ANSWERS);
    assert_eq!(life(runner.state(), P2), before);
}

/// V19. CR 603.2 + CR 603.10: an entrant its own continuation moved is not
/// refused. Flash's "sacrifice it unless you pay" is a later instruction of the
/// put, so the entry triggered Soul Warden whatever the sacrifice does after.
#[test]
fn a_later_instruction_moving_the_entrant_does_not_undo_its_entry() {
    fn flash_imp_board(seed: u64) -> (GameRunner, ObjectId, ObjectId) {
        let mut scenario = board3(seed);
        scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
        add_warden(&mut scenario);
        let imp = scenario
            .add_creature_to_hand_from_oracle(P0, "Jumbo Imp", 0, 0, JUMBO_IMP)
            .with_mana_cost(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Black],
            })
            .id();
        scenario.with_mana_pool(P0, green(3));
        let flash = free_spell(&mut scenario, "Flash", true, FLASH);
        (scenario.build(), imp, flash)
    }
    let seed = find_seed("a Flash Imp tie", |seed| {
        let (mut runner, imp, flash) = flash_imp_board(seed);
        put_until_roll(&mut runner, flash, imp);
        tie_prompt_results(runner.state()).is_some()
    });
    let (mut runner, imp, flash) = flash_imp_board(seed);
    let before = life(runner.state(), P2);
    let mut events = put_until_roll(&mut runner, flash, imp);
    answer_tie(&mut runner, &mut events);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(is_pending(runner.state(), imp));
    assert_eq!(plus_counters(runner.state(), imp), 0);
    act(
        &mut runner,
        GameAction::PayUnlessCost { pay: false },
        &mut events,
    );
    settle(&mut runner, &mut events, DECLINE_ANSWERS);
    assert_eq!(life(runner.state(), P2), before + 1);
    assert_eq!(zone(runner.state(), imp), Zone::Graveyard);
}

/// V19's Slime form: the printed Devour continuation, with Flash's tail merged
/// ahead of its `PutCounter`.
#[test]
fn a_later_instruction_moving_a_devour_entrant_does_not_undo_its_entry() {
    let mut scenario = board3(7);
    add_warden(&mut scenario);
    let victim = scenario.add_creature(P0, "Vanilla Bear", 2, 2).id();
    let slime = {
        let mut builder = scenario.add_creature_to_hand(P0, "Gluttonous Slime", 2, 2);
        builder.from_oracle_text_with_keywords(&["flash", "devour:1"], GLUTTONOUS_SLIME);
        builder.with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Green],
        });
        builder.id()
    };
    scenario.with_mana_pool(P0, green(3));
    let flash = free_spell(&mut scenario, "Flash", true, FLASH);
    let mut runner = scenario.build();
    let before = life(runner.state(), P2);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, flash, None, None, &mut events);
    act(
        &mut runner,
        GameAction::DecideOptionalEffect { accept: true },
        &mut events,
    );
    assert!(
        effect_zone_choice(runner.state()),
        "{:?}",
        runner.state().waiting_for
    );
    act(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![victim],
        },
        &mut events,
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    act(
        &mut runner,
        GameAction::PayUnlessCost { pay: false },
        &mut events,
    );
    settle(&mut runner, &mut events, DECLINE_ANSWERS);
    assert_eq!(life(runner.state(), P2), before + 1);
    assert_eq!(zone(runner.state(), slime), Zone::Graveyard);
}

fn scales_board3(seed: u64) -> (GameRunner, ObjectId) {
    let mut scenario = board3(seed);
    add_warden(&mut scenario);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
    let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 1);
    (scenario.build(), hydra)
}

/// V2. Decision 6, not a rule: the abandoned sibling of V1 on the teardown route.
/// The answered and bystander forms are its pair.
#[test]
fn an_abandoned_continuation_produces_no_entry() {
    let (mut runner, hydra) = scales_board3(7);
    let before = life(runner.state(), P2);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, hydra, Some(1), None, &mut events);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(is_pending(runner.state(), hydra));
    concede(&mut runner, P0, &mut events);
    settle(&mut runner, &mut events, DECLINE_ANSWERS);
    assert_eq!(life(runner.state(), P2), before);
    assert!(runner.state().deferred_entry_events.is_empty());
    assert!(runner.state().resolution_stack.is_empty());

    for bystander_leaves in [false, true] {
        let (mut runner, hydra) = scales_board3(7);
        let before = life(runner.state(), P2);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, hydra, Some(1), None, &mut events);
        if bystander_leaves {
            concede(&mut runner, P1, &mut events);
        }
        let index = replacement_index(runner.state(), "Hardened Scales");
        act(
            &mut runner,
            GameAction::ChooseReplacement { index },
            &mut events,
        );
        settle(&mut runner, &mut events, DECLINE_ANSWERS);
        assert_eq!(
            life(runner.state(), P2),
            before + 1,
            "bystander_leaves = {bystander_leaves}"
        );
    }
}

fn slime_board3() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = board3(7);
    add_warden(&mut scenario);
    let victim = scenario.add_creature(P0, "Vanilla Bear", 2, 2).id();
    let slime = gluttonous_slime(&mut scenario);
    (scenario.build(), slime, victim)
}

/// V3's pair: the answered Devour completes the entry. A concession at this
/// prompt leaves its frames on the resolution stack, so no row pins it.
#[test]
fn an_answered_devour_completes_the_entry_for_a_third_player() {
    let (mut runner, slime, victim) = slime_board3();
    let before = life(runner.state(), P2);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, slime, None, None, &mut events);
    assert!(is_pending(runner.state(), slime));
    act(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![victim],
        },
        &mut events,
    );
    settle(&mut runner, &mut events, DECLINE_ANSWERS);
    assert_eq!(life(runner.state(), P2), before + 1);
}

/// V9. CR 305.1: a land play a replacement kept off the battlefield reaches the
/// take completed, so "When you play another land" fires as it always has. Not
/// determined by the cards; preserved from base and labelled in `engine_replacement`.
#[test]
fn a_replaced_land_play_stays_collectable() {
    let mut scenario = board(7);
    let city = scenario
        .add_land_from_oracle(P0, "City of Traitors", CITY_OF_TRAITORS)
        .id();
    let vale = {
        let mut builder = scenario.add_land_to_hand(P0, "Lotus Vale");
        builder.from_oracle_text(LOTUS_VALE);
        builder.id()
    };
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&vale].card_id;
    let mut events = Vec::new();
    act(
        &mut runner,
        GameAction::PlayLand {
            object_id: vale,
            card_id,
        },
        &mut events,
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().deferred_entry_events.iter().any(|parked| {
            matches!(parked.event, GameEvent::LandPlayed { .. })
                && matches!(parked.state, ParkedEntryState::Pending { .. })
        }),
        "reach guard: the play is parked pending"
    );
    let decline = replacement_decline_index(runner.state());
    act(
        &mut runner,
        GameAction::ChooseReplacement { index: decline },
        &mut events,
    );
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert_eq!(zone(runner.state(), vale), Zone::Graveyard);
    assert_eq!(zone(runner.state(), city), Zone::Graveyard);
}

/// The last candidate of an optional replacement's choice is its decline.
fn replacement_decline_index(state: &GameState) -> usize {
    let WaitingFor::ReplacementChoice {
        candidate_count, ..
    } = &state.waiting_for
    else {
        panic!("expected ReplacementChoice, got {:?}", state.waiting_for);
    };
    candidate_count - 1
}

/// V10. A capture leaves another entrant's entry parked: Clone's copy prompt in
/// P2's seat does not erase Imp's completed entry, so P1's Soul Warden sees both.
/// Declining P2's put is the pair.
#[test]
fn a_mid_entry_capture_keeps_another_entrants_entry() {
    fn eureka_clone_board(seed: u64) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
        let mut scenario = board3(seed);
        scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
        scenario.add_creature_from_oracle(P1, "Soul Warden", 1, 1, SOUL_WARDEN);
        let donor = scenario.add_creature(P1, "Vanilla Bear", 2, 2).id();
        let imp = free_creature(&mut scenario, "Jumbo Imp", 0, 0, JUMBO_IMP);
        let eureka = free_spell(&mut scenario, "Eureka", false, EUREKA);
        scenario
            .add_creature_to_hand_from_oracle(P2, "Clone", 0, 0, CLONE)
            .with_mana_cost(ManaCost::generic(0));
        (scenario.build(), imp, eureka, donor)
    }
    /// Imp's entry completes and P1 declines, leaving P2's put open.
    fn to_p2_put(runner: &mut GameRunner, imp: ObjectId, eureka: ObjectId) -> Vec<GameEvent> {
        let mut events = put_until_roll(runner, eureka, imp);
        answer_tie(runner, &mut events);
        act(
            runner,
            GameAction::DecideOptionalEffect { accept: false },
            &mut events,
        );
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { player, .. } if player == P2
            ),
            "{:?}",
            runner.state().waiting_for
        );
        events
    }
    let seed = find_seed("a Eureka Imp tie", |seed| {
        let (mut runner, imp, eureka, _) = eureka_clone_board(seed);
        put_until_roll(&mut runner, eureka, imp);
        tie_prompt_results(runner.state()).is_some()
    });

    let (mut runner, imp, eureka, donor) = eureka_clone_board(seed);
    let before = life(runner.state(), P1);
    let mut events = to_p2_put(&mut runner, imp, eureka);
    act(
        &mut runner,
        GameAction::DecideOptionalEffect { accept: true },
        &mut events,
    );
    accept_replacement(&mut runner, &mut events);
    assert_eq!(
        parked_state_of(runner.state(), imp),
        Some(ParkedEntryState::Completed)
    );
    choose_copy_target(&mut runner, donor, &mut events);
    settle(&mut runner, &mut events, DECLINE_ANSWERS);
    assert_eq!(life(runner.state(), P1), before + 2);

    let (mut runner, imp, eureka, _) = eureka_clone_board(seed);
    let before = life(runner.state(), P1);
    let mut events = to_p2_put(&mut runner, imp, eureka);
    settle(&mut runner, &mut events, DECLINE_ANSWERS);
    assert_eq!(life(runner.state(), P1), before + 1);
}

// ---------------------------------------------------------------------------
// Delayed "enters this turn" triggers see a paused entry once
// ---------------------------------------------------------------------------

/// Cast First Day of Class's first sentence and resolve it; returns its id.
fn resolve_first_day_of_class(runner: &mut GameRunner, fdoc: ObjectId) {
    let mut events = Vec::new();
    cast_and_pass(runner, fdoc, None, None, &mut events);
    settle(runner, &mut events, DEFAULT_ANSWERS);
    assert!(
        !runner.state().delayed_triggers.is_empty(),
        "reach guard: First Day of Class's delayed trigger is installed"
    );
}

fn fdoc_hydra(seed: u64) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = board(seed);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let fdoc = free_spell(
        &mut scenario,
        "First Day of Class",
        false,
        FIRST_DAY_OF_CLASS,
    );
    let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 2);
    let mut runner = scenario.build();
    resolve_first_day_of_class(&mut runner, fdoc);
    (runner, hydra, fdoc)
}

/// R21. CR 603.2 + CR 603.7: a delayed "whenever a creature you control enters
/// this turn" sees a paused cast entry once, after its continuation completes.
#[test]
fn a_delayed_enters_trigger_sees_a_paused_cast_entry_once() {
    for tie in [true, false] {
        let seed = find_seed("an FDoC Hydra roll", |seed| {
            let (mut runner, hydra, _) = fdoc_hydra(seed);
            pauses_on_tie(&mut runner, hydra, 2).is_some() == tie
        });
        let (mut runner, hydra, fdoc) = fdoc_hydra(seed);
        reset_triggers_seen();
        let mut events = Vec::new();
        cast_and_pass(&mut runner, hydra, Some(2), None, &mut events);
        let survivors = if tie {
            let naturals = tie_prompt_results(runner.state()).expect("DieKeepChoice reached");
            assert_eq!(queued_or_stacked_from(runner.state(), fdoc), 0, "the hold");
            Some(survivors_total(&naturals))
        } else {
            None
        };
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let total = survivors.unwrap_or_else(|| die_results(&events).iter().sum());
        assert_eq!(
            plus_counters(runner.state(), hydra),
            total + 1,
            "tie = {tie}"
        );
        assert!(runner.state().objects[&hydra].has_keyword(&Keyword::Haste));
        assert_eq!(take_triggers_seen_from(fdoc), 1, "tie = {tie}");
    }
}

/// R24. CR 603.2 + CR 603.7: custody runs the paused member's delayed triggers
/// once, on the owner route.
#[test]
fn custody_runs_a_paused_members_delayed_triggers_once() {
    fn fdoc_zombify(seed: u64) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
        let mut scenario = board(seed);
        scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
        let fdoc = free_spell(
            &mut scenario,
            "First Day of Class",
            false,
            FIRST_DAY_OF_CLASS,
        );
        let imp = graveyard_creature(&mut scenario, "Jumbo Imp", JUMBO_IMP);
        let zombify = free_spell(&mut scenario, "Zombify", false, ZOMBIFY);
        let mut runner = scenario.build();
        resolve_first_day_of_class(&mut runner, fdoc);
        (runner, imp, zombify, fdoc)
    }
    for tie in [true, false] {
        let seed = find_seed("an FDoC Imp roll", |seed| {
            let (mut runner, imp, zombify, _) = fdoc_zombify(seed);
            let mut events = Vec::new();
            cast_and_pass(&mut runner, zombify, None, Some(imp), &mut events);
            tie_prompt_results(runner.state()).is_some() == tie
        });
        let (mut runner, imp, zombify, fdoc) = fdoc_zombify(seed);
        reset_triggers_seen();
        let mut events = Vec::new();
        cast_and_pass(&mut runner, zombify, None, Some(imp), &mut events);
        let survivors = if tie {
            let naturals = tie_prompt_results(runner.state()).expect("DieKeepChoice reached");
            let WaitingFor::DieKeepChoice {
                ignorable_indices,
                ignore_count,
                ..
            } = runner.state().waiting_for.clone()
            else {
                unreachable!()
            };
            act(
                &mut runner,
                GameAction::SelectDieRolls {
                    ignore_indices: ignorable_indices.into_iter().take(ignore_count).collect(),
                },
                &mut events,
            );
            assert!(runner.state().active_change_zone_frame().is_none());
            Some(survivors_total(&naturals))
        } else {
            None
        };
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let total = survivors.unwrap_or_else(|| die_results(&events).iter().sum());
        assert_eq!(plus_counters(runner.state(), imp), total + 1, "tie = {tie}");
        assert!(runner.state().objects[&imp].has_keyword(&Keyword::Haste));
        assert_eq!(take_triggers_seen_from(fdoc), 1, "tie = {tie}");
    }
}

/// R22. CR 603.2 + CR 603.7 + CR 603.3b: Beck's delayed trigger sees a Devour
/// entry once, in the sacrifice handler's drain beside the dies trigger.
#[test]
fn a_delayed_enters_trigger_sees_a_devour_entry_once() {
    for devour in [true, false] {
        let mut scenario = board(7);
        let beck = free_spell(&mut scenario, "Beck", false, BECK);
        let artist = scenario
            .add_creature_from_oracle(P0, "Blood Artist", 0, 1, BLOOD_ARTIST)
            .id();
        let victim = scenario.add_vanilla(P0, 2, 2);
        let elder = thunder_thrash_elder(&mut scenario);
        let mut runner = scenario.build();
        let mut events = Vec::new();
        cast_and_pass(&mut runner, beck, None, None, &mut events);
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        assert!(!runner.state().delayed_triggers.is_empty());
        let hand_before = hand(runner.state(), P0);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, elder, None, None, &mut events);
        assert!(effect_zone_choice(runner.state()));
        let cards = if devour { vec![victim] } else { vec![] };
        act(&mut runner, GameAction::SelectCards { cards }, &mut events);
        assert_eq!(
            plus_counters(runner.state(), elder),
            if devour { 3 } else { 0 }
        );
        if devour {
            match &runner.state().waiting_for {
                WaitingFor::OrderTriggers { triggers, .. } => {
                    let sources: Vec<ObjectId> = triggers.iter().map(|t| t.source_id).collect();
                    assert!(sources.contains(&artist), "{sources:?}");
                    assert!(sources.contains(&beck), "{sources:?}");
                }
                other => panic!("one drain for both triggers, got {other:?}"),
            }
        }
        let settled = settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let beck_offers = settled
            .optional_offers
            .iter()
            .filter(|source| **source == beck)
            .count();
        assert_eq!(beck_offers, 1, "devour = {devour}");
        assert_eq!(hand(runner.state(), P0) + 1 - hand_before, 1);
    }
}

// ---------------------------------------------------------------------------
// The release runs before the settling action's SBAs and its drain gate
// ---------------------------------------------------------------------------

fn slime_board(seed: u64, with_norn: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = board(seed);
    if with_norn {
        scenario.add_creature_from_oracle(P1, "Elesh Norn, Grand Cenobite", 4, 7, ELESH_NORN);
    }
    scenario.add_enchantment_from_oracle(P1, "Night of Souls' Betrayal", NIGHT_OF_SOULS_BETRAYAL);
    scenario.add_enchantment_from_oracle(P0, "Ajani's Welcome", AJANIS_WELCOME);
    let big = scenario.add_vanilla(P0, 4, 4);
    let slime = gluttonous_slime(&mut scenario);
    (scenario.build(), big, slime)
}

/// R23. CR 603.2 + CR 603.10: a completed entry's enters triggers survive the
/// entrant's SBA death in the settling action.
#[test]
fn a_completed_entry_triggers_even_if_the_entrant_dies_to_sbas() {
    for (with_norn, devour) in [(true, true), (false, true), (true, false)] {
        let (mut runner, big, slime) = slime_board(7, with_norn);
        let life_before = life(runner.state(), P0);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, slime, None, None, &mut events);
        assert!(effect_zone_choice(runner.state()));
        assert!(
            runner.state().deferred_triggers.is_empty(),
            "the hold: nothing is queued at the prompt"
        );
        let cards = if devour { vec![big] } else { vec![] };
        let answer = runner
            .act(GameAction::SelectCards { cards })
            .expect("devour answer");
        if devour {
            assert_eq!(zone(runner.state(), big), Zone::Graveyard);
            let placed: u32 = answer
                .events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::CounterAdded {
                        object_id,
                        counter_type: CounterType::Plus1Plus1,
                        count,
                        ..
                    } if *object_id == slime => Some(*count),
                    _ => None,
                })
                .sum();
            assert_eq!(placed, 1);
        }
        let dies = with_norn;
        if dies {
            assert_eq!(zone(runner.state(), slime), Zone::Graveyard, "0/0 or -1/-1");
        }
        events.extend(answer.events);
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        assert_eq!(
            zone(runner.state(), slime),
            if dies {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            }
        );
        assert_eq!(
            life(runner.state(), P0),
            life_before + 1,
            "norn = {with_norn}, devour = {devour}"
        );
    }
}

/// R25. CR 603.3b: a refused drain keeps the released entry queued with the
/// dies context; both are placed together.
#[test]
fn a_refused_drain_keeps_the_released_entry_beside_the_dies_context() {
    for with_opt in [true, false] {
        let mut scenario = board(7);
        let artist = scenario
            .add_creature_from_oracle(P0, "Blood Artist", 0, 1, BLOOD_ARTIST)
            .id();
        let bond = scenario
            .add_enchantment_from_oracle(P0, "Elemental Bond", ELEMENTAL_BOND)
            .id();
        let victim = scenario.add_vanilla(P0, 2, 2);
        let opt = free_spell(&mut scenario, "Opt", true, OPT);
        let slime = gluttonous_slime(&mut scenario);
        let mut runner = scenario.build();
        let hand_before = hand(runner.state(), P0);
        let (p0_before, p1_before) = (life(runner.state(), P0), life(runner.state(), P1));
        let mut events = Vec::new();
        if with_opt {
            let commit = runner.cast(opt).commit();
            events.extend(commit.events().iter().cloned());
        }
        {
            let commit = runner.cast(slime).commit();
            events.extend(commit.events().iter().cloned());
        }
        // Resolve only the Slime (the top of the stack).
        act(&mut runner, GameAction::PassPriority, &mut events);
        act(&mut runner, GameAction::PassPriority, &mut events);
        assert!(
            effect_zone_choice(runner.state()),
            "{:?}",
            runner.state().waiting_for
        );
        act(
            &mut runner,
            GameAction::SelectCards {
                cards: vec![victim],
            },
            &mut events,
        );
        assert_eq!(plus_counters(runner.state(), slime), 1);
        let state = runner.state();
        let queued = |source| {
            state
                .deferred_triggers
                .iter()
                .any(|context| context.pending.source_id == source)
        };
        let ordering = match &state.waiting_for {
            WaitingFor::OrderTriggers { triggers, .. } => {
                triggers.iter().map(|t| t.source_id).collect::<Vec<_>>()
            }
            _ => Vec::new(),
        };
        let stacked = |source| {
            state.stack.iter().any(|entry| entry.source_id == source) || ordering.contains(&source)
        };
        assert_eq!(
            queued(artist),
            queued(bond),
            "same place: {:?}",
            state.waiting_for
        );
        assert_eq!(
            stacked(artist),
            stacked(bond),
            "same place: {:?}",
            state.waiting_for
        );
        if with_opt {
            assert!(
                state.stack.iter().any(|entry| entry.id == opt),
                "Opt still on the stack"
            );
        } else {
            assert!(
                ordering.contains(&artist) && ordering.contains(&bond),
                "{ordering:?}"
            );
        }
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        // Cards cast from hand: the Slime, and Opt when it is cast; drawn: one
        // Elemental Bond draw, and Opt's own draw.
        let opt_cards = usize::from(with_opt);
        assert_eq!(
            hand(runner.state(), P0) + 1 + opt_cards - hand_before,
            1 + opt_cards
        );
        assert_eq!(life(runner.state(), P1), p1_before - 1);
        assert_eq!(life(runner.state(), P0), p0_before + 1);
    }
}

/// R2 (coverage half). The as-enters roll line leaves no `enters_with_clause`
/// gap on any class face; the game-side gap authority (`card_face_gaps`)
/// agrees with the parse. Temp of the Damned's upkeep "If you can't,
/// sacrifice it" rider is a separate strict-failure gap
/// (`cant_rider_without_zone_change_antecedent`, CR 608.2c): its generic guard
/// reads the zone-change ledger, which a counter removal never writes, so it
/// stays a gap until the guard is repaired. Jumbo Imp's end-step line ("remove
/// a number of +1/+1 counters … equal to the result") is a separate,
/// pre-existing `unparsed_quantity` gap.
#[test]
fn as_enters_roll_faces_carry_no_enters_with_gap() {
    for (name, oracle, keyword_names, expected_gaps) in [
        (
            "Neverwinter Hydra",
            NEVERWINTER_HYDRA,
            vec!["Trample", "Ward"],
            vec![],
        ),
        ("Hydradoodle", HYDRADOODLE, vec!["Reach", "Trample"], vec![]),
        ("Hammer Jammer", HAMMER_JAMMER, vec![], vec![]),
        (
            "Jumbo Imp",
            JUMBO_IMP,
            vec!["Flying"],
            vec!["Effect:unparsed_quantity"],
        ),
        (
            "Temp of the Damned",
            TEMP_OF_THE_DAMNED,
            vec![],
            vec!["Effect:cant_rider_without_zone_change_antecedent"],
        ),
    ] {
        let keyword_names: Vec<String> = keyword_names.iter().map(|k| k.to_string()).collect();
        let parsed = engine::parser::parse_oracle_text(
            oracle,
            name,
            &keyword_names,
            &["Creature".to_string()],
            &[],
        );
        // Reach guard: the as-enters roll composed into one entry replacement.
        assert!(
            parsed.replacements.iter().any(|repl| repl
                .execute
                .as_deref()
                .is_some_and(|execute| matches!(&*execute.effect, Effect::RollDie { .. }))),
            "{name}: {:#?}",
            parsed.replacements
        );
        let face = engine::types::card::CardFace {
            name: name.to_string(),
            oracle_text: Some(oracle.to_string()),
            keywords: parsed.extracted_keywords.clone(),
            abilities: parsed.abilities,
            triggers: parsed.triggers,
            static_abilities: parsed.statics,
            replacements: parsed.replacements,
            ..Default::default()
        };
        let gaps = engine::game::coverage::card_face_gaps(&face);
        assert!(
            gaps.iter().all(|gap| gap != "Effect:enters_with_clause"),
            "{name}: {gaps:?}"
        );
        let expected: Vec<String> = expected_gaps.iter().map(|g| g.to_string()).collect();
        assert_eq!(gaps, expected, "{name}");
    }
}

// ---------------------------------------------------------------------------
// An entrant does not observe its own as-enters roll
// ---------------------------------------------------------------------------

/// Which of Hardened Scales and Branching Evolution a counter prompt picks first.
#[derive(Clone, Copy, Debug)]
enum FirstPick {
    Scales,
    Evolution,
}

impl FirstPick {
    const BOTH: [FirstPick; 2] = [FirstPick::Scales, FirstPick::Evolution];

    fn source_name(self) -> &'static str {
        match self {
            FirstPick::Scales => "Hardened Scales",
            FirstPick::Evolution => "Branching Evolution",
        }
    }

    /// CR 614.5 + CR 616.1: `n` +1/+1 counters are one number, which each
    /// replacement modifies once, in the order chosen once (the two cards'
    /// rulings on entering with counters).
    fn counters(self, n: u32) -> u32 {
        match self {
            FirstPick::Scales => (n + 1) * 2,
            FirstPick::Evolution => n * 2 + 1,
        }
    }
}

/// Is this `ReplacementChoice` the +1/+1 counter ordering, not an entry's?
fn is_counter_prompt(state: &GameState) -> bool {
    matches!(
        &state.waiting_for,
        WaitingFor::ReplacementChoice { candidates, .. }
            if candidates.iter().any(|c| c.source_name == "Hardened Scales")
    )
}

fn assert_counter_prompt(state: &GameState) {
    assert!(
        is_counter_prompt(state),
        "expected the counter prompt, got {:?} on {:?}",
        state.waiting_for,
        stack_frames(state)
    );
}

fn counters_of(state: &GameState, id: ObjectId, kind: &CounterType) -> u32 {
    state.objects[&id].counters.get(kind).copied().unwrap_or(0)
}

fn vigilance_counter() -> CounterType {
    CounterType::Keyword(KeywordKind::Vigilance)
}

fn funk_counter() -> CounterType {
    CounterType::Generic("funk".to_string())
}

/// The `index`th line of a card's Oracle text, for a composed fixture face.
fn oracle_line(oracle: &str, index: usize) -> &str {
    oracle.lines().nth(index).expect("the card has that line")
}

fn free_jammer(scenario: &mut GameScenario) -> ObjectId {
    free_creature(scenario, "Hammer Jammer", 0, 0, HAMMER_JAMMER)
}

/// A Hammer Jammer already on the battlefield. Printed 0/0, so it carries a
/// +1/+1 counter to survive the first SBA check (CR 704.5f).
fn resident_jammer(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    let id = scenario
        .add_creature_from_oracle(player, "Hammer Jammer", 0, 0, HAMMER_JAMMER)
        .id();
    scenario.with_counter(id, CounterType::Plus1Plus1, 1);
    id
}

fn add_dwarf(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Brazen Dwarf", 1, 3, BRAZEN_DWARF)
        .id()
}

fn add_counter_replacements(scenario: &mut GameScenario) {
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
}

fn add_velocipede(scenario: &mut GameScenario) {
    scenario.add_artifact_from_oracle(P0, "Thunderous Velocipede", VELOCIPEDE);
}

fn add_grumgully(scenario: &mut GameScenario) {
    scenario.add_creature_from_oracle(P0, "Grumgully, the Generous", 3, 3, GRUMGULLY);
}

fn add_warden_p0(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id()
}

/// Seat `id`, staged in P0's hand, on top of P0's library. The scenario has no
/// library builder that keeps a card's printed power and toughness.
fn seat_on_library_top(runner: &mut GameRunner, id: ObjectId) {
    let state = runner.state_mut();
    state.players[0].hand.retain(|card| *card != id);
    state.players[0].library.insert(0, id);
    state.objects.get_mut(&id).expect("staged card").zone = Zone::Library;
}

/// A {3}{R} Hammer Jammer in P0's hand, for `seat_on_library_top`.
fn jammer_card(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Hammer Jammer", 0, 0, HAMMER_JAMMER)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .id()
}

fn assert_dig_choice(state: &GameState) {
    assert!(
        matches!(state.waiting_for, WaitingFor::DigChoice { .. }),
        "{:?}",
        state.waiting_for
    );
}

fn the_token_copy(state: &GameState, name: &str) -> ObjectId {
    state
        .objects
        .values()
        .find(|object| object.is_token && object.name == name && object.zone == Zone::Battlefield)
        .map(|object| object.id)
        .expect("the token copy is on the battlefield")
}

/// Answer an entry's replacement ordering prompt, applying `source_name` first.
fn order_entry(runner: &mut GameRunner, source_name: &str, events: &mut Vec<GameEvent>) {
    assert!(
        !is_counter_prompt(runner.state()),
        "{:?}",
        runner.state().waiting_for
    );
    let index = replacement_index(runner.state(), source_name);
    act(runner, GameAction::ChooseReplacement { index }, events);
}

/// V2. CR 603.2 + CR 706.2: once Hammer Jammer is on the battlefield, a later roll
/// by its controller triggers it: all counters off, then that result.
#[test]
fn hammer_jammer_sees_a_later_roll() {
    fn later_roll_board(seed: u64) -> (GameRunner, ObjectId, ObjectId) {
        let mut scenario = board(seed);
        let jammer = free_jammer(&mut scenario);
        let box_of_goblins = free_spell(
            &mut scenario,
            "Box of Free-Range Goblins",
            false,
            BOX_OF_FREE_RANGE_GOBLINS,
        );
        (scenario.build(), jammer, box_of_goblins)
    }
    fn drive(runner: &mut GameRunner, jammer: ObjectId, spell: ObjectId) -> Vec<GameEvent> {
        let mut events = Vec::new();
        cast_and_pass(runner, jammer, None, None, &mut events);
        settle(runner, &mut events, DEFAULT_ANSWERS);
        reset_triggers_seen();
        cast_and_pass(runner, spell, None, None, &mut events);
        settle(runner, &mut events, DEFAULT_ANSWERS);
        events
    }
    // Two different results, so the reset is visible.
    let seed = find_seed("a later roll with another result", |seed| {
        let (mut runner, jammer, spell) = later_roll_board(seed);
        let rolls = die_results(&drive(&mut runner, jammer, spell));
        rolls.len() == 2 && rolls[0] != rolls[1]
    });
    let (mut runner, jammer, spell) = later_roll_board(seed);
    let rolls = die_results(&drive(&mut runner, jammer, spell));
    assert_eq!(take_triggers_seen_from(jammer), 1, "the later roll only");
    assert_eq!(plus_counters(runner.state(), jammer), rolls[1]);
    let goblins = runner
        .battlefield_names()
        .iter()
        .filter(|name| name.contains("Goblin"))
        .count();
    assert_eq!(goblins as u32, rolls[1]);
}

/// V3. CR 603.2 + CR 706.2: the stamp is for an entry. A permanent's own activated
/// roll is an ordinary roll, and the permanent that made it observes it.
#[test]
fn a_source_sees_a_roll_it_makes_outside_its_entry() {
    fn monoxa_board(seed: u64) -> (GameRunner, ObjectId) {
        let mut scenario = board(seed);
        let monoxa = scenario
            .add_creature_from_oracle(P0, "Monoxa, Midway Manager", 3, 3, MONOXA)
            .id();
        scenario.with_mana_pool(P0, green(6));
        (scenario.build(), monoxa)
    }
    fn activate(runner: &mut GameRunner, monoxa: ObjectId) -> Vec<GameEvent> {
        let mut events = Vec::new();
        act(
            runner,
            GameAction::ActivateAbility {
                source_id: monoxa,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            },
            &mut events,
        );
        settle(runner, &mut events, DEFAULT_ANSWERS);
        events
    }
    let seed = find_seed("a result of 3 or higher", |seed| {
        let (mut runner, monoxa) = monoxa_board(seed);
        die_results(&activate(&mut runner, monoxa))
            .first()
            .is_some_and(|&r| r >= 3)
    });
    reset_triggers_seen();
    let (mut runner, monoxa) = monoxa_board(seed);
    let rolls = die_results(&activate(&mut runner, monoxa));
    assert!(rolls.len() == 1 && rolls[0] >= 3, "{rolls:?}");
    assert_eq!(take_triggers_seen_from(monoxa), 1);
    assert!(runner.state().objects[&monoxa].has_keyword(&Keyword::FirstStrike));
}

/// V4. CR 706.6 + CR 603.10: the keep route. The ignored roll never happened, and
/// the kept one is seen by the resident Hammer Jammer and Brazen Dwarf, and not by
/// the entrant. With no tie the ignore is forced and inline, with the same counts.
#[test]
fn a_kept_as_enters_roll_is_seen_by_others_only() {
    fn keep_board(seed: u64) -> (GameRunner, ObjectId, ObjectId) {
        let mut scenario = board(seed);
        scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
        let resident = resident_jammer(&mut scenario, P0);
        add_dwarf(&mut scenario);
        let entrant = free_jammer(&mut scenario);
        (scenario.build(), resident, entrant)
    }
    for tie in [true, false] {
        let seed = find_seed("a keep-route roll", |seed| {
            let (mut runner, _, entrant) = keep_board(seed);
            let mut events = Vec::new();
            cast_and_pass(&mut runner, entrant, None, None, &mut events);
            tie_prompt_results(runner.state()).is_some() == tie
        });
        reset_triggers_seen();
        let (mut runner, resident, entrant) = keep_board(seed);
        let before = life(runner.state(), P1);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, entrant, None, None, &mut events);
        if tie {
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::DieKeepChoice { player, .. } if player == P0
                ),
                "{:?}",
                runner.state().waiting_for
            );
            assert!(is_pending(runner.state(), entrant));
        }
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let rolls = die_results(&events);
        assert_eq!(rolls.len(), 1, "tie = {tie}: {rolls:?}");
        assert_eq!(ignored_rolls(&events), 1, "tie = {tie}");
        assert_eq!(triggers_seen_from(entrant), 0, "tie = {tie}");
        assert_eq!(triggers_seen_from(resident), 1, "tie = {tie}");
        assert_eq!(before - life(runner.state(), P1), 1, "tie = {tie}");
        assert_eq!(plus_counters(runner.state(), entrant), rolls[0]);
        assert_eq!(plus_counters(runner.state(), resident), rolls[0]);
    }
}

/// V5. CR 614.12 + CR 113.6: the counter `ReplacementChoice`. The roll precedes the
/// pause, in the entry, so the entrant is not an observer of it.
#[test]
fn a_counter_pause_does_not_show_the_roll_to_its_roller() {
    for pick in FirstPick::BOTH {
        reset_triggers_seen();
        let mut scenario = board(7);
        add_counter_replacements(&mut scenario);
        add_dwarf(&mut scenario);
        let jammer = free_jammer(&mut scenario);
        let mut runner = scenario.build();
        let before = life(runner.state(), P1);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, jammer, None, None, &mut events);
        assert_counter_prompt(runner.state());
        assert!(is_pending(runner.state(), jammer));
        assert_eq!(queued_or_stacked_from(runner.state(), jammer), 0);
        let r = die_results(&events)[0];
        settle_picking(&mut runner, &mut events, pick);
        assert_eq!(triggers_seen_from(jammer), 0, "{pick:?}");
        assert_eq!(before - life(runner.state(), P1), 1, "{pick:?}");
        assert_eq!(plus_counters(runner.state(), jammer), pick.counters(r));
    }
}

/// V6. CR 614.12 + CR 113.6: under a logical owner, inline and through custody. The
/// owner collects the entry, and the entrant still does not see its roll.
#[test]
fn a_reanimated_entrant_does_not_see_its_own_roll() {
    fn zombify_board(seed: u64, barbarian: bool) -> (GameRunner, ObjectId, ObjectId) {
        let mut scenario = board(seed);
        add_dwarf(&mut scenario);
        if barbarian {
            scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
        }
        let jammer = graveyard_creature(&mut scenario, "Hammer Jammer", HAMMER_JAMMER);
        let zombify = free_spell(&mut scenario, "Zombify", false, ZOMBIFY);
        (scenario.build(), jammer, zombify)
    }
    // (a) inline.
    reset_triggers_seen();
    let (mut runner, jammer, zombify) = zombify_board(7, false);
    let before = life(runner.state(), P1);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, zombify, None, Some(jammer), &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 1, "{rolls:?}");
    assert_eq!(take_triggers_seen_from(jammer), 0);
    assert_eq!(before - life(runner.state(), P1), 1);
    assert_eq!(plus_counters(runner.state(), jammer), rolls[0]);

    // (b) custody, after a tie.
    let seed = find_seed("a reanimated tie", |seed| {
        let (mut runner, jammer, zombify) = zombify_board(seed, true);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, zombify, None, Some(jammer), &mut events);
        tie_prompt_results(runner.state()).is_some()
    });
    reset_triggers_seen();
    let (mut runner, jammer, zombify) = zombify_board(seed, true);
    let before = life(runner.state(), P1);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, zombify, None, Some(jammer), &mut events);
    let naturals = answer_tie(&mut runner, &mut events);
    assert!(
        runner.state().active_change_zone_frame().is_none(),
        "the owner completed in the answering action"
    );
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert_eq!(take_triggers_seen_from(jammer), 0);
    assert_eq!(before - life(runner.state(), P1), 1);
    assert_eq!(
        plus_counters(runner.state(), jammer),
        survivors_total(&naturals)
    );
}

/// V8. CR 616.1 + CR 706.6: the roll's own `ReplacementChoice`. Two replacements of
/// the roll are ordered, three dice roll, and two never happened.
#[test]
fn an_ordered_as_enters_roll_is_seen_by_others_only() {
    reset_triggers_seen();
    let mut scenario = board(7);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    scenario.add_enchantment_from_oracle(P0, "Pixie Guide", PIXIE_GUIDE);
    add_dwarf(&mut scenario);
    let jammer = free_jammer(&mut scenario);
    let mut runner = scenario.build();
    let before = life(runner.state(), P1);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, jammer, None, None, &mut events);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(die_results(&events).is_empty());
    assert!(is_pending(runner.state(), jammer));
    let index = replacement_index(runner.state(), "Barbarian Class");
    act(
        &mut runner,
        GameAction::ChooseReplacement { index },
        &mut events,
    );
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 1, "{rolls:?}");
    assert_eq!(ignored_rolls(&events), 2);
    assert_eq!(take_triggers_seen_from(jammer), 0);
    assert_eq!(before - life(runner.state(), P1), 1);
    assert_eq!(plus_counters(runner.state(), jammer), rolls[0]);
}

/// V9. CR 603.2 + CR 603.2c + CR 603.10: every other observer keeps the roll,
/// once per surviving die for "a die" and once per instruction for "one or more
/// dice". P1's Hammer Jammer does not see P0's roll: "you".
#[test]
fn other_observers_see_the_as_enters_roll() {
    for hydra_entrant in [false, true] {
        reset_triggers_seen();
        let mut scenario = board(7);
        let resident = resident_jammer(&mut scenario, P0);
        add_dwarf(&mut scenario);
        let bystander = resident_jammer(&mut scenario, P1);
        let entrant = if hydra_entrant {
            x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 2)
        } else {
            free_jammer(&mut scenario)
        };
        let mut runner = scenario.build();
        let before = life(runner.state(), P1);
        let mut events = Vec::new();
        let x = hydra_entrant.then_some(2);
        cast_and_pass(&mut runner, entrant, x, None, &mut events);
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let rolls = die_results(&events);
        assert_eq!(rolls.len(), if hydra_entrant { 2 } else { 1 });
        assert_eq!(triggers_seen_from(entrant), 0);
        assert_eq!(triggers_seen_from(resident), rolls.len());
        assert_eq!(triggers_seen_from(bystander), 0);
        assert_eq!(before - life(runner.state(), P1), 1);
        assert!(rolls.contains(&plus_counters(runner.state(), resident)));
    }
}

/// V10. CR 603.3b: the roll's observer and the entry's observer are placed in one
/// batch, the active player's first.
#[test]
fn the_roll_and_the_entry_trigger_in_one_batch() {
    let mut scenario = board(7);
    let dwarf = add_dwarf(&mut scenario);
    let warden = scenario
        .add_creature_from_oracle(P1, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 1);
    let mut runner = scenario.build();
    let (p1_before, mut events) = (life(runner.state(), P1), Vec::new());
    {
        let commit = runner.cast(hydra).x(1).commit();
        events.extend(commit.events().iter().cloned());
    }
    while zone(runner.state(), hydra) == Zone::Stack {
        act(&mut runner, GameAction::PassPriority, &mut events);
    }
    let state = runner.state();
    let position = |source| {
        state
            .stack
            .iter()
            .position(|entry| entry.source_id == source)
            .expect("the trigger is on the stack")
    };
    assert!(
        position(dwarf) < position(warden),
        "the active player's trigger is placed first"
    );
    assert!(state.deferred_triggers.is_empty());
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert_eq!(life(runner.state(), P1), p1_before, "-1, then +1");
}

/// V11. CR 603.3 + CR 117.5: a spell beneath the entrant does not hold the roll's
/// observers back. They are on the stack before anyone receives priority.
#[test]
fn a_spell_beneath_does_not_hold_the_roll_observers() {
    let mut scenario = board(7);
    let dwarf = add_dwarf(&mut scenario);
    let opt = free_spell(&mut scenario, "Opt", true, OPT);
    let jammer = free_keyword_creature(
        &mut scenario,
        "Hammer Jammer",
        (0, 0),
        &["flash"],
        HAMMER_JAMMER,
    );
    let mut runner = scenario.build();
    let mut events = Vec::new();
    for spell in [opt, jammer] {
        let commit = runner.cast(spell).commit();
        events.extend(commit.events().iter().cloned());
    }
    while zone(runner.state(), jammer) == Zone::Stack {
        act(&mut runner, GameAction::PassPriority, &mut events);
    }
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
    }
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        state.waiting_for
    );
    assert_eq!(state.stack[0].id, opt, "Opt still on the stack");
    assert!(
        state
            .stack
            .iter()
            .skip(1)
            .any(|entry| entry.source_id == dwarf),
        "Brazen Dwarf's trigger is above Opt"
    );
    assert!(state.deferred_triggers.is_empty());
}

/// V12. CR 603.2c + CR 113.6: the roll is seen in a later action, when a later
/// member of the batch pauses, and its record travels with it.
#[test]
fn a_batch_member_does_not_see_its_roll_after_a_later_member_pauses() {
    reset_triggers_seen();
    let mut scenario = board(7);
    let resident = resident_jammer(&mut scenario, P0);
    add_dwarf(&mut scenario);
    let jammer = jammer_card(&mut scenario);
    let pool = {
        let mut builder = scenario.add_land_to_library_top(P0, "Breeding Pool");
        builder.from_oracle_text(BREEDING_POOL);
        builder.id()
    };
    let festival = free_spell(
        &mut scenario,
        "Storm the Festival",
        false,
        STORM_THE_FESTIVAL,
    );
    let mut runner = scenario.build();
    seat_on_library_top(&mut runner, jammer);
    let before = life(runner.state(), P1);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, festival, None, None, &mut events);
    assert_dig_choice(runner.state());

    let mut delivered = Vec::new();
    act(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![jammer, pool],
        },
        &mut delivered,
    );
    assert_eq!(
        replacement_index(runner.state(), "Breeding Pool"),
        0,
        "Breeding Pool's prompt"
    );
    assert!(
        die_results(&delivered).is_empty(),
        "reach: the roll sits in the batch's deferred events"
    );
    events.extend(delivered);
    let decline = replacement_decline_index(runner.state());
    act(
        &mut runner,
        GameAction::ChooseReplacement { index: decline },
        &mut events,
    );
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);

    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 1, "{rolls:?}");
    assert_eq!(triggers_seen_from(jammer), 0);
    assert_eq!(triggers_seen_from(resident), 1);
    assert_eq!(plus_counters(runner.state(), jammer), rolls[0]);
    assert_eq!(plus_counters(runner.state(), resident), rolls[0]);
    assert_eq!(before - life(runner.state(), P1), 1);
    assert_eq!(zone(runner.state(), pool), Zone::Battlefield);
    assert!(runner.state().objects[&pool].tapped);
}

/// V16. CR 113.6: the refusal is at observer presence, so every roll observer shape
/// is covered: batched, a result filter, and each die. Composed faces, not printed
/// cards: each is the Hydra's as-enters line plus one observer line, cut to the
/// sentence under test for Monoxa. A resident with the same line sees the roll, so
/// each face can trigger.
#[test]
fn no_roll_observer_shape_sees_its_entrants_roll() {
    const NAME: &str = "Monoxa, Midway Manager";
    fn composed_board(seed: u64, observer: &str) -> (GameRunner, ObjectId, ObjectId) {
        let mut scenario = board(seed);
        let resident = scenario
            .add_creature_from_oracle(P0, NAME, 1, 3, observer)
            .id();
        let face = format!("{}\n{observer}", oracle_line(NEVERWINTER_HYDRA, 0));
        let entrant = x_creature(&mut scenario, NAME, &face, 2);
        (scenario.build(), resident, entrant)
    }
    let per_instruction = |_: &[u32]| 1;
    let three_or_higher = |rolls: &[u32]| rolls.iter().filter(|&&r| r >= 3).count();
    let each_die = |rolls: &[u32]| rolls.len();
    type Expected<'a> = &'a dyn Fn(&[u32]) -> usize;
    let shapes: [(&str, Expected); 3] = [
        (BRAZEN_DWARF, &per_instruction),
        (oracle_line(MONOXA, 0), &three_or_higher),
        (oracle_line(HAMMER_JAMMER, 1), &each_die),
    ];
    for (observer, expected) in shapes {
        let seed = find_seed("a result of 3 or higher", |seed| {
            let (mut runner, _, entrant) = composed_board(seed, observer);
            let mut events = Vec::new();
            cast_and_pass(&mut runner, entrant, Some(2), None, &mut events);
            die_results(&events).iter().any(|&r| r >= 3)
        });
        reset_triggers_seen();
        let (mut runner, resident, entrant) = composed_board(seed, observer);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, entrant, Some(2), None, &mut events);
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let rolls = die_results(&events);
        assert_eq!(triggers_seen_from(entrant), 0, "{observer}");
        assert_eq!(triggers_seen_from(resident), expected(&rolls), "{observer}");
        assert!(expected(&rolls) >= 1, "{observer}");
    }
}

/// V17. CR 122.6 (Fathom Mage's ruling): the counters it enters with are part of
/// entering, so its own counter observer sees them. Only the roll is refused. A
/// composed face: Hammer Jammer's as-enters line and Fathom Mage's last line.
#[test]
fn an_entrants_counter_observer_still_sees_its_counters() {
    let mut scenario = board(7);
    let face = format!("{}\n{FATHOM_MAGE}", oracle_line(HAMMER_JAMMER, 0));
    let entrant = free_creature(&mut scenario, "Fathom Mage", 0, 0, &face);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    cast_and_pass(&mut runner, entrant, None, None, &mut events);
    let settled = settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 1, "{rolls:?}");
    assert_eq!(plus_counters(runner.state(), entrant), rolls[0]);
    let offers = settled
        .optional_offers
        .iter()
        .filter(|source| **source == entrant)
        .count();
    assert_eq!(offers as u32, rolls[0]);
}

fn stolen_identity_board(seed: u64) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = board(seed);
    let resident = resident_jammer(&mut scenario, P0);
    let spell = free_spell(&mut scenario, "Stolen Identity", false, STOLEN_IDENTITY);
    (scenario.build(), resident, spell)
}

/// V18. Stolen Identity's ruling + CR 614.12: a token copy rolls too, and it is not
/// on the battlefield at the roll, so it is no candidate at all.
#[test]
fn a_token_copy_does_not_see_its_own_roll() {
    reset_triggers_seen();
    let (mut runner, resident, spell) = stolen_identity_board(7);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, spell, None, Some(resident), &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 1, "{rolls:?}");
    let token = the_token_copy(runner.state(), "Hammer Jammer");
    assert_eq!(triggers_seen_from(token), 0);
    assert_eq!(triggers_seen_from(resident), 1);
    assert_eq!(plus_counters(runner.state(), token), rolls[0]);
}

/// What gives the entrant a fold in V20's rows.
#[derive(Clone, Copy, Debug)]
enum Fold {
    Velocipede,
    /// Grumgully, with the named replacement ordered first.
    Grumgully(&'static str),
    /// Rhythm of the Wild, riot ordered first and accepted.
    Riot,
}

struct FoldBoard {
    runner: GameRunner,
    jammer: ObjectId,
    warden: ObjectId,
    /// Zombify, when the fold's route is the owner's.
    zombify: Option<ObjectId>,
}

fn fold_board(seed: u64, fold: Fold, reanimated: bool) -> FoldBoard {
    let mut scenario = board(seed);
    add_counter_replacements(&mut scenario);
    add_dwarf(&mut scenario);
    let warden = add_warden_p0(&mut scenario);
    match fold {
        Fold::Velocipede => add_velocipede(&mut scenario),
        Fold::Grumgully(_) => add_grumgully(&mut scenario),
        Fold::Riot => {
            scenario.add_enchantment_from_oracle(P0, "Rhythm of the Wild", RHYTHM_OF_THE_WILD);
        }
    }
    let (jammer, zombify) = if reanimated {
        let jammer = graveyard_creature(&mut scenario, "Hammer Jammer", HAMMER_JAMMER);
        let zombify = free_spell(&mut scenario, "Zombify", false, ZOMBIFY);
        (jammer, Some(zombify))
    } else {
        (free_jammer(&mut scenario), None)
    };
    FoldBoard {
        runner: scenario.build(),
        jammer,
        warden,
        zombify,
    }
}

/// Cast the board's spell, and answer any entry ordering, to its counter prompt.
fn to_counter_prompt(board: &mut FoldBoard, fold: Fold, events: &mut Vec<GameEvent>) {
    let (spell, target) = match board.zombify {
        Some(zombify) => (zombify, Some(board.jammer)),
        None => (board.jammer, None),
    };
    cast_and_pass(&mut board.runner, spell, None, target, events);
    match fold {
        Fold::Velocipede => {}
        Fold::Grumgully(first) => order_entry(&mut board.runner, first, events),
        Fold::Riot => {
            order_entry(&mut board.runner, "Rhythm of the Wild", events);
            accept_replacement(&mut board.runner, events);
        }
    }
}

fn printed_chain(state: &GameState, id: ObjectId) -> Option<AbilityDefinition> {
    state.objects[&id]
        .replacement_definitions
        .iter_unchecked()
        .find(|def| def.event == ReplacementEvent::Moved)
        .and_then(|def| def.execute.as_deref().cloned())
}

/// One row of V20: `fold` on the route, through the counter prompt, to settled.
fn run_fold_row(fold: Fold, reanimated: bool, pick: FirstPick) {
    let what = format!("{fold:?}, reanimated = {reanimated}, {pick:?}");
    reset_triggers_seen();
    let mut board = fold_board(7, fold, reanimated);
    let printed = printed_chain(board.runner.state(), board.jammer);
    let before = life(board.runner.state(), P1);
    let warden_life = life(board.runner.state(), P0);
    let mut events = Vec::new();
    to_counter_prompt(&mut board, fold, &mut events);

    let FoldBoard {
        mut runner,
        jammer,
        warden,
        ..
    } = board;
    assert_counter_prompt(runner.state());
    assert_eq!(die_results(&events).len(), 1, "{what}");
    assert!(is_pending(runner.state(), jammer), "{what}");
    assert_eq!(plus_counters(runner.state(), jammer), 0, "{what}");
    assert_eq!(queued_or_stacked_from(runner.state(), warden), 0, "{what}");

    let settled = settle_picking(&mut runner, &mut events, pick);
    let r = die_results(&events)[0];
    assert_eq!(settled.counter_prompts, 1, "{what}");
    assert_eq!(
        plus_counters(runner.state(), jammer),
        pick.counters(r + 1),
        "{what}, r = {r}"
    );
    assert_eq!(triggers_seen_from(jammer), 0, "{what}");
    // Known gap: behind an ordering prompt, the other observer's trigger on this route.
    if matches!(fold, Fold::Velocipede) {
        assert_eq!(before - life(runner.state(), P1), 1, "{what}: Dwarf once");
    }
    assert_eq!(
        life(runner.state(), P0),
        warden_life + 1,
        "{what}: Warden once"
    );
    assert_eq!(
        printed_chain(runner.state(), jammer),
        printed,
        "{what}: the card's own chain is untouched, so a later entry starts clean"
    );
}

/// V20. CR 603.6d + CR 614.1c + CR 122.6 + CR 614.5: the roll and every counter the
/// entrant is given are inside the entry. No counter choice comes ahead of the
/// roll, they are one number, Scales and Evolution modify it once, and the entry is
/// observed after all of it. The frame shapes with no ordering prompt.
#[test]
fn a_fold_lands_with_the_roll_and_no_counter_prompt_precedes_it() {
    for reanimated in [false, true] {
        for pick in FirstPick::BOTH {
            run_fold_row(Fold::Velocipede, reanimated, pick);
        }
    }
}

/// V20's frame shapes behind an entry ordering prompt.
#[test]
fn a_fold_behind_an_ordering_prompt_lands_with_the_roll() {
    for (fold, reanimated) in [
        (Fold::Grumgully("Hammer Jammer"), false),
        (Fold::Grumgully("Grumgully, the Generous"), false),
        (Fold::Grumgully("Hammer Jammer"), true),
        (Fold::Riot, false),
    ] {
        for pick in FirstPick::BOTH {
            run_fold_row(fold, reanimated, pick);
        }
    }
}

/// V21. CR 603.6d + CR 603.10 (Kiora's ruling): with a fold the entry is observed
/// with its counters. Soul Warden cannot show this: a 0/0 and a 5/5 both enter.
#[test]
fn an_as_enters_roll_with_a_fold_is_observed_with_its_counters() {
    for pick in FirstPick::BOTH {
        let mut scenario = board(7);
        kiora(&mut scenario);
        add_counter_replacements(&mut scenario);
        add_velocipede(&mut scenario);
        let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 1);
        let mut runner = scenario.build();
        let hand_before = hand(runner.state(), P0);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, hydra, Some(1), None, &mut events);
        assert_counter_prompt(runner.state());
        settle_picking(&mut runner, &mut events, pick);
        let r = die_results(&events)[0];
        assert_eq!(plus_counters(runner.state(), hydra), pick.counters(r + 1));
        assert_eq!(hand(runner.state(), P0) + 1 - hand_before, 1, "{pick:?}");
    }
}

fn barbarian_grumgully_hydra(seed: u64) -> (GameRunner, ObjectId) {
    let mut scenario = board(seed);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    add_grumgully(&mut scenario);
    let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 1);
    (scenario.build(), hydra)
}

/// V22. CR 608.3a + CR 616.1f: a tie behind an ordering prompt finishes its spell.
/// The fold waits with the `PutCounter`, and the aggregate is the kept roll.
#[test]
fn a_tied_roll_behind_an_ordering_prompt_finishes_its_spell() {
    let to_prompt = |runner: &mut GameRunner, hydra, events: &mut Vec<GameEvent>| {
        cast_and_pass(runner, hydra, Some(1), None, events);
        order_entry(runner, "Neverwinter Hydra", events);
    };
    let seed = find_seed("a tie", |seed| {
        let (mut runner, hydra) = barbarian_grumgully_hydra(seed);
        to_prompt(&mut runner, hydra, &mut Vec::new());
        tie_prompt_results(runner.state()).is_some()
    });
    let (mut runner, hydra) = barbarian_grumgully_hydra(seed);
    let mut events = Vec::new();
    to_prompt(&mut runner, hydra, &mut events);
    let naturals = answer_tie(&mut runner, &mut events);
    assert!(
        runner.state().resolution_stack.is_empty(),
        "the answer finished the spell: {:?}",
        stack_frames(runner.state())
    );
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert_eq!(zone(runner.state(), hydra), Zone::Battlefield);
    assert_eq!(
        plus_counters(runner.state(), hydra),
        survivors_total(&naturals) + 1
    );
}

fn free_fanatic(scenario: &mut GameScenario) -> ObjectId {
    free_keyword_creature(
        scenario,
        "Fanatic of Xenagos",
        (3, 3),
        &["trample", "tribute:1"],
        FANATIC_OF_XENAGOS,
    )
}

/// Answer Tribute's opponent choice, when one is asked, and decline the tribute.
fn decline_tribute(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    if matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }) {
        answer_named_choice(runner, events);
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::TributeChoice { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    act(
        runner,
        GameAction::DecideOptionalEffect { accept: false },
        events,
    );
}

/// V22b. CR 608.3a + CR 616.1f: the spell frame's finish is not roll-shaped. A
/// Tribute behind an ordering prompt finishes its spell too.
#[test]
fn a_tribute_behind_an_ordering_prompt_finishes_its_spell() {
    let mut scenario = board(7);
    add_grumgully(&mut scenario);
    let fanatic = free_fanatic(&mut scenario);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    cast_and_pass(&mut runner, fanatic, None, None, &mut events);
    order_entry(&mut runner, "Fanatic of Xenagos", &mut events);
    decline_tribute(&mut runner, &mut events);
    assert!(
        runner.state().resolution_stack.is_empty(),
        "the answer finished the spell: {:?}",
        stack_frames(runner.state())
    );
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert_eq!(zone(runner.state(), fanatic), Zone::Battlefield);
}

/// Storm the Festival over Hammer Jammer and a Forest, with a fold, Scales,
/// Evolution, Brazen Dwarf and Soul Warden.
struct FestivalBoard {
    runner: GameRunner,
    jammer: ObjectId,
    forest: ObjectId,
    warden: ObjectId,
    festival: ObjectId,
}

fn festival_board(seed: u64, add_fold: fn(&mut GameScenario)) -> FestivalBoard {
    let mut scenario = board(seed);
    add_fold(&mut scenario);
    add_counter_replacements(&mut scenario);
    add_dwarf(&mut scenario);
    let warden = add_warden_p0(&mut scenario);
    let jammer = jammer_card(&mut scenario);
    let forest = scenario.add_land_to_library_top(P0, "Forest").id();
    let festival = free_spell(
        &mut scenario,
        "Storm the Festival",
        false,
        STORM_THE_FESTIVAL,
    );
    let mut runner = scenario.build();
    seat_on_library_top(&mut runner, jammer);
    FestivalBoard {
        runner,
        jammer,
        forest,
        warden,
        festival,
    }
}

/// Every die the action has rolled, whether returned or held in the batch's
/// deferred events.
fn rolled_so_far(state: &GameState, events: &[GameEvent]) -> Vec<u32> {
    let mut rolls = die_results(events);
    if let Some(batch) = state.active_batch_delivery() {
        rolls.extend(die_results(&batch.deferred_events));
    }
    rolls
}

fn run_festival(mut board: FestivalBoard, pick: FirstPick, ordering_prompt: bool) {
    reset_triggers_seen();
    let before = life(board.runner.state(), P1);
    let warden_life = life(board.runner.state(), P0);
    let mut events = Vec::new();
    let runner = &mut board.runner;
    cast_and_pass(runner, board.festival, None, None, &mut events);
    assert_dig_choice(runner.state());
    act(
        runner,
        GameAction::SelectCards {
            cards: vec![board.jammer, board.forest],
        },
        &mut events,
    );
    if ordering_prompt {
        order_entry(runner, "Hammer Jammer", &mut events);
    }
    assert_counter_prompt(runner.state());
    let rolled = rolled_so_far(runner.state(), &events);
    assert_eq!(rolled.len(), 1, "{pick:?}");
    assert!(is_pending(runner.state(), board.jammer), "{pick:?}");
    assert_eq!(queued_or_stacked_from(runner.state(), board.warden), 0);

    settle_picking(runner, &mut events, pick);
    let r = rolled[0];
    assert_eq!(triggers_seen_from(board.jammer), 0, "{pick:?}");
    assert_eq!(
        plus_counters(runner.state(), board.jammer),
        pick.counters(r + 1),
        "{pick:?}, r = {r}"
    );
    assert_eq!(zone(runner.state(), board.forest), Zone::Battlefield);
    // Known gap: behind an ordering prompt, the other observer's trigger on this route.
    if !ordering_prompt {
        assert_eq!(before - life(runner.state(), P1), 1, "Dwarf once");
    }
    assert_eq!(life(runner.state(), P0), warden_life + 1, "Warden once");
    assert!(runner.state().resolution_stack.is_empty());
}

/// V23. CR 603.6d + CR 603.2c: a mass member with a fold rolls before its counter
/// prompt, and the batch's custody takes its entry after the answer placed the
/// counters. The second member is a land, so one member pauses.
#[test]
fn a_mass_member_with_a_fold_rolls_before_its_counter_prompt() {
    for pick in FirstPick::BOTH {
        run_festival(festival_board(7, add_velocipede), pick, false);
    }
}

/// V23b. The same behind the entry's ordering prompt.
#[test]
fn a_mass_member_behind_an_ordering_prompt_rolls_before_its_counter_prompt() {
    for pick in FirstPick::BOTH {
        run_festival(festival_board(7, add_grumgully), pick, true);
    }
}

/// V24. The hand-on is the roll's alone: an entry with no roll keeps its counter
/// step. Not the card's reading for Tribute, whose counters are part of entering:
/// this pins the gate, and a fix for that class rewrites it.
#[test]
fn an_entry_with_no_roll_is_not_parked_at_a_counter_pause() {
    fn gate_board() -> (GameScenario, ObjectId) {
        let mut scenario = board(7);
        add_velocipede(&mut scenario);
        add_counter_replacements(&mut scenario);
        let warden = add_warden_p0(&mut scenario);
        (scenario, warden)
    }
    /// The entrant's own drain still holds its printed chain.
    fn assert_printed_chain(state: &GameState, entrant: ObjectId) {
        let printed = state.objects[&entrant]
            .replacement_definitions
            .iter_unchecked()
            .find(|def| def.event == ReplacementEvent::Moved)
            .and_then(|def| def.execute.clone())
            .expect("the entry replacement");
        let chain = stack_frames(state)
            .iter()
            .position(|kind| *kind == FrameKind::PostReplacement)
            .and_then(|slot| state.resolution_stack.iter().nth(slot))
            .and_then(|frame| match frame {
                ResolutionFrame::PostReplacement(drains) => drains.resident(),
                _ => None,
            })
            .map(|drain| drain.status.clone())
            .expect("a resident drain");
        assert_eq!(
            chain,
            DrainStatus::Ready(PostReplacementContinuation::Template(printed))
        );
    }
    // (a) Triskelion.
    let (mut scenario, _) = gate_board();
    let triskelion = free_creature(&mut scenario, "Triskelion", 1, 1, TRISKELION);
    let mut runner = scenario.build();
    let warden_life = life(runner.state(), P0);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, triskelion, None, None, &mut events);
    assert_counter_prompt(runner.state());
    assert_eq!(parked_state_of(runner.state(), triskelion), None);
    settle_picking(&mut runner, &mut events, FirstPick::Scales);
    assert_eq!(life(runner.state(), P0), warden_life + 1, "Warden once");

    // (b) Fanatic of Xenagos: the prompt comes ahead of the Tribute's, and the
    // chain under it is the card's own.
    let (mut scenario, _) = gate_board();
    let fanatic = free_fanatic(&mut scenario);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    cast_and_pass(&mut runner, fanatic, None, None, &mut events);
    assert_counter_prompt(runner.state());
    assert_eq!(parked_state_of(runner.state(), fanatic), None);
    assert_printed_chain(runner.state(), fanatic);

    // (c) Kavu Titan, kicked: its chain places +1/+1 counters on itself, behind a
    // head that is no roll, and the fold stays out of that placement.
    let (mut scenario, _) = gate_board();
    let titan = free_creature(&mut scenario, "Kavu Titan", 2, 2, KAVU_TITAN);
    scenario.with_mana_pool(P0, green(3));
    let mut runner = scenario.build();
    let mut events = Vec::new();
    let commit = runner.cast(titan).accept_optional().commit();
    events.extend(commit.events().iter().cloned());
    drop(commit);
    pass_until_prompt(&mut runner, &mut events);
    assert_counter_prompt(runner.state());
    assert_eq!(parked_state_of(runner.state(), titan), None);
    assert_printed_chain(runner.state(), titan);
}

/// V25. CR 122.1 + CR 614.5: each kind is its own placement, and Scales and
/// Evolution read only +1/+1 counters. A fold of another kind is not handed on and
/// keeps its counter step ahead of the roll. On Temp's board the roll's own action
/// then ends at Priority, so the roll stays where its observers see it.
#[test]
fn a_fold_of_another_kind_keeps_its_counter_step() {
    // (a) Temp's funk with Grumgully's +1/+1: the fold alone meets the replacements,
    // and the answer rolls, which Brazen Dwarf sees.
    for pick in FirstPick::BOTH {
        let mut scenario = board(7);
        add_grumgully(&mut scenario);
        add_counter_replacements(&mut scenario);
        add_dwarf(&mut scenario);
        let temp = free_creature(
            &mut scenario,
            "Temp of the Damned",
            0,
            0,
            TEMP_OF_THE_DAMNED,
        );
        let mut runner = scenario.build();
        let before = life(runner.state(), P1);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, temp, None, None, &mut events);
        order_entry(&mut runner, "Temp of the Damned", &mut events);
        assert_counter_prompt(runner.state());
        let rolled_at_prompt = die_results(&events).len();
        let settled = settle_picking(&mut runner, &mut events, pick);
        let rolls = die_results(&events);
        assert_eq!(rolls.len(), 1, "{pick:?}");
        let r = rolls[0];
        assert_eq!(settled.counter_prompts, 1, "{pick:?}");
        assert_eq!(counters_of(runner.state(), temp, &funk_counter()), r);
        assert_eq!(plus_counters(runner.state(), temp), pick.counters(1));
        assert_eq!(before - life(runner.state(), P1), 1, "Dwarf {pick:?}");
        assert_eq!(rolled_at_prompt, 0, "the counter step comes first");
    }

    // (b) Hammer Jammer with Tayam's vigilance, and nothing to pick.
    reset_triggers_seen();
    let mut scenario = board(7);
    scenario.add_creature_from_oracle(P0, "Tayam, Luminous Enigma", 3, 3, TAYAM);
    let jammer = free_jammer(&mut scenario);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    cast_and_pass(&mut runner, jammer, None, None, &mut events);
    order_entry(&mut runner, "Hammer Jammer", &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let r = die_results(&events)[0];
    assert_eq!(plus_counters(runner.state(), jammer), r);
    assert_eq!(counters_of(runner.state(), jammer, &vigilance_counter()), 1);
    assert_eq!(triggers_seen_from(jammer), 0);

    // (c) The rolled kind pauses, with the fold placed ahead of it.
    for pick in FirstPick::BOTH {
        reset_triggers_seen();
        let mut scenario = board(7);
        scenario.add_creature_from_oracle(P0, "Tayam, Luminous Enigma", 3, 3, TAYAM);
        add_counter_replacements(&mut scenario);
        let jammer = free_jammer(&mut scenario);
        let mut runner = scenario.build();
        let mut events = Vec::new();
        cast_and_pass(&mut runner, jammer, None, None, &mut events);
        order_entry(&mut runner, "Hammer Jammer", &mut events);
        assert_counter_prompt(runner.state());
        assert_eq!(die_results(&events).len(), 1, "{pick:?}");
        assert_eq!(counters_of(runner.state(), jammer, &vigilance_counter()), 1);
        let settled = settle_picking(&mut runner, &mut events, pick);
        let r = die_results(&events)[0];
        assert_eq!(settled.counter_prompts, 1, "{pick:?}");
        assert_eq!(plus_counters(runner.state(), jammer), pick.counters(r));
        assert_eq!(counters_of(runner.state(), jammer, &vigilance_counter()), 1);
    }
}

/// V26. CR 706.1 (Hydradoodle's ruling): X = 0 rolls no dice, and the fold still
/// lands, on the number zero plus the fold.
#[test]
fn an_x_of_zero_roll_still_lands_its_fold() {
    // (a) The fold alone.
    let mut scenario = board(7);
    add_velocipede(&mut scenario);
    let doodle = x_creature(&mut scenario, "Hydradoodle", HYDRADOODLE, 0);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    cast_and_pass(&mut runner, doodle, Some(0), None, &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert_eq!(roll_resolved_for(&events, doodle), 1);
    assert!(die_results(&events).is_empty());
    assert_eq!(zone(runner.state(), doodle), Zone::Battlefield);
    assert_eq!(plus_counters(runner.state(), doodle), 1);

    // (b) Under Scales and Evolution.
    for pick in FirstPick::BOTH {
        let mut scenario = board(7);
        add_velocipede(&mut scenario);
        add_counter_replacements(&mut scenario);
        let doodle = x_creature(&mut scenario, "Hydradoodle", HYDRADOODLE, 0);
        let mut runner = scenario.build();
        let mut events = Vec::new();
        cast_and_pass(&mut runner, doodle, Some(0), None, &mut events);
        settle_picking(&mut runner, &mut events, pick);
        assert_eq!(roll_resolved_for(&events, doodle), 1);
        assert!(die_results(&events).is_empty());
        assert_eq!(plus_counters(runner.state(), doodle), pick.counters(1));
    }
}

/// V27. CR 122.6 + CR 614.5: three contributions reach three lists, and they are one
/// number. Grumgully's and Velocipede's reach delivery apart, an effect's rides the
/// event, and a delayed one is pending for the object.
#[test]
fn every_entry_counter_of_one_kind_is_one_placement() {
    for pick in FirstPick::BOTH {
        // (a) The event's and the static's.
        let mut scenario = board(7);
        add_grumgully(&mut scenario);
        add_velocipede(&mut scenario);
        add_counter_replacements(&mut scenario);
        let jammer = free_jammer(&mut scenario);
        let mut runner = scenario.build();
        let mut events = Vec::new();
        cast_and_pass(&mut runner, jammer, None, None, &mut events);
        order_entry(&mut runner, "Hammer Jammer", &mut events);
        let settled = settle_picking(&mut runner, &mut events, pick);
        let r = die_results(&events)[0];
        assert_eq!(settled.counter_prompts, 1, "(a) {pick:?}");
        assert_eq!(plus_counters(runner.state(), jammer), pick.counters(r + 2));

        // (b) An effect's.
        let mut scenario = board(7);
        add_counter_replacements(&mut scenario);
        let jammer = graveyard_creature(&mut scenario, "Hammer Jammer", HAMMER_JAMMER);
        let spell = free_spell(&mut scenario, "Evil Reawakened", false, EVIL_REAWAKENED);
        let mut runner = scenario.build();
        let mut events = Vec::new();
        cast_and_pass(&mut runner, spell, None, Some(jammer), &mut events);
        let settled = settle_picking(&mut runner, &mut events, pick);
        let r = die_results(&events)[0];
        assert_eq!(settled.counter_prompts, 1, "(b) {pick:?}");
        assert_eq!(plus_counters(runner.state(), jammer), pick.counters(r + 2));

        // (c) A delayed one, which the success branch still clears.
        let mut scenario = board(7);
        add_counter_replacements(&mut scenario);
        let jammer = free_jammer(&mut scenario);
        let mut runner = scenario.build();
        runner
            .state_mut()
            .pending_etb_counters
            .push((jammer, CounterType::Plus1Plus1, 1));
        let mut events = Vec::new();
        cast_and_pass(&mut runner, jammer, None, None, &mut events);
        let settled = settle_picking(&mut runner, &mut events, pick);
        let r = die_results(&events)[0];
        assert_eq!(settled.counter_prompts, 1, "(c) {pick:?}");
        assert_eq!(plus_counters(runner.state(), jammer), pick.counters(r + 1));
        assert!(runner.state().pending_etb_counters.is_empty());
    }
}

/// V29. CR 616.1 + CR 706.6: behind the entry's ordering prompt and the roll's, the
/// entrant still gets the survivor plus the fold, and the spell finishes.
#[test]
fn a_fold_behind_an_ordered_roll_lands_with_the_survivor() {
    reset_triggers_seen();
    let mut scenario = board(7);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    scenario.add_enchantment_from_oracle(P0, "Pixie Guide", PIXIE_GUIDE);
    add_grumgully(&mut scenario);
    add_dwarf(&mut scenario);
    let jammer = free_jammer(&mut scenario);
    let mut runner = scenario.build();
    let before = life(runner.state(), P1);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, jammer, None, None, &mut events);
    order_entry(&mut runner, "Hammer Jammer", &mut events);
    assert!(die_results(&events).is_empty());
    let index = replacement_index(runner.state(), "Barbarian Class");
    act(
        &mut runner,
        GameAction::ChooseReplacement { index },
        &mut events,
    );
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 1, "{rolls:?}");
    assert_eq!(ignored_rolls(&events), 2);
    assert_eq!(triggers_seen_from(jammer), 0);
    assert_eq!(plus_counters(runner.state(), jammer), rolls[0] + 1);
    assert_eq!(before - life(runner.state(), P1), 1);
    assert!(runner.state().resolution_stack.is_empty());
}

/// V32. Yore-Tiller Nephilim + CR 508.4: an attacking entry keeps its counter step:
/// only that pause records `enters_attacking` for the resume, so the entrant is still
/// placed in combat after a chain that pauses. The hand-on is refused and Hammer
/// Jammer attacks. Not the card's reading for the count, which stays split on this
/// class: the row pins the refusal and asserts no count.
#[test]
fn an_attacking_entry_is_refused_the_hand_on_and_attacks() {
    let mut scenario = board(7);
    let nephilim = scenario
        .add_creature_from_oracle(P0, "Yore-Tiller Nephilim", 2, 2, YORE_TILLER_NEPHILIM)
        .id();
    add_velocipede(&mut scenario);
    add_counter_replacements(&mut scenario);
    let jammer = graveyard_creature(&mut scenario, "Hammer Jammer", HAMMER_JAMMER);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let attacked = runner
        .declare_attackers(&[(nephilim, AttackTarget::Player(P1))])
        .expect("Nephilim attacks");
    let mut events = attacked.events;
    for _ in 0..40 {
        if is_counter_prompt(runner.state()) {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => act(
                &mut runner,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(jammer)),
                },
                &mut events,
            ),
            WaitingFor::Priority { .. } => {
                act(&mut runner, GameAction::PassPriority, &mut events);
            }
            other => panic!("unhandled prompt: {other:?}"),
        }
    }
    assert_counter_prompt(runner.state());
    assert!(
        die_results(&events).is_empty(),
        "a counter step ran ahead of the roll, which only a refused hand-on does"
    );
    settle_picking(&mut runner, &mut events, FirstPick::Scales);
    assert_eq!(die_results(&events).len(), 1);
    let combat = runner.state().combat.as_ref().expect("combat");
    let attacker = combat
        .attackers
        .iter()
        .find(|attacker| attacker.object_id == jammer)
        .expect("Hammer Jammer attacks");
    assert_eq!(attacker.defending_player, P1);
}

/// V33. CR 122.1 + CR 122.6 + CR 614.5: an entry given counters of both kinds splits
/// them by kind. Tayam's vigilance counter stays on the counter step, which places it
/// with nothing to choose. Grumgully's +1/+1 counter joins the rolled counters as one
/// number, so the roll comes first, and Scales and Evolution modify that number once.
#[test]
fn a_fold_of_both_kinds_splits_by_kind() {
    // (a) Each replacement order: Scales first makes r + 1 into r + 2, then 2r + 4.
    // Evolution first makes it 2r + 2, then 2r + 3.
    for pick in FirstPick::BOTH {
        reset_triggers_seen();
        let mut scenario = board(7);
        scenario.add_creature_from_oracle(P0, "Tayam, Luminous Enigma", 3, 3, TAYAM);
        add_grumgully(&mut scenario);
        add_counter_replacements(&mut scenario);
        let jammer = free_jammer(&mut scenario);
        let mut runner = scenario.build();
        let mut events = Vec::new();
        cast_and_pass(&mut runner, jammer, None, None, &mut events);
        order_entry(&mut runner, "Hammer Jammer", &mut events);
        assert_counter_prompt(runner.state());
        assert_eq!(die_results(&events).len(), 1, "{pick:?}");
        assert_eq!(counters_of(runner.state(), jammer, &vigilance_counter()), 1);
        assert_eq!(plus_counters(runner.state(), jammer), 0, "{pick:?}");
        let settled = settle_picking(&mut runner, &mut events, pick);
        let rolls = die_results(&events);
        assert_eq!(rolls.len(), 1, "{pick:?}");
        let r = rolls[0];
        assert_eq!(settled.counter_prompts, 1, "{pick:?}");
        let expected = match pick {
            FirstPick::Scales => 2 * r + 4,
            FirstPick::Evolution => 2 * r + 3,
        };
        assert_eq!(plus_counters(runner.state(), jammer), expected, "{pick:?}");
        assert_eq!(counters_of(runner.state(), jammer, &vigilance_counter()), 1);
        assert_eq!(triggers_seen_from(jammer), 0, "{pick:?}");
    }

    // (b) Neverwinter Hydra, X = 2: the total plus Grumgully's counter is one number,
    // and Scales adds one to it.
    let mut scenario = board(7);
    scenario.add_creature_from_oracle(P0, "Tayam, Luminous Enigma", 3, 3, TAYAM);
    add_grumgully(&mut scenario);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    let hydra = x_creature(&mut scenario, "Neverwinter Hydra", NEVERWINTER_HYDRA, 2);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    cast_and_pass(&mut runner, hydra, Some(2), None, &mut events);
    order_entry(&mut runner, "Neverwinter Hydra", &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 2, "{rolls:?}");
    let total: u32 = rolls.iter().sum();
    assert_eq!(plus_counters(runner.state(), hydra), total + 1 + 1);
    assert_eq!(counters_of(runner.state(), hydra, &vigilance_counter()), 1);
}
