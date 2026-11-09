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
//! Storm the Festival carry only the lines under test, and Rite of the Moth its
//! first line, without its flashback.
//! Ever After, Myojin of Life's Web, Oblation, Llanowar Elves and Roaming Throne
//! are verbatim.
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
const ILHARG_THE_RAZE_BOAR: &str = "Trample\nWhenever Ilharg attacks, you may put a creature card from your hand onto the battlefield tapped and attacking. Return that creature to your hand at the beginning of the next end step.\nWhen Ilharg dies or is put into exile from the battlefield, you may put it into its owner's library third from the top.";
const ARTHUR_MARIGOLD_KNIGHT: &str = "Haste\nWhenever Arthur and at least one other creature attack, look at the top six cards of your library. You may put a creature card from among them onto the battlefield tapped and attacking. Put the rest on the bottom of your library in a random order. Return that creature to its owner's hand at end of combat.";
const CHORD_OF_CALLING: &str = "Convoke (Your creatures can help cast this spell. Each creature you tap while casting this spell pays for {1} or one mana of that creature's color.)\nSearch your library for a creature card with mana value X or less, put it onto the battlefield, then shuffle.";
const DOUBLING_SEASON: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";
const WINDING_CONSTRICTOR: &str = "If one or more counters would be put on an artifact or creature you control, that many plus one of each of those kinds of counters are put on that permanent instead.\nIf you would get one or more counters, you get that many plus one of each of those kinds of counters instead.";
const RITE_OF_THE_MOTH: &str = "Return target creature card from your graveyard to the battlefield with a finality counter on it. (If a creature with a finality counter on it would die, exile it instead.)";
const GRIM_REAPER_LETHAL_LEGIONNAIRE: &str = "Whenever Grim Reaper attacks, you may pay {3}{B}. When you do, return target creature card from your graveyard to the battlefield tapped and attacking with a finality counter on it. (If a creature with a finality counter on it would die, exile it instead.)";
const POLYMORPH: &str = "Destroy target creature. It can't be regenerated. Its controller reveals cards from the top of their library until they reveal a creature card. The player puts that card onto the battlefield, then shuffles all other cards revealed this way into their library.";
const OBLATION: &str =
    "The owner of target nonland permanent shuffles it into their library, then draws two cards.";
const LLANOWAR_ELVES: &str = "{T}: Add {G}.";
const EVER_AFTER: &str = "Return up to two target creature cards from your graveyard to the battlefield. Each of those creatures is a black Zombie in addition to its other colors and types. Put Ever After on the bottom of its owner's library.";
const MYOJIN_OF_LIFES_WEB: &str = "Myojin of Life's Web enters with a divinity counter on it if you cast it from your hand.\nMyojin of Life's Web has indestructible as long as it has a divinity counter on it.\nRemove a divinity counter from Myojin of Life's Web: Put any number of creature cards from your hand onto the battlefield.";

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
    dwarf: ObjectId,
    warden: ObjectId,
    /// Zombify, when the fold's route is the owner's.
    zombify: Option<ObjectId>,
}

fn fold_board(seed: u64, fold: Fold, reanimated: bool) -> FoldBoard {
    let mut scenario = board(seed);
    add_counter_replacements(&mut scenario);
    let dwarf = add_dwarf(&mut scenario);
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
        dwarf,
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
        dwarf,
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
    assert_eq!(before - life(runner.state(), P1), 1, "{what}: Dwarf once");
    assert_eq!(
        life(runner.state(), P0),
        warden_life + 1,
        "{what}: Warden once"
    );
    assert_finished(runner.state(), &[dwarf, warden], &what);
    assert_eq!(
        printed_chain(runner.state(), jammer),
        printed,
        "{what}: the card's own chain is untouched, so a later entry starts clean"
    );
}

/// The settled postcondition of a row: no resolution under way, nothing parked,
/// and no trigger of `observers` left queued or on the stack.
fn assert_finished(state: &GameState, observers: &[ObjectId], what: &str) {
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{what}: {:?}",
        state.waiting_for
    );
    assert!(
        state.resolution_stack.is_empty(),
        "{what}: {:?}",
        stack_frames(state)
    );
    assert!(
        state.deferred_entry_events.is_empty(),
        "{what}: {:?}",
        state.deferred_entry_events
    );
    for observer in observers {
        assert_eq!(queued_or_stacked_from(state, *observer), 0, "{what}");
    }
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
    dwarf: ObjectId,
    warden: ObjectId,
    festival: ObjectId,
}

fn festival_board(seed: u64, add_fold: fn(&mut GameScenario)) -> FestivalBoard {
    let mut scenario = board(seed);
    add_fold(&mut scenario);
    add_counter_replacements(&mut scenario);
    let dwarf = add_dwarf(&mut scenario);
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
        dwarf,
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
    let what = format!("{pick:?}, ordering prompt = {ordering_prompt}");
    assert_eq!(before - life(runner.state(), P1), 1, "{what}: Dwarf once");
    assert_eq!(
        life(runner.state(), P0),
        warden_life + 1,
        "{what}: Warden once"
    );
    assert_finished(runner.state(), &[board.dwarf, board.warden], &what);
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
        // CR 614.12a + CR 603.6d: the entry is held from this pause, before its
        // continuation runs.
        let pending_at_prompt = is_pending(runner.state(), temp);
        let settled = settle_picking(&mut runner, &mut events, pick);
        let rolls = die_results(&events);
        assert_eq!(rolls.len(), 1, "{pick:?}");
        let r = rolls[0];
        assert_eq!(settled.counter_prompts, 1, "{pick:?}");
        assert_eq!(counters_of(runner.state(), temp, &funk_counter()), r);
        assert_eq!(plus_counters(runner.state(), temp), pick.counters(1));
        assert_eq!(before - life(runner.state(), P1), 1, "Dwarf {pick:?}");
        assert_eq!(rolled_at_prompt, 0, "the counter step comes first");
        assert!(
            pending_at_prompt,
            "{pick:?}: the entry is held at the pause"
        );
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

/// Who puts Hammer Jammer onto the battlefield attacking.
#[derive(Clone, Copy, Debug)]
enum Putter {
    /// Yore-Tiller Nephilim returns it from the graveyard (the targeted loop).
    Nephilim,
    /// Ilharg, the Raze-Boar puts it from the hand, the only creature card there
    /// (the single-pick branch).
    Ilharg,
    /// Arthur, Marigold Knight puts it from among the top six (the batch owner).
    Arthur,
    /// Grim Reaper, Lethal Legionnaire returns it from the graveyard with a
    /// finality counter, once {3}{B} is paid (the targeted loop, reflexively).
    GrimReaper,
}

/// An attacking-entry board: the putter, Hammer Jammer where the putter takes it
/// from, Brazen Dwarf and P0's Soul Warden, plus `extras`.
struct AttackBoard {
    runner: GameRunner,
    /// The putter, and Arthur's co-attacker.
    attackers: Vec<ObjectId>,
    jammer: ObjectId,
    dwarf: ObjectId,
    warden: ObjectId,
}

fn attack_board(
    seed: u64,
    players: u8,
    putter: Putter,
    extras: fn(&mut GameScenario),
) -> AttackBoard {
    let mut scenario = if players == 3 {
        board3(seed)
    } else {
        board(seed)
    };
    extras(&mut scenario);
    let dwarf = add_dwarf(&mut scenario);
    let warden = add_warden_p0(&mut scenario);
    let (attackers, jammer, on_library_top) = match putter {
        Putter::Nephilim => {
            let nephilim = scenario
                .add_creature_from_oracle(P0, "Yore-Tiller Nephilim", 2, 2, YORE_TILLER_NEPHILIM)
                .id();
            let jammer = graveyard_creature(&mut scenario, "Hammer Jammer", HAMMER_JAMMER);
            (vec![nephilim], jammer, false)
        }
        Putter::Ilharg => {
            let ilharg = scenario
                .add_creature_from_oracle(P0, "Ilharg, the Raze-Boar", 6, 6, ILHARG_THE_RAZE_BOAR)
                .id();
            (vec![ilharg], free_jammer(&mut scenario), false)
        }
        Putter::Arthur => {
            let arthur = scenario
                .add_creature_from_oracle(
                    P0,
                    "Arthur, Marigold Knight",
                    4,
                    5,
                    ARTHUR_MARIGOLD_KNIGHT,
                )
                .id();
            let co_attacker = scenario.add_vanilla(P0, 2, 2);
            (vec![arthur, co_attacker], jammer_card(&mut scenario), true)
        }
        Putter::GrimReaper => {
            let reaper = scenario
                .add_creature_from_oracle(
                    P0,
                    "Grim Reaper, Lethal Legionnaire",
                    3,
                    4,
                    GRIM_REAPER_LETHAL_LEGIONNAIRE,
                )
                .id();
            let jammer = graveyard_creature(&mut scenario, "Hammer Jammer", HAMMER_JAMMER);
            (vec![reaper], jammer, false)
        }
    };
    let mut runner = scenario.build();
    if on_library_top {
        seat_on_library_top(&mut runner, jammer);
    }
    AttackBoard {
        runner,
        attackers,
        jammer,
        dwarf,
        warden,
    }
}

fn fold_and_counter_replacements(scenario: &mut GameScenario) {
    add_velocipede(scenario);
    add_counter_replacements(scenario);
}

/// `board.attackers` attack P1. Drive the attack trigger, answering its target, its
/// "you may" and its look-at choice with Hammer Jammer, until a prompt the driver
/// does not answer or a settled Priority.
fn attack_to_entry_prompt(board: &mut AttackBoard, events: &mut Vec<GameEvent>) {
    declare_board_attack(board, events);
    drive_attack_trigger_to_entry_prompt(board, events);
}

/// `board.attackers` attack P1.
fn declare_board_attack(board: &mut AttackBoard, events: &mut Vec<GameEvent>) {
    let runner = &mut board.runner;
    runner.advance_to_combat();
    let attacks: Vec<_> = board
        .attackers
        .iter()
        .map(|attacker| (*attacker, AttackTarget::Player(P1)))
        .collect();
    let attacked = runner
        .declare_attackers(&attacks)
        .expect("the attack is declared");
    events.extend(attacked.events);
}

/// Drive the declared attack's trigger, answering its target, its "you may" and
/// its look-at choice with Hammer Jammer, until a prompt the driver does not
/// answer or a settled Priority.
fn drive_attack_trigger_to_entry_prompt(board: &mut AttackBoard, events: &mut Vec<GameEvent>) {
    let runner = &mut board.runner;
    for _ in 0..40 {
        record_stack(runner.state());
        let action = match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Object(board.jammer)),
            },
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::DigChoice { .. } => GameAction::SelectCards {
                cards: vec![board.jammer],
            },
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::Priority { .. } if !resolution_is_settled(runner.state()) => {
                GameAction::PassPriority
            }
            _ => return,
        };
        act(runner, action, events);
    }
    panic!("the entry never paused or settled");
}

/// Pass priority until the entrant's attack target is asked.
fn pass_to_attack_target_choice(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..20 {
        match &runner.state().waiting_for {
            WaitingFor::EntryAttackTargetChoice { .. } => return,
            WaitingFor::Priority { .. } if !resolution_is_settled(runner.state()) => {
                act(runner, GameAction::PassPriority, events);
            }
            other => panic!(
                "expected the attack target choice, got {other:?} on {:?}",
                stack_frames(runner.state())
            ),
        }
    }
    panic!("the attack target was never asked");
}

/// Answer the one counter-ordering prompt, applying `pick` first.
fn answer_counter_prompt(runner: &mut GameRunner, pick: FirstPick, events: &mut Vec<GameEvent>) {
    assert_counter_prompt(runner.state());
    let index = replacement_index(runner.state(), pick.source_name());
    act(runner, GameAction::ChooseReplacement { index }, events);
}

/// Every attack record of `id`, as `(defending player, attack target)`.
fn attacks_of(state: &GameState, id: ObjectId) -> Vec<(PlayerId, AttackTarget)> {
    state
        .combat
        .as_ref()
        .map(|combat| {
            combat
                .attackers
                .iter()
                .filter(|attacker| attacker.object_id == id)
                .map(|attacker| (attacker.defending_player, attacker.attack_target))
                .collect()
        })
        .unwrap_or_default()
}

/// The `EffectResolved { kind: ChangeZone }` events `source` emitted.
fn change_zone_resolved_for(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::ChangeZone,
                    source_id,
                    ..
                } if *source_id == source
            )
        })
        .count()
}

/// M-1 (V32 rewritten). Yore-Tiller Nephilim + CR 508.4 + CR 614.1c + CR 122.6 +
/// CR 614.5: Hammer Jammer returned "tapped and attacking" under Thunderous
/// Velocipede, Hardened Scales and Branching Evolution enters with r + 1 counters as
/// one number, each replacement modifying it once. The roll comes before the only
/// counter prompt, Hammer Jammer does not see it (CR 113.6), Brazen Dwarf does
/// (CR 603.2), and once the entry completed Hammer Jammer attacks P1.
#[test]
fn an_attacking_entry_is_one_placement_and_attacks() {
    for pick in FirstPick::BOTH {
        let what = format!("{pick:?}");
        reset_triggers_seen();
        let mut board = attack_board(7, 2, Putter::Nephilim, fold_and_counter_replacements);
        let p1_before = life(board.runner.state(), P1);
        let p0_before = life(board.runner.state(), P0);
        let mut events = Vec::new();
        attack_to_entry_prompt(&mut board, &mut events);
        let AttackBoard {
            mut runner,
            jammer,
            dwarf,
            warden,
            ..
        } = board;
        assert_counter_prompt(runner.state());
        let rolled_at_prompt = die_results(&events).len();
        let pending_at_prompt = is_pending(runner.state(), jammer);
        let counters_at_prompt = plus_counters(runner.state(), jammer);

        let settled = settle_picking(&mut runner, &mut events, pick);
        let r = die_results(&events)[0];
        let state = runner.state();
        assert_eq!(rolled_at_prompt, 1, "{what}: the roll comes first");
        assert!(pending_at_prompt, "{what}");
        assert_eq!(counters_at_prompt, 0, "{what}");
        assert_eq!(settled.counter_prompts, 1, "{what}");
        assert_eq!(
            plus_counters(state, jammer),
            pick.counters(r + 1),
            "{what}, r = {r}"
        );
        assert_eq!(triggers_seen_from(jammer), 0, "{what}");
        assert_eq!(p1_before - life(state, P1), 1, "{what}: Dwarf once");
        assert_eq!(life(state, P0), p0_before + 1, "{what}: Warden once");
        assert_eq!(
            attacks_of(state, jammer),
            vec![(P1, AttackTarget::Player(P1))],
            "{what}"
        );
        assert_finished(state, &[dwarf, warden], &what);
    }
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

// ---------------------------------------------------------------------------
// The attacking seat travels with the paused entry to its owner
// ---------------------------------------------------------------------------

fn add_barbarian_class(scenario: &mut GameScenario) {
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
}

fn add_velocipede_only(scenario: &mut GameScenario) {
    add_velocipede(scenario);
}

fn grumgully_and_counter_replacements(scenario: &mut GameScenario) {
    add_grumgully(scenario);
    add_counter_replacements(scenario);
}

/// M-2. CR 508.4 + CR 706.6: with no fold, the chain pauses on Barbarian Class's tie,
/// not on a counter prompt. The owner that resumes the paused member seats Hammer
/// Jammer once its entry completed.
#[test]
fn a_tied_attacking_entry_is_seated_by_its_owner() {
    let seed = find_seed("an attacking Jammer tie", |seed| {
        let mut board = attack_board(seed, 2, Putter::Nephilim, add_barbarian_class);
        attack_to_entry_prompt(&mut board, &mut Vec::new());
        tie_prompt_results(board.runner.state()).is_some()
    });
    reset_triggers_seen();
    let mut board = attack_board(seed, 2, Putter::Nephilim, add_barbarian_class);
    let mut events = Vec::new();
    attack_to_entry_prompt(&mut board, &mut events);
    let AttackBoard {
        mut runner,
        jammer,
        dwarf,
        warden,
        ..
    } = board;
    let naturals = answer_tie(&mut runner, &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let state = runner.state();
    assert_eq!(
        plus_counters(state, jammer),
        survivors_total(&naturals),
        "{naturals:?}"
    );
    assert_eq!(triggers_seen_from(jammer), 0);
    assert_eq!(
        attacks_of(state, jammer),
        vec![(P1, AttackTarget::Player(P1))]
    );
    assert_finished(state, &[dwarf, warden], "tie");
}

/// M-3. CR 508.4 + CR 508.4a: with two defenders the owner asks where Hammer Jammer
/// attacks, after its counters landed, and leaves its frame in place for the
/// answer. The answer seats it once, and the entry is observed after it.
#[test]
fn an_owner_asks_the_attack_target_with_its_frame_in_place() {
    for pick in FirstPick::BOTH {
        let what = format!("{pick:?}");
        reset_triggers_seen();
        let mut board = attack_board(7, 3, Putter::Nephilim, fold_and_counter_replacements);
        let p0_before = life(board.runner.state(), P0);
        let mut events = Vec::new();
        attack_to_entry_prompt(&mut board, &mut events);
        let AttackBoard {
            mut runner,
            jammer,
            dwarf,
            warden,
            ..
        } = board;
        answer_counter_prompt(&mut runner, pick, &mut events);
        pass_to_attack_target_choice(&mut runner, &mut events);
        let r = die_results(&events)[0];
        let state = runner.state();
        let WaitingFor::EntryAttackTargetChoice {
            player,
            object_id,
            valid_targets,
        } = &state.waiting_for
        else {
            unreachable!("checked by the driver");
        };
        assert_eq!((*player, *object_id), (P0, jammer), "{what}");
        assert!(valid_targets.len() >= 2, "{what}: {valid_targets:?}");
        assert!(
            stack_frames(state).contains(&FrameKind::ChangeZone),
            "{what}: the owner's frame stays: {:?}",
            stack_frames(state)
        );
        assert_eq!(
            plus_counters(state, jammer),
            pick.counters(r + 1),
            "{what}: the counters landed before the prompt"
        );
        assert!(attacks_of(state, jammer).is_empty(), "{what}");
        assert_eq!(life(state, P0), p0_before, "{what}: Warden waits");
        assert_eq!(queued_or_stacked_from(state, warden), 0, "{what}");
        let target = valid_targets[1];
        let defender = match target {
            AttackTarget::Player(player) => player,
            other => panic!("{what}: a player target, got {other:?}"),
        };

        act(
            &mut runner,
            GameAction::ChooseEntryAttackTarget { target },
            &mut events,
        );
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let state = runner.state();
        assert_eq!(
            attacks_of(state, jammer),
            vec![(defender, target)],
            "{what}"
        );
        assert_eq!(plus_counters(state, jammer), pick.counters(r + 1), "{what}");
        assert_eq!(triggers_seen_from(jammer), 0, "{what}");
        assert_eq!(life(state, P0), p0_before + 1, "{what}: Warden once");
        assert_finished(state, &[dwarf, warden], &what);
    }
}

/// Is a `ChangeZone` logical owner on the resolution stack?
fn has_change_zone_owner(state: &GameState) -> bool {
    stack_frames(state).contains(&FrameKind::ChangeZone)
}

/// The targets of every delayed trigger `putter` installed (its "Return that
/// creature" rider).
fn returns_installed_by(state: &GameState, putter: ObjectId) -> Vec<Vec<TargetRef>> {
    state
        .delayed_triggers
        .iter()
        .filter(|delayed| delayed.source_id == putter)
        .map(|delayed| delayed.ability.targets.clone())
        .collect()
}

/// The ordering route, shared by M-4 and M-7b. The entry's ordering prompt comes
/// before the move, so its answer delivers under `CallerEpilogue`: Grumgully's
/// counter joins the roll's as one placement (CR 614.1c + CR 122.6), Hardened
/// Scales and Branching Evolution each modify it once (CR 614.5), and the
/// delivery's own seat places Hammer Jammer in combat (CR 508.4) before the chain
/// rolls. Brazen Dwarf sees the roll once (CR 603.2), Hammer Jammer does not
/// (CR 113.6), and Soul Warden sees the entry once.
fn run_ordered_attacking_entry(putter: Putter, owner_parked: bool) {
    for pick in FirstPick::BOTH {
        let what = format!("{putter:?}, {pick:?}");
        reset_triggers_seen();
        let mut board = attack_board(7, 2, putter, grumgully_and_counter_replacements);
        let p1_before = life(board.runner.state(), P1);
        let p0_before = life(board.runner.state(), P0);
        let mut events = Vec::new();
        attack_to_entry_prompt(&mut board, &mut events);
        let AttackBoard {
            mut runner,
            jammer,
            dwarf,
            warden,
            ..
        } = board;

        // Reach: the ordering prompt, before the move, with or without the
        // targeted owner parked. The driver stops at any other prompt, so no
        // `EffectZoneChoice` surfaced on the way.
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "{what}: {:?}",
            runner.state().waiting_for
        );
        assert_eq!(
            has_change_zone_owner(runner.state()),
            owner_parked,
            "{what}: {:?}",
            stack_frames(runner.state())
        );
        order_entry(&mut runner, "Hammer Jammer", &mut events);

        // Reach: the one counter prompt comes after the roll, with Hammer Jammer
        // already attacking.
        assert_counter_prompt(runner.state());
        assert_eq!(
            die_results(&events).len(),
            1,
            "{what}: the roll comes first"
        );
        assert_eq!(
            attacks_of(runner.state(), jammer),
            vec![(P1, AttackTarget::Player(P1))],
            "{what}: seated before the chain"
        );
        assert_eq!(
            has_change_zone_owner(runner.state()),
            owner_parked,
            "{what}: {:?}",
            stack_frames(runner.state())
        );

        let settled = settle_picking(&mut runner, &mut events, pick);
        let r = die_results(&events)[0];
        let state = runner.state();
        assert_eq!(settled.counter_prompts, 1, "{what}");
        assert_eq!(
            plus_counters(state, jammer),
            pick.counters(r + 1),
            "{what}, r = {r}"
        );
        assert_eq!(triggers_seen_from(jammer), 0, "{what}");
        assert_eq!(p1_before - life(state, P1), 1, "{what}: Dwarf once");
        assert_eq!(life(state, P0), p0_before + 1, "{what}: Warden once");
        assert_eq!(
            attacks_of(state, jammer),
            vec![(P1, AttackTarget::Player(P1))],
            "{what}"
        );
        assert_finished(state, &[dwarf, warden], &what);
    }
}

/// M-4. Yore-Tiller Nephilim + CR 508.4 + CR 616.1: the ordering route under the
/// targeted owner.
#[test]
fn an_ordered_attacking_entry_is_one_placement_and_attacks_once() {
    run_ordered_attacking_entry(Putter::Nephilim, true);
}

/// M-7b. Ilharg, the Raze-Boar + CR 508.4 + CR 616.1: the same ordering route with
/// no owner. Ilharg's single pick of Hammer Jammer, its only creature card in
/// hand, waits on Grumgully's ordering prompt before the move, so it delivers
/// under `CallerEpilogue` as M-4 does. Ilharg's instruction count and its "Return
/// that creature" rider on this route are not asserted: neither is produced here,
/// as before this change (filed separately).
#[test]
fn an_ordered_single_pick_attacking_entry_is_one_placement() {
    run_ordered_attacking_entry(Putter::Ilharg, false);
}

/// M-6. CR 508.4 + CR 122.6: with only the fold, the entry completes inline: no
/// counter prompt, r + 1 counters, and Hammer Jammer attacks.
#[test]
fn an_inline_attacking_entry_attacks_with_its_fold() {
    reset_triggers_seen();
    let mut board = attack_board(7, 2, Putter::Nephilim, add_velocipede_only);
    let mut events = Vec::new();
    attack_to_entry_prompt(&mut board, &mut events);
    let state = board.runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }) && resolution_is_settled(state),
        "no prompt: {:?}",
        state.waiting_for
    );
    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 1, "{rolls:?}");
    assert_eq!(plus_counters(state, board.jammer), rolls[0] + 1);
    assert_eq!(triggers_seen_from(board.jammer), 0);
    assert_eq!(
        attacks_of(state, board.jammer),
        vec![(P1, AttackTarget::Player(P1))]
    );
    assert_finished(state, &[board.dwarf, board.warden], "inline");
}

/// R1 (F1). Ilharg, the Raze-Boar + Thunderous Velocipede + Hardened Scales +
/// CR 603.6d + CR 614.1c + CR 122.6 + CR 614.5: Ilharg's single pick puts Hammer
/// Jammer onto the battlefield "tapped and attacking". Velocipede's counter is
/// given as it enters, so it is one placement with the roll's r: Hardened Scales
/// modifies r + 1 once, giving r + 2. Nothing pauses, so the delivery seats it
/// (CR 508.4). Hammer Jammer does not see its roll (CR 113.6), Brazen Dwarf does
/// (CR 603.2), and Soul Warden sees the entry once.
#[test]
fn a_single_pick_attacking_entry_is_one_placement() {
    reset_triggers_seen();
    let mut board = attack_board(7, 2, Putter::Ilharg, |scenario| {
        add_velocipede(scenario);
        scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    });
    let p1_before = life(board.runner.state(), P1);
    let p0_before = life(board.runner.state(), P0);
    let mut events = Vec::new();
    attack_to_entry_prompt(&mut board, &mut events);
    let AttackBoard {
        runner,
        attackers,
        jammer,
        dwarf,
        warden,
    } = board;
    let ilharg = attackers[0];
    let state = runner.state();
    // Reach: no counter prompt, and the put settled.
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }) && resolution_is_settled(state),
        "{:?} on {:?}",
        state.waiting_for,
        stack_frames(state)
    );
    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 1, "{rolls:?}");
    let r = rolls[0];
    assert_eq!(plus_counters(state, jammer), r + 2, "r = {r}");
    assert_eq!(
        attacks_of(state, jammer),
        vec![(P1, AttackTarget::Player(P1))]
    );
    assert_eq!(triggers_seen_from(jammer), 0);
    assert_eq!(p1_before - life(state, P1), 1, "Dwarf once");
    assert_eq!(life(state, P0), p0_before + 1, "Warden once");
    assert_eq!(change_zone_resolved_for(&events, ilharg), 1);
    assert_eq!(
        returns_installed_by(state, ilharg),
        vec![vec![TargetRef::Object(jammer)]],
        "the return is bound to Hammer Jammer"
    );
    assert_finished(state, &[dwarf, warden], "R1");
}

/// R2 (F1, the paused sibling; M-7 rewritten). Ilharg, the Raze-Boar + Thunderous
/// Velocipede + Hardened Scales + Branching Evolution + CR 614.1c + CR 122.6 +
/// CR 614.5 + CR 616.1 + CR 508.4: Velocipede's counter joins the roll's
/// placement, so the one counter prompt comes after the roll and each replacement
/// modifies r + 1 once. No logical owner resumes the put, so a seat frame parked
/// beneath the entry's whole child stack seats Hammer Jammer once its entry has
/// completed. Hammer Jammer does not see its roll (CR 113.6), Brazen Dwarf does
/// (CR 603.2), Soul Warden sees the entry once, and Ilharg's return names Hammer
/// Jammer (CR 608.2c).
#[test]
fn a_paused_single_pick_attacking_entry_is_seated_after_its_chain() {
    for pick in FirstPick::BOTH {
        let what = format!("{pick:?}");
        reset_triggers_seen();
        let mut board = attack_board(7, 2, Putter::Ilharg, fold_and_counter_replacements);
        let p1_before = life(board.runner.state(), P1);
        let p0_before = life(board.runner.state(), P0);
        let mut events = Vec::new();
        attack_to_entry_prompt(&mut board, &mut events);
        let AttackBoard {
            mut runner,
            attackers,
            jammer,
            dwarf,
            warden,
        } = board;
        let ilharg = attackers[0];
        assert_counter_prompt(runner.state());
        let rolled_at_prompt = die_results(&events).len();
        let frames_at_prompt = stack_frames(runner.state());
        let attacking_at_prompt = attacks_of(runner.state(), jammer);

        let settled = settle_picking(&mut runner, &mut events, pick);
        let r = die_results(&events)[0];
        let state = runner.state();
        assert_eq!(settled.counter_prompts, 1, "{what}: one prompt");
        assert_eq!(
            plus_counters(state, jammer),
            pick.counters(r + 1),
            "{what}, r = {r}"
        );
        assert_eq!(
            attacks_of(state, jammer),
            vec![(P1, AttackTarget::Player(P1))],
            "{what}"
        );
        assert_eq!(p1_before - life(state, P1), 1, "{what}: Dwarf once");
        // Reach: the roll came before the one prompt, beneath which the seat
        // frame waits, with no logical owner and Hammer Jammer not yet seated.
        assert_eq!(rolled_at_prompt, 1, "{what}: the roll comes first");
        assert_eq!(
            frames_at_prompt,
            vec![
                FrameKind::EntryAttackSeat,
                FrameKind::PostReplacement,
                FrameKind::AbilityContinuation,
                FrameKind::CounterAdditions,
            ],
            "{what}"
        );
        assert!(attacking_at_prompt.is_empty(), "{what}");
        assert_eq!(triggers_seen_from(jammer), 0, "{what}");
        assert_eq!(life(state, P0), p0_before + 1, "{what}: Warden once");
        assert_eq!(change_zone_resolved_for(&events, ilharg), 1, "{what}");
        assert_eq!(
            returns_installed_by(state, ilharg),
            vec![vec![TargetRef::Object(jammer)]],
            "{what}: the return is bound to Hammer Jammer"
        );
        assert_finished(state, &[dwarf, warden], &what);
    }
}

/// R3 (F1, two defenders). CR 508.4 + CR 508.4a: on R2's board with three seats,
/// the seat frame asks what Hammer Jammer attacks once the counter answer has
/// completed its entry, having left the stack before it asks. The entry is
/// observed after the answer.
#[test]
fn a_seat_frame_asks_the_attack_target_after_the_entry() {
    for pick in FirstPick::BOTH {
        let what = format!("{pick:?}");
        reset_triggers_seen();
        let mut board = attack_board(7, 3, Putter::Ilharg, fold_and_counter_replacements);
        let p0_before = life(board.runner.state(), P0);
        let mut events = Vec::new();
        attack_to_entry_prompt(&mut board, &mut events);
        let AttackBoard {
            mut runner,
            attackers,
            jammer,
            dwarf,
            warden,
        } = board;
        let ilharg = attackers[0];
        answer_counter_prompt(&mut runner, pick, &mut events);
        pass_to_attack_target_choice(&mut runner, &mut events);
        let r = die_results(&events)[0];
        let state = runner.state();
        let WaitingFor::EntryAttackTargetChoice {
            player,
            object_id,
            valid_targets,
        } = &state.waiting_for
        else {
            unreachable!("checked by the driver");
        };
        assert_eq!((*player, *object_id), (P0, jammer), "{what}");
        assert!(
            valid_targets.contains(&AttackTarget::Player(P2)),
            "{what}: {valid_targets:?}"
        );
        assert!(
            !stack_frames(state).contains(&FrameKind::EntryAttackSeat),
            "{what}: the seat frame left before it asked: {:?}",
            stack_frames(state)
        );
        assert_eq!(plus_counters(state, jammer), pick.counters(r + 1), "{what}");
        assert_eq!(life(state, P0), p0_before, "{what}: the release waits");
        act(
            &mut runner,
            GameAction::ChooseEntryAttackTarget {
                target: AttackTarget::Player(P2),
            },
            &mut events,
        );
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let state = runner.state();
        assert_eq!(
            attacks_of(state, jammer),
            vec![(P2, AttackTarget::Player(P2))],
            "{what}"
        );
        assert_eq!(triggers_seen_from(dwarf), 1, "{what}: Dwarf once");
        assert_eq!(triggers_seen_from(jammer), 0, "{what}");
        assert_eq!(life(state, P0), p0_before + 1, "{what}: Warden once");
        assert_eq!(change_zone_resolved_for(&events, ilharg), 1, "{what}");
        assert_finished(state, &[dwarf, warden], &what);
    }
}

/// Ilharg attacks with `entrant` the only creature card in P0's hand, P0's Soul
/// Warden on the battlefield, plus `extras`.
fn ilharg_board(
    entrant: fn(&mut GameScenario) -> ObjectId,
    extras: fn(&mut GameScenario),
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = board(7);
    extras(&mut scenario);
    let warden = add_warden_p0(&mut scenario);
    let ilharg = scenario
        .add_creature_from_oracle(P0, "Ilharg, the Raze-Boar", 6, 6, ILHARG_THE_RAZE_BOAR)
        .id();
    let entrant = entrant(&mut scenario);
    (scenario.build(), ilharg, entrant, warden)
}

/// Declare Ilharg's attack and accept its put, up to the entry's first prompt.
fn ilharg_attacks_to_entry_prompt(
    runner: &mut GameRunner,
    ilharg: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    runner.advance_to_combat();
    let attacked = runner
        .declare_attackers(&[(ilharg, AttackTarget::Player(P1))])
        .expect("Ilharg attacks");
    events.extend(attacked.events);
    for _ in 0..40 {
        record_stack(runner.state());
        let action = match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::Priority { .. } if !resolution_is_settled(runner.state()) => {
                GameAction::PassPriority
            }
            _ => return,
        };
        act(runner, action, events);
    }
    panic!("Ilharg's put never paused or settled");
}

const DENRY_KLIN_EDITOR_IN_CHIEF: &str = "Denry Klin enters with your choice of a +1/+1, first strike, or vigilance counter on it.\nWhenever a nontoken creature you control enters, if Denry Klin has counters on it, put the same number of each kind of counter on that creature.";

fn denry_in_hand(scenario: &mut GameScenario) -> ObjectId {
    free_creature(
        scenario,
        "Denry Klin, Editor in Chief",
        2,
        2,
        DENRY_KLIN_EDITOR_IN_CHIEF,
    )
}

fn hydradoodle_in_hand(scenario: &mut GameScenario) -> ObjectId {
    x_creature(scenario, "Hydradoodle", HYDRADOODLE, 0)
}

fn no_extras(_: &mut GameScenario) {}

fn is_enters_counter_choice(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. })
}

/// M-7f. Ilharg, the Raze-Boar + Denry Klin, Editor in Chief + CR 614.1c +
/// CR 614.12a + CR 603.2: Denry's counter choice is made as it enters, and with no
/// logical owner its replay is the entry's only collector, so Soul Warden sees
/// Denry once. Preservation: whether Denry attacks is the filed single-pick class
/// and is not asserted, nor is Ilharg's instruction count, which a single pick
/// paused on a choice other than a counter prompt does not produce, as before this
/// change (filed separately).
#[test]
fn a_single_pick_replayed_entry_choice_is_collected_once() {
    let (mut runner, ilharg, denry, _warden) = ilharg_board(denry_in_hand, no_extras);
    let p0_before = life(runner.state(), P0);
    let mut events = Vec::new();
    ilharg_attacks_to_entry_prompt(&mut runner, ilharg, &mut events);
    // Reach: Denry's enters-counter choice, with no logical owner parked.
    assert!(
        is_enters_counter_choice(runner.state()),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(
        !has_change_zone_owner(runner.state()),
        "{:?}",
        stack_frames(runner.state())
    );
    act(
        &mut runner,
        GameAction::ChooseBranch { index: 0 },
        &mut events,
    );
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let state = runner.state();
    assert_eq!(zone(state, denry), Zone::Battlefield);
    assert_eq!(plus_counters(state, denry), 1);
    assert_eq!(life(state, P0), p0_before + 1, "Warden once");
    assert_eq!(
        returns_installed_by(state, ilharg),
        vec![vec![TargetRef::Object(denry)]],
        "the return is bound to Denry Klin"
    );
    assert!(
        state.resolution_stack.is_empty(),
        "{:?}",
        stack_frames(state)
    );
    assert!(state.deferred_entry_events.is_empty());
}

/// R4 (M-7g rewritten). Ilharg, the Raze-Boar + Hydradoodle + CR 508.4 +
/// CR 107.3m + CR 706.1 + CR 614.1c + CR 122.6: put from the hand, not cast,
/// Hydradoodle's X is 0 and it rolls no dice. Its Velocipede counter joins the
/// zero-dice roll's placement, so the roll resolves before the one counter
/// prompt, and the seat frame seats it attacking once that placement completes.
#[test]
fn a_zero_dice_single_pick_attacking_entry_is_seated_after_its_chain() {
    for pick in FirstPick::BOTH {
        let what = format!("{pick:?}");
        let (mut runner, ilharg, doodle, _warden) =
            ilharg_board(hydradoodle_in_hand, fold_and_counter_replacements);
        let p0_before = life(runner.state(), P0);
        let mut events = Vec::new();
        ilharg_attacks_to_entry_prompt(&mut runner, ilharg, &mut events);
        // Reach: the zero-dice roll resolved before the one counter prompt, with
        // the seat frame and no logical owner parked.
        assert_counter_prompt(runner.state());
        assert_eq!(roll_resolved_for(&events, doodle), 1, "{what}");
        assert!(
            stack_frames(runner.state()).contains(&FrameKind::EntryAttackSeat),
            "{what}: {:?}",
            stack_frames(runner.state())
        );
        assert!(
            !has_change_zone_owner(runner.state()),
            "{what}: {:?}",
            stack_frames(runner.state())
        );
        let settled = settle_picking(&mut runner, &mut events, pick);
        let state = runner.state();
        assert_eq!(
            attacks_of(state, doodle),
            vec![(P1, AttackTarget::Player(P1))],
            "{what}"
        );
        assert_eq!(plus_counters(state, doodle), pick.counters(1), "{what}");
        assert_eq!(settled.counter_prompts, 1, "{what}");
        assert_eq!(roll_resolved_for(&events, doodle), 1, "{what}");
        assert!(die_results(&events).is_empty(), "{what}");
        assert_eq!(life(state, P0), p0_before + 1, "{what}: Warden once");
        assert_eq!(change_zone_resolved_for(&events, ilharg), 1, "{what}");
        assert!(state.resolution_stack.is_empty(), "{what}");
        assert!(state.deferred_entry_events.is_empty(), "{what}");
    }
}

/// M-10. Zombify + Denry Klin, Editor in Chief + CR 614.12a + CR 603.2 +
/// CR 603.6d: the targeted owner's paused delivery holds Denry's entry across its
/// replayed counter choice, so the replay leaves the entry to that owner, which
/// collects it once.
#[test]
fn an_owner_held_replayed_entry_choice_is_collected_once() {
    let mut scenario = board(7);
    add_warden_p0(&mut scenario);
    let denry = scenario
        .add_creature_to_graveyard(P0, "Denry Klin, Editor in Chief", 2, 2)
        .from_oracle_text(DENRY_KLIN_EDITOR_IN_CHIEF)
        .id();
    let zombify = free_spell(&mut scenario, "Zombify", false, ZOMBIFY);
    let mut runner = scenario.build();
    let p0_before = life(runner.state(), P0);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, zombify, None, Some(denry), &mut events);
    // Reach: Denry's enters-counter choice, with the targeted owner the exact
    // parent of the paused post-replacement frame, holding Denry's entry.
    let state = runner.state();
    assert!(is_enters_counter_choice(state), "{:?}", state.waiting_for);
    let frames = stack_frames(state);
    assert!(
        frames.ends_with(&[FrameKind::ChangeZone, FrameKind::PostReplacement]),
        "{frames:?}"
    );
    let held = state
        .resolution_stack
        .iter()
        .find_map(|frame| match frame {
            ResolutionFrame::ChangeZone(frame) => frame
                .pending
                .as_ref()
                .and_then(|pending| pending.paused_current.as_ref())
                .map(|paused| paused.delivery_events.clone()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(
        held.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == denry
        )),
        "{held:?}"
    );
    act(
        &mut runner,
        GameAction::ChooseBranch { index: 0 },
        &mut events,
    );
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let state = runner.state();
    assert_eq!(zone(state, denry), Zone::Battlefield);
    assert_eq!(plus_counters(state, denry), 1);
    assert_eq!(life(state, P0), p0_before + 1, "Warden once");
    assert_eq!(change_zone_resolved_for(&events, zombify), 1);
    assert!(
        state.resolution_stack.is_empty(),
        "{:?}",
        stack_frames(state)
    );
    assert!(state.deferred_entry_events.is_empty());
}

fn top_of_library(state: &GameState, count: usize) -> Vec<ObjectId> {
    state.players[0]
        .library
        .iter()
        .take(count)
        .copied()
        .collect()
}

/// The five other looked-at cards are the bottom five of P0's library.
fn assert_rest_on_bottom(state: &GameState, top_six: &[ObjectId], jammer: ObjectId, what: &str) {
    let library = &state.players[0].library;
    let mut bottom: Vec<ObjectId> = library.iter().skip(library.len() - 5).copied().collect();
    let mut rest: Vec<ObjectId> = top_six.iter().copied().filter(|id| *id != jammer).collect();
    bottom.sort();
    rest.sort();
    assert_eq!(bottom, rest, "{what}");
}

/// M-8. Arthur, Marigold Knight + CR 508.4: a kept card delivered by the batch pauses
/// on the counter prompt. The batch owner seats Hammer Jammer, and the rest go to
/// the bottom once.
#[test]
fn a_batch_attacking_entry_is_seated_by_its_owner() {
    for pick in FirstPick::BOTH {
        let what = format!("{pick:?}");
        reset_triggers_seen();
        let mut board = attack_board(7, 2, Putter::Arthur, fold_and_counter_replacements);
        let top_six: Vec<ObjectId> = top_of_library(board.runner.state(), 6);
        assert_eq!(top_six[0], board.jammer);
        let p1_before = life(board.runner.state(), P1);
        let p0_before = life(board.runner.state(), P0);
        let mut events = Vec::new();
        attack_to_entry_prompt(&mut board, &mut events);
        let AttackBoard {
            mut runner,
            jammer,
            dwarf,
            warden,
            ..
        } = board;
        assert_counter_prompt(runner.state());
        settle_picking(&mut runner, &mut events, pick);
        let r = die_results(&events)[0];
        let state = runner.state();
        assert_eq!(
            plus_counters(state, jammer),
            pick.counters(r + 1),
            "{what}, r = {r}"
        );
        assert_eq!(triggers_seen_from(jammer), 0, "{what}");
        assert_eq!(p1_before - life(state, P1), 1, "{what}: Dwarf once");
        assert_eq!(life(state, P0), p0_before + 1, "{what}: Warden once");
        assert_eq!(
            attacks_of(state, jammer),
            vec![(P1, AttackTarget::Player(P1))],
            "{what}"
        );
        assert_rest_on_bottom(state, &top_six, jammer, &what);
        assert_finished(state, &[dwarf, warden], &what);
    }
}

/// M-8b. CR 508.4 + CR 508.4a: with two defenders the batch owner asks where Hammer
/// Jammer attacks, with its frame left in place, and finishes once after the answer.
#[test]
fn a_batch_owner_asks_the_attack_target_with_its_frame_in_place() {
    let pick = FirstPick::Scales;
    reset_triggers_seen();
    let mut board = attack_board(7, 3, Putter::Arthur, fold_and_counter_replacements);
    let top_six: Vec<ObjectId> = top_of_library(board.runner.state(), 6);
    let p1_before = life(board.runner.state(), P1);
    let p2_before = life(board.runner.state(), P2);
    let p0_before = life(board.runner.state(), P0);
    let mut events = Vec::new();
    attack_to_entry_prompt(&mut board, &mut events);
    let AttackBoard {
        mut runner,
        jammer,
        dwarf,
        warden,
        ..
    } = board;
    answer_counter_prompt(&mut runner, pick, &mut events);
    pass_to_attack_target_choice(&mut runner, &mut events);
    let state = runner.state();
    let WaitingFor::EntryAttackTargetChoice { valid_targets, .. } = &state.waiting_for else {
        unreachable!("checked by the driver");
    };
    assert!(valid_targets.len() >= 2, "{valid_targets:?}");
    assert!(
        valid_targets.contains(&AttackTarget::Player(P2)),
        "{valid_targets:?}"
    );
    assert!(
        stack_frames(state).contains(&FrameKind::BatchDelivery),
        "the batch owner's frame stays: {:?}",
        stack_frames(state)
    );
    act(
        &mut runner,
        GameAction::ChooseEntryAttackTarget {
            target: AttackTarget::Player(P2),
        },
        &mut events,
    );
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let r = die_results(&events)[0];
    let state = runner.state();
    assert_eq!(
        attacks_of(state, jammer),
        vec![(P2, AttackTarget::Player(P2))]
    );
    assert_eq!(plus_counters(state, jammer), pick.counters(r + 1));
    assert_eq!(triggers_seen_from(jammer), 0);
    assert_eq!(p1_before - life(state, P1), 1, "Dwarf once, P1");
    assert_eq!(p2_before - life(state, P2), 1, "Dwarf once, P2");
    assert_eq!(life(state, P0), p0_before + 1, "Warden once");
    assert_rest_on_bottom(state, &top_six, jammer, "two defenders");
    assert_finished(state, &[dwarf, warden], "two defenders");
}

/// Does `delivery` hold `entrant`'s battlefield `ZoneChanged`?
fn holds_entry_of(delivery: &[GameEvent], entrant: ObjectId) -> bool {
    delivery.iter().any(|event| {
        matches!(
            event,
            GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == entrant
        )
    })
}

/// The batch owner's paused delivery, found as the top frame or as the exact
/// parent of the active post-replacement child.
fn batch_owner_delivery(runner: &mut GameRunner) -> Option<Vec<GameEvent>> {
    runner
        .state_mut()
        .resolution_stack
        .active_batch_delivery_or_post_replacement_child_mut()
        .map(|owner| {
            owner
                .paused_current
                .as_ref()
                .map(|paused| paused.delivery_events.clone())
                .unwrap_or_default()
        })
}

/// M-8c. Arthur, Marigold Knight + Clone + CR 508.4 + CR 707.9 + CR 614.12a +
/// CR 603.2: Clone, put from among the top six "tapped and attacking", enters as
/// a copy of the vanilla co-attacker. The batch owner's paused delivery holds its
/// entry across the copy tail's replay, so Soul Warden sees Clone once, Clone
/// attacks P1 once, and the rest go to the bottom once.
#[test]
fn a_batch_held_copy_entry_is_collected_once_and_attacks() {
    let mut scenario = board(7);
    add_warden_p0(&mut scenario);
    let arthur = scenario
        .add_creature_from_oracle(P0, "Arthur, Marigold Knight", 4, 5, ARTHUR_MARIGOLD_KNIGHT)
        .id();
    let vanilla = scenario.add_vanilla(P0, 2, 2);
    let clone = scenario
        .add_creature_to_hand_from_oracle(P0, "Clone", 0, 0, CLONE)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let mut runner = scenario.build();
    seat_on_library_top(&mut runner, clone);
    let top_six = top_of_library(runner.state(), 6);
    assert_eq!(top_six[0], clone);
    let p0_before = life(runner.state(), P0);
    let mut events = Vec::new();
    runner.advance_to_combat();
    let attacked = runner
        .declare_attackers(&[
            (arthur, AttackTarget::Player(P1)),
            (vanilla, AttackTarget::Player(P1)),
        ])
        .expect("the attack is declared");
    events.extend(attacked.events);
    for _ in 0..40 {
        record_stack(runner.state());
        let action = match &runner.state().waiting_for {
            WaitingFor::DigChoice { .. } => GameAction::SelectCards { cards: vec![clone] },
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::Priority { .. } if !resolution_is_settled(runner.state()) => {
                GameAction::PassPriority
            }
            _ => break,
        };
        act(&mut runner, action, &mut events);
    }
    // Reach: Clone's optional copy replacement, under the batch owner.
    assert!(
        stack_frames(runner.state()).contains(&FrameKind::BatchDelivery),
        "{:?}",
        stack_frames(runner.state())
    );
    accept_replacement(&mut runner, &mut events);
    // Reach: the copy choice, with the batch owner holding Clone's entry.
    let held = batch_owner_delivery(&mut runner).expect("the batch owner");
    assert!(holds_entry_of(&held, clone), "{held:?}");
    choose_copy_target(&mut runner, vanilla, &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let state = runner.state();
    assert_eq!(life(state, P0), p0_before + 1, "Warden once");
    assert_eq!(zone(state, clone), Zone::Battlefield);
    let copied = &state.objects[&clone];
    assert_eq!((copied.power, copied.toughness), (Some(2), Some(2)));
    assert_eq!(
        attacks_of(state, clone),
        vec![(P1, AttackTarget::Player(P1))]
    );
    assert_rest_on_bottom(state, &top_six, clone, "Clone");
    assert_eq!(
        returns_installed_by(state, arthur),
        vec![vec![TargetRef::Object(clone)]],
        "the return is bound to Clone"
    );
    assert!(
        state.resolution_stack.is_empty(),
        "{:?}",
        stack_frames(state)
    );
    assert!(state.deferred_entry_events.is_empty());
}

// ---------------------------------------------------------------------------
// The as-enters roll is parked with its entry
// ---------------------------------------------------------------------------

/// How many of the entrant's own as-enters rolls the park holds.
fn parked_rolls(state: &GameState) -> usize {
    state
        .deferred_entry_events
        .iter()
        .filter(|parked| matches!(parked.event, GameEvent::DieRolled { .. }))
        .count()
}

/// R-3. CR 603.3b: on the Grumgully ordering route the roll's observer and the
/// entry's observer both reach the stack by the Priority the counter answer
/// leads to. This does not tell one batch from two at that Priority.
#[test]
fn an_ordered_rolls_observer_and_entry_observer_reach_one_priority() {
    let fold = Fold::Grumgully("Hammer Jammer");
    let mut board = fold_board(7, fold, false);
    let mut events = Vec::new();
    to_counter_prompt(&mut board, fold, &mut events);
    let FoldBoard {
        mut runner,
        dwarf,
        warden,
        ..
    } = board;
    answer_counter_prompt(&mut runner, FirstPick::Scales, &mut events);
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
    }
    let state = runner.state();
    let on_stack = |source| {
        state
            .stack
            .iter()
            .filter(|entry| entry.source_id == source)
            .count()
    };
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        state.waiting_for
    );
    assert_eq!(
        (on_stack(dwarf), on_stack(warden)),
        (1, 1),
        "both triggers at the same Priority"
    );
}

fn tie_board(seed: u64) -> (GameRunner, ObjectId) {
    let mut scenario = board(seed);
    add_barbarian_class(&mut scenario);
    add_counter_replacements(&mut scenario);
    add_dwarf(&mut scenario);
    let jammer = free_jammer(&mut scenario);
    (scenario.build(), jammer)
}

/// R-6. CR 603.2 + CR 706.6: the tie's answer rolls, and the counter prompt that
/// follows does not lose the roll for Brazen Dwarf.
#[test]
fn a_tied_roll_before_a_counter_prompt_is_seen_once() {
    let seed = find_seed("a Jammer tie", |seed| {
        let (mut runner, jammer) = tie_board(seed);
        cast_and_pass(&mut runner, jammer, None, None, &mut Vec::new());
        tie_prompt_results(runner.state()).is_some()
    });
    for pick in FirstPick::BOTH {
        reset_triggers_seen();
        let (mut runner, jammer) = tie_board(seed);
        let before = life(runner.state(), P1);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, jammer, None, None, &mut events);
        let naturals = answer_tie(&mut runner, &mut events);
        assert_counter_prompt(runner.state());
        settle_picking(&mut runner, &mut events, pick);
        let state = runner.state();
        assert_eq!(before - life(state, P1), 1, "{pick:?}: Dwarf once");
        assert_eq!(triggers_seen_from(jammer), 0, "{pick:?}");
        assert_eq!(
            plus_counters(state, jammer),
            pick.counters(survivors_total(&naturals)),
            "{pick:?}: {naturals:?}"
        );
        assert_finished(state, &[], &format!("{pick:?}"));
    }
}

/// R-7. CR 616.1 + CR 706.6 + CR 603.2: the roll's own ordering answer rolls, and the
/// counter prompt that follows does not lose the roll for Brazen Dwarf.
#[test]
fn an_ordered_roll_before_a_counter_prompt_is_seen_once() {
    for pick in FirstPick::BOTH {
        reset_triggers_seen();
        let mut scenario = board(7);
        add_barbarian_class(&mut scenario);
        scenario.add_enchantment_from_oracle(P0, "Pixie Guide", PIXIE_GUIDE);
        add_counter_replacements(&mut scenario);
        add_dwarf(&mut scenario);
        let jammer = free_jammer(&mut scenario);
        let mut runner = scenario.build();
        let before = life(runner.state(), P1);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, jammer, None, None, &mut events);
        let index = replacement_index(runner.state(), "Barbarian Class");
        act(
            &mut runner,
            GameAction::ChooseReplacement { index },
            &mut events,
        );
        assert_counter_prompt(runner.state());
        settle_picking(&mut runner, &mut events, pick);
        let state = runner.state();
        let rolls = die_results(&events);
        assert_eq!(rolls.len(), 1, "{pick:?}: {rolls:?}");
        assert_eq!(ignored_rolls(&events), 2, "{pick:?}");
        assert_eq!(before - life(state, P1), 1, "{pick:?}: Dwarf once");
        assert_eq!(triggers_seen_from(jammer), 0, "{pick:?}");
        assert_eq!(
            plus_counters(state, jammer),
            pick.counters(rolls[0]),
            "{pick:?}"
        );
        assert_finished(state, &[], &format!("{pick:?}"));
    }
}

/// R-9. Eureka + CR 603.3 + CR 608.2: P0's ordered put completes inline, and P1's
/// "may put" follows in the same resolution. The roll waits parked across P1's
/// prompt, and Brazen Dwarf's trigger goes on the stack once, after Eureka.
///
/// The entry itself is not asserted: it completes inline inside the ordering
/// answer, whose action ends on P1's prompt without its events being scanned, so
/// an entry observer misses it, as before this change (filed separately).
#[test]
fn an_inline_roll_before_the_next_seats_choice_is_seen_once() {
    for first in ["Hammer Jammer", "Grumgully, the Generous"] {
        reset_triggers_seen();
        let mut scenario = board(7);
        add_grumgully(&mut scenario);
        let dwarf = add_dwarf(&mut scenario);
        let jammer = free_jammer(&mut scenario);
        let eureka = free_spell(&mut scenario, "Eureka", false, EUREKA);
        scenario
            .add_creature_to_hand(P1, "Vanilla Bear", 2, 2)
            .with_mana_cost(ManaCost::generic(0));
        let mut runner = scenario.build();
        let p1_before = life(runner.state(), P1);
        let mut events = put_until_roll(&mut runner, eureka, jammer);
        order_entry(&mut runner, first, &mut events);

        // Reach: the roll happened and the entry completed in this action, which
        // ends on P1's choice.
        let state = runner.state();
        let rolls = die_results(&events);
        assert_eq!(rolls.len(), 1, "{first}: {rolls:?}");
        assert_eq!(zone(state, jammer), Zone::Battlefield, "{first}");
        assert_eq!(plus_counters(state, jammer), rolls[0] + 1, "{first}");
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::OptionalEffectChoice { player, .. } if player == P1
            ),
            "{first}: {:?}",
            state.waiting_for
        );
        let parked_at_choice = parked_rolls(state);

        settle(&mut runner, &mut events, DECLINE_ANSWERS);
        let state = runner.state();
        assert_eq!(p1_before - life(state, P1), 1, "{first}: Dwarf once");
        assert_eq!(triggers_seen_from(jammer), 0, "{first}");
        assert_eq!(
            parked_at_choice, 1,
            "{first}: the roll waits across P1's prompt"
        );
        assert_finished(state, &[dwarf], first);
    }
}

/// R-10. Chord of Calling + CR 603.2 + CR 603.2c: a search tutor's inline as-enters
/// roll is released by the search's collector, and its copy in the action's events
/// is not collected again. Brazen Dwarf and Soul Warden each trigger once.
#[test]
fn a_searched_entrants_roll_is_seen_once() {
    reset_triggers_seen();
    let mut scenario = board(7);
    let dwarf = add_dwarf(&mut scenario);
    let warden = add_warden_p0(&mut scenario);
    let jammer = jammer_card(&mut scenario);
    let chord = scenario
        .add_spell_to_hand_from_oracle(P0, "Chord of Calling", true, CHORD_OF_CALLING)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![
                ManaCostShard::X,
                ManaCostShard::Green,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
        })
        .id();
    scenario.with_mana_pool(P0, green(7));
    let mut runner = scenario.build();
    seat_on_library_top(&mut runner, jammer);
    let p1_before = life(runner.state(), P1);
    let p0_before = life(runner.state(), P0);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, chord, Some(4), None, &mut events);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::SearchChoice { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    let mut answered = Vec::new();
    act(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![jammer],
        },
        &mut answered,
    );
    // Reach: the roll happened in the answering action.
    assert_eq!(die_results(&answered).len(), 1);
    assert_eq!(zone(runner.state(), jammer), Zone::Battlefield);
    events.extend(answered);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let state = runner.state();
    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 1, "{rolls:?}");
    assert_eq!(p1_before - life(state, P1), 1, "Dwarf once");
    assert_eq!(life(state, P0), p0_before + 1, "Warden once");
    assert_eq!(triggers_seen_from(jammer), 0);
    assert_eq!(plus_counters(state, jammer), rolls[0]);
    assert_finished(state, &[dwarf, warden], "Chord");
}

// ---------------------------------------------------------------------------
// The entry's custody from its delivery boundary, the seat frame, and the
// roll's observers collected when it is rolled
// ---------------------------------------------------------------------------

/// Which of Doubling Season and Winding Constrictor a counter prompt applies
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DoublerPick {
    DoublingSeason,
    WindingConstrictor,
}

impl DoublerPick {
    const BOTH: [DoublerPick; 2] = [DoublerPick::DoublingSeason, DoublerPick::WindingConstrictor];

    fn source_name(self) -> &'static str {
        match self {
            DoublerPick::DoublingSeason => "Doubling Season",
            DoublerPick::WindingConstrictor => "Winding Constrictor",
        }
    }

    /// CR 614.5 + CR 616.1: `n` counters of one kind are one number, which each
    /// replacement modifies once, in the order chosen. Doubling Season first
    /// doubles, then Winding Constrictor adds one; the other order adds one, then
    /// doubles.
    fn counters(self, n: u32) -> u32 {
        match self {
            DoublerPick::DoublingSeason => n * 2 + 1,
            DoublerPick::WindingConstrictor => (n + 1) * 2,
        }
    }
}

/// Is this `ReplacementChoice` the Doubling Season / Winding Constrictor counter
/// ordering?
fn is_doubler_prompt(state: &GameState) -> bool {
    matches!(
        &state.waiting_for,
        WaitingFor::ReplacementChoice { candidates, .. }
            if candidates.iter().any(|c| c.source_name == "Doubling Season")
    )
}

/// Answer one Doubling Season / Winding Constrictor counter prompt, applying
/// `pick` first.
fn answer_doubler_prompt(runner: &mut GameRunner, pick: DoublerPick, events: &mut Vec<GameEvent>) {
    assert!(
        is_doubler_prompt(runner.state()),
        "expected the doubler prompt, got {:?} on {:?}",
        runner.state().waiting_for,
        stack_frames(runner.state())
    );
    let index = replacement_index(runner.state(), pick.source_name());
    act(runner, GameAction::ChooseReplacement { index }, events);
}

fn add_doublers(scenario: &mut GameScenario) {
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON);
    scenario.add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR);
}

fn tayam_and_doublers(scenario: &mut GameScenario) {
    scenario.add_creature_from_oracle(P0, "Tayam, Luminous Enigma", 3, 3, TAYAM);
    add_doublers(scenario);
}

fn finality_counters(state: &GameState, id: ObjectId) -> u32 {
    counters_of(state, id, &CounterType::Finality)
}

/// Does the park record an owed attacking seat on `id`'s battlefield entry?
fn owes_seat(state: &GameState, id: ObjectId) -> bool {
    state.deferred_entry_events.iter().any(|parked| {
        matches!(
            parked.event,
            GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if object_id == id
        ) && parked.entry_attack.is_some()
    })
}

/// The battlefield `ZoneChanged` of `id` in `events`, by position.
fn battlefield_entry_index(events: &[GameEvent], id: ObjectId) -> Option<usize> {
    events.iter().position(|event| {
        matches!(
            event,
            GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == id
        )
    })
}

/// R5 (F2-a, no owner, `CallerEpilogue`). Hammer Jammer cast with Tayam,
/// Doubling Season and Winding Constrictor + CR 614.12a + CR 616.1 + CR 603.6d:
/// the entry's ordering prompt comes first, then the vigilance placement's
/// prompt before the roll, then the roll's +1/+1 placement's. The entry is held
/// from that first counter pause, so the roll is made before Hammer Jammer
/// enters (CR 113.6): Brazen Dwarf sees it once (CR 603.2) and Soul Warden sees
/// the entry once (CR 603.6a).
#[test]
fn a_cast_entry_held_across_its_counter_pause_is_seen_once() {
    for first in DoublerPick::BOTH {
        for second in DoublerPick::BOTH {
            let what = format!("{first:?}, {second:?}");
            reset_triggers_seen();
            let mut scenario = board(7);
            tayam_and_doublers(&mut scenario);
            let dwarf = add_dwarf(&mut scenario);
            let warden = add_warden_p0(&mut scenario);
            let jammer = free_jammer(&mut scenario);
            let mut runner = scenario.build();
            let p1_before = life(runner.state(), P1);
            let p0_before = life(runner.state(), P0);
            let mut events = Vec::new();
            cast_and_pass(&mut runner, jammer, None, None, &mut events);
            order_entry(&mut runner, "Hammer Jammer", &mut events);
            let rolled_at_first = die_results(&events).len();
            let parked_at_first = parked_state_of(runner.state(), jammer);
            let warden_queued_at_first = queued_or_stacked_from(runner.state(), warden);
            answer_doubler_prompt(&mut runner, first, &mut events);
            answer_doubler_prompt(&mut runner, second, &mut events);
            settle(&mut runner, &mut events, DEFAULT_ANSWERS);
            let r = die_results(&events)[0];
            let state = runner.state();
            assert_eq!(p1_before - life(state, P1), 1, "{what}: Dwarf once");
            assert_eq!(life(state, P0), p0_before + 1, "{what}: Warden once");
            // Reach: the first counter prompt precedes the roll, with the entry
            // held pending and Soul Warden not yet queued.
            assert_eq!(rolled_at_first, 0, "{what}");
            assert!(
                matches!(parked_at_first, Some(ParkedEntryState::Pending { .. })),
                "{what}: {parked_at_first:?}"
            );
            assert_eq!(warden_queued_at_first, 0, "{what}");
            assert_eq!(triggers_seen_from(jammer), 0, "{what}");
            assert_eq!(
                counters_of(state, jammer, &vigilance_counter()),
                first.counters(1),
                "{what}"
            );
            assert_eq!(plus_counters(state, jammer), second.counters(r), "{what}");
            assert_finished(state, &[dwarf, warden], &what);
        }
    }
}

/// Rite of the Moth's board: Doubling Season, Winding Constrictor, Soul Warden,
/// `entrant` in P0's graveyard, and Brazen Dwarf when `with_dwarf`.
fn rite_board(
    entrant: (&str, &str),
    with_dwarf: bool,
) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>, ObjectId) {
    let mut scenario = board(7);
    add_doublers(&mut scenario);
    let dwarf = with_dwarf.then(|| add_dwarf(&mut scenario));
    let warden = add_warden_p0(&mut scenario);
    let entrant = graveyard_creature(&mut scenario, entrant.0, entrant.1);
    let rite = free_spell(&mut scenario, "Rite of the Moth", false, RITE_OF_THE_MOTH);
    (scenario.build(), rite, entrant, dwarf, warden)
}

/// R5b (F2-a under the targeted owner, `DeliveryTail`). Rite of the Moth returns
/// Hammer Jammer with a finality counter under Doubling Season and Winding
/// Constrictor + CR 614.1c + CR 616.1 + CR 603.6d: the finality placement's
/// prompt comes before the roll, and the roll's +1/+1 placement prompts again.
/// The entry is held from the first pause, so Brazen Dwarf sees the roll once
/// and Soul Warden sees the entry once, collected by its owner (CR 603.2).
#[test]
fn a_returned_entry_held_across_its_counter_pause_is_seen_once() {
    for first in DoublerPick::BOTH {
        for second in DoublerPick::BOTH {
            let what = format!("{first:?}, {second:?}");
            reset_triggers_seen();
            let (mut runner, rite, jammer, dwarf, warden) =
                rite_board(("Hammer Jammer", HAMMER_JAMMER), true);
            let dwarf = dwarf.expect("Brazen Dwarf");
            let p1_before = life(runner.state(), P1);
            let p0_before = life(runner.state(), P0);
            let mut events = Vec::new();
            cast_and_pass(&mut runner, rite, None, Some(jammer), &mut events);
            let rolled_at_first = die_results(&events).len();
            let parked_at_first = parked_state_of(runner.state(), jammer);
            let frames_at_first = stack_frames(runner.state());
            answer_doubler_prompt(&mut runner, first, &mut events);
            answer_doubler_prompt(&mut runner, second, &mut events);
            settle(&mut runner, &mut events, DEFAULT_ANSWERS);
            let r = die_results(&events)[0];
            let state = runner.state();
            assert_eq!(p1_before - life(state, P1), 1, "{what}: Dwarf once");
            assert_eq!(life(state, P0), p0_before + 1, "{what}: Warden once");
            assert_eq!(rolled_at_first, 0, "{what}");
            assert!(
                matches!(parked_at_first, Some(ParkedEntryState::Pending { .. })),
                "{what}: {parked_at_first:?}"
            );
            assert_eq!(
                frames_at_first,
                vec![
                    FrameKind::ChangeZone,
                    FrameKind::PostReplacement,
                    FrameKind::CounterAdditions,
                ],
                "{what}"
            );
            assert_eq!(triggers_seen_from(jammer), 0, "{what}");
            assert_eq!(
                finality_counters(state, jammer),
                first.counters(1),
                "{what}"
            );
            assert_eq!(plus_counters(state, jammer), second.counters(r), "{what}");
            assert_finished(state, &[dwarf, warden], &what);
        }
    }
}

/// R5c (F2-a, an inline continuation under an owner). Rite of the Moth returns
/// Hydradoodle + CR 107.3m + CR 706.1: off the stack X is 0, so the continuation
/// rolls no dice and completes inline once the finality placement's prompt is
/// answered. Hydradoodle is a 0/0, so it is put into the graveyard (CR 704.5f)
/// and its finality counter exiles it instead. Soul Warden saw the entry once
/// (CR 603.6a + CR 603.10).
#[test]
fn an_inline_continuation_under_an_owner_is_collected_once() {
    for pick in DoublerPick::BOTH {
        let what = format!("{pick:?}");
        let (mut runner, rite, doodle, _, warden) = rite_board(("Hydradoodle", HYDRADOODLE), false);
        let p0_before = life(runner.state(), P0);
        let mut events = Vec::new();
        cast_and_pass(&mut runner, rite, None, Some(doodle), &mut events);
        let parked_at_prompt = parked_state_of(runner.state(), doodle);
        answer_doubler_prompt(&mut runner, pick, &mut events);
        let rolled_after_answer = roll_resolved_for(&events, doodle);
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let state = runner.state();
        assert_eq!(life(state, P0), p0_before + 1, "{what}: Warden once");
        assert!(
            matches!(parked_at_prompt, Some(ParkedEntryState::Pending { .. })),
            "{what}: {parked_at_prompt:?}"
        );
        assert_eq!(rolled_after_answer, 1, "{what}");
        assert!(die_results(&events).is_empty(), "{what}");
        assert_eq!(zone(state, doodle), Zone::Exile, "{what}");
        assert_finished(state, &[warden], &what);
    }
}

/// R6 (F2-b). Yore-Tiller Nephilim + Grumgully, the Generous on three seats +
/// CR 508.4 + CR 614.12a + CR 614.1c: the entry's ordering answer delivers under
/// `CallerEpilogue`, whose seat asks the attack target before the chain rolls.
/// The entry is held from that prompt, so the roll is made before Hammer Jammer
/// enters (CR 113.6) and Grumgully's counter joins its placement (r + 1). Brazen
/// Dwarf sees the roll once and Soul Warden sees the entry once.
#[test]
fn an_entry_held_across_its_attack_target_prompt_is_seen_once() {
    reset_triggers_seen();
    let mut board = attack_board(7, 3, Putter::Nephilim, add_grumgully);
    let p0_before = life(board.runner.state(), P0);
    let mut events = Vec::new();
    attack_to_entry_prompt(&mut board, &mut events);
    let AttackBoard {
        mut runner,
        jammer,
        dwarf,
        warden,
        ..
    } = board;
    order_entry(&mut runner, "Hammer Jammer", &mut events);
    pass_to_attack_target_choice(&mut runner, &mut events);
    let rolled_at_prompt = die_results(&events).len();
    let parked_at_prompt = parked_state_of(runner.state(), jammer);
    let frames_at_prompt = stack_frames(runner.state());
    act(
        &mut runner,
        GameAction::ChooseEntryAttackTarget {
            target: AttackTarget::Player(P2),
        },
        &mut events,
    );
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let r = die_results(&events)[0];
    let state = runner.state();
    assert_eq!(
        triggers_seen_from(jammer),
        0,
        "Hammer Jammer does not see its roll"
    );
    assert_eq!(plus_counters(state, jammer), r + 1, "r = {r}");
    assert_eq!(life(state, P0), p0_before + 1, "Warden once");
    assert_eq!(rolled_at_prompt, 0);
    assert!(
        matches!(parked_at_prompt, Some(ParkedEntryState::Pending { .. })),
        "{parked_at_prompt:?}"
    );
    assert_eq!(
        frames_at_prompt,
        vec![FrameKind::ChangeZone, FrameKind::PostReplacement]
    );
    assert_eq!(
        attacks_of(state, jammer),
        vec![(P2, AttackTarget::Player(P2))]
    );
    assert_eq!(triggers_seen_from(dwarf), 1, "Dwarf once");
    assert_finished(state, &[dwarf, warden], "R6");
}

/// R6b / R6c. An attacking entry delivered by its ordering answer under
/// `CallerEpilogue`, with Tayam, Doubling Season and Winding Constrictor: the
/// entry is held from the vigilance placement's prompt, the roll's placement
/// prompts again, and the delivery's own counter step seats Hammer Jammer after
/// the second answer (CR 508.4), asking its target on three seats.
fn run_ordered_attacking_doubler_entry(
    putter: Putter,
    players: u8,
    first: DoublerPick,
    second: DoublerPick,
) {
    let what = format!("{putter:?}, {players} seats, {first:?}, {second:?}");
    reset_triggers_seen();
    let mut board = attack_board(7, players, putter, tayam_and_doublers);
    let p0_before = life(board.runner.state(), P0);
    let mut events = Vec::new();
    attack_to_entry_prompt(&mut board, &mut events);
    let AttackBoard {
        mut runner,
        jammer,
        dwarf,
        warden,
        ..
    } = board;
    order_entry(&mut runner, "Hammer Jammer", &mut events);
    let parked_at_first = parked_state_of(runner.state(), jammer);
    let frames_at_first = stack_frames(runner.state());
    answer_doubler_prompt(&mut runner, first, &mut events);
    let attacking_at_second = attacks_of(runner.state(), jammer);
    answer_doubler_prompt(&mut runner, second, &mut events);
    let defender = if players == 3 {
        pass_to_attack_target_choice(&mut runner, &mut events);
        act(
            &mut runner,
            GameAction::ChooseEntryAttackTarget {
                target: AttackTarget::Player(P2),
            },
            &mut events,
        );
        P2
    } else {
        P1
    };
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let r = die_results(&events)[0];
    let state = runner.state();
    assert_eq!(triggers_seen_from(dwarf), 1, "{what}: Dwarf once");
    assert_eq!(life(state, P0), p0_before + 1, "{what}: Warden once");
    assert!(
        matches!(parked_at_first, Some(ParkedEntryState::Pending { .. })),
        "{what}: {parked_at_first:?}"
    );
    assert_eq!(
        frames_at_first.last(),
        Some(&FrameKind::CounterAdditions),
        "{what}: {frames_at_first:?}"
    );
    assert!(attacking_at_second.is_empty(), "{what}");
    assert_eq!(triggers_seen_from(jammer), 0, "{what}");
    assert_eq!(
        attacks_of(state, jammer),
        vec![(defender, AttackTarget::Player(defender))],
        "{what}"
    );
    assert_eq!(
        counters_of(state, jammer, &vigilance_counter()),
        first.counters(1),
        "{what}"
    );
    assert_eq!(plus_counters(state, jammer), second.counters(r), "{what}");
    assert_finished(state, &[dwarf, warden], &what);
}

/// R6b. Yore-Tiller Nephilim (the targeted owner) + CR 508.4 + CR 603.2: the
/// held entry is seen once by Brazen Dwarf's roll observer and Soul Warden.
#[test]
fn an_owned_ordered_attacking_entry_held_across_its_counter_pause_is_seen_once() {
    for first in DoublerPick::BOTH {
        for second in DoublerPick::BOTH {
            run_ordered_attacking_doubler_entry(Putter::Nephilim, 2, first, second);
        }
        run_ordered_attacking_doubler_entry(Putter::Nephilim, 3, first, first);
    }
}

/// R6c. Ilharg, the Raze-Boar (no owner) + CR 508.4 + CR 603.2: the same, with the
/// settled release collecting the entry. Ilharg's instruction count and its
/// "Return that creature" rider on this route are not asserted (filed
/// separately).
#[test]
fn an_unowned_ordered_attacking_entry_held_across_its_counter_pause_is_seen_once() {
    for first in DoublerPick::BOTH {
        for second in DoublerPick::BOTH {
            run_ordered_attacking_doubler_entry(Putter::Ilharg, 2, first, second);
        }
        run_ordered_attacking_doubler_entry(Putter::Ilharg, 3, first, first);
    }
}

/// Storm the Festival over `cards` (seated so the first is on top), with Soul
/// Warden, Brazen Dwarf when `resident_dwarf`, plus `extras`, and a free Box of
/// Free-Range Goblins in hand.
struct NewcomerFestival {
    runner: GameRunner,
    warden: ObjectId,
    resident_dwarf: Option<ObjectId>,
    festival: ObjectId,
    box_of_goblins: ObjectId,
}

fn newcomer_festival(
    mut scenario: GameScenario,
    extras: fn(&mut GameScenario),
    resident_dwarf: bool,
    cards: &[ObjectId],
) -> NewcomerFestival {
    extras(&mut scenario);
    let resident_dwarf = resident_dwarf.then(|| add_dwarf(&mut scenario));
    let warden = add_warden_p0(&mut scenario);
    let festival = free_spell(
        &mut scenario,
        "Storm the Festival",
        false,
        STORM_THE_FESTIVAL,
    );
    let box_of_goblins = free_spell(
        &mut scenario,
        "Box of Free-Range Goblins",
        false,
        BOX_OF_FREE_RANGE_GOBLINS,
    );
    let mut runner = scenario.build();
    for card in cards.iter().rev() {
        seat_on_library_top(&mut runner, *card);
    }
    NewcomerFestival {
        runner,
        warden,
        resident_dwarf,
        festival,
        box_of_goblins,
    }
}

/// A {1}{R} Brazen Dwarf in P0's hand, for `seat_on_library_top`.
fn dwarf_card(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Brazen Dwarf", 1, 3, BRAZEN_DWARF)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red],
        })
        .id()
}

/// Storm the Festival's look-at choice: keep `cards`.
fn festival_keeps(
    runner: &mut GameRunner,
    festival: ObjectId,
    cards: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) {
    cast_and_pass(runner, festival, None, None, events);
    assert_dig_choice(runner.state());
    act(runner, GameAction::SelectCards { cards }, events);
}

/// Cast Box of Free-Range Goblins and settle, answering any counter prompt with
/// `pick`: its roll is P0's, made with the newcomer on the battlefield.
fn cast_box_and_settle(runner: &mut GameRunner, box_of_goblins: ObjectId, pick: Option<FirstPick>) {
    let mut events = Vec::new();
    cast_and_pass(runner, box_of_goblins, None, None, &mut events);
    match pick {
        Some(pick) => {
            settle_picking(runner, &mut events, pick);
        }
        None => {
            settle(runner, &mut events, DEFAULT_ANSWERS);
        }
    }
}

/// A Storm the Festival row's counter pick and the replacements it adds.
type FestivalCase = (Option<FirstPick>, fn(&mut GameScenario));

/// R7 (F3). Storm the Festival puts Hammer Jammer, then Brazen Dwarf, onto the
/// battlefield + CR 603.2 + CR 603.10: Brazen Dwarf is not on the battlefield
/// when Hammer Jammer's as-enters roll is made, so it does not trigger on it. A
/// roll P0 makes once Brazen Dwarf is on the battlefield triggers it, and
/// Hammer Jammer once (CR 603.2). (a) No counter replacements; (b) Hardened
/// Scales and Branching Evolution, so Hammer Jammer's roll placement pauses
/// before Brazen Dwarf is delivered.
#[test]
fn a_newcomer_does_not_see_an_earlier_members_roll() {
    let cases: [FestivalCase; 3] = [
        (None, no_extras),
        (Some(FirstPick::Scales), add_counter_replacements),
        (Some(FirstPick::Evolution), add_counter_replacements),
    ];
    for (pick, extras) in cases {
        let what = format!("{pick:?}");
        reset_triggers_seen();
        let mut scenario = board(7);
        let jammer = jammer_card(&mut scenario);
        let dwarf = dwarf_card(&mut scenario);
        let NewcomerFestival {
            mut runner,
            warden,
            festival,
            box_of_goblins,
            ..
        } = newcomer_festival(scenario, extras, false, &[jammer, dwarf]);
        let p1_before = life(runner.state(), P1);
        let p0_before = life(runner.state(), P0);
        let mut events = Vec::new();
        festival_keeps(&mut runner, festival, vec![jammer, dwarf], &mut events);
        let frames_at_prompt = pick.map(|pick| {
            assert_counter_prompt(runner.state());
            let frames = stack_frames(runner.state());
            answer_counter_prompt(&mut runner, pick, &mut events);
            frames
        });
        settle(&mut runner, &mut events, DEFAULT_ANSWERS);
        let r = die_results(&events)[0];
        let state = runner.state();
        assert_eq!(
            life(state, P1),
            p1_before,
            "{what}: the newcomer does not see it"
        );
        let roll_at = events
            .iter()
            .position(|event| matches!(event, GameEvent::DieRolled { .. }))
            .expect("Hammer Jammer rolled");
        let dwarf_entry = battlefield_entry_index(&events, dwarf).expect("Brazen Dwarf entered");
        assert!(
            roll_at < dwarf_entry,
            "{what}: the roll precedes the newcomer"
        );
        assert_eq!(zone(state, dwarf), Zone::Battlefield, "{what}");
        if let Some(frames) = frames_at_prompt {
            assert_eq!(
                frames,
                vec![
                    FrameKind::PostReplacement,
                    FrameKind::CounterAdditions,
                    FrameKind::BatchDelivery,
                ],
                "{what}"
            );
        }
        assert_eq!(triggers_seen_from(jammer), 0, "{what}");
        assert_eq!(life(state, P0), p0_before + 2, "{what}: Warden twice");
        let expected = pick.map_or(r, |pick| pick.counters(r));
        assert_eq!(plus_counters(state, jammer), expected, "{what}, r = {r}");

        let p1_before_box = life(state, P1);
        let _ = take_triggers_seen_from(jammer);
        cast_box_and_settle(&mut runner, box_of_goblins, pick);
        let state = runner.state();
        assert_eq!(
            p1_before_box - life(state, P1),
            1,
            "{what}: the newcomer sees a later roll"
        );
        assert_eq!(
            triggers_seen_from(jammer),
            1,
            "{what}: Hammer Jammer sees the Box's roll"
        );
        assert_finished(state, &[dwarf, warden], &what);
    }
}

/// R9 (F3, two rollers). Storm the Festival puts two Hammer Jammers onto the
/// battlefield with Brazen Dwarf resident + CR 603.2 + CR 603.2c + CR 113.6: each
/// as-enters roll is its own instruction, so the resident Dwarf triggers twice.
/// The later Hammer Jammer is not on the battlefield at the earlier roll, and its
/// own roll is made before it enters, so it sees neither. CR 603.6a +
/// CR 603.6d: the two enter in one event, so the earlier Hammer Jammer is not
/// an observer of the later one's roll either, and each keeps its own result.
/// The seed is the first whose two rolls differ, so a counter replaced by the
/// other Jammer's roll is visible.
#[test]
fn each_as_enters_roll_of_a_batch_is_its_own_instruction() {
    let two_jammers = |seed| {
        let mut scenario = board(seed);
        let first = jammer_card(&mut scenario);
        let second = jammer_card(&mut scenario);
        let festival = newcomer_festival(scenario, no_extras, true, &[first, second]);
        (festival, first, second)
    };
    let seed = find_seed("two differing batch rolls", |seed| {
        let (mut board, first, second) = two_jammers(seed);
        let mut events = Vec::new();
        festival_keeps(
            &mut board.runner,
            board.festival,
            vec![first, second],
            &mut events,
        );
        settle(&mut board.runner, &mut events, DEFAULT_ANSWERS);
        let rolls = die_results(&events);
        rolls.len() == 2 && rolls[0] != rolls[1]
    });
    reset_triggers_seen();
    let (board, first, second) = two_jammers(seed);
    let NewcomerFestival {
        mut runner,
        warden,
        resident_dwarf,
        festival,
        ..
    } = board;
    let dwarf = resident_dwarf.expect("the resident Dwarf");
    let p0_before = life(runner.state(), P0);
    let mut events = Vec::new();
    festival_keeps(&mut runner, festival, vec![first, second], &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let state = runner.state();
    assert_eq!(
        triggers_seen_from(first),
        0,
        "the earlier entrant enters with the later roll, so it does not see it"
    );
    assert_eq!(
        triggers_seen_from(second),
        0,
        "the later entrant sees no roll"
    );
    assert_eq!(
        triggers_seen_from(dwarf),
        2,
        "one trigger per roll instruction"
    );
    let rollers: Vec<Option<ObjectId>> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DieRolled {
                pre_entry_source, ..
            } => Some(*pre_entry_source),
            _ => None,
        })
        .collect();
    assert_eq!(rollers, vec![Some(first), Some(second)]);
    let rolls = die_results(&events);
    assert_ne!(rolls[0], rolls[1], "reach: the seed's rolls differ");
    assert_eq!(
        plus_counters(state, first),
        rolls[0],
        "the earlier Jammer keeps its own roll, {rolls:?}"
    );
    assert_eq!(plus_counters(state, second), rolls[1], "{rolls:?}");
    assert_eq!(life(state, P0), p0_before + 2, "Warden twice");
    assert_finished(state, &[first, second, dwarf, warden], "R9");
}

/// R13 (F1-b, Devour, no owner). Ilharg, the Raze-Boar puts Thunder-Thrash Elder
/// onto the battlefield "tapped and attacking" + CR 702.82a + CR 614.12a +
/// CR 508.4: the Devour sacrifice is chosen as it enters, and the seat frame,
/// parked beneath the entry's Devour snapshot, seats it once its entry has
/// completed. It enters with three +1/+1 counters, and Soul Warden sees it once.
#[test]
fn a_single_pick_devour_entry_is_seated_after_its_sacrifice() {
    let mut scenario = board(7);
    let warden = add_warden_p0(&mut scenario);
    let victim = scenario.add_vanilla(P0, 2, 2);
    let ilharg = scenario
        .add_creature_from_oracle(P0, "Ilharg, the Raze-Boar", 6, 6, ILHARG_THE_RAZE_BOAR)
        .id();
    let elder = thunder_thrash_elder(&mut scenario);
    let mut runner = scenario.build();
    let p0_before = life(runner.state(), P0);
    let mut events = Vec::new();
    ilharg_attacks_to_entry_prompt(&mut runner, ilharg, &mut events);
    assert!(
        effect_zone_choice(runner.state()),
        "{:?}",
        runner.state().waiting_for
    );
    let frames_at_prompt = stack_frames(runner.state());
    let parked_at_prompt = parked_state_of(runner.state(), elder);
    act(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![victim],
        },
        &mut events,
    );
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let state = runner.state();
    assert_eq!(
        attacks_of(state, elder),
        vec![(P1, AttackTarget::Player(P1))]
    );
    assert_eq!(
        frames_at_prompt,
        vec![
            FrameKind::EntryAttackSeat,
            FrameKind::ChangeZone,
            FrameKind::PostReplacement,
            FrameKind::AbilityContinuation,
        ]
    );
    assert!(
        matches!(parked_at_prompt, Some(ParkedEntryState::Pending { .. })),
        "{parked_at_prompt:?}"
    );
    assert_eq!(plus_counters(state, elder), 3);
    assert_eq!(life(state, P0), p0_before + 1, "Warden once");
    assert_eq!(
        returns_installed_by(state, ilharg),
        vec![vec![TargetRef::Object(elder)]],
        "the return is bound to the Elder"
    );
    assert_eq!(zone(state, victim), Zone::Graveyard);
    assert!(!stack_frames(state).contains(&FrameKind::ChangeZone));
    assert_finished(state, &[warden], "R13");
}

/// R14 (F2-c). Grim Reaper, Lethal Legionnaire pays {3}{B} and returns Hammer
/// Jammer "tapped and attacking with a finality counter on it", under Doubling
/// Season and Winding Constrictor + CR 508.4 + CR 616.1 + CR 603.6d: the finality
/// placement's prompt comes before the roll and the roll's placement prompts
/// again. The targeted owner seats Hammer Jammer once, after both, asking its
/// target on three seats. Brazen Dwarf sees the roll once and Soul Warden the
/// entry once.
#[test]
fn an_owed_seat_at_an_owners_counter_pause_is_seated_once() {
    for players in [2u8, 3] {
        for first in DoublerPick::BOTH {
            for second in DoublerPick::BOTH {
                if players == 3 && second != first {
                    continue;
                }
                let what = format!("{players} seats, {first:?}, {second:?}");
                reset_triggers_seen();
                let mut board = attack_board(7, players, Putter::GrimReaper, add_doublers);
                let p0_before = life(board.runner.state(), P0);
                let mut events = Vec::new();
                declare_board_attack(&mut board, &mut events);
                // The pool empties between steps, so the {3}{B} is added once the
                // attack is declared.
                {
                    let state = board.runner.state_mut();
                    let _ = state.add_mana_to_pool(
                        P0,
                        ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
                    );
                    for unit in green(3) {
                        let _ = state.add_mana_to_pool(P0, unit);
                    }
                }
                drive_attack_trigger_to_entry_prompt(&mut board, &mut events);
                let AttackBoard {
                    mut runner,
                    jammer,
                    dwarf,
                    warden,
                    ..
                } = board;
                let frames_at_first = stack_frames(runner.state());
                answer_doubler_prompt(&mut runner, first, &mut events);
                let attacking_at_second = attacks_of(runner.state(), jammer);
                let owed_at_second = owes_seat(runner.state(), jammer);
                answer_doubler_prompt(&mut runner, second, &mut events);
                let mut attack_prompts = 0;
                let mut owner_at_attack_prompt = Vec::new();
                for _ in 0..20 {
                    record_stack(runner.state());
                    match &runner.state().waiting_for {
                        WaitingFor::EntryAttackTargetChoice { .. } => {
                            attack_prompts += 1;
                            owner_at_attack_prompt.push(has_change_zone_owner(runner.state()));
                            act(
                                &mut runner,
                                GameAction::ChooseEntryAttackTarget {
                                    target: AttackTarget::Player(P2),
                                },
                                &mut events,
                            );
                        }
                        WaitingFor::Priority { .. } if !resolution_is_settled(runner.state()) => {
                            act(&mut runner, GameAction::PassPriority, &mut events);
                        }
                        _ => break,
                    }
                }
                settle(&mut runner, &mut events, DEFAULT_ANSWERS);
                let r = die_results(&events)[0];
                let state = runner.state();
                assert_eq!(triggers_seen_from(dwarf), 1, "{what}: Dwarf once");
                assert_eq!(life(state, P0), p0_before + 1, "{what}: Warden once");
                assert_eq!(
                    frames_at_first,
                    vec![
                        FrameKind::ChangeZone,
                        FrameKind::PostReplacement,
                        FrameKind::CounterAdditions,
                    ],
                    "{what}"
                );
                assert!(attacking_at_second.is_empty(), "{what}");
                let defender = if players == 3 { P2 } else { P1 };
                if players == 3 {
                    assert_eq!(attack_prompts, 1, "{what}: the target is asked once");
                    assert_eq!(owner_at_attack_prompt, vec![true], "{what}: the owner asks");
                } else {
                    assert_eq!(attack_prompts, 0, "{what}");
                    assert!(
                        owed_at_second,
                        "{what}: the seat is owed at the second prompt"
                    );
                }
                assert_eq!(
                    attacks_of(state, jammer),
                    vec![(defender, AttackTarget::Player(defender))],
                    "{what}: attacks once"
                );
                assert_eq!(triggers_seen_from(jammer), 0, "{what}");
                assert_eq!(
                    finality_counters(state, jammer),
                    first.counters(1),
                    "{what}"
                );
                assert_eq!(plus_counters(state, jammer), second.counters(r), "{what}");
                assert_finished(state, &[dwarf, warden], &what);
            }
        }
    }
}

/// R17 / R17b's board: Storm the Festival over `cards` with Barbarian Class and
/// Soul Warden, and Brazen Dwarf resident when `resident_dwarf`.
fn barbarian_festival(
    seed: u64,
    resident_dwarf: bool,
    with_newcomer: bool,
) -> (NewcomerFestival, ObjectId, Option<ObjectId>) {
    let mut scenario = board(seed);
    let jammer = jammer_card(&mut scenario);
    let newcomer = with_newcomer.then(|| dwarf_card(&mut scenario));
    let cards: Vec<ObjectId> = std::iter::once(jammer).chain(newcomer).collect();
    let festival = newcomer_festival(scenario, add_barbarian_class, resident_dwarf, &cards);
    (festival, jammer, newcomer)
}

fn barbarian_festival_ties(seed: u64, resident_dwarf: bool, with_newcomer: bool) -> bool {
    let (mut board, jammer, newcomer) = barbarian_festival(seed, resident_dwarf, with_newcomer);
    let cards = std::iter::once(jammer).chain(newcomer).collect();
    festival_keeps(&mut board.runner, board.festival, cards, &mut Vec::new());
    tie_prompt_results(board.runner.state()).is_some()
}

/// R17 (a batch member's keep choice). Storm the Festival over Hammer Jammer and
/// a Brazen Dwarf card, with Barbarian Class + CR 706.6 + CR 614.12a: Hammer
/// Jammer's tied roll asks which die to ignore before it enters, so the rest of
/// the batch waits beneath its whole child stack, and the frame that owns the
/// choice stays on top. Hammer Jammer enters with the kept result, before Brazen
/// Dwarf is delivered, so the newcomer does not see the roll (CR 603.10). A later
/// roll by P0 triggers it.
#[test]
fn a_batch_members_keep_choice_owns_the_top_of_the_stack() {
    let seed = find_seed("a batch member's tie", |seed| {
        barbarian_festival_ties(seed, false, true)
    });
    reset_triggers_seen();
    let (board, jammer, newcomer) = barbarian_festival(seed, false, true);
    let dwarf = newcomer.expect("the Brazen Dwarf card");
    let NewcomerFestival {
        mut runner,
        warden,
        festival,
        box_of_goblins,
        ..
    } = board;
    let p1_before = life(runner.state(), P1);
    let p0_before = life(runner.state(), P0);
    let mut events = Vec::new();
    festival_keeps(&mut runner, festival, vec![jammer, dwarf], &mut events);
    let frames_at_tie = stack_frames(runner.state());
    let parked_at_tie = parked_state_of(runner.state(), jammer);
    let naturals = answer_tie(&mut runner, &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let state = runner.state();
    assert_eq!(
        frames_at_tie,
        vec![
            FrameKind::BatchDelivery,
            FrameKind::PostReplacement,
            FrameKind::AbilityContinuation,
            FrameKind::DieRoll,
        ]
    );
    assert!(
        matches!(parked_at_tie, Some(ParkedEntryState::Pending { .. })),
        "{parked_at_tie:?}"
    );
    let counters_at = events
        .iter()
        .position(|event| {
            matches!(
                event,
                GameEvent::CounterAdded { object_id, .. } if *object_id == jammer
            )
        })
        .expect("Hammer Jammer's counters");
    let dwarf_entry = battlefield_entry_index(&events, dwarf).expect("Brazen Dwarf entered");
    assert!(
        counters_at < dwarf_entry,
        "the batch resumes only after Hammer Jammer's entry"
    );
    assert_eq!(
        plus_counters(state, jammer),
        survivors_total(&naturals),
        "{naturals:?}"
    );
    assert_eq!(
        triggers_seen_from(dwarf),
        0,
        "the newcomer does not see the roll"
    );
    assert_eq!(life(state, P1), p1_before);
    assert_eq!(triggers_seen_from(jammer), 0);
    assert_eq!(life(state, P0), p0_before + 2, "Warden twice");

    let p1_before_box = life(state, P1);
    let _ = take_triggers_seen_from(jammer);
    cast_box_and_settle(&mut runner, box_of_goblins, None);
    let state = runner.state();
    assert_eq!(
        p1_before_box - life(state, P1),
        1,
        "the newcomer sees a later roll"
    );
    assert_eq!(
        triggers_seen_from(jammer),
        1,
        "Hammer Jammer sees the Box's roll"
    );
    assert_finished(state, &[dwarf, warden], "R17");
}

/// R17b (control: no newcomer). Storm the Festival over Hammer Jammer alone,
/// with Barbarian Class and a resident Brazen Dwarf + CR 706.6 + CR 603.2: the
/// same keep choice owns the top of the stack, and the resident Dwarf sees the
/// roll once.
#[test]
fn a_lone_batch_members_keep_choice_is_seen_by_a_resident_once() {
    let seed = find_seed("a lone batch member's tie", |seed| {
        barbarian_festival_ties(seed, true, false)
    });
    reset_triggers_seen();
    let (board, jammer, _) = barbarian_festival(seed, true, false);
    let NewcomerFestival {
        mut runner,
        warden,
        resident_dwarf,
        festival,
        ..
    } = board;
    let dwarf = resident_dwarf.expect("the resident Dwarf");
    let p0_before = life(runner.state(), P0);
    let mut events = Vec::new();
    festival_keeps(&mut runner, festival, vec![jammer], &mut events);
    let frames_at_tie = stack_frames(runner.state());
    let naturals = answer_tie(&mut runner, &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let state = runner.state();
    assert_eq!(
        frames_at_tie,
        vec![
            FrameKind::BatchDelivery,
            FrameKind::PostReplacement,
            FrameKind::AbilityContinuation,
            FrameKind::DieRoll,
        ]
    );
    assert_eq!(
        triggers_seen_from(dwarf),
        1,
        "the resident sees the roll once"
    );
    assert_eq!(triggers_seen_from(jammer), 0);
    assert_eq!(
        plus_counters(state, jammer),
        survivors_total(&naturals),
        "{naturals:?}"
    );
    assert_eq!(life(state, P0), p0_before + 1, "Warden once");
    assert_finished(state, &[dwarf, warden], "R17b");
}

/// R18's board: Polymorph, free in P0's hand, over Hammer Jammer on top of P0's
/// library, with Barbarian Class, Soul Warden, a resident Brazen Dwarf and P0's
/// vanilla creature, Polymorph's target.
struct PolymorphBoard {
    runner: GameRunner,
    polymorph: ObjectId,
    jammer: ObjectId,
    dwarf: ObjectId,
    warden: ObjectId,
    victim: ObjectId,
}

fn polymorph_board(seed: u64) -> PolymorphBoard {
    let mut scenario = board(seed);
    add_barbarian_class(&mut scenario);
    let dwarf = add_dwarf(&mut scenario);
    let warden = add_warden_p0(&mut scenario);
    let jammer = jammer_card(&mut scenario);
    let polymorph = free_spell(&mut scenario, "Polymorph", false, POLYMORPH);
    let victim = scenario.add_vanilla(P0, 2, 2);
    let mut runner = scenario.build();
    seat_on_library_top(&mut runner, jammer);
    PolymorphBoard {
        runner,
        polymorph,
        jammer,
        dwarf,
        warden,
        victim,
    }
}

/// Cast Polymorph at the vanilla creature and pass until Hammer Jammer's
/// as-enters roll pauses or the stack empties.
fn polymorph_to_roll(board: &mut PolymorphBoard, events: &mut Vec<GameEvent>) {
    cast_and_pass(
        &mut board.runner,
        board.polymorph,
        None,
        Some(board.victim),
        events,
    );
}

fn reveal_until_resolved_for(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::RevealUntil,
                    source_id,
                    ..
                } if *source_id == source
            )
        })
        .count()
}

/// R18 (Unit E's sibling, the simultaneous mover). Polymorph destroys P0's
/// vanilla creature and puts Hammer Jammer, revealed from the top of P0's
/// library, onto the battlefield under Barbarian Class. CR 706.6 +
/// CR 614.12a: a tie for the lowest asks which die to ignore before Hammer
/// Jammer enters, so the reveal-until's rest waits on the batch's own owner,
/// beneath Hammer Jammer's whole child stack, and the frame that owns the choice
/// stays on top. Hammer Jammer enters with the kept result and does not see its
/// own roll (CR 113.6); the resident Brazen Dwarf sees it once (CR 603.2), and
/// Soul Warden sees the entry once.
#[test]
fn a_reveal_until_kept_cards_keep_choice_owns_the_top_of_the_stack() {
    let seed = find_seed("a Polymorph tie", |seed| {
        let mut board = polymorph_board(seed);
        polymorph_to_roll(&mut board, &mut Vec::new());
        tie_prompt_results(board.runner.state()).is_some()
    });
    reset_triggers_seen();
    let mut board = polymorph_board(seed);
    let p0_before = life(board.runner.state(), P0);
    let mut events = Vec::new();
    polymorph_to_roll(&mut board, &mut events);
    let PolymorphBoard {
        mut runner,
        polymorph,
        jammer,
        dwarf,
        warden,
        victim,
    } = board;
    let frames_at_tie = stack_frames(runner.state());
    let naturals = answer_tie(&mut runner, &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let state = runner.state();
    assert_eq!(
        frames_at_tie,
        vec![
            FrameKind::BatchDelivery,
            FrameKind::PostReplacement,
            FrameKind::AbilityContinuation,
            FrameKind::DieRoll,
        ]
    );
    assert_eq!(zone(state, jammer), Zone::Battlefield);
    assert_eq!(
        plus_counters(state, jammer),
        survivors_total(&naturals),
        "{naturals:?}"
    );
    assert_eq!(triggers_seen_from(jammer), 0);
    assert_eq!(triggers_seen_from(dwarf), 1, "the resident once");
    assert_eq!(life(state, P0), p0_before + 1, "Warden once");
    assert_eq!(reveal_until_resolved_for(&events, polymorph), 1);
    assert_eq!(zone(state, victim), Zone::Graveyard);
    assert_finished(state, &[dwarf, warden], "R18");
}

// ---------------------------------------------------------------------------
// Simultaneous co-entrants (review 5 F2) and held contexts in projections (F1)
// ---------------------------------------------------------------------------

/// The member a parked logical owner (a `ChangeZone` or `BatchDelivery` frame)
/// is delivering, wherever that owner sits on the stack.
fn paused_owner_member(state: &GameState) -> Option<ObjectId> {
    state.resolution_stack.iter().find_map(|frame| match frame {
        ResolutionFrame::ChangeZone(frame) => frame
            .pending
            .as_ref()
            .and_then(|pending| pending.paused_current.as_ref())
            .map(|paused| paused.member.object_id),
        ResolutionFrame::BatchDelivery(pending) => pending
            .paused_current
            .as_ref()
            .map(|paused| paused.member.object_id),
        _ => None,
    })
}

fn first_roll_index(events: &[GameEvent]) -> usize {
    events
        .iter()
        .position(|event| matches!(event, GameEvent::DieRolled { .. }))
        .expect("Hammer Jammer rolled")
}

/// R20's cases: where Hammer Jammer's as-enters roll is made relative to the
/// batch owner's pauses.
#[derive(Clone, Copy, Debug)]
enum CoEntry {
    /// No replacement: the batch delivers both members inline.
    Inline,
    /// Grumgully, the Generous: the entry's ordering prompt parks the batch,
    /// its frame on top when the roll is made.
    Ordered,
    /// Barbarian Class: a tie parks the batch beneath the roll's child stack.
    Tied,
    /// Hardened Scales and Branching Evolution: the roll is made inline, then the
    /// counter prompt pauses.
    CounterPrompt,
}

impl CoEntry {
    const ALL: [CoEntry; 4] = [
        CoEntry::Inline,
        CoEntry::Ordered,
        CoEntry::Tied,
        CoEntry::CounterPrompt,
    ];

    fn extras(self) -> fn(&mut GameScenario) {
        match self {
            CoEntry::Inline => no_extras,
            CoEntry::Ordered => add_grumgully,
            CoEntry::Tied => add_barbarian_class,
            CoEntry::CounterPrompt => add_counter_replacements,
        }
    }

    fn counter_pick(self) -> Option<FirstPick> {
        match self {
            CoEntry::CounterPrompt => Some(FirstPick::Scales),
            CoEntry::Inline | CoEntry::Ordered | CoEntry::Tied => None,
        }
    }
}

/// Storm the Festival over a Brazen Dwarf card, then Hammer Jammer, with a
/// resident Brazen Dwarf and `case`'s replacements.
fn co_entry_festival(seed: u64, case: CoEntry) -> (NewcomerFestival, ObjectId, ObjectId) {
    let mut scenario = board(seed);
    let newcomer = dwarf_card(&mut scenario);
    let jammer = jammer_card(&mut scenario);
    let festival = newcomer_festival(scenario, case.extras(), true, &[newcomer, jammer]);
    (festival, newcomer, jammer)
}

fn run_co_entry(case: CoEntry) {
    let what = format!("{case:?}");
    let seed = match case {
        CoEntry::Tied => find_seed("a co-entrant's tie", |seed| {
            let (mut board, newcomer, jammer) = co_entry_festival(seed, case);
            festival_keeps(
                &mut board.runner,
                board.festival,
                vec![newcomer, jammer],
                &mut Vec::new(),
            );
            tie_prompt_results(board.runner.state()).is_some()
        }),
        CoEntry::Inline | CoEntry::Ordered | CoEntry::CounterPrompt => 7,
    };
    reset_triggers_seen();
    let (board, newcomer, jammer) = co_entry_festival(seed, case);
    let NewcomerFestival {
        mut runner,
        warden,
        resident_dwarf,
        festival,
        box_of_goblins,
    } = board;
    let resident = resident_dwarf.expect("the resident Dwarf");
    let p1_before = life(runner.state(), P1);
    let p0_before = life(runner.state(), P0);
    let mut events = Vec::new();
    festival_keeps(&mut runner, festival, vec![newcomer, jammer], &mut events);
    let expected_counters = match case {
        CoEntry::Inline => {
            settle(&mut runner, &mut events, DEFAULT_ANSWERS);
            die_results(&events)[0]
        }
        CoEntry::Ordered => {
            let state = runner.state();
            assert_eq!(
                stack_frames(state),
                vec![FrameKind::BatchDelivery],
                "{what}"
            );
            assert_eq!(paused_owner_member(state), Some(jammer), "{what}");
            assert_eq!(zone(state, newcomer), Zone::Battlefield, "{what}");
            assert!(die_results(&events).is_empty(), "{what}: the roll follows");
            order_entry(&mut runner, "Hammer Jammer", &mut events);
            settle(&mut runner, &mut events, DEFAULT_ANSWERS);
            die_results(&events)[0] + 1
        }
        CoEntry::Tied => {
            let state = runner.state();
            assert_eq!(
                stack_frames(state),
                vec![
                    FrameKind::BatchDelivery,
                    FrameKind::PostReplacement,
                    FrameKind::AbilityContinuation,
                    FrameKind::DieRoll,
                ],
                "{what}"
            );
            assert_eq!(paused_owner_member(state), Some(jammer), "{what}");
            assert_eq!(zone(state, newcomer), Zone::Battlefield, "{what}");
            let naturals = answer_tie(&mut runner, &mut events);
            settle(&mut runner, &mut events, DEFAULT_ANSWERS);
            survivors_total(&naturals)
        }
        CoEntry::CounterPrompt => {
            let pick = case.counter_pick().expect("a counter pick");
            let settled = settle_picking(&mut runner, &mut events, pick);
            assert_eq!(
                settled.counter_prompts, 1,
                "{what}: reach the counter prompt"
            );
            pick.counters(die_results(&events)[0])
        }
    };
    let state = runner.state();
    let newcomer_entry = battlefield_entry_index(&events, newcomer).expect("Brazen Dwarf entered");
    assert!(
        newcomer_entry < first_roll_index(&events),
        "{what}: the co-entrant is delivered before the roll"
    );
    assert_eq!(
        triggers_seen_from(newcomer),
        0,
        "{what}: the co-entrant enters with the roll's event, so it does not see the roll"
    );
    assert_eq!(
        p1_before - life(state, P1),
        1,
        "{what}: only the resident Dwarf deals damage"
    );
    assert_eq!(
        triggers_seen_from(resident),
        1,
        "{what}: reach: the resident sees the roll"
    );
    assert_eq!(triggers_seen_from(jammer), 0, "{what}");
    assert_eq!(zone(state, jammer), Zone::Battlefield, "{what}");
    assert_eq!(
        plus_counters(state, jammer),
        expected_counters,
        "{what}: {:?}",
        die_results(&events)
    );
    assert_eq!(life(state, P0), p0_before + 2, "{what}: Warden twice");

    let p1_before_box = life(state, P1);
    let _ = take_triggers_seen_from(jammer);
    cast_box_and_settle(&mut runner, box_of_goblins, case.counter_pick());
    let state = runner.state();
    assert_eq!(
        p1_before_box - life(state, P1),
        2,
        "{what}: both Dwarves see a later roll"
    );
    assert_eq!(
        triggers_seen_from(jammer),
        1,
        "{what}: Hammer Jammer sees the Box's roll"
    );
    assert_finished(state, &[newcomer, resident, warden], &what);
}

/// R20 (review 5 F2). Storm the Festival puts a Brazen Dwarf card, then Hammer
/// Jammer, onto the battlefield, with Brazen Dwarf resident + CR 603.6a +
/// CR 603.6d + CR 614.12a: the two enter in one event, and Hammer Jammer's roll is
/// part of that event, so the earlier-delivered Dwarf is not an observer of it
/// (CR 113.6). The resident sees it once (CR 603.2), and a later roll triggers
/// both Dwarves. (a) inline; (b) behind Grumgully's ordering prompt, the batch's
/// frame on top; (c) Barbarian Class's tie, the batch beneath the roll's child
/// stack; (d) the inline roll, then the counter prompt.
#[test]
fn a_co_entrant_does_not_see_a_later_members_roll() {
    for case in CoEntry::ALL {
        run_co_entry(case);
    }
}

/// A co-entry row's board: `spell` puts the newcomer Brazen Dwarf and Hammer
/// Jammer onto the battlefield, with a resident Brazen Dwarf.
struct CoEntryBoard {
    runner: GameRunner,
    spell: ObjectId,
    newcomer: ObjectId,
    jammer: ObjectId,
    resident: ObjectId,
}

fn graveyard_dwarf(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Brazen Dwarf", 1, 3)
        .from_oracle_text(BRAZEN_DWARF)
        .id()
}

fn graveyard_pair(scenario: &mut GameScenario, dwarf_first: bool) -> (ObjectId, ObjectId) {
    if dwarf_first {
        let newcomer = graveyard_dwarf(scenario);
        let jammer = graveyard_creature(scenario, "Hammer Jammer", HAMMER_JAMMER);
        (newcomer, jammer)
    } else {
        let jammer = graveyard_creature(scenario, "Hammer Jammer", HAMMER_JAMMER);
        (graveyard_dwarf(scenario), jammer)
    }
}

/// Rise of the Dark Realms, free in P0's hand, over Brazen Dwarf and Hammer
/// Jammer in P0's graveyard (the Dwarf staged, so delivered, first when
/// `dwarf_first`), with a resident Brazen Dwarf and `extras`.
fn rise_board(seed: u64, dwarf_first: bool, extras: fn(&mut GameScenario)) -> CoEntryBoard {
    let mut scenario = board(seed);
    extras(&mut scenario);
    let (newcomer, jammer) = graveyard_pair(&mut scenario, dwarf_first);
    let resident = add_dwarf(&mut scenario);
    let spell = free_spell(
        &mut scenario,
        "Rise of the Dark Realms",
        false,
        RISE_OF_THE_DARK_REALMS,
    );
    CoEntryBoard {
        runner: scenario.build(),
        spell,
        newcomer,
        jammer,
        resident,
    }
}

/// Ever After, free in P0's hand, targeting Brazen Dwarf and Hammer Jammer in
/// P0's graveyard, with a resident Brazen Dwarf and `extras`.
fn ever_after_board(seed: u64, extras: fn(&mut GameScenario)) -> CoEntryBoard {
    let mut scenario = board(seed);
    extras(&mut scenario);
    let (newcomer, jammer) = graveyard_pair(&mut scenario, true);
    let resident = add_dwarf(&mut scenario);
    let spell = free_spell(&mut scenario, "Ever After", false, EVER_AFTER);
    CoEntryBoard {
        runner: scenario.build(),
        spell,
        newcomer,
        jammer,
        resident,
    }
}

fn cast_ever_after(board: &mut CoEntryBoard, targets: &[ObjectId], events: &mut Vec<GameEvent>) {
    let commit = board
        .runner
        .cast(board.spell)
        .target_objects(targets)
        .commit();
    events.extend(commit.events().iter().cloned());
    drop(commit);
    pass_until_prompt(&mut board.runner, events);
}

/// The settled readings of a co-entry row whose Hammer Jammer enters with
/// `extra` counters beyond its roll: the co-entrant does not see the roll, the
/// resident sees it once, and P1 is dealt 1 damage.
fn assert_co_entry_settled(
    board: &CoEntryBoard,
    events: &[GameEvent],
    p1_before: i32,
    extra: u32,
    what: &str,
) {
    let state = board.runner.state();
    assert_eq!(zone(state, board.newcomer), Zone::Battlefield, "{what}");
    assert_eq!(zone(state, board.jammer), Zone::Battlefield, "{what}");
    assert_eq!(
        triggers_seen_from(board.newcomer),
        0,
        "{what}: a co-entrant does not see the roll"
    );
    assert_eq!(
        p1_before - life(state, P1),
        1,
        "{what}: only the resident Dwarf deals damage"
    );
    assert_eq!(
        triggers_seen_from(board.resident),
        1,
        "{what}: reach: the resident sees the roll"
    );
    assert_eq!(triggers_seen_from(board.jammer), 0, "{what}");
    let rolls = die_results(events);
    assert_eq!(rolls.len(), 1, "{what}");
    assert_eq!(
        plus_counters(state, board.jammer),
        rolls[0] + extra,
        "{what}"
    );
    assert_finished(state, &[board.newcomer, board.resident], what);
}

/// R21 (review 5 F2, the mass owner). Rise of the Dark Realms puts Brazen Dwarf
/// and Hammer Jammer from P0's graveyard onto the battlefield + CR 603.6a +
/// CR 603.6d + CR 113.6: they enter in one event, so neither Dwarf delivery order
/// lets the newcomer see Hammer Jammer's roll. The resident Dwarf sees it once.
#[test]
fn a_mass_return_co_entrant_does_not_see_the_roll() {
    for dwarf_first in [true, false] {
        let what = format!("dwarf first: {dwarf_first}");
        reset_triggers_seen();
        let mut board = rise_board(7, dwarf_first, no_extras);
        let p1_before = life(board.runner.state(), P1);
        let mut events = Vec::new();
        cast_and_pass(&mut board.runner, board.spell, None, None, &mut events);
        settle(&mut board.runner, &mut events, DEFAULT_ANSWERS);
        let newcomer_entry =
            battlefield_entry_index(&events, board.newcomer).expect("Brazen Dwarf entered");
        assert_eq!(
            newcomer_entry < first_roll_index(&events),
            dwarf_first,
            "{what}: reach: the staging order is the delivery order"
        );
        assert_co_entry_settled(&board, &events, p1_before, 0, &what);
    }
}

/// R21b (review 5 F2, the parked mass owner). R21's board with Grumgully, the
/// Generous resident, the Dwarf delivered first + CR 616.1 + CR 603.6a +
/// CR 614.12a: Hammer Jammer's entry asks its replacement order, so the mass
/// owner is parked on its `ChangeZone` frame when the roll is made. The Dwarf is
/// already on the battlefield and still does not see the roll.
#[test]
fn a_parked_mass_return_co_entrant_does_not_see_the_roll() {
    reset_triggers_seen();
    let mut board = rise_board(7, true, add_grumgully);
    let p1_before = life(board.runner.state(), P1);
    let mut events = Vec::new();
    cast_and_pass(&mut board.runner, board.spell, None, None, &mut events);
    let state = board.runner.state();
    assert_eq!(stack_frames(state), vec![FrameKind::ChangeZone]);
    assert_eq!(paused_owner_member(state), Some(board.jammer));
    assert_eq!(zone(state, board.newcomer), Zone::Battlefield);
    assert!(
        die_results(&events).is_empty(),
        "the roll follows the prompt"
    );
    order_entry(&mut board.runner, "Hammer Jammer", &mut events);
    settle(&mut board.runner, &mut events, DEFAULT_ANSWERS);
    assert_co_entry_settled(&board, &events, p1_before, 1, "R21b");
}

/// R22 (review 5 F2, the targeted owner). Ever After returns Brazen Dwarf and
/// Hammer Jammer, in either target order + CR 603.6a + CR 603.6d + CR 113.6: they
/// enter in one event, so the Dwarf does not see Hammer Jammer's roll. The
/// resident Dwarf sees it once.
#[test]
fn a_targeted_co_entrant_does_not_see_the_roll() {
    for dwarf_first in [true, false] {
        let what = format!("dwarf targeted first: {dwarf_first}");
        reset_triggers_seen();
        let mut board = ever_after_board(7, no_extras);
        let targets = if dwarf_first {
            vec![board.newcomer, board.jammer]
        } else {
            vec![board.jammer, board.newcomer]
        };
        let p1_before = life(board.runner.state(), P1);
        let mut events = Vec::new();
        cast_ever_after(&mut board, &targets, &mut events);
        settle(&mut board.runner, &mut events, DEFAULT_ANSWERS);
        let newcomer_entry =
            battlefield_entry_index(&events, board.newcomer).expect("Brazen Dwarf entered");
        assert_eq!(
            newcomer_entry < first_roll_index(&events),
            dwarf_first,
            "{what}: reach: the target order is the delivery order"
        );
        assert_co_entry_settled(&board, &events, p1_before, 0, &what);
    }
}

/// R22b (review 5 F2, the parked targeted owner). R22's board with Grumgully,
/// the Generous resident, the Dwarf targeted first + CR 616.1 + CR 603.6a +
/// CR 614.12a: Hammer Jammer's entry asks its replacement order, so the targeted
/// owner is parked on its `ChangeZone` frame, beneath the spell's continuation,
/// when the roll is made. The Dwarf is already on the battlefield and still does
/// not see the roll.
#[test]
fn a_parked_targeted_co_entrant_does_not_see_the_roll() {
    reset_triggers_seen();
    let mut board = ever_after_board(7, add_grumgully);
    let targets = vec![board.newcomer, board.jammer];
    let p1_before = life(board.runner.state(), P1);
    let mut events = Vec::new();
    cast_ever_after(&mut board, &targets, &mut events);
    let state = board.runner.state();
    assert_eq!(
        stack_frames(state),
        vec![FrameKind::AbilityContinuation, FrameKind::ChangeZone]
    );
    assert_eq!(paused_owner_member(state), Some(board.jammer));
    assert_eq!(zone(state, board.newcomer), Zone::Battlefield);
    assert!(
        die_results(&events).is_empty(),
        "the roll follows the prompt"
    );
    order_entry(&mut board.runner, "Hammer Jammer", &mut events);
    settle(&mut board.runner, &mut events, DEFAULT_ANSWERS);
    assert_co_entry_settled(&board, &events, p1_before, 1, "R22b");
}

/// Myojin of Life's Web (`spell`) with a divinity counter, Brazen Dwarf and
/// Hammer Jammer cards in P0's hand, and a resident Brazen Dwarf.
fn myojin_board(seed: u64) -> CoEntryBoard {
    let mut scenario = board(seed);
    let newcomer = dwarf_card(&mut scenario);
    let jammer = jammer_card(&mut scenario);
    let resident = add_dwarf(&mut scenario);
    let myojin = scenario
        .add_creature_from_oracle(P0, "Myojin of Life's Web", 8, 8, MYOJIN_OF_LIFES_WEB)
        .id();
    scenario.with_counter(myojin, CounterType::Generic("divinity".to_string()), 1);
    CoEntryBoard {
        runner: scenario.build(),
        spell: myojin,
        newcomer,
        jammer,
        resident,
    }
}

/// R23 (review 5 F2, the choose-at-resolution owner). Myojin of Life's Web puts
/// Brazen Dwarf and Hammer Jammer from P0's hand onto the battlefield, chosen in
/// either order + CR 603.6a + CR 603.6d + CR 113.6: they enter in one event, so
/// the Dwarf does not see Hammer Jammer's roll. The resident Dwarf sees it once.
#[test]
fn a_chosen_co_entrant_does_not_see_the_roll() {
    for dwarf_first in [true, false] {
        let what = format!("dwarf chosen first: {dwarf_first}");
        reset_triggers_seen();
        let mut board = myojin_board(7);
        let p1_before = life(board.runner.state(), P1);
        let mut events = Vec::new();
        act(
            &mut board.runner,
            GameAction::ActivateAbility {
                source_id: board.spell,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            },
            &mut events,
        );
        pass_until_prompt(&mut board.runner, &mut events);
        assert!(
            effect_zone_choice(board.runner.state()),
            "{what}: {:?}",
            board.runner.state().waiting_for
        );
        let cards = if dwarf_first {
            vec![board.newcomer, board.jammer]
        } else {
            vec![board.jammer, board.newcomer]
        };
        act(
            &mut board.runner,
            GameAction::SelectCards { cards },
            &mut events,
        );
        settle(&mut board.runner, &mut events, DEFAULT_ANSWERS);
        let newcomer_entry =
            battlefield_entry_index(&events, board.newcomer).expect("Brazen Dwarf entered");
        assert_eq!(
            newcomer_entry < first_roll_index(&events),
            dwarf_first,
            "{what}: reach: the choice order is the delivery order"
        );
        assert_co_entry_settled(&board, &events, p1_before, 0, &what);
    }
}

// ---------------------------------------------------------------------------
// A co-entrant's trigger doubler (Roaming Throne) and an as-enters roll
// ---------------------------------------------------------------------------

/// Verbatim Oracle text (card-data), Roaming Throne.
const ROAMING_THRONE: &str = "Ward {2}\nAs this creature enters, choose a creature type.\nThis creature is the chosen type in addition to its other types.\nIf a triggered ability of another creature you control of the chosen type triggers, it triggers an additional time.";

/// A resident Brazen Dwarf with its printed types (Dwarf Shaman), so a Roaming
/// Throne that chose Dwarf doubles its trigger.
fn add_typed_dwarf(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Brazen Dwarf", 1, 3, BRAZEN_DWARF)
        .with_subtypes(vec!["Dwarf", "Shaman"])
        .id()
}

/// A Roaming Throne card (printed artifact creature — Golem, 4/4) in P0's hand
/// costing `cost`.
fn throne_card(scenario: &mut GameScenario, cost: ManaCost) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Roaming Throne", 4, 4, ROAMING_THRONE)
        .as_artifact_creature()
        .with_subtypes(vec!["Golem"])
        .with_mana_cost(cost)
        .id()
}

/// A Roaming Throne card in P0's graveyard.
fn graveyard_throne(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Roaming Throne", 4, 4)
        .from_oracle_text(ROAMING_THRONE)
        .as_artifact_creature()
        .with_subtypes(vec!["Golem"])
        .id()
}

/// The creature types a Roaming Throne row can name.
fn seed_creature_types(runner: &mut GameRunner) {
    runner.state_mut().all_creature_types = vec![
        "Dwarf".into(),
        "Goblin".into(),
        "Golem".into(),
        "Shaman".into(),
    ];
}

/// Answer the entry prompts that come before Hammer Jammer's roll, in delivery
/// order: an entry's replacement ordering (the entrant's own replacement first,
/// then Grumgully's) and Roaming Throne's creature type (Dwarf). Returns how many
/// ordering prompts were answered.
fn answer_pre_roll_entry_prompts(runner: &mut GameRunner, events: &mut Vec<GameEvent>) -> usize {
    let mut ordering_prompts = 0;
    for _ in 0..8 {
        let state = runner.state();
        let action = match &state.waiting_for {
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::CreatureType { .. },
                ..
            } => GameAction::ChooseOption {
                choice: "Dwarf".to_string(),
            },
            WaitingFor::ReplacementChoice { candidates, .. } if !is_counter_prompt(state) => {
                ordering_prompts += 1;
                GameAction::ChooseReplacement {
                    index: candidates
                        .iter()
                        .position(|candidate| candidate.source_name != "Grumgully, the Generous")
                        .unwrap_or_else(|| panic!("an entrant's own replacement: {candidates:?}")),
                }
            }
            _ => return ordering_prompts,
        };
        act(runner, action, events);
    }
    panic!("the entry prompts never ended");
}

fn chose_dwarf(state: &GameState, throne: ObjectId) -> bool {
    state.objects[&throne]
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Dwarf")
}

/// Where Hammer Jammer's as-enters roll is made relative to the owner's pauses,
/// on a Roaming Throne co-entry row.
#[derive(Clone, Copy, Debug)]
enum ThroneCoEntry {
    /// No replacement beyond the two entrants' own.
    Inline,
    /// Grumgully, the Generous: each entry asks its replacement order, so the
    /// owner is parked when the roll is made.
    Ordered,
}

impl ThroneCoEntry {
    fn extras(self) -> fn(&mut GameScenario) {
        match self {
            ThroneCoEntry::Inline => no_extras,
            ThroneCoEntry::Ordered => add_grumgully,
        }
    }

    /// The entry ordering prompts before the roll: Roaming Throne's, then Hammer
    /// Jammer's, which parks the owner when the roll is made.
    fn ordering_prompts(self) -> usize {
        match self {
            ThroneCoEntry::Inline => 0,
            ThroneCoEntry::Ordered => 2,
        }
    }

    /// Hammer Jammer's +1/+1 counters for its roll `n`.
    fn counters(self, n: u32) -> u32 {
        match self {
            ThroneCoEntry::Inline => n,
            ThroneCoEntry::Ordered => n + 1,
        }
    }
}

/// A Roaming Throne co-entry row's board: `spell` puts Roaming Throne, then
/// Hammer Jammer, onto the battlefield, with a resident Brazen Dwarf and a free
/// Box of Free-Range Goblins in hand.
struct ThroneCoEntryBoard {
    runner: GameRunner,
    spell: ObjectId,
    throne: ObjectId,
    jammer: ObjectId,
    resident: ObjectId,
    box_of_goblins: ObjectId,
}

/// Storm the Festival over Roaming Throne, then Hammer Jammer.
fn throne_festival_board(case: ThroneCoEntry) -> ThroneCoEntryBoard {
    let mut scenario = board(7);
    let resident = add_typed_dwarf(&mut scenario);
    let throne = throne_card(&mut scenario, ManaCost::generic(4));
    let jammer = jammer_card(&mut scenario);
    let NewcomerFestival {
        mut runner,
        festival,
        box_of_goblins,
        ..
    } = newcomer_festival(scenario, case.extras(), false, &[throne, jammer]);
    seed_creature_types(&mut runner);
    ThroneCoEntryBoard {
        runner,
        spell: festival,
        throne,
        jammer,
        resident,
        box_of_goblins,
    }
}

/// Rise of the Dark Realms over Roaming Throne, then Hammer Jammer, in P0's
/// graveyard.
fn throne_rise_board(case: ThroneCoEntry) -> ThroneCoEntryBoard {
    let mut scenario = board(7);
    case.extras()(&mut scenario);
    let throne = graveyard_throne(&mut scenario);
    let jammer = graveyard_creature(&mut scenario, "Hammer Jammer", HAMMER_JAMMER);
    let resident = add_typed_dwarf(&mut scenario);
    let spell = free_spell(
        &mut scenario,
        "Rise of the Dark Realms",
        false,
        RISE_OF_THE_DARK_REALMS,
    );
    let box_of_goblins = free_spell(
        &mut scenario,
        "Box of Free-Range Goblins",
        false,
        BOX_OF_FREE_RANGE_GOBLINS,
    );
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);
    ThroneCoEntryBoard {
        runner,
        spell,
        throne,
        jammer,
        resident,
        box_of_goblins,
    }
}

#[derive(Clone, Copy, Debug)]
enum ThroneOwner {
    Festival,
    Rise,
}

fn run_throne_co_entry(owner: ThroneOwner, case: ThroneCoEntry) {
    let what = format!("{owner:?} {case:?}");
    reset_triggers_seen();
    let mut board = match owner {
        ThroneOwner::Festival => throne_festival_board(case),
        ThroneOwner::Rise => throne_rise_board(case),
    };
    let p1_before = life(board.runner.state(), P1);
    let mut events = Vec::new();
    match owner {
        ThroneOwner::Festival => festival_keeps(
            &mut board.runner,
            board.spell,
            vec![board.throne, board.jammer],
            &mut events,
        ),
        ThroneOwner::Rise => {
            cast_and_pass(&mut board.runner, board.spell, None, None, &mut events);
        }
    }
    assert_eq!(
        answer_pre_roll_entry_prompts(&mut board.runner, &mut events),
        case.ordering_prompts(),
        "{what}: reach: the entry ordering prompts"
    );
    settle(&mut board.runner, &mut events, DEFAULT_ANSWERS);
    let state = board.runner.state();
    assert_eq!(zone(state, board.throne), Zone::Battlefield, "{what}");
    assert!(
        chose_dwarf(state, board.throne),
        "{what}: reach: Roaming Throne chose Dwarf"
    );
    let throne_entry =
        battlefield_entry_index(&events, board.throne).expect("Roaming Throne entered");
    assert!(
        throne_entry < first_roll_index(&events),
        "{what}: reach: Roaming Throne is delivered before the roll"
    );
    assert_eq!(
        triggers_seen_from(board.resident),
        1,
        "{what}: Roaming Throne enters with the roll's event, so it does not double the \
         resident Dwarf's trigger"
    );
    assert_eq!(
        p1_before - life(state, P1),
        1,
        "{what}: the resident Dwarf deals 1 damage once"
    );
    assert_eq!(triggers_seen_from(board.jammer), 0, "{what}");
    let rolls = die_results(&events);
    assert_eq!(rolls.len(), 1, "{what}");
    assert_eq!(
        plus_counters(state, board.jammer),
        case.counters(rolls[0]),
        "{what}: {rolls:?}"
    );
    assert_finished(state, &[board.resident], &what);

    // Reach: once Roaming Throne is on the battlefield, it doubles the Dwarf's
    // trigger on a later roll (CR 603.2d).
    let p1_before_box = life(state, P1);
    let _ = take_triggers_seen_from(board.resident);
    cast_box_and_settle(&mut board.runner, board.box_of_goblins, None);
    let state = board.runner.state();
    assert_eq!(
        triggers_seen_from(board.resident),
        2,
        "{what}: reach: Roaming Throne doubles the Dwarf's trigger on a later roll"
    );
    assert_eq!(p1_before_box - life(state, P1), 2, "{what}");
    assert_finished(state, &[board.resident, board.jammer], &what);
}

/// R23b (Codex review of m5). Storm the Festival puts Roaming Throne, then Hammer
/// Jammer, onto the battlefield, Throne choosing Dwarf, with Brazen Dwarf
/// resident + CR 603.6a + CR 603.6d + CR 614.12a + CR 611.3b + CR 603.2d: the two
/// enter in one event, and Hammer Jammer's roll is made before that event, so
/// Throne is not on the battlefield when it is rolled and its doubling static
/// does not apply. The resident Dwarf triggers once; a later roll is doubled. No
/// replacement beyond the two entrants' own.
#[test]
fn a_co_entrant_doubler_does_not_double_a_later_members_roll() {
    run_throne_co_entry(ThroneOwner::Festival, ThroneCoEntry::Inline);
}

/// R23b-ordered. R23b behind Grumgully, the Generous's ordering prompts
/// (CR 616.1): Storm the Festival's batch is parked when Hammer Jammer's roll is
/// made.
#[test]
fn a_parked_co_entrant_doubler_does_not_double_a_later_members_roll() {
    run_throne_co_entry(ThroneOwner::Festival, ThroneCoEntry::Ordered);
}

/// R23c (Codex review of m5, the mass owner). R23b on Rise of the Dark Realms over
/// Roaming Throne, then Hammer Jammer, in P0's graveyard.
#[test]
fn a_mass_return_co_entrant_doubler_does_not_double_the_roll() {
    run_throne_co_entry(ThroneOwner::Rise, ThroneCoEntry::Inline);
}

/// R23c-ordered. R23c behind Grumgully, the Generous's ordering prompts
/// (CR 616.1): the mass owner is parked on its `ChangeZone` frame when the roll
/// is made.
#[test]
fn a_parked_mass_return_co_entrant_doubler_does_not_double_the_roll() {
    run_throne_co_entry(ThroneOwner::Rise, ThroneCoEntry::Ordered);
}

/// R23d (positive control). Roaming Throne is cast first and chooses Dwarf, then
/// Hammer Jammer is cast with Brazen Dwarf resident + CR 603.2d + CR 611.3b: Throne
/// is on the battlefield when the roll is made, so the Dwarf's trigger triggers
/// an additional time.
#[test]
fn a_resident_doubler_doubles_an_as_enters_roll() {
    reset_triggers_seen();
    let mut scenario = board(7);
    let resident = add_typed_dwarf(&mut scenario);
    let throne = throne_card(&mut scenario, ManaCost::generic(0));
    let jammer = free_jammer(&mut scenario);
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, throne, None, None, &mut events);
    assert_eq!(answer_pre_roll_entry_prompts(&mut runner, &mut events), 0);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    assert!(
        chose_dwarf(runner.state(), throne),
        "reach: Roaming Throne chose Dwarf"
    );
    let p1_before = life(runner.state(), P1);
    let _ = take_triggers_seen_from(resident);
    let mut events = Vec::new();
    cast_and_pass(&mut runner, jammer, None, None, &mut events);
    settle(&mut runner, &mut events, DEFAULT_ANSWERS);
    let state = runner.state();
    assert_eq!(die_results(&events).len(), 1);
    assert_eq!(
        triggers_seen_from(resident),
        2,
        "a resident Roaming Throne doubles the Dwarf's trigger"
    );
    assert_eq!(p1_before - life(state, P1), 2);
    assert_eq!(triggers_seen_from(jammer), 0);
    assert_eq!(plus_counters(state, jammer), die_results(&events)[0]);
    assert_finished(state, &[resident], "R23d");
}

/// R24's board: Llanowar Elves, Hardened Scales and Branching Evolution on P0's
/// battlefield; a {1}{R} Brazen Dwarf, a free Oblation and a free Hammer Jammer in
/// P0's hand; {R} in P0's pool.
struct HeldContextBoard {
    runner: GameRunner,
    elf: ObjectId,
    dwarf: ObjectId,
    oblation: ObjectId,
    jammer: ObjectId,
}

fn held_context_board(seed: u64) -> HeldContextBoard {
    let mut scenario = board(seed);
    add_counter_replacements(&mut scenario);
    let elf = scenario
        .add_creature_from_oracle(P0, "Llanowar Elves", 1, 1, LLANOWAR_ELVES)
        .id();
    let dwarf = dwarf_card(&mut scenario);
    let oblation = free_spell(&mut scenario, "Oblation", true, OBLATION);
    let jammer = free_jammer(&mut scenario);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    HeldContextBoard {
        runner: scenario.build(),
        elf,
        dwarf,
        oblation,
        jammer,
    }
}

/// Float the Elf's {G} and cast Brazen Dwarf with it, then shuffle the Elf into
/// P0's library with Oblation (P0 draws two).
fn shuffle_the_paying_elf_away(board: &mut HeldContextBoard, events: &mut Vec<GameEvent>) {
    act(
        &mut board.runner,
        GameAction::ActivateAbility {
            source_id: board.elf,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
        events,
    );
    cast_and_pass(&mut board.runner, board.dwarf, None, None, events);
    settle(&mut board.runner, events, DEFAULT_ANSWERS);
    cast_and_pass(
        &mut board.runner,
        board.oblation,
        None,
        Some(board.elf),
        events,
    );
    settle(&mut board.runner, events, DEFAULT_ANSWERS);
}

/// Every JSON pointer in `value` whose string is `needle`.
fn json_paths_of(value: &serde_json::Value, needle: &str) -> Vec<String> {
    fn walk(value: &serde_json::Value, path: String, needle: &str, found: &mut Vec<String>) {
        match value {
            serde_json::Value::String(text) if text == needle => found.push(path),
            serde_json::Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    walk(item, format!("{path}/{index}"), needle, found);
                }
            }
            serde_json::Value::Object(fields) => {
                for (key, field) in fields {
                    walk(field, format!("{path}/{key}"), needle, found);
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk(value, String::new(), needle, &mut found);
    found
}

/// R24 (review 5 F1). Brazen Dwarf is cast with Llanowar Elves's {G}, Oblation
/// shuffles the Elf into P0's library, and Hammer Jammer's as-enters roll is held
/// with the Dwarf's trigger context while the Hardened Scales / Branching
/// Evolution prompt is open + CR 400.2 + CR 401.2 + CR 603.3: that context names
/// the Elf, now a hidden library card, so no audience's projection (opponent,
/// spectator, or the owner, who cannot look at their library either) carries the
/// held contexts. The authoritative state keeps them, and the answer places the
/// Dwarf's trigger once.
#[test]
fn held_roll_contexts_stay_out_of_every_viewer_projection() {
    let seed = find_seed("the Elf stays in the library", |seed| {
        let mut board = held_context_board(seed);
        shuffle_the_paying_elf_away(&mut board, &mut Vec::new());
        zone(board.runner.state(), board.elf) == Zone::Library
    });
    reset_triggers_seen();
    let mut board = held_context_board(seed);
    let mut events = Vec::new();
    shuffle_the_paying_elf_away(&mut board, &mut events);
    let p1_before = life(board.runner.state(), P1);
    cast_and_pass(&mut board.runner, board.jammer, None, None, &mut events);
    let state = board.runner.state();
    assert_counter_prompt(state);
    assert_eq!(zone(state, board.elf), Zone::Library);
    assert!(
        state.objects[&board.dwarf]
            .mana_spent_source_snapshots
            .iter()
            .any(|snapshot| snapshot.source_id == board.elf),
        "reach: the Dwarf's payment names the Elf"
    );
    let held: Vec<_> = state
        .deferred_entry_events
        .iter()
        .flat_map(|parked| parked.held_triggers.iter())
        .collect();
    assert_eq!(held.len(), 1, "reach: the roll holds one context");
    assert_eq!(held[0].pending.source_id, board.dwarf);
    let held_snapshot = held[0]
        .pending
        .ability
        .trigger_source
        .as_ref()
        .expect("the held context's source")
        .mana_spent_source_snapshots
        .iter()
        .find(|snapshot| snapshot.source_id == board.elf)
        .expect("reach: the held source context names the Elf");
    assert_eq!(held_snapshot.lki.name, "Llanowar Elves");

    let authoritative = state.deferred_entry_events.clone();
    for (audience, view) in [
        (
            "opponent",
            engine::game::visibility::filter_state_for_viewer(state, P1),
        ),
        (
            "spectator",
            engine::game::visibility::filter_state_for_unseated_viewer(state),
        ),
        (
            "owner",
            engine::game::visibility::filter_state_for_viewer(state, P0),
        ),
    ] {
        assert_eq!(
            view.deferred_entry_events.len(),
            authoritative.len(),
            "{audience}: the parked entries stay"
        );
        let json = serde_json::to_value(&view).expect("the projection serializes");
        let parked = json["deferred_entry_events"]
            .as_array()
            .expect("the parked entries are projected");
        assert!(
            parked
                .iter()
                .all(|entry| entry.get("held_triggers").is_none()),
            "{audience}: {parked:?}"
        );
        let paths = json_paths_of(&json, "Llanowar Elves");
        assert!(paths.is_empty(), "{audience}: {paths:?}");
    }
    assert_eq!(
        board.runner.state().deferred_entry_events,
        authoritative,
        "projecting leaves the authoritative held contexts"
    );

    let r = die_results(&events)[0];
    settle_picking(&mut board.runner, &mut events, FirstPick::Scales);
    let state = board.runner.state();
    assert_eq!(
        p1_before - life(state, P1),
        1,
        "the held context is placed once"
    );
    assert_eq!(triggers_seen_from(board.dwarf), 1);
    assert_eq!(
        plus_counters(state, board.jammer),
        FirstPick::Scales.counters(r)
    );
    assert_finished(state, &[board.dwarf], "R24");
}
