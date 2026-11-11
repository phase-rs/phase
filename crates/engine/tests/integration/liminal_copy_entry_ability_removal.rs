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

use std::collections::BTreeMap;

use crate::support::shared_card_db;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ChosenAttribute, Effect};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{PersistedGameState, PersistedRestoreFinalization, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
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

const MARGLE_NAME: &str = "Margle, Cousin of Yargle";
const TRISKELION_NAME: &str = "Triskelion";
const VOICE_NAME: &str = "Voice of All";
const THRONE_NAME: &str = "Roaming Throne";
const GEMSTONE_NAME: &str = "Gemstone Mine";

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
    /// A CR 616.1 replacement-order choice.
    Replacement,
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
}

/// The engine's own save / undo / P2P restore pipeline (`PersistedGameState`'s trusted
/// envelope), with the deferred production reload (as
/// `twinflame_paused_copy_referent.rs::save_and_restore`) — not a bare
/// `serde_json::from_str::<GameState>`.
fn save_and_restore(runner: &GameRunner) -> GameRunner {
    let db = shared_card_db().expect("REACH-GUARD: a restore needs the integration CardDB");
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the paused state serializes");
    let state = serde_json::from_str::<PersistedGameState>(&json)
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
    drive_carried(runner, PauseCarry::Live, observe).prompts
}

/// [`drive`], carrying each prompt per `carry` before it is observed and answered. The
/// final priority pass is never carried.
fn drive_carried(
    runner: &mut GameRunner,
    carry: PauseCarry,
    mut observe: impl FnMut(&GameRunner, &Prompt),
) -> Answered {
    let mut answered = Answered {
        prompts: Vec::new(),
        parked_created_ids: Vec::new(),
    };
    for _ in 0..64 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::NamedChoice { .. } | WaitingFor::ReplacementChoice { .. }
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
            WaitingFor::ReplacementChoice { .. } => (
                GameAction::ChooseReplacement { index: 0 },
                Some(Prompt::Replacement),
            ),
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
            Prompt::Replacement => None,
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
    let (runner, answered) = clone_legion_carried(donors, extra, PauseCarry::Live, observe);
    (runner, answered.prompts)
}

/// [`clone_legion`], carrying each prompt per `carry`.
fn clone_legion_carried(
    donors: &[Donor],
    extra: impl FnOnce(&mut GameScenario),
    carry: PauseCarry,
    observe: impl FnMut(&GameRunner, &Prompt),
) -> (GameRunner, Answered) {
    let mut scenario = board();
    extra(&mut scenario);
    for donor in donors {
        add_donor(&mut scenario, P1, *donor);
    }
    let spell = free_spell(&mut scenario, "Clone Legion", false, CLONE_LEGION);
    let mut runner = scenario.build();
    runner.cast(spell).target_player(P1).commit();
    let answered = drive_carried(&mut runner, carry, observe);
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
        carry,
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
        carry,
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
        carry,
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
    let answered = drive_carried(&mut runner, carry, |runner, prompt| match prompt {
        Prompt::Replacement => replacement_prompts.push((
            runner
                .state()
                .resolution_stack
                .active_copy_token()
                .is_some(),
            battlefield_tokens(runner, MARGLE_NAME).len(),
            battlefield_tokens(runner, "Clue").len(),
        )),
        Prompt::Named { .. } => margle_at_named.push(battlefield_tokens(runner, MARGLE_NAME).len()),
    });
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
