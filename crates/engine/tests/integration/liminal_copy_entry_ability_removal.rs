//! CR 614.12 for token copies under a resident ability remover.
//!
//! - CR 614.12: which replacement effects modify how a permanent enters is decided by "the
//!   characteristics of the permanent as it would exist on the battlefield, taking into
//!   account ... continuous effects that already exist and would apply to the permanent".
//!   A copied "enters with" (CR 614.1c) or "as enters" (CR 614.12a) ability that an existing
//!   ability remover (CR 613.1f) would strip doesn't apply. Its first example is a token
//!   copy of Voice of All.
//! - Overwhelming Splendor (ruling 2017-09-29): "Any 'as [this creature] enters the
//!   battlefield' or '[this creature] enters the battlefield with' abilities won't be applied."
//! - Stolen Identity, Saw in Half, Littjara Mirrorlake, Clone Legion (rulings): with no
//!   remover, the copied "as enters" / "enters with" abilities work.
//! - Clone Legion (ruling 2015-02-25): "The token creatures all enter the battlefield at the
//!   same time." A co-entering copy's continuous effect doesn't exist yet as its siblings
//!   enter.
//!
//! Every row casts or activates through the production pipeline, answers each prompt, and
//! settles at the completion boundary. The paused Clone Legion rows also run through the
//! engine's save/restore pipeline at every prompt, and must settle exactly as they do live.

use std::collections::{BTreeMap, BTreeSet};

use crate::support::shared_card_db;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ChosenAttribute, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::game_state::{
    PersistedGameState, PersistedRestoreFinalization, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::resolution::ResolutionFrame;
use engine::types::zones::Zone;

// Verbatim Oracle text (MTGJSON AtomicCards).
const HUMILITY: &str = "All creatures lose all abilities and have base power and toughness 1/1.";
const DRESS_DOWN: &str = "Flash\nWhen this enchantment enters, draw a card.\nCreatures lose all abilities.\nAt the beginning of the end step, sacrifice this enchantment.";
const SPLENDOR: &str = "Enchant player\nCreatures enchanted player controls lose all abilities and have base power and toughness 1/1.\nEnchanted player can't activate abilities that aren't mana abilities or loyalty abilities.";
const MARGLE: &str =
    "Other creatures you control lose all abilities, can't gain abilities, and get +2/+2.";
const TRISKELION: &str = "This creature enters with three +1/+1 counters on it.\nRemove a +1/+1 counter from this creature: It deals 1 damage to any target.";
const STOLEN_IDENTITY: &str = "Create a token that's a copy of target artifact or creature.\nCipher (Then you may exile this spell card encoded on a creature you control. Whenever that creature deals combat damage to a player, its controller may cast a copy of the encoded card without paying its mana cost.)";
const VOICE_OF_ALL: &str = "Flying\nAs this creature enters, choose a color.\nThis creature has protection from the chosen color.";
const ROAMING_THRONE: &str = "Ward {2}\nAs this creature enters, choose a creature type.\nThis creature is the chosen type in addition to its other types.\nIf a triggered ability of another creature you control of the chosen type triggers, it triggers an additional time.";
const SAW_IN_HALF: &str = "Destroy target creature. If that creature dies this way, its controller creates two tokens that are copies of that creature, except their power is half that creature's power and their toughness is half that creature's toughness. Round up each time.";
const LITTJARA: &str = "This land enters tapped.\n{T}: Add {U}.\n{2}{G}{G}{U}, {T}, Sacrifice this land: Create a token that's a copy of target creature you control, except it enters with an additional +1/+1 counter on it. Activate only as a sorcery.";
const TWINFLAME: &str = "Strive \u{2014} This spell costs {2}{R} more to cast for each target beyond the first.\nChoose any number of target creatures you control. For each of them, create a token that's a copy of that creature, except it has haste. Exile those tokens at the beginning of the next end step.";
const CLONE_LEGION: &str =
    "For each creature target player controls, create a token that's a copy of that creature.";
const APPLIED_GEOMETRY: &str = "Create a token that's a copy of target non-Aura permanent you control, except it's a 0/0 Fractal creature in addition to its other types. Put six +1/+1 counters on it.";
const GEMSTONE_MINE: &str = "This land enters with three mining counters on it.\n{T}, Remove a mining counter from this land: Add one mana of any color. If there are no mining counters on this land, sacrifice it.";
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";
const CASE_OF_THE_PILFERED_PROOF: &str = "Whenever a Detective you control enters or is turned face up, put a +1/+1 counter on it.\nTo solve \u{2014} You control three or more Detectives. (If unsolved, solve at the beginning of your end step.)\nSolved \u{2014} If one or more tokens would be created under your control, those tokens plus a Clue token are created instead. (It's an artifact with \"{2}, Sacrifice this token: Draw a card.\")";
const HALVING_SEASON: &str = "If an opponent would create one or more tokens, they create half that many of each of those kinds of tokens instead, rounded down.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";
const QUICKSILVER_GARGANTUAN: &str = "You may have this creature enter as a copy of any creature on the battlefield, except it's 7/7.";
const SPARK_DOUBLE: &str = "You may have this creature enter as a copy of a creature or planeswalker you control, except it enters with an additional +1/+1 counter on it if it's a creature, it enters with an additional loyalty counter on it if it's a planeswalker, and it isn't legendary.";
const CLONE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";
const HARDENED_SCALES: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
const BRANCHING_EVOLUTION: &str = "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.";
const MINION_OF_THE_WASTES: &str = "Trample\nAs this creature enters, pay any amount of life.\nMinion of the Wastes's power and toughness are each equal to the life paid as it entered.";
const PLATINUM_EMPERION: &str = "Your life total can't change. (You can't gain or lose life. You can't pay any amount of life except 0.)";
const SQUAD_CAPTAIN: &str = "Vigilance (Attacking doesn't cause this creature to tap.)\nThis creature enters with a +1/+1 counter on it for each other creature you control.";
/// Grizzly Bears has no rules text.
const GRIZZLY_BEARS: &str = "";

const MARGLE_NAME: &str = "Margle, Cousin of Yargle";
const TRISKELION_NAME: &str = "Triskelion";
const VOICE_NAME: &str = "Voice of All";
const THRONE_NAME: &str = "Roaming Throne";
const GEMSTONE_NAME: &str = "Gemstone Mine";
const GARGANTUAN_NAME: &str = "Quicksilver Gargantuan";
const MINION_NAME: &str = "Minion of the Wastes";
const EMPERION_NAME: &str = "Platinum Emperion";
const SQUAD_CAPTAIN_NAME: &str = "Squad Captain";
const BEARS_NAME: &str = "Grizzly Bears";

/// A copied permanent: name, Oracle text, printed power and toughness.
#[derive(Clone, Copy)]
struct Donor {
    name: &'static str,
    oracle: &'static str,
    power: i32,
    toughness: i32,
}

const MARGLE_DONOR: Donor = Donor {
    name: MARGLE_NAME,
    oracle: MARGLE,
    power: 5,
    toughness: 2,
};
const TRISKELION_DONOR: Donor = Donor {
    name: TRISKELION_NAME,
    oracle: TRISKELION,
    power: 1,
    toughness: 1,
};
const VOICE_DONOR: Donor = Donor {
    name: VOICE_NAME,
    oracle: VOICE_OF_ALL,
    power: 2,
    toughness: 2,
};
const THRONE_DONOR: Donor = Donor {
    name: THRONE_NAME,
    oracle: ROAMING_THRONE,
    power: 4,
    toughness: 4,
};

const GARGANTUAN_DONOR: Donor = Donor {
    name: GARGANTUAN_NAME,
    oracle: QUICKSILVER_GARGANTUAN,
    power: 7,
    toughness: 7,
};
const EMPERION_DONOR: Donor = Donor {
    name: EMPERION_NAME,
    oracle: PLATINUM_EMPERION,
    power: 8,
    toughness: 8,
};
const SPARK_DOUBLE_DONOR: Donor = Donor {
    name: "Spark Double",
    oracle: SPARK_DOUBLE,
    power: 0,
    toughness: 0,
};
const BEARS_DONOR: Donor = Donor {
    name: BEARS_NAME,
    oracle: GRIZZLY_BEARS,
    power: 2,
    toughness: 2,
};

/// A resident ability remover, placed before the copy is made.
#[derive(Clone, Copy, Debug)]
enum Remover {
    Humility,
    DressDown,
    /// Overwhelming Splendor enchanting this player.
    Splendor(PlayerId),
    /// Margle, Cousin of Yargle under this player's control.
    Margle(PlayerId),
}

/// The four removers that strip a token P0 creates.
const STRIPPING_P0: [Remover; 4] = [
    Remover::Humility,
    Remover::DressDown,
    Remover::Splendor(P0),
    Remover::Margle(P0),
];

fn board() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let library: Vec<String> = (0..12).map(|i| format!("Library Card {i}")).collect();
    let names: Vec<&str> = library.iter().map(String::as_str).collect();
    scenario.with_library_top(P0, &names);
    scenario.with_library_top(P1, &names);
    scenario
}

/// Places `remover`; Overwhelming Splendor's id is returned so it can be attached once the
/// game is built.
fn add_remover(scenario: &mut GameScenario, remover: Option<Remover>) -> Option<ObjectId> {
    match remover? {
        Remover::Humility => {
            scenario.add_enchantment_from_oracle(P0, "Humility", HUMILITY);
            None
        }
        Remover::DressDown => {
            scenario.add_enchantment_from_oracle(P0, "Dress Down", DRESS_DOWN);
            None
        }
        Remover::Splendor(_) => Some(
            scenario
                .add_enchantment_from_oracle(P1, "Overwhelming Splendor", SPLENDOR)
                .with_subtypes(vec!["Aura", "Curse"])
                .id(),
        ),
        Remover::Margle(controller) => {
            add_donor(scenario, controller, MARGLE_DONOR);
            None
        }
    }
}

fn attach_remover(runner: &mut GameRunner, remover: Option<Remover>, splendor: Option<ObjectId>) {
    if let (Some(Remover::Splendor(player)), Some(aura)) = (remover, splendor) {
        engine::game::effects::attach::attach_to_player(runner.state_mut(), aura, player);
        assert_eq!(
            runner.state().objects[&aura].attached_to,
            Some(AttachTarget::Player(player)),
            "REACH-GUARD: Overwhelming Splendor enchants {player:?}"
        );
        engine::game::layers::mark_layers_full(runner.state_mut());
        engine::game::layers::flush_layers(runner.state_mut());
    }
}

fn add_donor(scenario: &mut GameScenario, controller: PlayerId, donor: Donor) -> ObjectId {
    scenario
        .add_creature_from_oracle(
            controller,
            donor.name,
            donor.power,
            donor.toughness,
            donor.oracle,
        )
        .id()
}

fn free_spell(scenario: &mut GameScenario, name: &str, instant: bool, oracle: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, instant, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn battlefield_tokens(runner: &GameRunner, name: &str) -> Vec<ObjectId> {
    let mut tokens: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.is_token && obj.zone == Zone::Battlefield && obj.name == name)
        .map(|obj| obj.id)
        .collect();
    tokens.sort();
    tokens
}

fn the_token(runner: &GameRunner, name: &str) -> ObjectId {
    let tokens = battlefield_tokens(runner, name);
    assert_eq!(
        tokens.len(),
        1,
        "REACH-GUARD: one {name} token, got {tokens:?}"
    );
    tokens[0]
}

fn counters_of(runner: &GameRunner, id: ObjectId, counter: &CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(counter)
        .copied()
        .unwrap_or(0)
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    counters_of(runner, id, &CounterType::Plus1Plus1)
}

fn mining() -> CounterType {
    CounterType::Generic("mining".to_string())
}

fn power_toughness(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let obj = &runner.state().objects[&id];
    (obj.power, obj.toughness)
}

/// The completion boundary: priority, an empty stack, nothing parked.
fn assert_settled(runner: &GameRunner, row: &str) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{row}: completed to Priority, got {}",
        runner.waiting_for_kind()
    );
    assert!(state.stack.is_empty(), "{row}: the stack is empty");
    assert!(
        state.pending_replacement.is_none(),
        "{row}: no parked replacement"
    );
    assert!(
        state.active_ability_continuation().is_none(),
        "{row}: no stranded continuation"
    );
}

/// What one prompt looked like when it was raised.
#[derive(Debug, PartialEq)]
enum Prompt {
    /// An as-enters `NamedChoice`, and the player it went to.
    Named { player: PlayerId },
    /// A CR 616.1 replacement-order choice, or an optional replacement's offer.
    Replacement,
    /// A `CopyTargetChoice`: the player choosing, and each legal donor in prompt order.
    CopyTarget {
        player: PlayerId,
        valid: Vec<CopyPick>,
    },
    /// An as-enters life payment (`PayAmountChoice`): its payer and range.
    PayAmount {
        player: PlayerId,
        min: u32,
        max: u32,
    },
}

/// A copy donor, as a `CopyTargetChoice` offers it and as a row picks it.
#[derive(Clone, Debug, PartialEq)]
struct CopyPick {
    name: String,
    controller: PlayerId,
    token: bool,
}

impl CopyPick {
    fn card(name: &str, controller: PlayerId) -> Self {
        Self {
            name: name.to_string(),
            controller,
            token: false,
        }
    }

    fn token(name: &str, controller: PlayerId) -> Self {
        Self {
            name: name.to_string(),
            controller,
            token: true,
        }
    }

    fn of(runner: &GameRunner, id: ObjectId) -> Self {
        let obj = &runner.state().objects[&id];
        Self {
            name: obj.name.clone(),
            controller: obj.controller,
            token: obj.is_token,
        }
    }
}

/// How a driven row answers the prompts that take a pick. Anything unscripted takes the
/// first option (an optional copy offer: accept).
#[derive(Clone, Default)]
struct Script {
    /// Each `CopyTargetChoice`, in turn, picks this donor.
    copy_targets: Vec<CopyPick>,
    /// Each `PayAmountChoice`, in turn, pays this much life.
    pays: Vec<u32>,
    /// A CR 616.1 order prompt applies this source's replacement first when it offers one.
    first: Option<&'static str>,
}

/// How a row carries its pauses: in memory, or through the save/restore wire.
#[derive(Clone, Copy)]
enum PauseCarry {
    Live,
    SavedAndRestored,
}

/// The prompts a driven row answered, each with the tokens the innermost parked CopyToken
/// owner had already created when it was answered (`None` with no owner parked).
#[derive(Debug, PartialEq)]
struct Answered {
    prompts: Vec<Prompt>,
    parked_created_ids: Vec<Option<Vec<ObjectId>>>,
    stack_trigger_sources: BTreeSet<ObjectId>,
}

/// The engine's own save / undo / P2P restore pipeline (`PersistedGameState`'s trusted
/// envelope), with the deferred production reload (as
/// `twinflame_paused_copy_referent.rs::save_and_restore`) — not a bare
/// `serde_json::from_str::<GameState>`.
fn save_and_restore(runner: &GameRunner) -> GameRunner {
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the paused state serializes");
    restore_json(&json)
}

fn restore_json(json: &str) -> GameRunner {
    let db = shared_card_db().expect("REACH-GUARD: a restore needs the integration CardDB");
    let state = serde_json::from_str::<PersistedGameState>(json)
        .expect("the paused state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("the paused state is admissible")
        .finalize_after_rehydration(|state| {
            engine::game::rehydrate_game_from_card_db_with_finalization(
                state,
                db,
                engine::game::CardDbRehydrationFinalization::Defer,
            );
            Ok(())
        })
        .expect("the restored state is publishable");
    GameRunner::from_state(state)
}

/// Carries the paused game to the answer. Saved and restored, it comes back at the same
/// prompt with the same resolution stack: the parked CopyToken owner's `created_ids` (and a
/// liminal entry's) are what a paused resume reads for its co-entrants
/// (`token_copy::copy_token_co_entrants`), since the tokens a drain publishes aren't
/// serialized.
fn carry_pause(runner: &mut GameRunner, carry: PauseCarry) {
    match carry {
        PauseCarry::Live => {}
        PauseCarry::SavedAndRestored => {
            let waiting = runner.state().waiting_for.clone();
            let stack = runner.state().resolution_stack.clone();
            *runner = save_and_restore(runner);
            assert_eq!(
                runner.state().waiting_for,
                waiting,
                "the restored game is at the same prompt"
            );
            assert_eq!(
                runner.state().resolution_stack,
                stack,
                "the parked resolution stack crosses the wire unchanged"
            );
        }
    }
}

/// The tokens the innermost parked CopyToken owner has created, the frame
/// `copy_token_co_entrants` reads.
fn parked_created_ids(runner: &GameRunner) -> Option<Vec<ObjectId>> {
    runner
        .state()
        .resolution_stack
        .iter()
        .filter_map(|frame| match frame {
            ResolutionFrame::CopyToken(owner) => Some(owner.created_ids.clone()),
            _ => None,
        })
        .last()
}

/// Passes priority and answers every prompt (first option / first replacement) until the
/// game settles. `observe` sees the state at each prompt, before it is answered.
fn drive(runner: &mut GameRunner, observe: impl FnMut(&GameRunner, &Prompt)) -> Vec<Prompt> {
    drive_carried(runner, PauseCarry::Live, &Script::default(), observe).prompts
}

/// [`drive`], carrying each prompt per `carry` before it is observed and answered, and
/// answering the prompts `script` names. The final priority pass is never carried.
fn drive_carried(
    runner: &mut GameRunner,
    carry: PauseCarry,
    script: &Script,
    mut observe: impl FnMut(&GameRunner, &Prompt),
) -> Answered {
    let mut copy_picks = script.copy_targets.iter().cloned();
    let mut pays = script.pays.iter().copied();
    let mut answered = Answered {
        prompts: Vec::new(),
        parked_created_ids: Vec::new(),
        stack_trigger_sources: BTreeSet::new(),
    };
    for _ in 0..64 {
        answered
            .stack_trigger_sources
            .extend(runner.state().stack.iter().filter_map(|entry| {
                matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                    .then_some(entry.source_id)
            }));
        if matches!(
            runner.state().waiting_for,
            WaitingFor::NamedChoice { .. }
                | WaitingFor::ReplacementChoice { .. }
                | WaitingFor::CopyTargetChoice { .. }
                | WaitingFor::PayAmountChoice { .. }
        ) {
            carry_pause(runner, carry);
        }
        let (action, prompt) = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return answered,
            WaitingFor::Priority { .. } => (GameAction::PassPriority, None),
            WaitingFor::NamedChoice {
                player, options, ..
            } => (
                GameAction::ChooseOption {
                    choice: options[0].clone(),
                },
                Some(Prompt::Named { player: *player }),
            ),
            WaitingFor::ReplacementChoice { candidates, .. } => {
                let index = script
                    .first
                    .and_then(|first| {
                        candidates
                            .iter()
                            .position(|candidate| candidate.source_name == first)
                    })
                    .unwrap_or(0);
                (
                    GameAction::ChooseReplacement { index },
                    Some(Prompt::Replacement),
                )
            }
            WaitingFor::CopyTargetChoice {
                player,
                valid_targets,
                ..
            } => {
                let valid: Vec<CopyPick> = valid_targets
                    .iter()
                    .map(|id| CopyPick::of(runner, *id))
                    .collect();
                let pick = copy_picks
                    .next()
                    .unwrap_or_else(|| panic!("an unscripted CopyTargetChoice offering {valid:?}"));
                let target = valid_targets
                    .iter()
                    .zip(&valid)
                    .find(|(_, donor)| **donor == pick)
                    .map(|(id, _)| *id)
                    .unwrap_or_else(|| {
                        panic!("REACH-GUARD: the copy choice offers {pick:?}, got {valid:?}")
                    });
                (
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    },
                    Some(Prompt::CopyTarget {
                        player: *player,
                        valid,
                    }),
                )
            }
            WaitingFor::PayAmountChoice {
                player, min, max, ..
            } => {
                let amount = pays
                    .next()
                    .unwrap_or_else(|| panic!("an unscripted PayAmountChoice"));
                (
                    GameAction::SubmitPayAmount { amount },
                    Some(Prompt::PayAmount {
                        player: *player,
                        min: *min,
                        max: *max,
                    }),
                )
            }
            _ => panic!("unexpected prompt {}", runner.waiting_for_kind()),
        };
        if let Some(prompt) = prompt {
            observe(runner, &prompt);
            answered.parked_created_ids.push(parked_created_ids(runner));
            answered.prompts.push(prompt);
        }
        runner
            .act(action)
            .unwrap_or_else(|err| panic!("action rejected: {err:?}"));
    }
    panic!("the game did not settle");
}

/// One battlefield token as a settled row left it.
#[derive(Debug, PartialEq)]
struct TokenOutcome {
    id: ObjectId,
    name: String,
    counters: BTreeMap<CounterType, u32>,
    power: Option<i32>,
    toughness: Option<i32>,
    chosen: Vec<ChosenAttribute>,
}

/// What a settled row produced: its answered prompts and every battlefield token.
#[derive(Debug, PartialEq)]
struct RowOutcome {
    answered: Answered,
    tokens: Vec<TokenOutcome>,
}

fn row_outcome(runner: &GameRunner, answered: Answered) -> RowOutcome {
    let mut tokens: Vec<TokenOutcome> = runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.is_token && obj.zone == Zone::Battlefield)
        .map(|obj| TokenOutcome {
            id: obj.id,
            name: obj.name.clone(),
            counters: obj.counters.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            power: obj.power,
            toughness: obj.toughness,
            chosen: obj.chosen_attributes.clone(),
        })
        .collect();
    tokens.sort_by_key(|token| token.id);
    RowOutcome { answered, tokens }
}

/// Runs `row` saved and restored at every prompt, then live, and requires the same
/// outcome: the same prompts, the same parked owner at each, and the same tokens with the
/// same counters, P/T and choices. Reach-guarded on a save made while a parked owner
/// already held a created token.
fn assert_restored_row_matches_live(row: impl Fn(PauseCarry) -> RowOutcome, name: &str) {
    let restored = row(PauseCarry::SavedAndRestored);
    assert!(
        restored
            .answered
            .parked_created_ids
            .iter()
            .flatten()
            .any(|ids| !ids.is_empty()),
        "REACH-GUARD {name}: a save was made with a parked owner holding created tokens, got \
         {:?}",
        restored.answered.parked_created_ids
    );
    assert_eq!(
        restored,
        row(PauseCarry::Live),
        "{name}: the restored run settles as the live run does"
    );
}

fn named_prompts(prompts: &[Prompt]) -> Vec<PlayerId> {
    prompts
        .iter()
        .filter_map(|prompt| match prompt {
            Prompt::Named { player } => Some(*player),
            Prompt::Replacement | Prompt::CopyTarget { .. } | Prompt::PayAmount { .. } => None,
        })
        .collect()
}

fn has_color_choice(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .chosen_attributes
        .iter()
        .any(|attr| matches!(attr, ChosenAttribute::Color(_)))
}

/// Stolen Identity copying P0's `donor`, under `remover`.
fn stolen_identity(remover: Option<Remover>, donor: Donor) -> (GameRunner, Vec<Prompt>) {
    let mut scenario = board();
    let splendor = add_remover(&mut scenario, remover);
    let target = add_donor(&mut scenario, P0, donor);
    let spell = free_spell(&mut scenario, "Stolen Identity", false, STOLEN_IDENTITY);
    let mut runner = scenario.build();
    attach_remover(&mut runner, remover, splendor);
    runner.cast(spell).target_object(target).commit();
    let prompts = drive(&mut runner, |_, _| {});
    (runner, prompts)
}

/// T1 (R1–R4): a Stolen Identity copy of Triskelion under a remover that strips P0's
/// token gets none of the copied "enters with" counters (CR 614.12 + CR 614.1c +
/// CR 613.1f). Margle's +2/+2 still applies to the 1/1 copy.
#[test]
fn stolen_identity_triskelion_copy_under_a_remover_enters_without_its_counters() {
    for remover in STRIPPING_P0 {
        let row = format!("T1 {remover:?}");
        let (runner, prompts) = stolen_identity(Some(remover), TRISKELION_DONOR);
        assert!(prompts.is_empty(), "{row}: no prompt");
        assert_settled(&runner, &row);
        let token = the_token(&runner, TRISKELION_NAME);
        assert!(
            !runner.state().objects[&token]
                .base_replacement_definitions
                .is_empty(),
            "REACH-GUARD {row}: the token copied the \"enters with\" ability"
        );
        let expected = match remover {
            Remover::Margle(_) => (Some(3), Some(3)),
            _ => (Some(1), Some(1)),
        };
        assert_eq!(
            plus_counters(&runner, token),
            0,
            "{row}: no copied counters"
        );
        assert_eq!(power_toughness(&runner, token), expected, "{row}: P/T");
    }
}

/// C1–C3: with no remover, or one that strips only P1's creatures, the copied counters
/// apply.
#[test]
fn stolen_identity_triskelion_copy_keeps_its_counters_when_no_remover_applies_to_it() {
    for remover in [None, Some(Remover::Splendor(P1)), Some(Remover::Margle(P1))] {
        let row = format!("C1-C3 {remover:?}");
        let (runner, prompts) = stolen_identity(remover, TRISKELION_DONOR);
        assert!(prompts.is_empty(), "{row}: no prompt");
        assert_settled(&runner, &row);
        let token = the_token(&runner, TRISKELION_NAME);
        assert_eq!(plus_counters(&runner, token), 3, "{row}: copied counters");
        assert_eq!(power_toughness(&runner, token), (Some(4), Some(4)), "{row}");
    }
}

/// T2 (R5, R6): Stolen Identity copies of Voice of All and Roaming Throne under each of
/// the four removers: the copied "as enters, choose" ability doesn't apply, so no
/// choice is offered and nothing is chosen (CR 614.12 + CR 614.12a + CR 613.1f).
#[test]
fn stolen_identity_as_enters_choice_copy_under_a_remover_is_not_offered() {
    for donor in [VOICE_DONOR, THRONE_DONOR] {
        for remover in STRIPPING_P0 {
            let row = format!("T2 {} {remover:?}", donor.name);
            let (runner, prompts) = stolen_identity(Some(remover), donor);
            assert!(
                named_prompts(&prompts).is_empty(),
                "{row}: no as-enters choice"
            );
            assert_settled(&runner, &row);
            let token = the_token(&runner, donor.name);
            assert!(
                !runner.state().objects[&token]
                    .base_replacement_definitions
                    .is_empty(),
                "REACH-GUARD {row}: the token copied the \"as enters\" ability"
            );
            assert!(
                runner.state().objects[&token].chosen_attributes.is_empty(),
                "{row}: nothing was chosen"
            );
        }
    }
}

/// C4: with no remover, or one that strips only P1's creatures, the copy's "as enters"
/// choice goes to P0, and the answer is recorded on the token.
#[test]
fn stolen_identity_as_enters_choice_copy_is_offered_when_no_remover_applies_to_it() {
    for donor in [VOICE_DONOR, THRONE_DONOR] {
        for remover in [None, Some(Remover::Splendor(P1)), Some(Remover::Margle(P1))] {
            let row = format!("C4 {} {remover:?}", donor.name);
            let (runner, prompts) = stolen_identity(remover, donor);
            assert_eq!(
                named_prompts(&prompts),
                vec![P0],
                "{row}: one choice, to P0"
            );
            assert_settled(&runner, &row);
            let token = the_token(&runner, donor.name);
            assert!(
                !runner.state().objects[&token].chosen_attributes.is_empty(),
                "{row}: the answer is recorded"
            );
        }
    }
}

/// Twinflame copying P0's `donor`, under Humility or nothing.
fn twinflame(humility: bool, donor: Donor) -> (GameRunner, Vec<Prompt>) {
    let mut scenario = board();
    add_remover(&mut scenario, humility.then_some(Remover::Humility));
    let target = add_donor(&mut scenario, P0, donor);
    let spell = free_spell(&mut scenario, "Twinflame", false, TWINFLAME);
    let mut runner = scenario.build();
    runner.cast(spell).target_object(target).commit();
    let prompts = drive(&mut runner, |_, _| {});
    (runner, prompts)
}

/// T3 (R7, R8): Twinflame's copies under Humility. The exception's haste is a copiable
/// ability (CR 707.9a) that Humility removes (CR 613.1f), and so are the copied entry
/// abilities: Voice of All's copy offers no choice, Triskelion's gets no counters. The
/// delayed exile is still created. With no remover the haste, the choice and the counters
/// are all there.
#[test]
fn twinflame_copies_under_humility_lose_their_entry_abilities() {
    for humility in [true, false] {
        let row = format!("T3 humility={humility}");
        let (voice_runner, voice_prompts) = twinflame(humility, VOICE_DONOR);
        assert_settled(&voice_runner, &row);
        let voice = the_token(&voice_runner, VOICE_NAME);
        assert_eq!(
            voice_runner.state().objects[&voice].has_keyword(&Keyword::Haste),
            !humility,
            "REACH-GUARD {row}: the exception's haste reached the token"
        );
        assert!(
            !voice_runner.state().delayed_triggers.is_empty(),
            "REACH-GUARD {row}: the delayed exile is registered"
        );
        if humility {
            assert!(named_prompts(&voice_prompts).is_empty(), "{row}: no choice");
            assert!(voice_runner.state().objects[&voice]
                .chosen_attributes
                .is_empty());
        } else {
            assert_eq!(named_prompts(&voice_prompts), vec![P0], "{row}: choice");
            assert!(has_color_choice(&voice_runner, voice));
        }

        let (trisk_runner, trisk_prompts) = twinflame(humility, TRISKELION_DONOR);
        assert!(trisk_prompts.is_empty());
        assert_settled(&trisk_runner, &row);
        let trisk = the_token(&trisk_runner, TRISKELION_NAME);
        assert_eq!(
            trisk_runner.state().objects[&trisk].has_keyword(&Keyword::Haste),
            !humility,
            "REACH-GUARD {row}: the exception's haste reached the token"
        );
        let (counters, pt) = if humility {
            (0, (Some(1), Some(1)))
        } else {
            (3, (Some(4), Some(4)))
        };
        assert_eq!(plus_counters(&trisk_runner, trisk), counters, "{row}");
        assert_eq!(power_toughness(&trisk_runner, trisk), pt, "{row}");
    }
}

/// Saw in Half on `victim_controller`'s Triskelion, under `remover`.
fn saw_in_half(remover: Option<Remover>, victim_controller: PlayerId) -> (GameRunner, ObjectId) {
    let mut scenario = board();
    let splendor = add_remover(&mut scenario, remover);
    let target = add_donor(&mut scenario, victim_controller, TRISKELION_DONOR);
    let spell = free_spell(&mut scenario, "Saw in Half", true, SAW_IN_HALF);
    let mut runner = scenario.build();
    attach_remover(&mut runner, remover, splendor);
    runner.cast(spell).target_object(target).commit();
    let prompts = drive(&mut runner, |_, _| {});
    assert!(prompts.is_empty(), "no prompt");
    (runner, target)
}

/// T4 (R9, R10, C5): Saw in Half's two copies are created by the destroyed creature's
/// controller (CR 111.2), and CR 614.12 reads the remover that would apply under that
/// player: Humility strips P0's copies; Splendor on P1 strips P1's copies; Splendor on P0
/// doesn't strip P1's.
#[test]
fn saw_in_half_copies_read_the_remover_under_their_controller() {
    for (remover, victim, stripped) in [
        (Remover::Humility, P0, true),
        (Remover::Splendor(P1), P1, true),
        (Remover::Splendor(P0), P1, false),
    ] {
        let row = format!("T4 {remover:?} victim={victim:?}");
        let (runner, original) = saw_in_half(Some(remover), victim);
        assert_settled(&runner, &row);
        assert_eq!(
            runner.state().objects[&original].zone,
            Zone::Graveyard,
            "REACH-GUARD {row}: the original was destroyed"
        );
        let tokens = battlefield_tokens(&runner, TRISKELION_NAME);
        assert_eq!(tokens.len(), 2, "REACH-GUARD {row}: two copies");
        let (counters, pt) = if stripped {
            (0, (Some(1), Some(1)))
        } else {
            (3, (Some(4), Some(4)))
        };
        for token in tokens {
            assert_eq!(
                runner.state().objects[&token].controller,
                victim,
                "REACH-GUARD {row}: the victim's controller creates the copies"
            );
            assert_eq!(plus_counters(&runner, token), counters, "{row}");
            assert_eq!(power_toughness(&runner, token), pt, "{row}");
        }
    }
}

/// Littjara Mirrorlake copying P0's Triskelion, under Humility or nothing.
fn littjara(humility: bool) -> (GameRunner, ObjectId) {
    let mut scenario = board();
    add_remover(&mut scenario, humility.then_some(Remover::Humility));
    let target = add_donor(&mut scenario, P0, TRISKELION_DONOR);
    let land = scenario
        .add_land_from_oracle(P0, "Littjara Mirrorlake", LITTJARA)
        .id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Green,
            ManaType::Green,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    let index = runner.state().objects[&land]
        .abilities
        .iter()
        .position(|ability| matches!(*ability.effect, Effect::CopyTokenOf { .. }))
        .expect("REACH-GUARD: Littjara's copy ability");
    runner.activate(land, index).target_object(target).resolve();
    let prompts = drive(&mut runner, |_, _| {});
    assert!(prompts.is_empty(), "no prompt");
    (runner, land)
}

/// T5 (R11, C6): Littjara's "enters with an additional +1/+1 counter" is part of the creating
/// effect, not an ability of the token, so no ability remover touches it (CR 614.12 reads only
/// the token's own abilities); the copied Triskelion ability goes. With no remover both apply.
#[test]
fn littjara_copy_under_humility_keeps_only_the_exception_counter() {
    for (humility, counters, pt) in [
        (true, 1, (Some(2), Some(2))),
        (false, 4, (Some(5), Some(5))),
    ] {
        let row = format!("T5 humility={humility}");
        let (runner, land) = littjara(humility);
        assert_settled(&runner, &row);
        assert_eq!(
            runner.state().objects[&land].zone,
            Zone::Graveyard,
            "REACH-GUARD {row}: Littjara was sacrificed"
        );
        let token = the_token(&runner, TRISKELION_NAME);
        assert_eq!(plus_counters(&runner, token), counters, "{row}");
        assert_eq!(power_toughness(&runner, token), pt, "{row}");
    }
}

/// Clone Legion targeting P1, who controls `donors` (in this order), after `extra` is
/// placed.
fn clone_legion(
    donors: &[Donor],
    extra: impl FnOnce(&mut GameScenario),
    observe: impl FnMut(&GameRunner, &Prompt),
) -> (GameRunner, Vec<Prompt>) {
    let (runner, answered) = clone_legion_carried(
        donors,
        extra,
        |_| {},
        PauseCarry::Live,
        &Script::default(),
        observe,
    );
    (runner, answered.prompts)
}

/// [`clone_legion`], after `before` has run on the built game, carrying each prompt per
/// `carry` and answering per `script`.
fn clone_legion_carried(
    donors: &[Donor],
    extra: impl FnOnce(&mut GameScenario),
    before: impl FnOnce(&mut GameRunner),
    carry: PauseCarry,
    script: &Script,
    observe: impl FnMut(&GameRunner, &Prompt),
) -> (GameRunner, Answered) {
    let mut scenario = board();
    extra(&mut scenario);
    for donor in donors {
        add_donor(&mut scenario, P1, *donor);
    }
    let spell = free_spell(&mut scenario, "Clone Legion", false, CLONE_LEGION);
    let mut runner = scenario.build();
    before(&mut runner);
    runner.cast(spell).target_player(P1).commit();
    let answered = drive_carried(&mut runner, carry, script, observe);
    (runner, answered)
}

/// T6 (X1): Clone Legion over [Margle, Triskelion]. The tokens enter at the same time, so
/// the Margle copy's effect doesn't exist yet as the Triskelion copy enters: it gets its
/// counters, then Margle's +2/+2 (CR 614.12, Clone Legion ruling 2015-02-25).
#[test]
fn clone_legion_co_entering_margle_copy_does_not_strip_the_triskelion_copy_counters() {
    let (runner, prompts) = clone_legion(&[MARGLE_DONOR, TRISKELION_DONOR], |_| {}, |_, _| {});
    assert!(prompts.is_empty());
    assert_settled(&runner, "T6");
    let margle = the_token(&runner, MARGLE_NAME);
    assert_eq!(
        runner.state().objects[&margle].controller,
        P0,
        "REACH-GUARD: the Margle copy is P0's"
    );
    let trisk = the_token(&runner, TRISKELION_NAME);
    assert!(
        runner.state().objects[&trisk]
            .replacement_definitions
            .is_empty(),
        "REACH-GUARD: the Margle copy strips the Triskelion copy once both are there"
    );
    assert_eq!(plus_counters(&runner, trisk), 3, "the counters were placed");
    assert_eq!(power_toughness(&runner, trisk), (Some(6), Some(6)));
}

/// T7 (X2): Clone Legion over [Margle, Voice of All]: the Voice copy's choice is offered,
/// though the Margle copy is already on the battlefield when it is.
#[test]
fn clone_legion_co_entering_margle_copy_does_not_strip_the_voice_copy_choice() {
    let mut margle_at_prompt = Vec::new();
    let (runner, prompts) = clone_legion(
        &[MARGLE_DONOR, VOICE_DONOR],
        |_| {},
        |runner, _| margle_at_prompt.push(battlefield_tokens(runner, MARGLE_NAME).len()),
    );
    assert_eq!(named_prompts(&prompts), vec![P0], "the Voice copy's choice");
    assert_eq!(
        margle_at_prompt,
        vec![1],
        "REACH-GUARD: the Margle copy is on the battlefield at the prompt"
    );
    assert_settled(&runner, "T7");
    let voice = the_token(&runner, VOICE_NAME);
    assert!(has_color_choice(&runner, voice));
}

/// T8 (X3): Clone Legion over [Voice, Margle, Voice]: both Voice copies' choices are
/// offered, the first before the Margle copy exists and the second after.
#[test]
fn clone_legion_choices_before_and_after_a_co_entering_margle_copy() {
    let mut margle_at_prompt = Vec::new();
    let (runner, prompts) = clone_legion(
        &[VOICE_DONOR, MARGLE_DONOR, VOICE_DONOR],
        |_| {},
        |runner, _| margle_at_prompt.push(battlefield_tokens(runner, MARGLE_NAME).len()),
    );
    assert_eq!(named_prompts(&prompts), vec![P0, P0], "two choices");
    assert_eq!(
        margle_at_prompt,
        vec![0, 1],
        "REACH-GUARD: the first prompt comes before the Margle copy's batch"
    );
    assert_settled(&runner, "T8");
    let voices = battlefield_tokens(&runner, VOICE_NAME);
    assert_eq!(voices.len(), 2);
    assert!(voices.iter().all(|voice| has_color_choice(&runner, *voice)));
}

/// T10 (X4): Clone Legion over [Margle, Voice, Voice]: the second Voice copy is scanned
/// after the first one's pause, with the Margle copy from an earlier batch on the
/// battlefield; both choices are offered.
#[test]
fn clone_legion_choice_after_a_pause_still_sees_the_earlier_margle_copy_as_co_entrant() {
    clone_legion_choice_after_a_pause(PauseCarry::Live);
}

/// T10 saved and restored at each choice: the second Voice copy's batch is drained on resume,
/// and the drain publishes the parked owner's serialized `created_ids` (the Margle copy
/// included) as its co-entrants (`token_copy::publish_committed_copy_tokens`).
#[test]
fn clone_legion_choice_after_a_pause_still_sees_the_earlier_margle_copy_as_co_entrant_across_save_and_restore(
) {
    assert_restored_row_matches_live(clone_legion_choice_after_a_pause, "T10");
}

fn clone_legion_choice_after_a_pause(carry: PauseCarry) -> RowOutcome {
    let mut margle_at_prompt = Vec::new();
    let (runner, answered) = clone_legion_carried(
        &[MARGLE_DONOR, VOICE_DONOR, VOICE_DONOR],
        |_| {},
        |_| {},
        carry,
        &Script::default(),
        |runner, _| margle_at_prompt.push(battlefield_tokens(runner, MARGLE_NAME).len()),
    );
    assert_eq!(
        named_prompts(&answered.prompts),
        vec![P0, P0],
        "two choices"
    );
    assert_eq!(
        margle_at_prompt,
        vec![1, 1],
        "REACH-GUARD: the Margle copy is on the battlefield at both prompts"
    );
    assert_settled(&runner, "T10");
    let voices = battlefield_tokens(&runner, VOICE_NAME);
    assert_eq!(voices.len(), 2);
    assert!(voices.iter().all(|voice| has_color_choice(&runner, *voice)));
    row_outcome(&runner, answered)
}

/// T11 (X5): Parallel Lives doubles each batch. Both Margle copies co-enter with both Voice
/// copies, so both choices are offered.
#[test]
fn clone_legion_doubled_batches_keep_every_margle_copy_as_co_entrant() {
    clone_legion_doubled_batches(PauseCarry::Live);
}

/// T11 saved and restored at each choice: the second Voice copy of the doubled batch enters
/// on resume from the first one's choice, where both Margle copies are co-entrants through
/// the parked owner's serialized `created_ids` (`token_copy::copy_token_co_entrants`).
#[test]
fn clone_legion_doubled_batches_keep_every_margle_copy_as_co_entrant_across_save_and_restore() {
    assert_restored_row_matches_live(clone_legion_doubled_batches, "T11");
}

fn clone_legion_doubled_batches(carry: PauseCarry) -> RowOutcome {
    let mut margles_at_prompt = Vec::new();
    let (runner, answered) = clone_legion_carried(
        &[MARGLE_DONOR, VOICE_DONOR],
        |scenario| {
            scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
        },
        |_| {},
        carry,
        &Script::default(),
        |runner, _| margles_at_prompt.push(battlefield_tokens(runner, MARGLE_NAME)),
    );
    assert_eq!(
        named_prompts(&answered.prompts),
        vec![P0, P0],
        "two choices"
    );
    assert_eq!(
        margles_at_prompt.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![2, 2],
        "REACH-GUARD: both Margle copies are on the battlefield at both prompts"
    );
    let parked_at_first = answered.parked_created_ids[0]
        .as_deref()
        .expect("REACH-GUARD: the CopyToken owner is parked at the first choice");
    assert!(
        margles_at_prompt[0]
            .iter()
            .all(|margle| parked_at_first.contains(margle)),
        "REACH-GUARD: the parked owner lists both Margle copies when the first choice is \
         answered, got {parked_at_first:?}"
    );
    assert_settled(&runner, "T11");
    let voices = battlefield_tokens(&runner, VOICE_NAME);
    assert_eq!(voices.len(), 2, "Parallel Lives doubled the Voice copy");
    assert!(voices.iter().all(|voice| has_color_choice(&runner, *voice)));
    row_outcome(&runner, answered)
}

/// T12 (X6): Clone Legion over [Margle, Voice, Triskelion]: the non-liminal Triskelion
/// batch runs after the liminal Voice batch paused, and still sees the Margle copy as a
/// co-entrant.
#[test]
fn clone_legion_non_liminal_batch_after_a_liminal_pause_sees_the_margle_copy_as_co_entrant() {
    clone_legion_non_liminal_batch_after_a_liminal_pause(PauseCarry::Live);
}

/// T12 saved and restored at the Voice copy's choice: the Triskelion batch is drained on
/// resume, and the drain publishes the parked owner's serialized `created_ids` (the Margle
/// copy included) as its co-entrants (`token_copy::publish_committed_copy_tokens`).
#[test]
fn clone_legion_non_liminal_batch_after_a_liminal_pause_sees_the_margle_copy_as_co_entrant_across_save_and_restore(
) {
    assert_restored_row_matches_live(clone_legion_non_liminal_batch_after_a_liminal_pause, "T12");
}

fn clone_legion_non_liminal_batch_after_a_liminal_pause(carry: PauseCarry) -> RowOutcome {
    let mut margle_at_prompt = Vec::new();
    let (runner, answered) = clone_legion_carried(
        &[MARGLE_DONOR, VOICE_DONOR, TRISKELION_DONOR],
        |_| {},
        |_| {},
        carry,
        &Script::default(),
        |runner, _| margle_at_prompt.push(battlefield_tokens(runner, MARGLE_NAME).len()),
    );
    assert_eq!(
        named_prompts(&answered.prompts),
        vec![P0],
        "the Voice copy's choice"
    );
    assert_eq!(
        margle_at_prompt,
        vec![1],
        "REACH-GUARD: Margle copy present"
    );
    assert_settled(&runner, "T12");
    let voice = the_token(&runner, VOICE_NAME);
    assert!(
        has_color_choice(&runner, voice),
        "REACH-GUARD: the Voice copy chose"
    );
    let trisk = the_token(&runner, TRISKELION_NAME);
    assert_eq!(plus_counters(&runner, trisk), 3, "the counters were placed");
    assert_eq!(power_toughness(&runner, trisk), (Some(6), Some(6)));
    row_outcome(&runner, answered)
}

/// Applied Geometry copying P0's Gemstone Mine, under Humility or nothing.
fn applied_geometry(humility: bool) -> GameRunner {
    let mut scenario = board();
    add_remover(&mut scenario, humility.then_some(Remover::Humility));
    let mine = scenario
        .add_land_from_oracle(P0, GEMSTONE_NAME, GEMSTONE_MINE)
        .id();
    let spell = free_spell(&mut scenario, "Applied Geometry", false, APPLIED_GEOMETRY);
    let mut runner = scenario.build();
    runner.cast(spell).target_object(mine).commit();
    let prompts = drive(&mut runner, |_, _| {});
    assert!(prompts.is_empty(), "no prompt");
    runner
}

/// T13 (R12, C7): Applied Geometry's exception makes the land copy a creature
/// (CR 707.9b), so Humility applies to it as it would exist on the battlefield
/// (CR 614.12) and its copied "enters with three mining counters" doesn't apply. The six
/// +1/+1 counters are the spell's own instruction. With no remover the mining counters
/// are placed.
#[test]
fn applied_geometry_creature_copy_of_a_land_under_humility_enters_without_its_counters() {
    for (humility, mining_count, pt) in [
        (true, 0, (Some(7), Some(7))),
        (false, 3, (Some(6), Some(6))),
    ] {
        let row = format!("T13 humility={humility}");
        let runner = applied_geometry(humility);
        assert_settled(&runner, &row);
        let token = the_token(&runner, GEMSTONE_NAME);
        let types = &runner.state().objects[&token].card_types.core_types;
        assert!(
            types.contains(&CoreType::Land) && types.contains(&CoreType::Creature),
            "REACH-GUARD {row}: the exception made the land copy a creature, got {types:?}"
        );
        assert_eq!(
            counters_of(&runner, token, &mining()),
            mining_count,
            "{row}"
        );
        assert_eq!(
            plus_counters(&runner, token),
            6,
            "{row}: the spell's counters"
        );
        assert_eq!(power_toughness(&runner, token), pt, "{row}");
    }
}

/// T14: P0's Case of the Pilfered Proof (solved) adds a Clue batch to each Clone Legion
/// batch, so Parallel Lives, Halving Season and the Case raise a replacement-order choice
/// on the Voice batch's event, and that compound event delivers the Voice copy while it
/// holds the CopyToken owner off the resolution stack. The Margle copy from the earlier
/// batch is still a co-entrant there, so the Voice copy's choice is offered.
#[test]
fn clone_legion_compound_event_delivery_keeps_the_earlier_margle_copy_as_co_entrant() {
    clone_legion_compound_event_delivery(PauseCarry::Live);
}

/// T14 saved and restored at every replacement-order choice and at the Voice copy's
/// choice: each is raised with the owner parked, and the resumed compound-event delivery
/// publishes its serialized `created_ids` (the Margle copy included) as the Voice copy's
/// co-entrants (`token_copy::publish_committed_copy_tokens`).
#[test]
fn clone_legion_compound_event_delivery_keeps_the_earlier_margle_copy_as_co_entrant_across_save_and_restore(
) {
    assert_restored_row_matches_live(clone_legion_compound_event_delivery, "T14");
}

fn clone_legion_compound_event_delivery(carry: PauseCarry) -> RowOutcome {
    let mut scenario = board();
    let case = {
        let mut builder = scenario.add_creature(P0, "Case of the Pilfered Proof", 0, 0);
        builder
            .as_enchantment()
            .with_subtypes(vec!["Case"])
            .from_oracle_text(CASE_OF_THE_PILFERED_PROOF);
        builder.id()
    };
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    scenario.add_enchantment_from_oracle(P1, "Halving Season", HALVING_SEASON);
    add_donor(&mut scenario, P1, MARGLE_DONOR);
    add_donor(&mut scenario, P1, VOICE_DONOR);
    let spell = free_spell(&mut scenario, "Clone Legion", false, CLONE_LEGION);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&case)
        .and_then(|obj| obj.case_state.as_mut())
        .expect("REACH-GUARD: the Case carries its solve state")
        .is_solved = true;
    engine::game::layers::mark_layers_full(runner.state_mut());
    engine::game::layers::flush_layers(runner.state_mut());
    runner.cast(spell).target_player(P1).commit();

    // (CopyToken owner on top, Margle copies, Clues) at each replacement-order prompt.
    let mut replacement_prompts = Vec::new();
    let mut margle_at_named = Vec::new();
    let answered =
        drive_carried(
            &mut runner,
            carry,
            &Script::default(),
            |runner, prompt| match prompt {
                Prompt::Replacement => replacement_prompts.push((
                    runner
                        .state()
                        .resolution_stack
                        .active_copy_token()
                        .is_some(),
                    battlefield_tokens(runner, MARGLE_NAME).len(),
                    battlefield_tokens(runner, "Clue").len(),
                )),
                Prompt::Named { .. } => {
                    margle_at_named.push(battlefield_tokens(runner, MARGLE_NAME).len())
                }
                Prompt::CopyTarget { .. } | Prompt::PayAmount { .. } => {}
            },
        );
    assert_eq!(
        named_prompts(&answered.prompts),
        vec![P0],
        "the Voice copy's choice"
    );
    assert_eq!(
        margle_at_named,
        vec![1],
        "the Margle copy is on the battlefield at the prompt"
    );
    let voice_batch: Vec<_> = replacement_prompts
        .iter()
        .filter(|(_, margles, _)| *margles == 1)
        .collect();
    assert!(
        !voice_batch.is_empty() && voice_batch.iter().all(|(on_top, _, _)| *on_top),
        "REACH-GUARD: the Voice batch's prompts come with the CopyToken owner on top, got \
         {replacement_prompts:?}"
    );
    let clues_before_voice_batch = voice_batch[0].2;
    assert_settled(&runner, "T14");
    assert!(
        battlefield_tokens(&runner, "Clue").len() > clues_before_voice_batch,
        "REACH-GUARD: the Voice batch's event created a Clue"
    );
    let voice = the_token(&runner, VOICE_NAME);
    assert!(has_color_choice(&runner, voice), "the answer is recorded");
    assert!(runner
        .state()
        .resolution_stack
        .iter()
        .all(|frame| !matches!(frame, ResolutionFrame::CopyToken(_))));
    row_outcome(&runner, answered)
}

// ---------------------------------------------------------------------------------------
// A second copy choice inside a copy-token batch (maintainer review 1 on #10767).
//
// - Quicksilver Gargantuan: "You may have this creature enter as a copy of any creature on
//   the battlefield, except it's 7/7." Ruling 2011-01-01: "Any 'as [this creature] enters'
//   or '[this creature] enters with' abilities of the chosen creature will also work." and
//   "If Quicksilver Gargantuan somehow enters at the same time as another creature ..., it
//   can't become a copy of that creature. You may choose only a creature that's already on
//   the battlefield."
// - CR 707.5: an object that enters "as a copy" becomes a copy as it enters, and copied
//   "enters with" / "as enters" abilities take effect. CR 614.12: whether they apply is read
//   on the permanent as it would exist on the battlefield, where the tokens entering in the
//   same event (Clone Legion ruling 2015-02-25) don't exist yet.
// ---------------------------------------------------------------------------------------

/// The kind of each answered prompt, in order.
fn prompt_kinds(prompts: &[Prompt]) -> Vec<&'static str> {
    prompts
        .iter()
        .map(|prompt| match prompt {
            Prompt::Named { .. } => "Named",
            Prompt::Replacement => "Replacement",
            Prompt::CopyTarget { .. } => "CopyTarget",
            Prompt::PayAmount { .. } => "PayAmount",
        })
        .collect()
}

/// The donors each `CopyTargetChoice` offered, in order. REACH-GUARD: each went to P0.
fn copy_offers(prompts: &[Prompt]) -> Vec<&[CopyPick]> {
    prompts
        .iter()
        .filter_map(|prompt| match prompt {
            Prompt::CopyTarget { player, valid } => {
                assert_eq!(*player, P0, "REACH-GUARD: P0 makes the copy choice");
                Some(valid.as_slice())
            }
            _ => None,
        })
        .collect()
}

/// The source names a CR 616.1 order prompt (or a copy offer) offers, sorted.
fn replacement_sources(runner: &GameRunner) -> Vec<String> {
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        return Vec::new();
    };
    let mut names: Vec<String> = candidates
        .iter()
        .map(|candidate| candidate.source_name.clone())
        .collect();
    names.sort();
    names
}

/// REACH-GUARD: P0's token named `name`, created by an earlier batch of the resolution, is
/// on the battlefield and phased in, and the parked CopyToken owner lists it.
fn assert_co_entrant_parked(runner: &GameRunner, name: &str, row: &str) -> ObjectId {
    let id = the_token(runner, name);
    let obj = &runner.state().objects[&id];
    assert!(
        obj.is_phased_in_permanent() && obj.controller == P0,
        "REACH-GUARD {row}: the {name} copy is P0's, on the battlefield, phased in"
    );
    let parked = parked_created_ids(runner).unwrap_or_default();
    assert!(
        parked.contains(&id),
        "REACH-GUARD {row}: the parked CopyToken owner lists the {name} copy, got {parked:?}"
    );
    id
}

/// [`clone_legion_carried`] for a row whose first donor's copy, `co_entrant`, co-enters
/// with the later batches: it is asserted at every prompt by [`assert_co_entrant_parked`].
/// `observe` sees each prompt afterwards.
fn clone_legion_beside(
    co_entrant: &str,
    donors: &[Donor],
    extra: impl FnOnce(&mut GameScenario),
    before: impl FnOnce(&mut GameRunner),
    carry: PauseCarry,
    script: &Script,
    mut observe: impl FnMut(&GameRunner, &Prompt),
) -> (GameRunner, Answered) {
    clone_legion_carried(donors, extra, before, carry, script, |runner, prompt| {
        assert_co_entrant_parked(runner, co_entrant, "co-entrant");
        observe(runner, prompt);
    })
}

#[test]
fn clone_legion_copy_choice_excludes_artifact_animated_by_a_co_entrant() {
    let mut scenario = board();
    let bello = "Bello, Bard of the Brambles";
    scenario
        .add_creature_from_oracle(
            P1,
            bello,
            3,
            3,
            "During your turn, each non-Equipment artifact and non-Aura enchantment you control with mana value 4 or greater is a 4/4 Elemental creature in addition to its other types and has indestructible, haste, and \"Whenever this creature deals combat damage to a player, draw a card.\"",
        )
        .as_legendary()
        .with_subtypes(vec!["Raccoon", "Bard"]);
    // This copy flushes the live layers between the Bello and Gargantuan entries.
    scenario
        .add_creature_from_oracle(P1, TRISKELION_NAME, 1, 1, TRISKELION)
        .as_artifact_creature()
        .with_subtypes(vec!["Construct"])
        .with_mana_cost(ManaCost::generic(6));
    add_donor(&mut scenario, P1, GARGANTUAN_DONOR);
    let dynamo = scenario
        .add_artifact_from_oracle(P0, "Thran Dynamo", "{T}: Add {C}{C}{C}.")
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let spell = free_spell(&mut scenario, "Clone Legion", false, CLONE_LEGION);
    let mut runner = scenario.build();
    runner.cast(spell).target_player(P1).commit();
    let answered = drive_carried(
        &mut runner,
        PauseCarry::Live,
        &Script {
            copy_targets: vec![CopyPick::card(TRISKELION_NAME, P1)],
            ..Script::default()
        },
        |runner, prompt| {
            if let Prompt::CopyTarget { .. } = prompt {
                assert_co_entrant_parked(runner, bello, "Bello donor eligibility");
                assert!(
                    runner.state().objects[&dynamo]
                        .card_types
                        .core_types
                        .contains(&CoreType::Creature),
                    "Dynamo is animated on the live board at the copy prompt"
                );
            }
        },
    );
    let offers = copy_offers(&answered.prompts);
    assert_eq!(offers.len(), 1, "Gargantuan offers a copy choice");
    assert!(offers[0].contains(&CopyPick::card(bello, P1)));
    assert!(offers[0].contains(&CopyPick::card(TRISKELION_NAME, P1)));
    assert!(
        !offers[0].contains(&CopyPick::card("Thran Dynamo", P0)),
        "Dynamo is ineligible on the board before Bello enters, got {:?}",
        offers[0]
    );
    assert_settled(&runner, "Bello donor eligibility");
    assert_eq!(battlefield_tokens(&runner, TRISKELION_NAME).len(), 2);
}

#[test]
fn clone_legion_frostkite_offers_donor_weakened_only_by_a_co_entrant() {
    frostkite_pre_entry_donor(PauseCarry::Live);
}

#[test]
fn clone_legion_frostkite_offers_donor_weakened_only_by_a_co_entrant_across_save_and_restore() {
    assert_restored_row_matches_live(frostkite_pre_entry_donor, "Frostkite donor eligibility");
}

fn frostkite_pre_entry_donor(carry: PauseCarry) -> RowOutcome {
    let mut scenario = board();
    let geth = "Geth, Thane of Contracts";
    scenario
        .add_creature_from_oracle(P1, geth, 3, 4, "Other creatures you control get -1/-1.\n{1}{B}{B}, {T}: Return target creature card from your graveyard to the battlefield. It gains \"If this creature would leave the battlefield, exile it instead of putting it anywhere else.\" Activate only as a sorcery.");
    let triskelion = scenario
        .add_creature_from_oracle(P1, TRISKELION_NAME, 1, 1, TRISKELION)
        .as_artifact_creature()
        .with_subtypes(vec!["Construct"])
        .with_mana_cost(ManaCost::generic(6))
        .id();
    let frostkite = scenario
        .add_creature_from_oracle(P1, "Deceptive Frostkite", 1, 1, "Flying\nYou may have this creature enter as a copy of a creature you control with power 4 or greater, except it's a Dragon in addition to its other types and it has flying.").id();
    // Keep the opponent's printed 1/1 creatures alive beside Geth.
    scenario.with_counter(triskelion, CounterType::Plus1Plus1, 3);
    scenario.with_counter(frostkite, CounterType::Plus1Plus1, 1);
    let baloth = scenario
        .add_creature_from_oracle(P0, "Rumbling Baloth", 4, 4, "")
        .with_subtypes(vec!["Beast"])
        .id();
    let spell = free_spell(&mut scenario, "Clone Legion", false, CLONE_LEGION);
    let mut runner = scenario.build();
    runner.cast(spell).target_player(P1).commit();
    let answered = drive_carried(
        &mut runner,
        carry,
        &Script {
            copy_targets: vec![CopyPick::card("Rumbling Baloth", P0)],
            ..Script::default()
        },
        |runner, prompt| {
            if let Prompt::CopyTarget { .. } = prompt {
                assert_co_entrant_parked(runner, geth, "Frostkite donor eligibility");
                assert_eq!(
                    power_toughness(runner, baloth),
                    (Some(3), Some(3)),
                    "REACH-GUARD: Geth weakens the resident donor on the live board"
                );
            }
        },
    );
    // CR 614.12a: choose before entry, when the co-entering Geth is absent.
    assert_eq!(
        copy_offers(&answered.prompts),
        vec![vec![CopyPick::card("Rumbling Baloth", P0)]]
    );
    assert_settled(&runner, "Frostkite donor eligibility");
    let tokens = battlefield_tokens(&runner, "Rumbling Baloth");
    assert_eq!(tokens.len(), 1);
    let token = &runner.state().objects[&tokens[0]];
    assert!(token.card_types.subtypes.iter().any(|s| s == "Dragon"));
    assert!(token.has_keyword(&Keyword::Flying));
    row_outcome(&runner, answered)
}

/// G1: Clone Legion over P1's [Margle, Quicksilver Gargantuan]. The Gargantuan copy
/// accepts its copy offer and copies P0's Triskelion. The Margle copy co-enters, so the
/// copied "enters with three +1/+1 counters" applies (CR 614.12 + CR 707.5), and the
/// Margle copy isn't a legal donor (Quicksilver Gargantuan ruling 2011-01-01).
#[test]
fn clone_legion_gargantuan_copy_of_triskelion_keeps_its_counters_beside_a_co_entering_margle_copy()
{
    gargantuan_copy_of_triskelion_beside_margle(PauseCarry::Live);
}

/// G1 saved and restored at the copy offer and the copy choice: the co-entrant board is
/// rebuilt from the parked owner and the batch ledger, both serialized.
#[test]
fn clone_legion_gargantuan_copy_of_triskelion_keeps_its_counters_beside_a_co_entering_margle_copy_across_save_and_restore(
) {
    assert_restored_row_matches_live(gargantuan_copy_of_triskelion_beside_margle, "G1");
}

fn gargantuan_copy_of_triskelion_beside_margle(carry: PauseCarry) -> RowOutcome {
    let (runner, answered) = clone_legion_beside(
        MARGLE_NAME,
        &[MARGLE_DONOR, GARGANTUAN_DONOR],
        |scenario| {
            add_donor(scenario, P0, TRISKELION_DONOR);
        },
        |_| {},
        carry,
        &Script {
            copy_targets: vec![CopyPick::card(TRISKELION_NAME, P0)],
            ..Script::default()
        },
        |_, _| {},
    );
    assert_eq!(
        prompt_kinds(&answered.prompts),
        vec!["Replacement", "CopyTarget"],
        "REACH-GUARD G1: the copy offer, then the copy choice"
    );
    let offers = copy_offers(&answered.prompts);
    assert!(
        !offers[0].contains(&CopyPick::token(MARGLE_NAME, P0)),
        "G1: the co-entering Margle copy is not a legal donor, got {:?}",
        offers[0]
    );
    assert_settled(&runner, "G1");
    let copy = the_token(&runner, TRISKELION_NAME);
    assert!(
        runner.state().objects[&copy]
            .replacement_definitions
            .is_empty(),
        "REACH-GUARD G1: the Margle copy strips the Gargantuan copy once both are there"
    );
    assert_eq!(plus_counters(&runner, copy), 3, "G1: the copied counters");
    assert_eq!(
        power_toughness(&runner, copy),
        (Some(12), Some(12)),
        "G1: 7/7 (CR 707.9b) + three counters + Margle's +2/+2"
    );
    row_outcome(&runner, answered)
}

/// G2: as G1, but the Gargantuan copy copies P0's Voice of All: its copied "as enters,
/// choose a color" is offered and latched.
#[test]
fn clone_legion_gargantuan_copy_of_voice_chooses_beside_a_co_entering_margle_copy() {
    gargantuan_copy_of_voice_beside_margle(PauseCarry::Live);
}

/// G2 saved and restored at the copy offer, the copy choice and the color choice.
#[test]
fn clone_legion_gargantuan_copy_of_voice_chooses_beside_a_co_entering_margle_copy_across_save_and_restore(
) {
    assert_restored_row_matches_live(gargantuan_copy_of_voice_beside_margle, "G2");
}

fn gargantuan_copy_of_voice_beside_margle(carry: PauseCarry) -> RowOutcome {
    let (runner, answered) = clone_legion_beside(
        MARGLE_NAME,
        &[MARGLE_DONOR, GARGANTUAN_DONOR],
        |scenario| {
            add_donor(scenario, P0, VOICE_DONOR);
        },
        |_| {},
        carry,
        &Script {
            copy_targets: vec![CopyPick::card(VOICE_NAME, P0)],
            ..Script::default()
        },
        |_, _| {},
    );
    assert_eq!(
        prompt_kinds(&answered.prompts),
        vec!["Replacement", "CopyTarget", "Named"],
        "G2: the copied color choice follows the copy choice"
    );
    assert_eq!(named_prompts(&answered.prompts), vec![P0]);
    let offers = copy_offers(&answered.prompts);
    assert!(
        !offers[0].contains(&CopyPick::token(MARGLE_NAME, P0)),
        "G2: the co-entering Margle copy is not a legal donor, got {:?}",
        offers[0]
    );
    assert_settled(&runner, "G2");
    let copy = the_token(&runner, VOICE_NAME);
    assert!(has_color_choice(&runner, copy), "G2: the color is latched");
    assert_eq!(power_toughness(&runner, copy), (Some(9), Some(9)));
    row_outcome(&runner, answered)
}

/// G3: P0's Parallel Lives doubles Stolen Identity's copy of P1's Quicksilver Gargantuan.
/// Copy #1 copies P1's Margle; copy #2 copies P0's Triskelion. #1 co-enters with #2, so
/// #2's copied counters apply and #1 isn't a legal donor for #2.
#[test]
fn doubled_gargantuan_copy_keeps_its_counters_beside_its_co_entering_margle_twin() {
    let mut scenario = board();
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    add_donor(&mut scenario, P1, MARGLE_DONOR);
    let gargantuan = add_donor(&mut scenario, P1, GARGANTUAN_DONOR);
    add_donor(&mut scenario, P0, TRISKELION_DONOR);
    let spell = free_spell(&mut scenario, "Stolen Identity", false, STOLEN_IDENTITY);
    let mut runner = scenario.build();
    runner.cast(spell).target_object(gargantuan).commit();
    let mut at_second_choice = None;
    let mut copy_choices = 0;
    let answered = drive_carried(
        &mut runner,
        PauseCarry::Live,
        &Script {
            copy_targets: vec![
                CopyPick::card(MARGLE_NAME, P1),
                CopyPick::card(TRISKELION_NAME, P0),
            ],
            ..Script::default()
        },
        |runner, prompt| {
            if let Prompt::CopyTarget { .. } = prompt {
                copy_choices += 1;
                if copy_choices == 2 {
                    let twin = the_token(runner, MARGLE_NAME);
                    assert!(
                        runner.state().objects[&twin].is_phased_in_permanent(),
                        "REACH-GUARD G3: copy #1 is on the battlefield at #2's copy choice"
                    );
                    at_second_choice = Some((
                        twin,
                        parked_created_ids(runner),
                        runner.state().last_created_token_ids.clone(),
                    ));
                }
            }
        },
    );
    assert_eq!(
        prompt_kinds(&answered.prompts),
        vec!["Replacement", "CopyTarget", "Replacement", "CopyTarget"],
        "REACH-GUARD G3: Parallel Lives doubled the Gargantuan copy"
    );
    let (twin, parked, ledger) = at_second_choice.expect("REACH-GUARD G3: a second choice");
    assert!(
        ledger.contains(&twin) && !parked.unwrap_or_default().contains(&twin),
        "REACH-GUARD G3: #1 reaches #2 through its batch (the batch ledger), not the parked owner"
    );
    let offers = copy_offers(&answered.prompts);
    assert!(
        !offers[1].contains(&CopyPick::token(MARGLE_NAME, P0)),
        "G3: copy #1 is not a legal donor for #2, got {:?}",
        offers[1]
    );
    assert_settled(&runner, "G3");
    assert_eq!(power_toughness(&runner, twin), (Some(7), Some(7)), "G3: #1");
    let copy = the_token(&runner, TRISKELION_NAME);
    assert_eq!(plus_counters(&runner, copy), 3, "G3: #2's copied counters");
    assert_eq!(
        power_toughness(&runner, copy),
        (Some(12), Some(12)),
        "G3: #2"
    );
}

/// G4: Clone Legion over P1's [Margle, Spark Double]. The Spark Double copy copies P0's
/// Voice of All; its additional +1/+1 counter (an exception, CR 707.9b) meets P0's
/// Hardened Scales and Branching Evolution, so P0 orders them (CR 616.1), Hardened Scales
/// first: (1 + 1) × 2 = 4 counters. The copied color choice is read on that pause's
/// resume, with the Margle copy co-entering.
#[test]
fn clone_legion_spark_double_copy_chooses_after_a_counter_order_beside_a_co_entering_margle_copy() {
    spark_double_copy_of_voice_beside_margle(PauseCarry::Live);
}

/// G4 saved and restored at the copy offer, the copy choice, the counter order and the
/// color choice.
#[test]
fn clone_legion_spark_double_copy_chooses_after_a_counter_order_beside_a_co_entering_margle_copy_across_save_and_restore(
) {
    assert_restored_row_matches_live(spark_double_copy_of_voice_beside_margle, "G4");
}

#[test]
fn legacy_counter_pause_restores_the_owed_color_choice_without_repeating_counters() {
    let mut paused = None;
    clone_legion_beside(
        MARGLE_NAME,
        &[MARGLE_DONOR, SPARK_DOUBLE_DONOR],
        |scenario| {
            add_donor(scenario, P0, VOICE_DONOR);
            scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
            scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
        },
        |_| {},
        PauseCarry::Live,
        &Script {
            copy_targets: vec![CopyPick::card(VOICE_NAME, P0)],
            first: Some("Hardened Scales"),
            ..Script::default()
        },
        |runner, prompt| {
            if matches!(prompt, Prompt::Replacement)
                && replacement_sources(runner).contains(&"Hardened Scales".to_string())
            {
                assert!(!has_color_choice(runner, the_token(runner, VOICE_NAME)));
                paused = Some(runner.state().clone());
            }
        },
    );
    let state = paused.expect("reached the G4 counter-order pause");
    let copy = the_token(&GameRunner::from_state(state.clone()), VOICE_NAME);
    let mut json = serde_json::to_value(PersistedGameState::capture(state)).unwrap();
    // Reconstruct the old pause: no answer was latched and no resume stage existed.
    fn legacy_shape(value: &mut serde_json::Value) -> (usize, usize) {
        let mut removed = (0, 0);
        match value {
            serde_json::Value::Object(map) => {
                if let Some(action) = map.get_mut("FinishCopyTargetChoiceEntry") {
                    assert_eq!(action["resume_stage"], "ChoiceOwedAfterCounters");
                    action.as_object_mut().unwrap().remove("resume_stage");
                    removed.0 += 1;
                }
                if let Some(attributes) = map.get_mut("chosen_attributes") {
                    let attributes = attributes.as_array_mut().unwrap();
                    removed.1 += attributes.len();
                    attributes.clear();
                }
                for child in map.values_mut() {
                    let count = legacy_shape(child);
                    removed.0 += count.0;
                    removed.1 += count.1;
                }
            }
            serde_json::Value::Array(values) => {
                for child in values {
                    let count = legacy_shape(child);
                    removed.0 += count.0;
                    removed.1 += count.1;
                }
            }
            _ => {}
        }
        removed
    }
    assert_eq!(
        legacy_shape(&mut json),
        (1, 0),
        "only the continuation stage is removed"
    );
    let mut runner = restore_json(&serde_json::to_string(&json).unwrap());
    assert!(!has_color_choice(&runner, copy));
    let answered = drive_carried(
        &mut runner,
        PauseCarry::Live,
        &Script {
            first: Some("Hardened Scales"),
            ..Script::default()
        },
        |runner, prompt| {
            if matches!(prompt, Prompt::Named { .. }) {
                assert_eq!(plus_counters(runner, copy), 4);
            }
        },
    );
    // CR 707.5 + CR 614.12a: the copied entry choice is still mandatory.
    assert_eq!(
        prompt_kinds(&answered.prompts),
        vec!["Replacement", "Named"]
    );
    assert!(has_color_choice(&runner, copy));
    assert_eq!(plus_counters(&runner, copy), 4);
    assert_settled(&runner, "legacy G4");
}

fn spark_double_copy_of_voice_beside_margle(carry: PauseCarry) -> RowOutcome {
    let mut sources_at_replacement = Vec::new();
    let (runner, answered) = clone_legion_beside(
        MARGLE_NAME,
        &[MARGLE_DONOR, SPARK_DOUBLE_DONOR],
        |scenario| {
            add_donor(scenario, P0, VOICE_DONOR);
            scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
            scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
        },
        |_| {},
        carry,
        &Script {
            copy_targets: vec![CopyPick::card(VOICE_NAME, P0)],
            first: Some("Hardened Scales"),
            ..Script::default()
        },
        |runner, prompt| {
            if let Prompt::Replacement = prompt {
                sources_at_replacement.push(replacement_sources(runner));
            }
        },
    );
    assert_eq!(
        prompt_kinds(&answered.prompts),
        vec!["Replacement", "CopyTarget", "Replacement", "Named"],
        "G4: the copy offer, the copy choice, the counter order, then the color choice"
    );
    assert_eq!(
        sources_at_replacement[1],
        vec![
            "Branching Evolution".to_string(),
            "Hardened Scales".to_string()
        ],
        "REACH-GUARD G4: the counter order is between Hardened Scales and Branching Evolution"
    );
    let offers = copy_offers(&answered.prompts);
    assert!(
        !offers[0].contains(&CopyPick::token(MARGLE_NAME, P0)),
        "G4: the co-entering Margle copy is not a legal donor, got {:?}",
        offers[0]
    );
    assert_settled(&runner, "G4");
    let copy = the_token(&runner, VOICE_NAME);
    assert!(has_color_choice(&runner, copy), "G4: the color is latched");
    assert_eq!(plus_counters(&runner, copy), 4, "G4: Hardened Scales first");
    assert_eq!(
        power_toughness(&runner, copy),
        (Some(8), Some(8)),
        "G4: 2/2 + four counters + Margle's +2/+2"
    );
    assert!(
        !runner.state().objects[&copy]
            .card_types
            .supertypes
            .contains(&Supertype::Legendary),
        "G4: it isn't legendary"
    );
    row_outcome(&runner, answered)
}

/// P0's Squad Captain. Its keyword line carries reminder text, so the keyword is named
/// (MTGJSON `keywords`), as the card data names it.
fn add_squad_captain(scenario: &mut GameScenario) {
    scenario
        .add_creature(P0, SQUAD_CAPTAIN_NAME, 2, 2)
        .from_oracle_text_with_keywords(&["Vigilance"], SQUAD_CAPTAIN);
}

/// Puts P0's Minion of the Wastes onto the battlefield by casting it, paying 3 life.
fn cast_minion_paying_three(runner: &mut GameRunner, minion: ObjectId) {
    runner.cast(minion).commit();
    let answered = drive_carried(
        runner,
        PauseCarry::Live,
        &Script {
            pays: vec![3],
            ..Script::default()
        },
        |_, _| {},
    );
    assert_eq!(
        answered.prompts,
        vec![Prompt::PayAmount {
            player: P0,
            min: 0,
            max: 20
        }],
        "REACH-GUARD: the Minion's own payment"
    );
    assert_eq!(chosen_number(runner, minion), Some(3));
    assert_eq!(runner.state().players[0].life, 17, "REACH-GUARD: P0 paid 3");
}

fn chosen_number(runner: &GameRunner, id: ObjectId) -> Option<u32> {
    runner.state().objects[&id]
        .chosen_attributes
        .iter()
        .find_map(|attr| match attr {
            ChosenAttribute::Number(n) => Some(*n),
            _ => None,
        })
}

/// Clone Legion over P1's `donors`, after P0 casts Minion of the Wastes paying 3; the
/// Gargantuan copy copies the Minion and pays 5 for its copied payment.
fn gargantuan_copy_of_minion(
    co_entrant: Option<&str>,
    donors: &[Donor],
    carry: PauseCarry,
    row: &str,
) -> (GameRunner, Answered, ObjectId) {
    let minion = std::cell::Cell::new(None);
    let script = Script {
        copy_targets: vec![CopyPick::card(MINION_NAME, P0)],
        pays: vec![5],
        ..Script::default()
    };
    let extra = |scenario: &mut GameScenario| {
        minion.set(Some(
            scenario
                .add_creature_to_hand_from_oracle(P0, MINION_NAME, 0, 0, MINION_OF_THE_WASTES)
                .with_mana_cost(ManaCost::generic(0))
                .id(),
        ));
    };
    let before = |runner: &mut GameRunner| {
        cast_minion_paying_three(runner, minion.get().expect("the Minion is in hand"));
    };
    let observe = |runner: &GameRunner, prompt: &Prompt| {
        let Prompt::PayAmount { .. } = prompt else {
            return;
        };
        let state = runner.state();
        let WaitingFor::PayAmountChoice { source_id, .. } = &state.waiting_for else {
            unreachable!("a PayAmount prompt");
        };
        assert_eq!(
            state.objects[source_id].name, MINION_NAME,
            "REACH-GUARD {row}: the Gargantuan copy pays"
        );
        assert!(
            state.objects[source_id].is_token,
            "REACH-GUARD {row}: the payer is the token"
        );
        assert!(
            state.last_created_token_ids.contains(source_id),
            "REACH-GUARD {row}: the ledger lists the paying copy"
        );
        let copy_surface = state.pending_replacement.as_ref().is_some_and(|pending| {
            matches!(
                pending.proposed,
                ProposedEvent::ZoneChange {
                    from: Zone::Battlefield,
                    to: Zone::Battlefield,
                    ..
                }
            )
        });
        assert!(
            copy_surface,
            "REACH-GUARD {row}: the payment is parked as a copy surface"
        );
        if co_entrant.is_some() {
            assert!(
                state.objects[source_id].replacement_definitions.is_empty(),
                "REACH-GUARD {row}: the live copy has lost its payment to the Margle copy"
            );
        }
    };
    let (runner, answered) = match co_entrant {
        Some(name) => clone_legion_beside(name, donors, extra, before, carry, &script, observe),
        None => clone_legion_carried(donors, extra, before, carry, &script, observe),
    };
    let minion = minion.get().expect("the Minion is in hand");
    (runner, answered, minion)
}

/// G5: Clone Legion over P1's [Margle, Quicksilver Gargantuan]; the Gargantuan copy copies
/// P0's Minion of the Wastes. Its copied "As this creature enters, pay any amount of life"
/// is offered with the Margle copy co-entering (CR 614.12a + CR 707.5), up to P0's life
/// (CR 119.4), and P0 pays it.
#[test]
fn clone_legion_gargantuan_copy_of_minion_pays_beside_a_co_entering_margle_copy() {
    gargantuan_copy_of_minion_beside_margle(PauseCarry::Live);
}

/// G5 saved and restored at the copy offer, the copy choice and the copied payment: the
/// payment's answer re-reads it on the board it was surfaced on.
#[test]
fn clone_legion_gargantuan_copy_of_minion_pays_beside_a_co_entering_margle_copy_across_save_and_restore(
) {
    assert_restored_row_matches_live(gargantuan_copy_of_minion_beside_margle, "G5");
}

fn gargantuan_copy_of_minion_beside_margle(carry: PauseCarry) -> RowOutcome {
    let (runner, answered, minion) = gargantuan_copy_of_minion(
        Some(MARGLE_NAME),
        &[MARGLE_DONOR, GARGANTUAN_DONOR],
        carry,
        "G5",
    );
    assert_eq!(
        prompt_kinds(&answered.prompts),
        vec!["Replacement", "CopyTarget", "PayAmount"],
        "G5: the copied payment follows the copy choice"
    );
    assert_eq!(
        answered.prompts[2],
        Prompt::PayAmount {
            player: P0,
            min: 0,
            max: 17
        },
        "G5: P0 may pay up to their life total"
    );
    let offers = copy_offers(&answered.prompts);
    assert!(
        !offers[0].contains(&CopyPick::token(MARGLE_NAME, P0)),
        "G5: the co-entering Margle copy is not a legal donor, got {:?}",
        offers[0]
    );
    assert_settled(&runner, "G5");
    assert_eq!(runner.state().players[0].life, 12, "G5: P0 paid 5");
    let copy = the_token(&runner, MINION_NAME);
    assert_eq!(
        chosen_number(&runner, copy),
        Some(5),
        "G5: the copy's latch"
    );
    assert_eq!(
        chosen_number(&runner, minion),
        Some(3),
        "G5: the original's"
    );
    row_outcome(&runner, answered)
}

/// G8: CR 614.12a + CR 119.4: the copied payment happens before simultaneous
/// entry, so a co-entering Emperion cannot prohibit it.
#[test]
fn clone_legion_copied_minion_pays_beside_a_co_entering_emperion() {
    copied_minion_beside_emperion(PauseCarry::Live);
}

/// G8-SR: the same pre-entry prohibition board must survive the payment pause.
#[test]
fn clone_legion_copied_minion_pays_beside_a_co_entering_emperion_across_save_and_restore() {
    assert_restored_row_matches_live(copied_minion_beside_emperion, "G8");
}

fn copied_minion_beside_emperion(carry: PauseCarry) -> RowOutcome {
    let (runner, answered, minion) =
        gargantuan_copy_of_minion(None, &[EMPERION_DONOR, GARGANTUAN_DONOR], carry, "G8");
    assert_eq!(
        prompt_kinds(&answered.prompts),
        vec!["Replacement", "CopyTarget", "PayAmount"],
        "G8: the copied payment follows the copy choice"
    );
    assert_eq!(
        answered.prompts[2],
        Prompt::PayAmount {
            player: P0,
            min: 0,
            max: 17
        },
        "G8: the co-entering prohibition cannot reduce the offer"
    );
    assert_settled(&runner, "G8");
    assert_eq!(
        runner.state().players[0].life,
        12,
        "G8: payment lost five life"
    );
    assert_eq!(
        chosen_number(&runner, the_token(&runner, MINION_NAME)),
        Some(5)
    );
    assert_eq!(chosen_number(&runner, minion), Some(3));
    assert_eq!(
        runner.state().objects[&the_token(&runner, EMPERION_NAME)].controller,
        P0
    );
    row_outcome(&runner, answered)
}

const VILIS_NAME: &str = "Vilis, Broker of Blood";
const VILIS: &str = "Flying\n{B}, Pay 2 life: Target creature gets -1/-1 until end of turn.\nWhenever you lose life, draw that many cards. (Damage causes loss of life.)";
const VILIS_DONOR: Donor = Donor {
    name: VILIS_NAME,
    oracle: VILIS,
    power: 8,
    toughness: 8,
};

/// CR 614.12a + CR 603.2: the payment precedes entry, so its life loss
/// is visible to a resident Vilis but not to a simultaneous Vilis copy.
fn copied_minion_payment_observers(carry: PauseCarry, resident: Option<Donor>) -> RowOutcome {
    let minion = std::cell::Cell::new(None);
    let hand_before_payment = std::cell::Cell::new(None);
    let donors = match resident {
        Some(_) => vec![MARGLE_DONOR, GARGANTUAN_DONOR],
        None => vec![VILIS_DONOR, EMPERION_DONOR, GARGANTUAN_DONOR],
    };
    let (runner, answered) = clone_legion_carried(
        &donors,
        |scenario| {
            if let Some(donor) = resident {
                add_donor(scenario, P0, donor);
            }
            minion.set(Some(
                scenario
                    .add_creature_to_hand_from_oracle(P0, MINION_NAME, 0, 0, MINION_OF_THE_WASTES)
                    .with_mana_cost(ManaCost::generic(0))
                    .id(),
            ));
        },
        |runner| cast_minion_paying_three(runner, minion.get().unwrap()),
        carry,
        &Script {
            copy_targets: vec![CopyPick::card(MINION_NAME, P0)],
            pays: vec![5],
            ..Script::default()
        },
        |runner, prompt| {
            if let Prompt::PayAmount { player, min, max } = prompt {
                assert_eq!((*player, *min, *max), (P0, 0, 17));
                assert_eq!(runner.state().players[0].life, 17);
                hand_before_payment.set(Some(runner.state().players[0].hand.len()));
                let WaitingFor::PayAmountChoice { source_id, .. } = runner.state().waiting_for
                else {
                    unreachable!("the copied payment owns its prompt")
                };
                assert!(runner.state().objects[&source_id].is_token);
                assert_eq!(runner.state().objects[&source_id].name, MINION_NAME);
                if resident.is_some() {
                    assert_eq!(battlefield_tokens(runner, MARGLE_NAME).len(), 1);
                    let vilis = runner
                        .state()
                        .objects
                        .values()
                        .find(|object| {
                            object.name == VILIS_NAME && object.controller == P0 && !object.is_token
                        })
                        .unwrap();
                    assert!(
                        vilis.trigger_definitions.is_empty(),
                        "REACH-GUARD: co-entering Margle has stripped the live resident's trigger"
                    );
                }
            }
        },
    );
    assert_eq!(
        prompt_kinds(&answered.prompts),
        vec!["Replacement", "CopyTarget", "PayAmount"]
    );
    assert_eq!(runner.state().players[0].life, 12);
    assert_eq!(
        chosen_number(&runner, the_token(&runner, MINION_NAME)),
        Some(5)
    );
    assert_eq!(chosen_number(&runner, minion.get().unwrap()), Some(3));
    let drawn = match resident {
        Some(_) => 5,
        None => 0,
    };
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before_payment.get().unwrap() + drawn,
        "only a Vilis present before the group enters may draw for the payment"
    );
    assert_settled(&runner, "G8 payment observers");
    assert!(runner.state().deferred_triggers.is_empty());
    match resident {
        Some(_) => {
            let vilis = runner
                .state()
                .objects
                .values()
                .find(|object| {
                    object.name == VILIS_NAME && object.controller == P0 && !object.is_token
                })
                .expect("the resident Vilis is still present");
            assert!(answered.stack_trigger_sources.contains(&vilis.id));
        }
        None => assert!(
            !answered
                .stack_trigger_sources
                .contains(&the_token(&runner, VILIS_NAME)),
            "the co-entering Vilis must not put a payment trigger on the stack"
        ),
    }
    row_outcome(&runner, answered)
}

#[test]
fn clone_legion_copied_minion_payment_excludes_co_entering_vilis() {
    copied_minion_payment_observers(PauseCarry::Live, None);
}

#[test]
fn clone_legion_copied_minion_payment_excludes_co_entering_vilis_across_save_and_restore() {
    assert_restored_row_matches_live(
        |carry| copied_minion_payment_observers(carry, None),
        "G8 Vilis payment observers",
    );
}

#[test]
fn clone_legion_copied_minion_payment_is_observed_by_resident_vilis_across_save_and_restore() {
    assert_restored_row_matches_live(
        |carry| copied_minion_payment_observers(carry, Some(VILIS_DONOR)),
        "G8 resident Vilis beside co-entering Margle",
    );
}

#[test]
fn clone_legion_copied_minion_payment_is_observed_by_resident_vilis() {
    copied_minion_payment_observers(PauseCarry::Live, Some(VILIS_DONOR));
}

/// C8: CR 614.12a + CR 119.8 + CR 119.4b: a resident Emperion prohibits
/// positive payment. The determined-zero surface latches zero without a prompt.
#[test]
fn clone_legion_copied_minion_can_only_pay_zero_beside_a_resident_emperion() {
    let minion = std::cell::Cell::new(None);
    let emperion = std::cell::Cell::new(None);
    let script = Script {
        copy_targets: vec![CopyPick::card(MINION_NAME, P0)],
        ..Script::default()
    };
    let (runner, answered) = clone_legion_carried(
        &[GARGANTUAN_DONOR],
        |scenario| {
            minion.set(Some(
                scenario
                    .add_creature_to_hand_from_oracle(P0, MINION_NAME, 0, 0, MINION_OF_THE_WASTES)
                    .with_mana_cost(ManaCost::generic(0))
                    .id(),
            ));
            emperion.set(Some(
                scenario
                    .add_creature_to_hand_from_oracle(P0, EMPERION_NAME, 8, 8, PLATINUM_EMPERION)
                    .with_mana_cost(ManaCost::generic(0))
                    .id(),
            ));
        },
        |runner| {
            cast_minion_paying_three(runner, minion.get().unwrap());
            runner.cast(emperion.get().unwrap()).resolve();
            assert_settled(runner, "C8: resident Emperion resolved");
            assert_eq!(
                runner.state().objects[&emperion.get().unwrap()].zone,
                Zone::Battlefield
            );
            assert_eq!(runner.state().players[0].life, 17);
        },
        PauseCarry::Live,
        &script,
        |_, prompt| {
            assert!(
                !matches!(prompt, Prompt::PayAmount { .. }),
                "C8: only zero is payable, so no payment prompt is raised"
            )
        },
    );
    assert_eq!(
        prompt_kinds(&answered.prompts),
        vec!["Replacement", "CopyTarget"]
    );
    assert_settled(&runner, "C8");
    assert_eq!(runner.state().players[0].life, 17);
    assert_eq!(
        chosen_number(&runner, the_token(&runner, MINION_NAME)),
        Some(0)
    );
    assert_eq!(chosen_number(&runner, minion.get().unwrap()), Some(3));
}

/// G6: Clone Legion over P1's [Grizzly Bears, Quicksilver Gargantuan]; the Gargantuan copy
/// copies P0's Squad Captain. The Bears copy co-enters, so it isn't counted (Squad Captain
/// ruling 2019-07-12: "It counts only creatures that are already on the battlefield") and
/// isn't a legal donor.
#[test]
fn clone_legion_gargantuan_copy_of_squad_captain_does_not_count_a_co_entering_bears_copy() {
    let (runner, answered) = clone_legion_beside(
        BEARS_NAME,
        &[BEARS_DONOR, GARGANTUAN_DONOR],
        add_squad_captain,
        |_| {},
        PauseCarry::Live,
        &Script {
            copy_targets: vec![CopyPick::card(SQUAD_CAPTAIN_NAME, P0)],
            ..Script::default()
        },
        |_, _| {},
    );
    assert_eq!(
        prompt_kinds(&answered.prompts),
        vec!["Replacement", "CopyTarget"]
    );
    let offers = copy_offers(&answered.prompts);
    assert!(
        !offers[0].contains(&CopyPick::token(BEARS_NAME, P0)),
        "G6: the co-entering Bears copy is not a legal donor, got {:?}",
        offers[0]
    );
    assert_settled(&runner, "G6");
    let p0_creatures = runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield
                && obj.controller == P0
                && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .count();
    assert_eq!(
        p0_creatures, 3,
        "REACH-GUARD G6: Squad Captain, the Bears copy and the Gargantuan copy"
    );
    let copy = the_token(&runner, SQUAD_CAPTAIN_NAME);
    assert_eq!(
        plus_counters(&runner, copy),
        1,
        "G6: only Squad Captain is counted"
    );
    assert_eq!(power_toughness(&runner, copy), (Some(8), Some(8)));
    assert!(runner.state().objects[&copy].has_keyword(&Keyword::Vigilance));
}

/// G7: P0's Parallel Lives and P1's Halving Season. Clone Legion over P1's [Margle, Voice of
/// All]: P0 chooses Parallel Lives first (CR 616.1e), so doubling then halving
/// preserves the copies. These assertions also hold when the engine delivers the
/// simultaneous creation as one group; held-owner custody is tested separately.
#[test]
fn clone_legion_ordered_single_batch_keeps_the_earlier_margle_copy_as_co_entrant() {
    ordered_single_batch_beside_margle(PauseCarry::Live);
}

/// G7 saved and restored at the order and color choices.
#[test]
fn clone_legion_ordered_single_batch_keeps_the_earlier_margle_copy_as_co_entrant_across_save_and_restore(
) {
    assert_eq!(
        ordered_single_batch_beside_margle(PauseCarry::SavedAndRestored),
        ordered_single_batch_beside_margle(PauseCarry::Live),
        "G7: restored and live order/choice answers settle identically"
    );
}

fn ordered_single_batch_beside_margle(carry: PauseCarry) -> RowOutcome {
    let mut at_replacement = Vec::new();
    let (runner, answered) = clone_legion_carried(
        &[MARGLE_DONOR, VOICE_DONOR],
        |scenario| {
            scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
            scenario.add_enchantment_from_oracle(P1, "Halving Season", HALVING_SEASON);
        },
        |_| {},
        carry,
        &Script {
            first: Some("Parallel Lives"),
            ..Script::default()
        },
        |runner, prompt| match prompt {
            Prompt::Replacement => at_replacement.push(replacement_sources(runner)),
            Prompt::Named { .. } | Prompt::CopyTarget { .. } | Prompt::PayAmount { .. } => {}
        },
    );
    assert_eq!(answered.prompts.first(), Some(&Prompt::Replacement));
    assert!(
        !at_replacement.is_empty(),
        "G7: replacement order is offered"
    );
    let ordered = vec!["Halving Season".to_string(), "Parallel Lives".to_string()];
    assert!(
        at_replacement.iter().all(|sources| *sources == ordered),
        "REACH-GUARD G7: each order is between Parallel Lives and Halving Season, got \
         {at_replacement:?}"
    );
    assert_eq!(
        named_prompts(&answered.prompts),
        vec![P0],
        "G7: the Voice copy's choice"
    );
    assert_settled(&runner, "G7");
    let margle = the_token(&runner, MARGLE_NAME);
    assert_eq!(power_toughness(&runner, margle), (Some(5), Some(2)));
    let voice = the_token(&runner, VOICE_NAME);
    assert!(has_color_choice(&runner, voice), "G7: the color is latched");
    assert_eq!(power_toughness(&runner, voice), (Some(4), Some(4)));
    row_outcome(&runner, answered)
}

/// C1: Clone Legion over P1's [Quicksilver Gargantuan] alone: the copy of P0's Triskelion
/// gets its counters (10/10), and the copy of P0's Voice of All its choice (7/7).
#[test]
fn clone_legion_lone_gargantuan_copy_keeps_its_copied_entry_abilities() {
    for (donor, name) in [
        (TRISKELION_DONOR, TRISKELION_NAME),
        (VOICE_DONOR, VOICE_NAME),
    ] {
        let row = format!("C1 {name}");
        let (runner, answered) = clone_legion_carried(
            &[GARGANTUAN_DONOR],
            |scenario| {
                add_donor(scenario, P0, donor);
            },
            |_| {},
            PauseCarry::Live,
            &Script {
                copy_targets: vec![CopyPick::card(name, P0)],
                ..Script::default()
            },
            |_, _| {},
        );
        assert_settled(&runner, &row);
        let copy = the_token(&runner, name);
        assert_eq!(
            runner
                .state()
                .objects
                .values()
                .filter(|obj| obj.is_token)
                .count(),
            1,
            "REACH-GUARD {row}: one token"
        );
        if name == TRISKELION_NAME {
            assert_eq!(
                prompt_kinds(&answered.prompts),
                vec!["Replacement", "CopyTarget"]
            );
            assert_eq!(plus_counters(&runner, copy), 3, "{row}");
            assert_eq!(
                power_toughness(&runner, copy),
                (Some(10), Some(10)),
                "{row}"
            );
        } else {
            assert_eq!(
                prompt_kinds(&answered.prompts),
                vec!["Replacement", "CopyTarget", "Named"]
            );
            assert!(has_color_choice(&runner, copy), "{row}");
            assert_eq!(power_toughness(&runner, copy), (Some(7), Some(7)), "{row}");
        }
    }
}

/// C2: Clone Legion over P1's [Quicksilver Gargantuan, Margle]: the Gargantuan batch comes
/// first, so no Margle copy exists at its copy choice; it ends as G1 does.
#[test]
fn clone_legion_gargantuan_copy_before_the_margle_batch_keeps_its_counters() {
    let mut margles_at_choice = Vec::new();
    let (runner, answered) = clone_legion_carried(
        &[GARGANTUAN_DONOR, MARGLE_DONOR],
        |scenario| {
            add_donor(scenario, P0, TRISKELION_DONOR);
        },
        |_| {},
        PauseCarry::Live,
        &Script {
            copy_targets: vec![CopyPick::card(TRISKELION_NAME, P0)],
            ..Script::default()
        },
        |runner, prompt| {
            if let Prompt::CopyTarget { .. } = prompt {
                margles_at_choice.push(battlefield_tokens(runner, MARGLE_NAME).len());
            }
        },
    );
    assert_eq!(
        margles_at_choice,
        vec![0],
        "REACH-GUARD C2: no Margle copy yet"
    );
    assert_eq!(
        prompt_kinds(&answered.prompts),
        vec!["Replacement", "CopyTarget"]
    );
    assert_settled(&runner, "C2");
    let copy = the_token(&runner, TRISKELION_NAME);
    assert_eq!(plus_counters(&runner, copy), 3);
    assert_eq!(power_toughness(&runner, copy), (Some(12), Some(12)));
}

/// C4: P0's Stolen Identity copies P0's Triskelion (token `t`), then P0 casts Clone, which
/// copies `t`. The batch ledger still lists `t` at Clone's choice, but a card isn't in it,
/// so `t` stays a legal donor and Clone gets the copied counters.
#[test]
fn clone_copying_an_earlier_token_keeps_it_as_a_donor_over_a_stale_ledger() {
    let mut scenario = board();
    let trisk = add_donor(&mut scenario, P0, TRISKELION_DONOR);
    let stolen = free_spell(&mut scenario, "Stolen Identity", false, STOLEN_IDENTITY);
    let clone = scenario
        .add_creature_to_hand_from_oracle(P0, "Clone", 0, 0, CLONE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(stolen).target_object(trisk).commit();
    drive(&mut runner, |_, _| {});
    let token = the_token(&runner, TRISKELION_NAME);
    runner.cast(clone).commit();
    let mut ledger_at_choice = Vec::new();
    let answered = drive_carried(
        &mut runner,
        PauseCarry::Live,
        &Script {
            copy_targets: vec![CopyPick::token(TRISKELION_NAME, P0)],
            ..Script::default()
        },
        |runner, prompt| {
            if let Prompt::CopyTarget { .. } = prompt {
                ledger_at_choice.push(runner.state().last_created_token_ids.clone());
            }
        },
    );
    assert_eq!(
        ledger_at_choice,
        vec![vec![token]],
        "REACH-GUARD C4: the stale ledger lists the earlier token"
    );
    let offers = copy_offers(&answered.prompts);
    assert!(
        offers[0].contains(&CopyPick::token(TRISKELION_NAME, P0)),
        "C4: the earlier token is a legal donor, got {:?}",
        offers[0]
    );
    assert_settled(&runner, "C4");
    assert_eq!(runner.state().objects[&clone].name, TRISKELION_NAME);
    assert_eq!(plus_counters(&runner, clone), 3);
    assert_eq!(power_toughness(&runner, clone), (Some(4), Some(4)));
}

/// C5: under Humility, Clone Legion's copy of P1's Quicksilver Gargantuan has no copy
/// ability as it would exist on the battlefield (CR 614.12), so nothing is offered: it is a
/// 1/1 Gargantuan.
#[test]
fn clone_legion_gargantuan_copy_under_humility_is_not_offered_its_copy() {
    let (runner, answered) = clone_legion_carried(
        &[GARGANTUAN_DONOR],
        |scenario| {
            add_remover(scenario, Some(Remover::Humility));
        },
        |_| {},
        PauseCarry::Live,
        &Script::default(),
        |_, _| {},
    );
    assert!(answered.prompts.is_empty(), "C5: no copy offer");
    assert_settled(&runner, "C5");
    let copy = the_token(&runner, GARGANTUAN_NAME);
    assert_eq!(plus_counters(&runner, copy), 0);
    assert_eq!(power_toughness(&runner, copy), (Some(1), Some(1)));
}

/// C6: as G5 with no co-entrant: Clone Legion over P1's [Quicksilver Gargantuan] alone.
#[test]
fn clone_legion_lone_gargantuan_copy_of_minion_pays() {
    let (runner, answered, minion) =
        gargantuan_copy_of_minion(None, &[GARGANTUAN_DONOR], PauseCarry::Live, "C6");
    assert_eq!(
        answered.prompts.last(),
        Some(&Prompt::PayAmount {
            player: P0,
            min: 0,
            max: 17
        }),
        "C6"
    );
    assert_settled(&runner, "C6");
    assert_eq!(runner.state().players[0].life, 12);
    let copy = the_token(&runner, MINION_NAME);
    assert_eq!(chosen_number(&runner, copy), Some(5));
    assert_eq!(chosen_number(&runner, minion), Some(3));
}

/// C7: as G6 with no co-entrant: Clone Legion over P1's [Quicksilver Gargantuan] alone.
#[test]
fn clone_legion_lone_gargantuan_copy_of_squad_captain_counts_the_board() {
    let (runner, answered) = clone_legion_carried(
        &[GARGANTUAN_DONOR],
        add_squad_captain,
        |_| {},
        PauseCarry::Live,
        &Script {
            copy_targets: vec![CopyPick::card(SQUAD_CAPTAIN_NAME, P0)],
            ..Script::default()
        },
        |_, _| {},
    );
    assert_eq!(
        prompt_kinds(&answered.prompts),
        vec!["Replacement", "CopyTarget"]
    );
    assert_settled(&runner, "C7");
    let copy = the_token(&runner, SQUAD_CAPTAIN_NAME);
    assert_eq!(plus_counters(&runner, copy), 1);
    assert_eq!(power_toughness(&runner, copy), (Some(8), Some(8)));
    assert!(runner.state().objects[&copy].has_keyword(&Keyword::Vigilance));
}

/// D8: CR 614.12a + CR 119.8 + CR 603.2: direct token payments use the
/// pre-entry population for both prohibitions and life-loss observers.
fn direct_minion_payment(
    carry: PauseCarry,
    co: Option<Donor>,
    resident: Option<Donor>,
) -> RowOutcome {
    let minion = std::cell::Cell::new(None);
    let resident_id = std::cell::Cell::new(None);
    let hand_before = std::cell::Cell::new(None);
    let donors: Vec<Donor> = if co.is_none() {
        vec![VOICE_DONOR, VOICE_DONOR]
    } else {
        co.into_iter().collect()
    };
    let (runner, answered) = clone_legion_carried(
        &donors,
        |scenario| {
            if resident.is_some_and(|donor| donor.name == EMPERION_NAME) {
                // CR 613.1g: keep the determined-zero token alive to inspect its latch.
                scenario.add_enchantment_from_oracle(
                    P0,
                    "Glorious Anthem",
                    "Creatures you control get +1/+1.",
                );
            }

            minion.set(Some(
                scenario
                    .add_creature_to_hand_from_oracle(P0, MINION_NAME, 0, 0, MINION_OF_THE_WASTES)
                    .with_mana_cost(ManaCost::generic(0))
                    .id(),
            ));
            if let Some(donor) = resident {
                resident_id.set(Some(
                    scenario
                        .add_creature_to_hand_from_oracle(
                            P0,
                            donor.name,
                            donor.power,
                            donor.toughness,
                            donor.oracle,
                        )
                        .with_mana_cost(ManaCost::generic(0))
                        .id(),
                ));
            }
        },
        |runner| {
            cast_minion_paying_three(runner, minion.get().unwrap());
            // Fixture control assignment leaves the donor's production-paid latch intact.
            // It is delivered after the earlier donor, so its co-entrant is live at payment.
            let donor = runner
                .state_mut()
                .objects
                .get_mut(&minion.get().unwrap())
                .unwrap();
            donor.controller = P1;
            donor.base_controller = Some(P1);
            engine::game::layers::mark_layers_full(runner.state_mut());
            engine::game::layers::flush_layers(runner.state_mut());
            if let Some(id) = resident_id.get() {
                runner.cast(id).resolve();
                assert_settled(runner, "D8 resident resolved");
                assert_eq!(
                    runner.state().objects[&id].zone,
                    Zone::Battlefield,
                    "REACH-GUARD D8: resident is live before Clone Legion"
                );
            }
        },
        carry,
        &Script {
            pays: vec![5],
            ..Script::default()
        },
        |runner, prompt| {
            if let Prompt::PayAmount { player, min, max } = prompt {
                assert_eq!((*player, *min, *max), (P0, 0, 17));
                assert!(
                    matches!(
                        runner
                            .state()
                            .pending_replacement
                            .as_ref()
                            .unwrap()
                            .proposed,
                        ProposedEvent::TokenEntry { .. }
                    ),
                    "REACH-GUARD D8: direct TokenEntry park"
                );
                if let Some(donor) = co {
                    let token = the_token(runner, donor.name);
                    assert_eq!(
                        runner.state().objects[&token].zone,
                        Zone::Battlefield,
                        "REACH-GUARD D8: co-entrant delivered before Minion payment"
                    );
                }
                hand_before.set(Some(runner.state().players[0].hand.len()));
            }
        },
    );
    assert_settled(&runner, "D8");
    let locked = resident.is_some_and(|d| d.name == EMPERION_NAME);
    if locked {
        assert!(
            !answered
                .prompts
                .iter()
                .any(|prompt| matches!(prompt, Prompt::PayAmount { .. })),
            "D8 resident Emperion: determined zero"
        );
        assert_eq!(runner.state().players[0].life, 17);
        assert_eq!(
            chosen_number(&runner, the_token(&runner, MINION_NAME)),
            Some(0)
        );
    } else {
        assert_eq!(prompt_kinds(&answered.prompts), vec!["PayAmount"]);
        assert_eq!(runner.state().players[0].life, 12);
        assert_eq!(
            chosen_number(&runner, the_token(&runner, MINION_NAME)),
            Some(5)
        );
        let drawn = if resident.is_some() { 5 } else { 0 };
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand_before.get().unwrap() + drawn,
            "D8: only resident Vilis observes pre-entry payment"
        );
        if let Some(id) = resident_id.get() {
            assert!(answered.stack_trigger_sources.contains(&id));
        } else if co.is_some_and(|d| d.name == VILIS_NAME) {
            assert!(!answered
                .stack_trigger_sources
                .contains(&the_token(&runner, VILIS_NAME)));
        }
    }
    assert_eq!(chosen_number(&runner, minion.get().unwrap()), Some(3));
    assert!(runner.state().deferred_triggers.is_empty());
    row_outcome(&runner, answered)
}

#[test]
fn d8_direct_minion_co_entering_emperion() {
    assert_restored_row_matches_live(
        |carry| direct_minion_payment(carry, Some(EMPERION_DONOR), None),
        "D8 Emperion",
    );
}

#[test]
fn d8_direct_minion_co_entering_vilis() {
    assert_restored_row_matches_live(
        |carry| direct_minion_payment(carry, Some(VILIS_DONOR), None),
        "D8 Vilis",
    );
}

#[test]
fn d8_direct_minion_resident_vilis() {
    assert_restored_row_matches_live(
        |carry| direct_minion_payment(carry, Some(VILIS_DONOR), Some(VILIS_DONOR)),
        "D8 resident Vilis",
    );
}

#[test]
fn d8_direct_minion_resident_emperion() {
    assert_restored_row_matches_live(
        |carry| direct_minion_payment(carry, None, Some(EMPERION_DONOR)),
        "D8 resident Emperion",
    );
}

/// CR 603.6a + CR 603.2: simultaneous newcomers observe each other's entries,
/// even when the last token parks for an as-enters payment.
fn minion_payment_soul_warden(carry: PauseCarry, direct: bool) -> RowOutcome {
    let minion = std::cell::Cell::new(None);
    let warden = Donor {
        name: "Soul Warden",
        oracle: "Whenever another creature enters, you gain 1 life.",
        power: 1,
        toughness: 1,
    };
    let donors = if direct {
        vec![warden, BEARS_DONOR]
    } else {
        vec![warden, BEARS_DONOR, GARGANTUAN_DONOR]
    };
    let (runner, answered) = clone_legion_carried(
        &donors,
        |scenario| {
            minion.set(Some(
                scenario
                    .add_creature_to_hand_from_oracle(P0, MINION_NAME, 0, 0, MINION_OF_THE_WASTES)
                    .with_mana_cost(ManaCost::generic(0))
                    .id(),
            ));
        },
        |runner| {
            cast_minion_paying_three(runner, minion.get().unwrap());
            if direct {
                let donor = runner
                    .state_mut()
                    .objects
                    .get_mut(&minion.get().unwrap())
                    .unwrap();
                donor.controller = P1;
                donor.base_controller = Some(P1);
            }
            engine::game::layers::mark_layers_full(runner.state_mut());
            engine::game::layers::flush_layers(runner.state_mut());
        },
        carry,
        &Script {
            pays: vec![5],
            copy_targets: if direct {
                vec![]
            } else {
                vec![CopyPick::card(MINION_NAME, P0)]
            },
            ..Script::default()
        },
        |runner, prompt| {
            if let Prompt::PayAmount { player, min, max } = prompt {
                assert_eq!((*player, *min, *max), (P0, 0, 17));
                assert_eq!(runner.state().players[0].life, 17);
                let proposed = &runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .proposed;
                if direct {
                    assert!(
                        matches!(proposed, ProposedEvent::TokenEntry { .. }),
                        "REACH-GUARD D8 Warden: direct TokenEntry park"
                    );
                } else {
                    assert!(
                        matches!(
                            proposed,
                            ProposedEvent::ZoneChange {
                                from: Zone::Battlefield,
                                to: Zone::Battlefield,
                                ..
                            }
                        ),
                        "REACH-GUARD G8 Warden: synthetic copy-surface park"
                    );
                }
                for name in ["Soul Warden", BEARS_NAME] {
                    let token = the_token(runner, name);
                    assert_eq!(
                        runner.state().objects[&token].zone,
                        Zone::Battlefield,
                        "REACH-GUARD D8 Warden: {name} token live before Minion payment"
                    );
                }
            }
        },
    );
    assert_eq!(
        prompt_kinds(&answered.prompts),
        if direct {
            vec!["PayAmount"]
        } else {
            vec!["Replacement", "CopyTarget", "PayAmount"]
        }
    );
    assert_settled(&runner, "Soul Warden simultaneous entries");
    assert_eq!(
        chosen_number(&runner, the_token(&runner, MINION_NAME)),
        Some(5)
    );
    assert_eq!(
        runner.state().players[0].life,
        14,
        "D8 Soul Warden: 20 - 3 - 5 + two simultaneous-entry triggers"
    );
    row_outcome(&runner, answered)
}

#[test]
fn d8_direct_minion_soul_warden_observes_simultaneous_entries() {
    minion_payment_soul_warden(PauseCarry::Live, true);
}

#[test]
fn d8_direct_minion_soul_warden_observes_simultaneous_entries_across_save_and_restore() {
    assert_restored_row_matches_live(
        |carry| minion_payment_soul_warden(carry, true),
        "D8 Soul Warden",
    );
}

#[test]
fn g8_copied_minion_soul_warden_observes_simultaneous_entries() {
    minion_payment_soul_warden(PauseCarry::Live, false);
}

#[test]
fn g8_copied_minion_soul_warden_observes_simultaneous_entries_across_save_and_restore() {
    assert_restored_row_matches_live(
        |carry| minion_payment_soul_warden(carry, false),
        "G8 Soul Warden",
    );
}

#[test]
fn clone_legion_gargantuan_copy_of_canker_counts_chosen_opponents_creatures() {
    gargantuan_copy_of_canker(PauseCarry::Live);
}

#[test]
fn clone_legion_gargantuan_copy_of_canker_counts_chosen_opponents_creatures_across_save_and_restore(
) {
    assert_restored_row_matches_live(gargantuan_copy_of_canker, "F2 Canker");
}

fn gargantuan_copy_of_canker(carry: PauseCarry) -> RowOutcome {
    const CANKER_NAME: &str = "Canker Abomination";
    const CANKER: Donor = Donor {
        name: CANKER_NAME,
        oracle: "As this creature enters, choose an opponent. This creature enters with a -1/-1 counter on it for each creature that player controls.",
        power: 6,
        toughness: 6,
    };
    let (runner, answered) = clone_legion_carried(
        &[BEARS_DONOR, BEARS_DONOR, GARGANTUAN_DONOR],
        |scenario| {
            add_donor(scenario, P0, CANKER);
        },
        |_| {},
        carry,
        &Script {
            copy_targets: vec![CopyPick::card(CANKER_NAME, P0)],
            ..Script::default()
        },
        |runner, prompt| {
            let bears = battlefield_tokens(runner, BEARS_NAME);
            assert_eq!(
                bears.len(),
                2,
                "REACH-GUARD F2: both Bears copies precede the pause"
            );
            let parked = parked_created_ids(runner).expect("REACH-GUARD F2: parked copy batch");
            assert!(
                bears.iter().all(|id| parked.contains(id)),
                "REACH-GUARD F2: the batch retains both Bears"
            );
            if matches!(prompt, Prompt::Named { .. }) {
                let WaitingFor::NamedChoice {
                    player,
                    choice_type,
                    options,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!("REACH-GUARD F2: acquired named choice");
                };
                assert_eq!(*player, P0);
                assert!(matches!(
                    choice_type,
                    engine::types::ability::ChoiceType::Opponent { .. }
                ));
                assert_eq!(options, &[P1.0.to_string()]);
                let creatures = |player| {
                    runner
                        .state()
                        .objects
                        .values()
                        .filter(|object| {
                            object.is_phased_in_permanent()
                                && object.controller == player
                                && object.card_types.core_types.contains(&CoreType::Creature)
                        })
                        .count()
                };
                assert_eq!(
                    creatures(P1),
                    3,
                    "REACH-GUARD F2: chosen opponent has three creatures"
                );
                assert_ne!(
                    creatures(P0),
                    creatures(P1),
                    "REACH-GUARD F2: populations differ"
                );
            }
        },
    );
    assert_eq!(
        prompt_kinds(&answered.prompts),
        vec!["Replacement", "CopyTarget", "Named"],
        "REACH-GUARD F2: the Gargantuan acquires Canker's choice"
    );
    assert_settled(&runner, "F2 Canker");
    let copy = &runner.state().objects[&the_token(&runner, CANKER_NAME)];
    assert!(
        copy.chosen_attributes
            .contains(&ChosenAttribute::Player(P1)),
        "REACH-GUARD F2: the chosen opponent is latched"
    );
    // CR 607.2d + CR 614.12a: the linked counter quantity uses the opponent
    // chosen by the acquired as-enters ability, before the copy enters.
    assert_eq!(
        copy.counters
            .get(&CounterType::Minus1Minus1)
            .copied()
            .unwrap_or(0),
        3,
        "F2: copied Canker counts the chosen opponent's three creatures"
    );
    row_outcome(&runner, answered)
}
