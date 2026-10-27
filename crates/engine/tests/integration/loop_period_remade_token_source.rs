//! CR 732.2a: a loop whose mana source is a token it sacrifices and makes again every cycle
//! (CR 111.7, CR 111.1) is offered at its first recurrence, and its take performs the cycles.
//!
//! Every board is driven through `apply()` in each mode that offers. A reference is the same
//! board with detection off, driven by hand for as many cycles as the live board performed.
//! A row that pins a refusal asks the certifier about frames driven by hand with detection off.

use std::collections::BTreeSet;
use std::path::Path;

use engine::analysis::decision_template::{IterationCount, ShortcutDecisionSchema};
use engine::analysis::loop_check::{LoopCertificate, OfferRoad, ShortcutResponse};
use engine::analysis::resource::{
    history_covers_for_tests, loop_states_equal_modulo_resources, CounterClass, FodderCoverRefusal,
    ObjectClass, ObjectGrowthVerdict, RecurrenceCover, ResourceAxis,
};
use engine::database::card_db::CardDatabase;
use engine::game::engine::{apply, certify_object_growth_frames_for_tests};
use engine::game::mana_abilities::is_mana_ability;
use engine::game::period_confirm::PeriodPerformance;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::{play_trace_view, NamingCause};
use engine::types::ability::{AbilityKind, TargetFilter, TargetRef};
use engine::types::actions::{DebugAction, DebugTokenRequest, GameAction};
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastPaymentMode, GameState, LoopDetectionMode, ManaChoice, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// Bounds a runaway drive; no assertion reads it.
const BEAT_CAP: usize = 400;

const MODES: [LoopDetectionMode; 2] = [LoopDetectionMode::On, LoopDetectionMode::Interactive];

/// The cycles a fixed-count take is declared for.
const TAKEN: u32 = 5;

const POOL: [ManaType; 6] = [
    ManaType::White,
    ManaType::Blue,
    ManaType::Black,
    ManaType::Red,
    ManaType::Green,
    ManaType::Colorless,
];

/// How a drive answers the prompts a board's loop asks.
#[derive(Clone)]
struct Answers {
    color: ManaType,
    /// What every target prompt names, and every source choice and cost `cost` leaves unnamed.
    aim: Option<TargetRef>,
    /// The permanent a sacrifice or tap cost names when it is not `aim`.
    cost: Option<ObjectId>,
    /// The answer to a "you may".
    accept: bool,
}

impl Answers {
    /// The object `aim` names.
    fn aimed(&self) -> Option<ObjectId> {
        match self.aim {
            Some(TargetRef::Object(aimed)) => Some(aimed),
            _ => None,
        }
    }
}

fn act(state: &mut GameState, action: GameAction) {
    let actor = state
        .waiting_for
        .acting_player()
        .unwrap_or_else(|| panic!("nobody acts at {:?}", state.waiting_for));
    let shown = format!("{action:?}");
    if let Err(error) = apply(state, actor, action) {
        panic!("{shown} rejected: {error:?}");
    }
}

/// Answers prompts until a player has priority, an offer is made or the game ends.
fn answer_prompts(state: &mut GameState, answers: &Answers) {
    for _ in 0..BEAT_CAP {
        let action = match &state.waiting_for {
            WaitingFor::Priority { .. }
            | WaitingFor::LoopShortcut { .. }
            | WaitingFor::GameOver { .. } => return,
            WaitingFor::ChooseManaColor { .. } => GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(answers.color),
                count: 1,
            },
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                GameAction::ChooseTarget {
                    target: answers.aim.clone(),
                }
            }
            WaitingFor::PayCost { .. } => match answers.cost.or(answers.aimed()) {
                Some(paid) => GameAction::SelectCards { cards: vec![paid] },
                None => panic!("no object to pay {:?} with", state.waiting_for),
            },
            WaitingFor::DamageSourceChoice { .. } => match answers.aimed() {
                Some(source) => GameAction::ChooseDamageSource { source },
                None => panic!("no source to choose at {:?}", state.waiting_for),
            },
            // The pool holds the payment.
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::OptionalEffectChoice { .. } => GameAction::DecideOptionalEffect {
                accept: answers.accept,
            },
            other => panic!("the drive has no answer to {other:?}"),
        };
        act(state, action);
    }
    panic!("the prompts did not end");
}

/// [`answer_prompts`], with every player passing priority until the stack is empty.
fn settle(state: &mut GameState, answers: &Answers) {
    for _ in 0..BEAT_CAP {
        answer_prompts(state, answers);
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) || state.stack.is_empty() {
            return;
        }
        act(state, GameAction::PassPriority);
    }
    panic!("the stack did not empty");
}

fn activate(state: &mut GameState, source_id: ObjectId, ability_index: usize) {
    act(
        state,
        GameAction::ActivateAbility {
            source_id,
            ability_index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        },
    );
}

/// Activates `source`'s first mana ability (CR 605.1a).
fn tap_for_mana(state: &mut GameState, source: ObjectId) {
    let index = state.objects[&source]
        .abilities
        .iter()
        .position(is_mana_ability)
        .expect("a mana ability");
    activate(state, source, index);
}

/// Activates `source`'s first activated ability that is not a mana ability.
fn activate_ability(state: &mut GameState, source: ObjectId) {
    let index = state.objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated && !is_mana_ability(ability))
        .expect("an activated ability");
    activate(state, source, index);
}

/// P0's untapped tokens named `name`, oldest first.
fn tokens(state: &GameState, name: &str) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &state.objects[id];
            object.is_token && object.name == name && object.controller == P0 && !object.tapped
        })
        .collect();
    ids.sort_unstable();
    ids
}

/// Makes `count` Treasures under `owner` with the debug action a live game makes them with.
pub(crate) fn make_treasures(state: &mut GameState, owner: PlayerId, count: u32) {
    let preset_id = engine::game::token_presets::known_token_presets()
        .iter()
        .find(|preset| preset.body.display_name == "Treasure")
        .expect("the Treasure preset")
        .id
        .clone();
    let debug = std::mem::replace(&mut state.debug_mode, true);
    act(
        state,
        GameAction::Debug(DebugAction::CreateToken {
            request: DebugTokenRequest::Preset {
                preset_id,
                owner,
                power_override: None,
                toughness_override: None,
                enter_with_counters: vec![],
            },
            count,
            run_etb: false,
        }),
    );
    state.debug_mode = debug;
}

fn in_mode(board: &GameState, mode: LoopDetectionMode) -> GameState {
    let mut state = board.clone();
    state.loop_detection = mode;
    state
}

fn is_offer(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::LoopShortcut { .. })
}

/// The standing offer, which must be the recorded period's.
fn offer(state: &GameState) -> (&LoopCertificate, &ShortcutDecisionSchema) {
    let WaitingFor::LoopShortcut {
        certificate,
        schema,
        road,
        ..
    } = &state.waiting_for
    else {
        panic!("no offer at {:?}", state.waiting_for);
    };
    assert_eq!(*road, OfferRoad::RecordedPeriod);
    (certificate, schema)
}

/// Declares the standing offer at `count`, and every seat asked accepts it.
fn take(state: &mut GameState, count: u32) {
    act(
        state,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(count),
            template: None,
        },
    );
    while matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }) {
        act(
            state,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        );
    }
}

/// Whether P0 has priority.
fn has_priority(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
}

/// One pass of priority, then the prompts.
fn pass(state: &mut GameState, answers: &Answers) {
    act(state, GameAction::PassPriority);
    answer_prompts(state, answers);
}

/// Passes priority until `reached`, with no offer made on the way.
fn pass_until(state: &mut GameState, answers: &Answers, reached: impl Fn(&GameState) -> bool) {
    for _ in 0..BEAT_CAP {
        if reached(state) {
            return;
        }
        assert!(!is_offer(state), "an offer at {:?}", state.waiting_for);
        pass(state, answers);
    }
    panic!("the drive did not arrive");
}

/// Passes priority until P0 has it at the empty stack of the next step, which starts with no
/// last-known record.
fn pass_to_the_next_step(state: &mut GameState, answers: &Answers) {
    let began = state.phase;
    pass_until(state, answers, |state| {
        state.phase != began && has_priority(state) && state.stack.is_empty()
    });
    assert!(state.lki_cache.is_empty());
}

/// The top-level fields in which `taken`, read with detection off, and `reference` differ once
/// each is serialized whole.
pub(crate) fn differing_fields(taken: &GameState, reference: &GameState) -> BTreeSet<String> {
    let fields = |state: &GameState| match serde_json::to_value(state) {
        Ok(serde_json::Value::Object(fields)) => fields,
        other => panic!("a state serialized to {other:?}"),
    };
    let (taken, reference) = (
        fields(&in_mode(taken, LoopDetectionMode::Off)),
        fields(reference),
    );
    taken
        .keys()
        .chain(reference.keys())
        .filter(|key| taken.get(*key) != reference.get(*key))
        .cloned()
        .collect()
}

/// CR 732.2a: a take of each of `counts` from the offer leaves, on the whole serialized state,
/// what that many hand-driven `period`s leave from the frame the offer is declined at; returns
/// each count with its taken state and its reference.
fn taken_and_hand_driven(
    offered: &GameState,
    counts: &[u32],
    period: impl Fn(&mut GameState),
    context: &str,
) -> Vec<(u32, GameState, GameState)> {
    let mut declined = offered.clone();
    act(&mut declined, GameAction::DeclineShortcut);
    counts
        .iter()
        .map(|count| {
            let mut taken = offered.clone();
            take(&mut taken, *count);
            let mut reference = in_mode(&declined, LoopDetectionMode::Off);
            for _ in 0..*count {
                period(&mut reference);
            }
            let differing = differing_fields(&taken, &reference);
            assert!(
                differing.is_empty(),
                "{context}, {count} cycles: {differing:?}"
            );
            (*count, taken, reference)
        })
        .collect()
}

/// One play of a period, with its prompts answered and its stack resolved.
type Step<'a> = &'a dyn Fn(&mut GameState);

/// Plays `steps` in order: no offer stands before the last, and the one after it stands at the
/// empty stack.
fn offered_after_one_period(state: &mut GameState, steps: &[Step], context: &str) {
    let (last, earlier) = steps.split_last().expect("a step");
    for step in earlier {
        step(state);
        assert!(!is_offer(state), "{context}");
    }
    last(state);
    assert!(is_offer(state), "{context}: {:?}", state.waiting_for);
    assert!(state.stack.is_empty(), "{context}");
}

/// The certifier's verdict on three frames, `casts` being the spells their periods cast.
fn certified(frames: &[GameState], casts: &[ObjectId]) -> ObjectGrowthVerdict {
    let [first, second, third] = frames else {
        panic!("three frames, not {}", frames.len());
    };
    certify_object_growth_frames_for_tests([first, second, third], casts, P0)
}

/// A floating replacement or prevention effect as a row reads it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Floating {
    used_up: bool,
    /// It holds an ability to execute.
    executes: bool,
    /// It holds a filter on the source of damage.
    filters_source: bool,
}

/// Noble Vestige's shield, not yet used up.
const SHIELD: Floating = Floating {
    used_up: false,
    executes: false,
    filters_source: false,
};

/// Circle of Protection: Red's effect on its chosen source, not yet used up.
const CHOSEN_SOURCE: Floating = Floating {
    filters_source: true,
    ..SHIELD
};

/// Mystic Reflection's effect on its chosen creature, not yet used up.
const COPY: Floating = Floating {
    executes: true,
    ..SHIELD
};

/// The floating effects `state` holds, oldest first.
fn floating(state: &GameState) -> Vec<Floating> {
    state
        .pending_damage_replacements
        .iter()
        .map(|effect| Floating {
            used_up: effect.is_consumed,
            executes: effect.execute.is_some(),
            filters_source: effect.damage_source_filter.is_some(),
        })
        .collect()
}

/// What a loop moves: every seat's life and standing, P0's pool by mana type and token count,
/// and the damage and +1/+1 counters on one permanent.
#[derive(Debug, PartialEq)]
struct Reading {
    life: Vec<i32>,
    eliminated: Vec<bool>,
    mana: Vec<usize>,
    tokens: usize,
    damage: u32,
    counters: u32,
}

fn reading(state: &GameState, tracked: ObjectId) -> Reading {
    let object = &state.objects[&tracked];
    Reading {
        life: state.players.iter().map(|player| player.life).collect(),
        eliminated: state.players.iter().map(|p| p.is_eliminated).collect(),
        mana: POOL
            .iter()
            .map(|color| state.players[0].mana_pool.count_color(*color))
            .collect(),
        tokens: state
            .battlefield
            .iter()
            .filter(|id| state.objects[*id].is_token && state.objects[*id].controller == P0)
            .count(),
        damage: object.damage_marked,
        counters: object
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
    }
}

fn mana_of(color: ManaType, amount: usize) -> Vec<usize> {
    POOL.iter()
        .map(|pooled| if *pooled == color { amount } else { 0 })
        .collect()
}

/// The opponents of P0 still in the game.
fn living_opponents(state: &GameState) -> Vec<PlayerId> {
    state
        .players
        .iter()
        .filter(|player| player.id != P0 && !player.is_eliminated)
        .map(|player| player.id)
        .collect()
}

/// CR 704.5a: the cycles until the last living opponent is at 0 or less life, each losing
/// `per_cycle` life a cycle.
fn cycles_to_the_last_loss(state: &GameState, per_cycle: i32) -> u32 {
    living_opponents(state)
        .into_iter()
        .map(|seat| (state.players[seat.0 as usize].life + per_cycle - 1) / per_cycle)
        .max()
        .expect("a living opponent") as u32
}

/// Four players at 40 life in P0's precombat main, with Smaug the Impenetrable ("Whenever Smaug
/// is dealt noncombat damage, create that many Treasure tokens.") and Mayhem Devil ("Whenever a
/// player sacrifices a permanent, this creature deals 1 damage to any target.") under P0.
fn smaug_scenario(db: &CardDatabase) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in 0..4 {
        scenario.with_life(PlayerId(seat), 40);
    }
    let smaug = scenario.add_real_card(P0, "Smaug the Impenetrable", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Mayhem Devil", Zone::Battlefield, db);
    (scenario, smaug)
}

/// The built scenario with `treasures` Treasures under P0.
fn built(scenario: GameScenario, treasures: u32) -> GameState {
    let mut state = scenario.build().state().clone();
    make_treasures(&mut state, P0, treasures);
    state
}

/// One cycle of a Treasure engine: P0's oldest Treasure sacrificed for mana, then the stack.
fn treasure_cycle(state: &mut GameState, answers: &Answers) {
    let treasure = tokens(state, "Treasure")[0];
    tap_for_mana(state, treasure);
    settle(state, answers);
}

/// CR 111.7 + CR 702.12b: the sacrificed Treasure ceases to exist, the Devil's damage on
/// indestructible Smaug makes another, and each cycle nets one mana.
#[test]
fn a_treasure_remade_by_damage_on_smaug_is_offered_and_taken() {
    let db = shared_card_db().expect("the card fixture");
    let (scenario, smaug) = smaug_scenario(db);
    let board = built(scenario, 1);
    let answers = Answers {
        color: ManaType::Red,
        aim: Some(TargetRef::Object(smaug)),
        cost: None,
        accept: true,
    };
    let mut reference = in_mode(&board, LoopDetectionMode::Off);
    for _ in 0..=TAKEN {
        treasure_cycle(&mut reference, &answers);
    }
    assert_eq!(
        reading(&reference, smaug),
        Reading {
            life: vec![40; 4],
            eliminated: vec![false; 4],
            mana: mana_of(ManaType::Red, 6),
            tokens: 1,
            damage: 6,
            counters: 0,
        }
    );
    for mode in MODES {
        let mut live = in_mode(&board, mode);
        treasure_cycle(&mut live, &answers);
        let (certificate, _) = offer(&live);
        assert_eq!(
            certificate.unbounded,
            [ResourceAxis::Mana(ManaType::Red)],
            "{mode:?}"
        );
        // CR 603.3: the Treasure the next repetition sacrifices is made by a resolving trigger.
        let WaitingFor::LoopShortcut { period, .. } = &live.waiting_for else {
            unreachable!("offer")
        };
        assert_eq!(
            period.performance(),
            Some(PeriodPerformance::UsesTheStack),
            "{mode:?}"
        );
        take(&mut live, TAKEN);
        assert_eq!(
            reading(&live, smaug),
            reading(&reference, smaug),
            "{mode:?}"
        );
    }
}

/// A land tapped through its mana ability earlier in the step is a consumed play, and the
/// Treasure made again is still offered at its first recurrence, as a play restored.
#[test]
fn a_land_tapped_earlier_in_the_step_does_not_delay_the_offer() {
    let db = shared_card_db().expect("the card fixture");
    let (mut scenario, smaug) = smaug_scenario(db);
    let land = scenario.add_basic_land(P0, ManaColor::Red);
    let board = built(scenario, 1);
    let answers = Answers {
        color: ManaType::Red,
        aim: Some(TargetRef::Object(smaug)),
        cost: None,
        accept: true,
    };
    for mode in MODES {
        let mut live = in_mode(&board, mode);
        tap_for_mana(&mut live, land);
        settle(&mut live, &answers);
        assert!(live.objects[&land].tapped, "{mode:?}");
        treasure_cycle(&mut live, &answers);
        offer(&live);
        let offered = play_trace_view(&live)
            .and_then(|view| view.offered)
            .expect("the offered span");
        assert_eq!(offered.cause, NamingCause::Restored, "{mode:?}");
    }
}

/// CR 111.7: a Treasure spent and never made again ceases to exist, and Phyrexian Altar
/// ("Sacrifice a creature: Add one mana of any color.") sacrificing Kitchen Finks ("When this
/// creature enters, you gain 2 life." Persist) beside Vizier of Remedies ("If one or more -1/-1
/// counters would be put on a creature you control, that many -1/-1 counters minus one are put
/// on it instead.") is offered after its first activation all the same.
#[test]
fn a_treasure_spent_earlier_in_the_step_does_not_delay_another_plays_offer() {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    let finks = scenario.add_real_card(P0, "Kitchen Finks", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Vizier of Remedies", Zone::Battlefield, db);
    let board = built(scenario, 1);
    let answers = Answers {
        color: ManaType::Black,
        aim: Some(TargetRef::Object(finks)),
        cost: None,
        accept: true,
    };
    for mode in MODES {
        let mut live = in_mode(&board, mode);
        let treasure = tokens(&live, "Treasure")[0];
        treasure_cycle(&mut live, &answers);
        assert!(!live.objects.contains_key(&treasure), "{mode:?}");
        assert_eq!(live.players[0].mana_pool.total(), 1, "{mode:?}");
        let life = live.players[0].life;

        tap_for_mana(&mut live, altar);
        settle(&mut live, &answers);
        assert!(is_offer(&live), "{mode:?}: {:?}", live.waiting_for);
        assert_eq!(live.players[0].life, life + 2, "{mode:?}");
        let returned = &live.objects[&finks];
        assert_eq!(returned.zone, Zone::Battlefield, "{mode:?}");
        assert!(returned.counters.is_empty(), "{mode:?}");
    }
}

/// The captured four-player game, one seat already eliminated, with Mirkwood Bats ("Whenever
/// you create or sacrifice a token, each opponent loses 1 life.") beside Smaug and Mayhem Devil,
/// resolved with detection off to the empty stack its loop starts from.
fn bats_capture() -> (GameState, ObjectId) {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smaug_devil_bats_4p.json.gz");
    let mut state = crate::committed_dump_walk::restore_committed(&path).expect("the capture");
    state.loop_detection = LoopDetectionMode::Off;
    let smaug = state
        .battlefield
        .iter()
        .copied()
        .find(|id| state.objects[id].name == "Smaug the Impenetrable")
        .expect("Smaug");
    assert_eq!(state.objects[&smaug].controller, P0);
    let answers = Answers {
        color: ManaType::Red,
        aim: Some(TargetRef::Object(smaug)),
        cost: None,
        accept: true,
    };
    settle(&mut state, &answers);
    (state, smaug)
}

/// CR 704.5a + CR 732.2a: each cycle of the capture costs every living opponent 2 life, so the
/// offer is bounded by the last of them; `pick` names the Treasure of the stock each cycle
/// sacrifices.
fn bats_capture_offered_and_taken(pick: fn(&[ObjectId]) -> Option<&ObjectId>) {
    let (board, smaug) = bats_capture();
    let answers = Answers {
        color: ManaType::Red,
        aim: Some(TargetRef::Object(smaug)),
        cost: None,
        accept: true,
    };
    let cycle = |state: &mut GameState| {
        let stock = tokens(state, "Treasure");
        tap_for_mana(state, *pick(&stock).expect("a Treasure"));
        settle(state, &answers);
    };
    let mut reference = in_mode(&board, LoopDetectionMode::Off);
    cycle(&mut reference);
    let bound = cycles_to_the_last_loss(&reference, 2);
    assert!(bound > TAKEN);
    let mut references = Vec::new();
    for count in 1..=bound {
        cycle(&mut reference);
        if count == TAKEN || count == bound {
            references.push((count, reference.clone()));
        }
    }
    for mode in MODES {
        let mut live = in_mode(&board, mode);
        assert!(tokens(&live, "Treasure").len() > 1, "{mode:?}");
        cycle(&mut live);
        let (_, schema) = offer(&live);
        assert_eq!(schema.measured_repetition_bound, Some(bound), "{mode:?}");

        for (count, reference) in &references {
            let mut taken = live.clone();
            take(&mut taken, *count);
            assert_eq!(
                reading(&taken, smaug),
                reading(reference, smaug),
                "{mode:?}, {count} cycles"
            );
            assert_eq!(
                taken.waiting_for, reference.waiting_for,
                "{mode:?}, {count} cycles"
            );
            assert_eq!(
                matches!(taken.waiting_for, WaitingFor::GameOver { .. }),
                *count == bound,
                "{mode:?}, {count} cycles"
            );
        }
    }
}

/// The sacrificed Treasure is older than the trace.
#[test]
fn the_bats_capture_sacrificing_its_oldest_treasure_is_offered_a_bounded_shortcut_and_taken() {
    bats_capture_offered_and_taken(<[ObjectId]>::first);
}

/// The sacrificed Treasure is the stock's newest.
#[test]
fn the_bats_capture_sacrificing_its_newest_treasure_is_offered_a_bounded_shortcut_and_taken() {
    bats_capture_offered_and_taken(<[ObjectId]>::last);
}

/// CR 111.7: with the Devil's damage aimed at an opponent nothing makes a Treasure, so each
/// cycle sacrifices by hand a Treasure the next cycle's replay no longer finds, and every span
/// is refused (phase-rs/phase#9928).
#[test]
fn a_treasure_stock_spent_without_remaking_is_not_offered() {
    let (board, _) = bats_capture();
    let victim = living_opponents(&board)[0];
    let answers = Answers {
        color: ManaType::Red,
        aim: Some(TargetRef::Player(victim)),
        cost: None,
        accept: true,
    };
    let lives = |state: &GameState| -> Vec<i32> {
        living_opponents(state)
            .into_iter()
            .map(|seat| state.players[seat.0 as usize].life)
            .collect()
    };
    for mode in MODES {
        let mut live = in_mode(&board, mode);
        for cycle in 0..6 {
            let (stock, before) = (tokens(&live, "Treasure").len(), lives(&live));
            treasure_cycle(&mut live, &answers);
            assert!(!is_offer(&live), "{mode:?}, cycle {cycle}");
            assert_eq!(tokens(&live, "Treasure").len(), stock - 1, "{mode:?}");
            let after = lives(&live);
            assert_eq!(after.len(), before.len(), "{mode:?}");
            assert!(
                after.iter().zip(&before).all(|(now, then)| now < then),
                "{mode:?}: {before:?} -> {after:?}"
            );
        }
    }
}

/// Four players in P0's precombat main, with Basking Broodscale ("{1}{G}: Adapt 1. Whenever one
/// or more +1/+1 counters are put on this creature, you may create a 0/1 colorless Eldrazi Spawn
/// creature token with 'Sacrifice this token: Add {C}.'"), Blade of the Bloodchief ("Whenever a
/// creature dies, put a +1/+1 counter on equipped creature." Equip {1}) and three Forests.
fn broodscale_scenario(db: &CardDatabase) -> (GameScenario, ObjectId, ObjectId, [ObjectId; 3]) {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let broodscale = scenario.add_real_card(P0, "Basking Broodscale", Zone::Battlefield, db);
    let blade = scenario.add_real_card(P0, "Blade of the Bloodchief", Zone::Battlefield, db);
    let forests = [(); 3].map(|()| scenario.add_basic_land(P0, ManaColor::Green));
    (scenario, broodscale, blade, forests)
}

/// [`broodscale_scenario`], built.
fn broodscale_board(db: &CardDatabase) -> (GameState, ObjectId, ObjectId, [ObjectId; 3]) {
    let (scenario, broodscale, blade, forests) = broodscale_scenario(db);
    (scenario.build().state().clone(), broodscale, blade, forests)
}

/// The Forests tapped, the Blade equipped and Adapt paid, which leaves the first Spawn at the
/// empty stack.
fn start_spawn_engine(
    state: &mut GameState,
    answers: &Answers,
    forests: [ObjectId; 3],
    blade: ObjectId,
    broodscale: ObjectId,
) {
    for forest in forests {
        tap_for_mana(state, forest);
        settle(state, answers);
    }
    for source in [blade, broodscale] {
        activate_ability(state, source);
        settle(state, answers);
    }
    assert_eq!(state.objects[&blade].attached_to, Some(broodscale.into()));
    assert_eq!(tokens(state, "Eldrazi Spawn").len(), 1);
    assert!(state.stack.is_empty());
}

/// One cycle of the Spawn engine: the Spawn sacrificed for mana, then the stack.
fn spawn_cycle(state: &mut GameState, answers: &Answers) {
    let spawn = tokens(state, "Eldrazi Spawn")[0];
    tap_for_mana(state, spawn);
    settle(state, answers);
}

/// Casts Mystic Reflection ("Choose target nonlegendary creature. The next time one or more
/// creatures or planeswalkers enter this turn, they enter as copies of the chosen creature.")
/// from P0's hand on `chosen`, paid from the pool, and resolves it.
fn cast_reflection(state: &mut GameState, reflection: ObjectId, chosen: ObjectId) {
    cast_on(state, reflection, chosen);
}

/// Casts `spell` from P0's hand on `chosen`, paid from the pool, and resolves it.
fn cast_on(state: &mut GameState, spell: ObjectId, chosen: ObjectId) {
    let card_id = state.objects[&spell].card_id;
    act(
        state,
        GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    );
    let answers = Answers {
        color: ManaType::Blue,
        aim: Some(TargetRef::Object(chosen)),
        cost: None,
        accept: true,
    };
    settle(state, &answers);
    assert!(state.stack.is_empty());
}

/// One hand-driven period of the Spawn engine that ends with Broodscale's "may" trigger on the
/// stack and no Spawn: the Spawn is made, `reflection` if any is cast on it, and it is
/// sacrificed; returns that Spawn.
fn spawn_period_under_the_may(
    state: &mut GameState,
    answers: &Answers,
    broodscale: ObjectId,
    reflection: Option<ObjectId>,
) -> ObjectId {
    let counters = reading(state, broodscale).counters;
    pass_until(state, answers, |state| {
        has_priority(state) && state.stack.is_empty() && !tokens(state, "Eldrazi Spawn").is_empty()
    });
    let spawn = tokens(state, "Eldrazi Spawn")[0];
    if let Some(reflection) = reflection {
        cast_reflection(state, reflection, spawn);
    }
    tap_for_mana(state, spawn);
    answer_prompts(state, answers);
    pass_until(state, answers, |state| {
        has_priority(state)
            && state.stack.len() == 1
            && tokens(state, "Eldrazi Spawn").is_empty()
            && reading(state, broodscale).counters == counters + 1
    });
    spawn
}

/// CR 702.6a + CR 701.46a + CR 603.5 + CR 732.2a: with its Forests tapped, the Blade equipped
/// and Adapt paid under detection, the Spawn sacrificed for {C} dies, the Blade's counter puts
/// Broodscale's "may" trigger on the stack, and the cycle is offered there, before the "may" is
/// asked, with it as its one decision.
#[test]
fn a_spawn_remade_by_broodscale_is_offered_and_taken() {
    let db = shared_card_db().expect("the card fixture");
    let (board, broodscale, blade, forests) = broodscale_board(db);
    let answers = Answers {
        color: ManaType::Green,
        aim: Some(TargetRef::Object(broodscale)),
        cost: None,
        accept: true,
    };
    let decline = Answers {
        accept: false,
        ..answers.clone()
    };
    let set_up = |mode: LoopDetectionMode| {
        let mut state = in_mode(&board, mode);
        start_spawn_engine(&mut state, &answers, forests, blade, broodscale);
        state
    };
    for mode in MODES {
        let mut live = set_up(mode);
        spawn_cycle(&mut live, &answers);
        let (certificate, schema) = offer(&live);
        assert_eq!(
            certificate.unbounded,
            [
                ResourceAxis::Mana(ManaType::Colorless),
                ResourceAxis::Counter(CounterClass::Plus1Plus1, ObjectClass::Creature),
            ],
            "{mode:?}"
        );
        assert_eq!(schema.points.len(), 1, "{mode:?}");
        assert_eq!(live.stack.len(), 1, "{mode:?}");
        assert!(tokens(&live, "Eldrazi Spawn").is_empty(), "{mode:?}");
        let offered = play_trace_view(&live)
            .and_then(|view| view.offered)
            .expect("the offered span");
        assert_eq!(offered.cause, NamingCause::TriggerTop, "{mode:?}");

        let hand_period = |state: &mut GameState| {
            spawn_period_under_the_may(state, &answers, broodscale, None);
        };
        let takes = taken_and_hand_driven(&live, &[1, 2, TAKEN], hand_period, &format!("{mode:?}"));
        for (count, taken, reference) in takes {
            assert_eq!(taken.stack.len(), 1, "{mode:?}, {count} cycles");
            assert_eq!(
                reading(&taken, broodscale),
                Reading {
                    life: vec![20; 4],
                    eliminated: vec![false; 4],
                    mana: mana_of(ManaType::Colorless, 1 + count as usize),
                    tokens: 0,
                    damage: 0,
                    counters: 2 + count,
                },
                "{mode:?}, {count} cycles"
            );
            for may in [&answers, &decline] {
                let (mut taken, mut reference) = (taken.clone(), reference.clone());
                taken.loop_detection = LoopDetectionMode::Off;
                settle(&mut taken, may);
                settle(&mut reference, may);
                assert_eq!(
                    tokens(&taken, "Eldrazi Spawn").len(),
                    usize::from(may.accept),
                    "{mode:?}, {count} cycles"
                );
                let differing = differing_fields(&taken, &reference);
                assert!(
                    differing.is_empty(),
                    "{mode:?}, {count} cycles, the \"may\" answered {}: {differing:?}",
                    may.accept
                );
            }
        }

        let mut declined = set_up(mode);
        spawn_cycle(&mut declined, &decline);
        assert!(is_offer(&declined), "{mode:?}");
        act(&mut declined, GameAction::DeclineShortcut);
        settle(&mut declined, &decline);
        assert!(!is_offer(&declined), "{mode:?}");
        assert_eq!(
            reading(&declined, broodscale),
            Reading {
                life: vec![20; 4],
                eliminated: vec![false; 4],
                mana: mana_of(ManaType::Colorless, 1),
                tokens: 0,
                damage: 0,
                counters: 2,
            },
            "{mode:?}"
        );
    }
}

/// CR 111.7 + CR 111.1: two Treasures sacrificed while the first's triggers are on the stack
/// are two plays of one ability, each made again from its own Treasure; Adapt on a Broodscale
/// that has its counter spends the mana, and Mirkwood Bats costs each opponent 4 life a cycle.
#[test]
fn two_treasures_sacrificed_before_the_stack_empties_are_offered_and_taken() {
    let db = shared_card_db().expect("the card fixture");
    let (mut scenario, smaug) = smaug_scenario(db);
    scenario.add_real_card(P0, "Mirkwood Bats", Zone::Battlefield, db);
    let broodscale = scenario.add_real_card(P0, "Basking Broodscale", Zone::Battlefield, db);
    let mut board = in_mode(&built(scenario, 2), LoopDetectionMode::Off);
    let answers = Answers {
        color: ManaType::Green,
        aim: Some(TargetRef::Object(smaug)),
        cost: None,
        accept: true,
    };
    let cycle = |state: &mut GameState| {
        for _ in 0..2 {
            let treasure = tokens(state, "Treasure")[0];
            tap_for_mana(state, treasure);
            answer_prompts(state, &answers);
            assert!(!state.stack.is_empty());
        }
        activate_ability(state, broodscale);
        settle(state, &answers);
    };
    cycle(&mut board);
    assert_eq!(
        board.objects[&broodscale].counters[&CounterType::Plus1Plus1],
        1
    );
    for mode in MODES {
        let mut live = in_mode(&board, mode);
        let mut reference = in_mode(&board, LoopDetectionMode::Off);
        cycle(&mut live);
        cycle(&mut reference);
        let (_, schema) = offer(&live);
        assert_eq!(
            schema.measured_repetition_bound,
            Some(cycles_to_the_last_loss(&live, 4)),
            "{mode:?}"
        );
        let before = reading(&reference, smaug);
        take(&mut live, TAKEN);
        for _ in 0..TAKEN {
            cycle(&mut reference);
        }
        let after = reading(&reference, smaug);
        assert_eq!(reading(&live, smaug), after, "{mode:?}");
        assert_eq!(
            (after.mana, after.tokens, after.damage),
            (before.mana, before.tokens, before.damage + 10)
        );
        assert_eq!(
            after.life[1..],
            before.life[1..]
                .iter()
                .map(|life| life - 20)
                .collect::<Vec<_>>()
        );
    }
}

/// CR 601.2g + CR 605.3a: Warren Soultrader ("Pay 1 life, Sacrifice another creature: Create a
/// Treasure token.") sacrificing Gravecrawler ("You may cast this card from your graveyard as
/// long as you control a Zombie.") beside Blood Artist ("Whenever this creature or another
/// creature dies, target player loses 1 life and you gain 1 life.") makes again the Treasure
/// that pays for Gravecrawler, however `payment` has that Treasure spent.
fn a_treasure_remade_after_paying_for_a_cast_is_offered_and_taken(payment: CastPaymentMode) {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let trader = scenario.add_real_card(P0, "Warren Soultrader", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Blood Artist", Zone::Battlefield, db);
    let crawler = scenario.add_real_card(P0, "Gravecrawler", Zone::Graveyard, db);
    let board = built(scenario, 1);
    let answers = Answers {
        color: ManaType::Black,
        aim: Some(TargetRef::Player(PlayerId(1))),
        cost: None,
        accept: true,
    };
    let cycle = |state: &mut GameState| {
        let treasure = tokens(state, "Treasure")[0];
        let card_id = state.objects[&crawler].card_id;
        act(
            state,
            GameAction::CastSpell {
                object_id: crawler,
                card_id,
                targets: vec![],
                payment_mode: payment,
            },
        );
        match payment {
            CastPaymentMode::Auto => {}
            CastPaymentMode::Manual => {
                assert!(matches!(state.waiting_for, WaitingFor::ManaPayment { .. }));
                tap_for_mana(state, treasure);
            }
            CastPaymentMode::AutoExceptSacrificialMana => {
                let WaitingFor::ManaSourceSelection { options, .. } = &state.waiting_for else {
                    panic!("no mana source asked for at {:?}", state.waiting_for);
                };
                let [selection] = &options[..] else {
                    panic!("one Treasure offers {options:?}");
                };
                assert_eq!(selection.source.object_id, treasure);
                let selection = selection.clone();
                act(state, GameAction::ActivateManaSource { selection });
            }
        }
        settle(state, &answers);
        assert!(!state.objects.contains_key(&treasure));
        assert_eq!(state.objects[&crawler].zone, Zone::Battlefield);
        activate_ability(state, trader);
        act(
            state,
            GameAction::SelectCards {
                cards: vec![crawler],
            },
        );
        settle(state, &answers);
    };
    let mut reference = in_mode(&board, LoopDetectionMode::Off);
    for _ in 0..4 {
        cycle(&mut reference);
    }
    assert_eq!(
        reading(&reference, trader),
        Reading {
            life: vec![20, 16],
            eliminated: vec![false; 2],
            mana: mana_of(ManaType::Black, 0),
            tokens: 1,
            damage: 0,
            counters: 0,
        }
    );
    assert_eq!(reference.objects[&crawler].zone, Zone::Graveyard);
    for mode in MODES {
        let mut live = in_mode(&board, mode);
        cycle(&mut live);
        let (certificate, schema) = offer(&live);
        assert_eq!(
            certificate.unbounded,
            [ResourceAxis::Life(PlayerId(1))],
            "{mode:?}"
        );
        assert_eq!(schema.measured_repetition_bound, Some(19), "{mode:?}");
        take(&mut live, 3);
        assert_eq!(
            reading(&live, trader),
            reading(&reference, trader),
            "{mode:?}"
        );
        assert_eq!(live.objects[&crawler].zone, Zone::Graveyard, "{mode:?}");
        assert_eq!(live.waiting_for, reference.waiting_for, "{mode:?}");
    }
}

/// The engine spends the Treasure, and no play records it.
#[test]
fn a_treasure_the_engine_pays_a_cast_with_is_offered_and_taken() {
    a_treasure_remade_after_paying_for_a_cast_is_offered_and_taken(CastPaymentMode::Auto);
}

/// The Treasure's ability is activated at the payment.
#[test]
fn a_treasure_activated_at_a_casts_payment_is_offered_and_taken() {
    a_treasure_remade_after_paying_for_a_cast_is_offered_and_taken(CastPaymentMode::Manual);
}

/// The Treasure is the mana source selected when the engine asks before sacrificing one.
#[test]
fn a_treasure_selected_as_a_casts_sacrificial_mana_is_offered_and_taken() {
    a_treasure_remade_after_paying_for_a_cast_is_offered_and_taken(
        CastPaymentMode::AutoExceptSacrificialMana,
    );
}

/// Answers for plays that name no target.
fn untargeted() -> Answers {
    Answers {
        color: ManaType::Green,
        aim: None,
        cost: None,
        accept: true,
    }
}

/// Three cards in each player's library, for the draws of the steps a drive passes through.
fn stock_libraries(scenario: &mut GameScenario, db: &CardDatabase) {
    for seat in [P0, PlayerId(1)] {
        for _ in 0..3 {
            scenario.add_real_card(seat, "Mayhem Devil", Zone::Library, db);
        }
    }
}

/// Two players, the opponent at 200 life, with Smaug the Impenetrable, Mayhem Devil, Mirkwood
/// Bats and one Treasure under P0.
fn chained_board(db: &CardDatabase) -> (GameState, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(PlayerId(1), 200);
    let smaug = scenario.add_real_card(P0, "Smaug the Impenetrable", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Mayhem Devil", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Mirkwood Bats", Zone::Battlefield, db);
    (built(scenario, 1), smaug)
}

/// P0's Treasure sacrificed for mana and the prompts answered, the stack left standing.
fn sacrifice_treasure(state: &mut GameState, answers: &Answers) {
    let treasure = tokens(state, "Treasure")[0];
    tap_for_mana(state, treasure);
    answer_prompts(state, answers);
}

/// CR 111.7 + CR 732.2a: the Treasure made again is sacrificed while the first sacrifice's
/// triggers are still on the stack, the ceased Treasures leave only last-known information
/// (CR 608.2h), and the period is offered under Mirkwood Bats' and Smaug's triggers; played one
/// Treasure at a time, the same board keeps its offer at the empty stack.
#[test]
fn the_chained_smaug_play_is_offered_under_its_triggers_and_taken() {
    let db = shared_card_db().expect("the card fixture");
    let (board, smaug) = chained_board(db);
    let answers = Answers {
        color: ManaType::Red,
        aim: Some(TargetRef::Object(smaug)),
        cost: None,
        accept: true,
    };
    let treasured = |state: &GameState| !tokens(state, "Treasure").is_empty();
    let moved = |state: &GameState| {
        (
            state.players[0].mana_pool.count_color(ManaType::Red) as i32,
            state.objects[&smaug].damage_marked as i32,
            state.players[1].life,
        )
    };
    let cause = |state: &GameState| {
        play_trace_view(state)
            .and_then(|view| view.offered)
            .expect("the offered span")
            .cause
    };
    for mode in MODES {
        let context = format!("{mode:?}");
        let mut live = in_mode(&board, mode);
        sacrifice_treasure(&mut live, &answers);
        pass_until(&mut live, &answers, |state| {
            has_priority(state) && treasured(state)
        });
        assert!(!live.stack.is_empty(), "{context}");
        sacrifice_treasure(&mut live, &answers);
        pass_until(&mut live, &answers, |state| {
            is_offer(state) || state.stack.is_empty()
        });

        let (certificate, schema) = offer(&live);
        let mut triggers: Vec<&str> = live
            .stack
            .iter()
            .map(|entry| live.objects[&entry.source_id].name.as_str())
            .collect();
        triggers.sort_unstable();
        assert_eq!(
            triggers,
            ["Mirkwood Bats", "Smaug the Impenetrable"],
            "{context}"
        );
        assert!(!treasured(&live), "{context}");
        assert_eq!(
            certificate.unbounded,
            [
                ResourceAxis::Mana(ManaType::Red),
                ResourceAxis::Life(PlayerId(1)),
            ],
            "{context}"
        );
        assert_eq!(
            schema.measured_repetition_bound,
            Some(cycles_to_the_last_loss(&live, 2)),
            "{context}"
        );
        assert_eq!(cause(&live), NamingCause::Repeat, "{context}");

        let depth = live.stack.len();
        let hand_period = |state: &mut GameState| {
            let damage = state.objects[&smaug].damage_marked;
            pass_until(state, &answers, |state| {
                has_priority(state) && treasured(state) && state.stack.len() + 1 == depth
            });
            sacrifice_treasure(state, &answers);
            pass_until(state, &answers, |state| {
                has_priority(state)
                    && !treasured(state)
                    && state.stack.len() == depth
                    && state.objects[&smaug].damage_marked == damage + 1
            });
        };
        let (mana, damage, life) = moved(&live);
        for (count, taken, _) in taken_and_hand_driven(&live, &[1, 3, TAKEN], hand_period, &context)
        {
            let cycles = count as i32;
            assert_eq!(
                moved(&taken),
                (mana + cycles, damage + cycles, life - 2 * cycles),
                "{context}, {count} cycles"
            );
        }

        let mut unchained = in_mode(&board, mode);
        treasure_cycle(&mut unchained, &answers);
        offer(&unchained);
        assert!(unchained.stack.is_empty(), "{context}");
        assert_eq!(cause(&unchained), NamingCause::LegalNow, "{context}");
    }
}

/// Mystic Reflection cast on a Spawn that is then sacrificed waits to copy a creature only
/// last-known information (CR 608.2h) describes, so the period that leaves it waiting is not
/// covered, and the same periods without the cast are.
#[test]
fn a_copy_effect_naming_a_departed_spawn_keeps_the_period_refused() {
    let db = shared_card_db().expect("the card fixture");
    let (mut scenario, broodscale, blade, forests) = broodscale_scenario(db);
    let reflection = scenario.add_real_card(P0, "Mystic Reflection", Zone::Hand, db);
    let islands = [(); 2].map(|()| scenario.add_basic_land(P0, ManaColor::Blue));
    let answers = Answers {
        color: ManaType::Green,
        aim: Some(TargetRef::Object(broodscale)),
        cost: None,
        accept: true,
    };
    let mut board = in_mode(scenario.build().state(), LoopDetectionMode::Off);
    start_spawn_engine(&mut board, &answers, forests, blade, broodscale);
    for island in islands {
        tap_for_mana(&mut board, island);
        settle(&mut board, &answers);
    }
    let periods = |cast: Option<ObjectId>| {
        let mut state = board.clone();
        let mut sacrificed = Vec::new();
        let frames = [None, cast, None].map(|cast| {
            sacrificed.push(spawn_period_under_the_may(
                &mut state, &answers, broodscale, cast,
            ));
            state.clone()
        });
        (frames, sacrificed[1])
    };

    let (frames, chosen) = periods(Some(reflection));
    let used_up = Floating {
        used_up: true,
        ..COPY
    };
    assert_eq!(
        frames.each_ref().map(floating),
        [vec![], vec![COPY], vec![used_up]]
    );
    assert!(frames
        .iter()
        .all(|frame| !frame.objects.contains_key(&chosen)));
    let (plain, _) = periods(None);
    assert_eq!(
        [
            certified(&frames, &[reflection]),
            certified(&plain, &[reflection]),
        ],
        [
            ObjectGrowthVerdict::ResourceRecurrence(None),
            ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::History)),
        ]
    );
}

/// CR 614.3: Mystic Reflection cast on each period's Spawn is used up when the next Spawn
/// enters as a copy of it, so every frame holds only used-up effects, one more a period, and
/// the periods are covered as they are without the casts.
#[test]
fn copy_effects_used_up_inside_each_spawn_period_leave_the_periods_covered() {
    let db = shared_card_db().expect("the card fixture");
    let (mut scenario, broodscale, blade, forests) = broodscale_scenario(db);
    let reflections =
        [(); 4].map(|()| scenario.add_real_card(P0, "Mystic Reflection", Zone::Hand, db));
    let islands = [(); 8].map(|()| scenario.add_basic_land(P0, ManaColor::Blue));
    let answers = Answers {
        color: ManaType::Green,
        aim: Some(TargetRef::Object(broodscale)),
        cost: None,
        accept: true,
    };
    let mut board = in_mode(scenario.build().state(), LoopDetectionMode::Off);
    start_spawn_engine(&mut board, &answers, forests, blade, broodscale);
    for island in islands {
        tap_for_mana(&mut board, island);
        settle(&mut board, &answers);
    }
    let periods = |cast: bool| {
        let mut state = board.clone();
        reflections.map(|reflection| {
            let spawn = tokens(&state, "Eldrazi Spawn")[0];
            if cast {
                cast_reflection(&mut state, reflection, spawn);
            }
            spawn_cycle(&mut state, &answers);
            assert!(has_priority(&state) && state.stack.is_empty());
            assert!(!state.objects.contains_key(&spawn));
            assert_eq!(tokens(&state, "Eldrazi Spawn").len(), 1);
            state.clone()
        })
    };

    let frames = periods(true);
    let used_up = Floating {
        used_up: true,
        ..COPY
    };
    for (period, frame) in frames.iter().enumerate() {
        assert_eq!(floating(frame), vec![used_up; period + 1]);
    }
    assert_eq!(
        vec![
            certified(&frames[..3], &reflections),
            certified(&frames[1..], &reflections),
            certified(&periods(false)[..3], &reflections),
        ],
        vec![ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::History)); 3]
    );
}

/// Sliver Queen's ability, then the stack.
fn make_sliver(state: &mut GameState, queen: ObjectId, answers: &Answers) {
    activate_ability(state, queen);
    settle(state, answers);
}

/// P0's newest Sliver sacrificed to `altar` for mana, then the stack.
fn sacrifice_newest_sliver(state: &mut GameState, altar: ObjectId, answers: &Answers) {
    let sliver = *tokens(state, "Sliver").last().expect("a Sliver");
    tap_for_mana(state, altar);
    let answers = Answers {
        cost: Some(sliver),
        ..answers.clone()
    };
    settle(state, &answers);
}

/// Sliver Queen ("{2}: Create a 1/1 colorless Sliver creature token.") and Peregrin Took ("If
/// one or more tokens would be created under your control, those tokens plus an additional Food
/// token are created instead.") over `forests` Forests tapped for mana, with Ashnod's Altar
/// ("Sacrifice a creature: Add {C}{C}.") when `outlet`. Each of three periods makes a Sliver and
/// a Food, sacrifices the Sliver to the Altar if there is one, and sacrifices the Food for its
/// own ability (CR 111.10b); returns the frames with that ability on the stack, then the frames
/// with it resolved.
fn food_periods(db: &CardDatabase, forests: usize, outlet: bool) -> [Vec<GameState>; 2] {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let queen = scenario.add_real_card(P0, "Sliver Queen", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Peregrin Took", Zone::Battlefield, db);
    let altar = outlet.then(|| scenario.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db));
    let forests: Vec<ObjectId> = (0..forests)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    let answers = untargeted();
    let mut state = in_mode(scenario.build().state(), LoopDetectionMode::Off);
    for forest in forests {
        tap_for_mana(&mut state, forest);
        settle(&mut state, &answers);
    }
    let (mut on_stack, mut resolved) = (Vec::new(), Vec::new());
    for _ in 0..3 {
        make_sliver(&mut state, queen, &answers);
        if let Some(altar) = altar {
            sacrifice_newest_sliver(&mut state, altar, &answers);
        }
        let food = tokens(&state, "Food")[0];
        activate_ability(&mut state, food);
        answer_prompts(&mut state, &answers);
        let sources: Vec<ObjectId> = state.stack.iter().map(|entry| entry.source_id).collect();
        assert_eq!(sources, [food]);
        assert!(!state.objects.contains_key(&food) && state.lki_cache.contains_key(&food));
        on_stack.push(state.clone());
        settle(&mut state, &answers);
        resolved.push(state.clone());
    }
    [on_stack, resolved]
}

/// CR 113.7a: a Food's ability on the stack exists apart from the Food sacrificed for it, and
/// the periods that hold it there stay refused while the periods that resolved it are covered.
#[test]
fn a_departed_foods_ability_on_the_stack_keeps_the_pair_refused() {
    let db = shared_card_db().expect("the card fixture");
    let [on_stack, resolved] = food_periods(db, 6, true);
    assert_eq!(
        [certified(&on_stack, &[]), certified(&resolved, &[])],
        [
            ObjectGrowthVerdict::ResourceRecurrence(None),
            ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::History)),
        ]
    );
}

/// With no outlet the Slivers accumulate, and the same Food ability on the stack keeps the
/// growing periods refused while the periods that resolved it are covered.
#[test]
fn a_departed_foods_ability_on_the_stack_keeps_the_sliver_period_refused() {
    let db = shared_card_db().expect("the card fixture");
    let [on_stack, resolved] = food_periods(db, 12, false);
    for frames in [&on_stack, &resolved] {
        let slivers: Vec<usize> = frames
            .iter()
            .map(|frame| tokens(frame, "Sliver").len())
            .collect();
        assert_eq!(slivers, [1, 2, 3]);
    }
    let refused = vec![FodderCoverRefusal::NonObjectRemainder];
    assert_eq!(
        [certified(&on_stack, &[]), certified(&resolved, &[])],
        [
            ObjectGrowthVerdict::FodderGrowth([refused.clone(), refused]),
            ObjectGrowthVerdict::FodderGrowth([vec![], vec![]]),
        ]
    );
}

/// Sliver Queen, Parallel Lives ("If an effect would create one or more tokens under your
/// control, it creates twice that many of those tokens instead."), Ashnod's Altar and two
/// Forests in P0's precombat main: a period makes two Slivers and sacrifices one.
fn netted_sliver_scenario(db: &CardDatabase) -> (GameScenario, ObjectId, ObjectId, [ObjectId; 2]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let queen = scenario.add_real_card(P0, "Sliver Queen", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Parallel Lives", Zone::Battlefield, db);
    let altar = scenario.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db);
    let forests = [(); 2].map(|()| scenario.add_basic_land(P0, ManaColor::Green));
    (scenario, queen, altar, forests)
}

/// CR 111.7 + CR 732.2a: each period nets one Sliver while the sacrificed one ceases to exist,
/// the growing board is offered at its first sacrifice, and a take performs its periods and
/// marks nothing unbounded.
#[test]
fn slivers_netted_while_one_is_sacrificed_are_offered_and_taken() {
    let db = shared_card_db().expect("the card fixture");
    let (scenario, queen, altar, forests) = netted_sliver_scenario(db);
    let board = scenario.build().state().clone();
    let answers = untargeted();
    let make = |state: &mut GameState| make_sliver(state, queen, &answers);
    let sacrifice = |state: &mut GameState| sacrifice_newest_sliver(state, altar, &answers);
    let steps: [Step; 2] = [&make, &sacrifice];
    let hand_period = |state: &mut GameState| steps.iter().for_each(|step| step(state));
    for mode in MODES {
        let context = format!("{mode:?}");
        let mut live = in_mode(&board, mode);
        for forest in forests {
            tap_for_mana(&mut live, forest);
            settle(&mut live, &answers);
        }
        offered_after_one_period(&mut live, &steps, &context);
        let (certificate, _) = offer(&live);
        assert_eq!(
            certificate.unbounded,
            [ResourceAxis::TokensCreated],
            "{context}"
        );
        assert_eq!(tokens(&live, "Sliver").len(), 1, "{context}");
        for (count, taken, _) in taken_and_hand_driven(&live, &[1, 3, TAKEN], hand_period, &context)
        {
            assert!(
                taken.unbounded_resources.is_empty(),
                "{context}, {count} cycles: {:?}",
                taken.unbounded_resources
            );
            assert_eq!(
                (
                    tokens(&taken, "Sliver").len(),
                    taken.players[0].mana_pool.total()
                ),
                (1 + count as usize, 2),
                "{context}, {count} cycles"
            );
        }
    }
}

/// CR 111.7 + CR 732.2a: under Training Grounds ("Activated abilities of creatures you control
/// cost {2} less to activate. This effect can't reduce the mana in that cost to less than one
/// mana.") Sliver Queen's ability costs {1}, and Ashnod's Altar adds {C}{C} for the Sliver, which
/// ceases to exist, so each period nets one mana and leaves no Sliver; it is offered at its first
/// sacrifice, and a take performs its periods and marks nothing unbounded.
#[test]
fn a_sliver_made_and_sacrificed_for_more_mana_than_it_costs_is_offered_and_taken() {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let queen = scenario.add_real_card(P0, "Sliver Queen", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Training Grounds", Zone::Battlefield, db);
    let altar = scenario.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db);
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let board = scenario.build().state().clone();
    let answers = untargeted();
    let make = |state: &mut GameState| make_sliver(state, queen, &answers);
    let sacrifice = |state: &mut GameState| sacrifice_newest_sliver(state, altar, &answers);
    let steps: [Step; 2] = [&make, &sacrifice];
    let hand_period = |state: &mut GameState| steps.iter().for_each(|step| step(state));
    for mode in MODES {
        let context = format!("{mode:?}");
        let mut live = in_mode(&board, mode);
        tap_for_mana(&mut live, forest);
        settle(&mut live, &answers);
        offered_after_one_period(&mut live, &steps, &context);
        let (certificate, _) = offer(&live);
        assert_eq!(
            certificate.unbounded,
            [ResourceAxis::Mana(ManaType::Colorless)],
            "{context}"
        );
        assert!(tokens(&live, "Sliver").is_empty(), "{context}");
        for (count, taken, _) in taken_and_hand_driven(&live, &[1, 3], hand_period, &context) {
            assert!(
                taken.unbounded_resources.is_empty(),
                "{context}, {count} cycles: {:?}",
                taken.unbounded_resources
            );
            assert_eq!(
                (
                    tokens(&taken, "Sliver").len(),
                    taken.players[0].mana_pool.total()
                ),
                (0, 2 + count as usize),
                "{context}, {count} cycles"
            );
        }
    }
}

/// Mystic Reflection cast on a Sliver that is then sacrificed waits to copy a creature only
/// last-known information (CR 608.2h) describes, so the growing period that uses it up is
/// refused, and the same periods without the cast are covered.
#[test]
fn a_copy_effect_naming_a_departed_sliver_keeps_the_period_refused() {
    let db = shared_card_db().expect("the card fixture");
    let (mut scenario, queen, altar, forests) = netted_sliver_scenario(db);
    let reflection = scenario.add_real_card(P0, "Mystic Reflection", Zone::Hand, db);
    let islands = [(); 2].map(|()| scenario.add_basic_land(P0, ManaColor::Blue));
    let answers = untargeted();
    let mut board = in_mode(scenario.build().state(), LoopDetectionMode::Off);
    for forest in forests {
        tap_for_mana(&mut board, forest);
        settle(&mut board, &answers);
    }
    let periods = |cast: bool| {
        let mut state = board.clone();
        [false, cast, false].map(|cast| {
            make_sliver(&mut state, queen, &answers);
            if cast {
                for island in islands {
                    tap_for_mana(&mut state, island);
                    settle(&mut state, &answers);
                }
                let sliver = *tokens(&state, "Sliver").last().expect("a Sliver");
                cast_reflection(&mut state, reflection, sliver);
            }
            sacrifice_newest_sliver(&mut state, altar, &answers);
            state.clone()
        })
    };

    let frames = periods(true);
    let used_up = Floating {
        used_up: true,
        ..COPY
    };
    assert_eq!(
        frames.each_ref().map(floating),
        [vec![], vec![COPY], vec![used_up]]
    );
    let ObjectGrowthVerdict::FodderGrowth([_, second]) = certified(&frames, &[reflection]) else {
        panic!("the Slivers did not grow");
    };
    assert_eq!(second, [FodderCoverRefusal::NonObjectRemainder]);

    assert_eq!(
        certified(&periods(false), &[reflection]),
        ObjectGrowthVerdict::FodderGrowth([vec![], vec![]])
    );
}

/// Kitchen Finks returns under the id it died with, so dying untapped and then tapped by
/// Earthcraft ("Tap an untapped creature you control: Untap target basic land.") changes its
/// last-known information (CR 608.2h) between two frames, and that pair is not covered.
#[test]
fn a_changed_record_of_a_returned_creature_keeps_the_pair_refused() {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    let finks = scenario.add_real_card(P0, "Kitchen Finks", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Vizier of Remedies", Zone::Battlefield, db);
    let earthcraft = scenario.add_real_card(P0, "Earthcraft", Zone::Battlefield, db);
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let answers = Answers {
        color: ManaType::Green,
        aim: Some(TargetRef::Object(forest)),
        cost: Some(finks),
        accept: true,
    };
    let sacrifice = |state: &mut GameState| {
        tap_for_mana(state, altar);
        settle(state, &answers);
    };
    let mut state = in_mode(scenario.build().state(), LoopDetectionMode::Off);
    tap_for_mana(&mut state, forest);
    settle(&mut state, &answers);
    sacrifice(&mut state);
    let first = state.clone();
    sacrifice(&mut state);
    let second = state.clone();
    activate_ability(&mut state, earthcraft);
    settle(&mut state, &answers);
    assert!(state.objects[&finks].tapped && !state.objects[&forest].tapped);
    tap_for_mana(&mut state, forest);
    settle(&mut state, &answers);
    sacrifice(&mut state);
    let third = state;

    assert!(loop_states_equal_modulo_resources(&first, &second));
    assert_eq!(
        [&second, &third].map(|frame| frame.lki_cache[&finks].tapped),
        [false, true]
    );
    assert!(!history_covers_for_tests(&second, &third));
}

/// Kitchen Finks returns under the id it died with, so its first death of a step adds
/// last-known information (CR 608.2h) of a creature both frames hold, and that period is not
/// covered while the periods after it are.
#[test]
fn a_first_record_of_a_returned_creature_keeps_the_period_refused() {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    let finks = scenario.add_real_card(P0, "Kitchen Finks", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Vizier of Remedies", Zone::Battlefield, db);
    stock_libraries(&mut scenario, db);
    let answers = Answers {
        color: ManaType::Black,
        aim: None,
        cost: Some(finks),
        accept: true,
    };
    let sacrifice = |state: &mut GameState| {
        tap_for_mana(state, altar);
        settle(state, &answers);
    };
    let mut state = in_mode(scenario.build().state(), LoopDetectionMode::Off);
    sacrifice(&mut state);
    pass_to_the_next_step(&mut state, &answers);
    let mut frames = vec![state.clone()];
    for _ in 0..3 {
        sacrifice(&mut state);
        frames.push(state.clone());
    }

    assert!(!frames[0].lki_cache.contains_key(&finks) && frames[1].lki_cache.contains_key(&finks));
    assert!(frames
        .iter()
        .all(|frame| frame.objects[&finks].zone == Zone::Battlefield));
    assert_eq!(
        [certified(&frames[..3], &[]), certified(&frames[1..], &[])],
        [
            ObjectGrowthVerdict::ResourceRecurrence(None),
            ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::Equal)),
        ]
    );
}

/// The same first death beside Slivers netted each period: the pair that adds the returned
/// Finks' last-known information (CR 608.2h) is refused, and the pairs after it are covered.
#[test]
fn a_first_record_of_a_returned_creature_keeps_the_sliver_period_refused() {
    let db = shared_card_db().expect("the card fixture");
    let (mut scenario, queen, altar, forests) = netted_sliver_scenario(db);
    scenario.at_phase(Phase::Upkeep);
    let finks = scenario.add_real_card(P0, "Kitchen Finks", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Vizier of Remedies", Zone::Battlefield, db);
    stock_libraries(&mut scenario, db);
    let answers = untargeted();
    let sacrifice_finks = |state: &mut GameState| {
        tap_for_mana(state, altar);
        let answers = Answers {
            cost: Some(finks),
            ..answers.clone()
        };
        settle(state, &answers);
    };
    let sliver_period = |state: &mut GameState| {
        make_sliver(state, queen, &answers);
        sacrifice_newest_sliver(state, altar, &answers);
    };
    let mut state = in_mode(scenario.build().state(), LoopDetectionMode::Off);
    sacrifice_finks(&mut state);
    pass_to_the_next_step(&mut state, &answers);
    for forest in forests {
        tap_for_mana(&mut state, forest);
        settle(&mut state, &answers);
    }
    // The land taps are then no longer the step's last mana plays.
    sliver_period(&mut state);
    let mut frames = vec![state.clone()];
    for _ in 0..3 {
        sliver_period(&mut state);
        sacrifice_finks(&mut state);
        frames.push(state.clone());
    }

    assert!(!frames[0].lki_cache.contains_key(&finks) && frames[1].lki_cache.contains_key(&finks));
    assert!(frames
        .iter()
        .all(|frame| frame.objects[&finks].zone == Zone::Battlefield));
    assert_eq!(
        [certified(&frames[..3], &[]), certified(&frames[1..], &[])],
        [
            ObjectGrowthVerdict::FodderGrowth([
                vec![FodderCoverRefusal::NonObjectRemainder],
                vec![]
            ]),
            ObjectGrowthVerdict::FodderGrowth([vec![], vec![]]),
        ]
    );
}

/// CR 615.7: Noble Vestige ("{T}: Prevent the next 1 damage that would be dealt to target player
/// or planeswalker this turn.") shields P0 every period while Mayhem Devil is aimed at the
/// opponent, and Kitchen Finks sacrificed to Phyrexian Altar beside one more unused shield each
/// period (Intruder Alarm, "Whenever a creature enters, untap all creatures.", untaps the
/// Vestige) keeps its offer and its take.
#[test]
fn a_shield_left_unused_each_period_keeps_its_offer_and_take() {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    let finks = scenario.add_real_card(P0, "Kitchen Finks", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Vizier of Remedies", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Mayhem Devil", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Intruder Alarm", Zone::Battlefield, db);
    let vestige = scenario.add_real_card(P0, "Noble Vestige", Zone::Battlefield, db);
    let board = scenario.build().state().clone();
    let aimed_at = |seat: PlayerId| Answers {
        color: ManaType::Green,
        aim: Some(TargetRef::Player(seat)),
        cost: Some(finks),
        accept: true,
    };
    let shield = |state: &mut GameState| {
        activate_ability(state, vestige);
        settle(state, &aimed_at(P0));
    };
    let sacrifice = |state: &mut GameState| {
        tap_for_mana(state, altar);
        settle(state, &aimed_at(PlayerId(1)));
    };
    let steps: [Step; 2] = [&shield, &sacrifice];
    let hand_period = |state: &mut GameState| steps.iter().for_each(|step| step(state));
    let moved = |state: &GameState| {
        (
            state.players[0].mana_pool.total() as i32,
            state.players[0].life,
            state.players[1].life,
        )
    };
    for mode in MODES {
        let context = format!("{mode:?}");
        let mut live = in_mode(&board, mode);
        offered_after_one_period(&mut live, &steps, &context);
        let (certificate, schema) = offer(&live);
        assert_eq!(
            certificate.unbounded,
            [
                ResourceAxis::Mana(ManaType::Green),
                ResourceAxis::Life(P0),
                ResourceAxis::Life(PlayerId(1)),
            ],
            "{context}"
        );
        assert_eq!(
            schema.measured_repetition_bound,
            Some(cycles_to_the_last_loss(&live, 1)),
            "{context}"
        );
        assert_eq!(floating(&live), [SHIELD], "{context}");
        let (mana, life, opposing) = moved(&live);
        for (count, taken, _) in taken_and_hand_driven(&live, &[1, 3], hand_period, &context) {
            let cycles = count as i32;
            assert_eq!(
                floating(&taken),
                vec![SHIELD; 1 + count as usize],
                "{context}, {count} cycles"
            );
            assert_eq!(
                moved(&taken),
                (mana + cycles, life + 2 * cycles, opposing - cycles),
                "{context}, {count} cycles"
            );
        }
    }
}

/// The floating effects a [`SliverBoard`]'s period stands beside.
#[derive(Clone, Copy, PartialEq)]
enum Beside {
    /// Noble Vestige's shield on P0, made every period.
    Shield,
    /// Circle of Protection: Red's effect on Mayhem Devil, made once in the upkeep before.
    Circle,
    CircleAndShield,
}

/// Sliver Queen, Ashnod's Altar, Mayhem Devil, Mirkwood Bats and two Forests under P0, the
/// opponent at 200 life: a period makes a Sliver and sacrifices it. A shield adds Intruder Alarm
/// and Noble Vestige, activated on P0 before each Sliver is made; a Circle adds Circle of
/// Protection: Red ("{1}: The next time a red source of your choice would deal damage to you
/// this turn, prevent that damage.") and a Plains, and starts the board in P0's upkeep.
struct SliverBoard {
    state: GameState,
    queen: ObjectId,
    altar: ObjectId,
    devil: ObjectId,
    forests: [ObjectId; 2],
    vestige: Option<ObjectId>,
    /// Circle of Protection: Red and the Plains that pays for it.
    circle: Option<(ObjectId, ObjectId)>,
    /// Whom Mayhem Devil's trigger is aimed at.
    devil_aim: PlayerId,
}

impl SliverBoard {
    fn new(db: &CardDatabase, beside: Beside, devil_aim: PlayerId) -> Self {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(PlayerId(1), 200);
        let queen = scenario.add_real_card(P0, "Sliver Queen", Zone::Battlefield, db);
        let altar = scenario.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db);
        let devil = scenario.add_real_card(P0, "Mayhem Devil", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Mirkwood Bats", Zone::Battlefield, db);
        let vestige = (beside != Beside::Circle).then(|| {
            scenario.add_real_card(P0, "Intruder Alarm", Zone::Battlefield, db);
            scenario.add_real_card(P0, "Noble Vestige", Zone::Battlefield, db)
        });
        let circle = (beside != Beside::Shield).then(|| {
            scenario.at_phase(Phase::Upkeep);
            stock_libraries(&mut scenario, db);
            (
                scenario.add_real_card(P0, "Circle of Protection: Red", Zone::Battlefield, db),
                scenario.add_basic_land(P0, ManaColor::White),
            )
        });
        let forests = [(); 2].map(|()| scenario.add_basic_land(P0, ManaColor::Green));
        SliverBoard {
            state: scenario.build().state().clone(),
            queen,
            altar,
            devil,
            forests,
            vestige,
            circle,
            devil_aim,
        }
    }

    fn answers(&self, cost: Option<ObjectId>) -> Answers {
        Answers {
            color: ManaType::Green,
            aim: Some(TargetRef::Player(self.devil_aim)),
            cost,
            accept: true,
        }
    }

    /// The board under `mode` with its Forests tapped for mana, after the Circle, if there is
    /// one, chose Mayhem Devil in the upkeep and that step passed.
    fn started(&self, mode: LoopDetectionMode) -> GameState {
        let mut state = in_mode(&self.state, mode);
        let answers = self.answers(None);
        if let Some((circle, plains)) = self.circle {
            tap_for_mana(&mut state, plains);
            settle(&mut state, &answers);
            activate_ability(&mut state, circle);
            let source = Answers {
                aim: Some(TargetRef::Object(self.devil)),
                ..answers.clone()
            };
            settle(&mut state, &source);
            pass_to_the_next_step(&mut state, &answers);
        }
        for forest in self.forests {
            tap_for_mana(&mut state, forest);
            settle(&mut state, &answers);
        }
        state
    }

    /// Noble Vestige activated on P0, then the stack.
    fn shield(&self, state: &mut GameState) {
        activate_ability(state, self.vestige.expect("Noble Vestige"));
        let shielded = Answers {
            aim: Some(TargetRef::Player(P0)),
            ..self.answers(None)
        };
        settle(state, &shielded);
    }

    fn make(&self, state: &mut GameState) {
        make_sliver(state, self.queen, &self.answers(None));
    }

    fn sacrifice(&self, state: &mut GameState) {
        sacrifice_newest_sliver(state, self.altar, &self.answers(None));
    }
}

/// CR 732.2a: in each mode the board's whole period is offered after its first sacrifice and
/// not before, at the empty stack with no Sliver, as costing the opponent `per_cycle` life a
/// cycle; takes of 1 and 3 equal the hand-driven periods, and `stored` is what the floating
/// effects are after that many taken periods.
fn a_sliver_period_beside_floating_effects_is_offered_and_taken(
    board: &SliverBoard,
    per_cycle: i32,
    stored: impl Fn(usize) -> Vec<Floating>,
) {
    let shield = |state: &mut GameState| board.shield(state);
    let make = |state: &mut GameState| board.make(state);
    let sacrifice = |state: &mut GameState| board.sacrifice(state);
    let mut steps: Vec<Step> = vec![&make, &sacrifice];
    if board.vestige.is_some() {
        steps.insert(0, &shield);
    }
    let hand_period = |state: &mut GameState| steps.iter().for_each(|step| step(state));
    let moved = |state: &GameState| {
        (
            state.players[0].mana_pool.total(),
            state.players[0].life,
            state.players[1].life,
        )
    };
    for mode in MODES {
        let context = format!("{mode:?}");
        let mut live = board.started(mode);
        offered_after_one_period(&mut live, &steps, &context);
        let (certificate, schema) = offer(&live);
        assert!(tokens(&live, "Sliver").is_empty(), "{context}");
        assert_eq!(
            certificate.unbounded,
            [ResourceAxis::Life(PlayerId(1))],
            "{context}"
        );
        assert_eq!(
            schema.measured_repetition_bound,
            Some(cycles_to_the_last_loss(&live, per_cycle)),
            "{context}"
        );
        assert_eq!(floating(&live), stored(0), "{context}");
        let (mana, life, opposing) = moved(&live);
        for (count, taken, _) in taken_and_hand_driven(&live, &[1, 3], hand_period, &context) {
            assert_eq!(
                floating(&taken),
                stored(count as usize),
                "{context}, {count} cycles"
            );
            assert_eq!(
                moved(&taken),
                (mana, life, opposing - per_cycle * count as i32),
                "{context}, {count} cycles"
            );
        }
    }
}

/// CR 615.7: Noble Vestige's shield on P0 is used up each period by Mayhem Devil's damage, and
/// the period, which also adds a ceased Sliver's last-known information (CR 608.2h), is offered.
#[test]
fn a_shield_used_up_beside_a_sliver_made_and_sacrificed_is_offered_and_taken() {
    let db = shared_card_db().expect("the card fixture");
    let board = SliverBoard::new(db, Beside::Shield, P0);
    let used_up = Floating {
        used_up: true,
        ..SHIELD
    };
    a_sliver_period_beside_floating_effects_is_offered_and_taken(&board, 2, |taken| {
        vec![used_up; 1 + taken]
    });
}

/// CR 615.7: each period activates Noble Vestige on P0, makes a Sliver with Sliver Queen, which
/// untaps the Vestige through Intruder Alarm, and sacrifices it to Ashnod's Altar with Mayhem
/// Devil aimed at the opponent, so it leaves one more shield unused, and the whole period, the
/// shield's activation included, is offered.
#[test]
fn a_shield_left_unused_each_period_beside_a_sliver_made_and_sacrificed_is_offered_and_taken() {
    let db = shared_card_db().expect("the card fixture");
    let board = SliverBoard::new(db, Beside::Shield, PlayerId(1));
    a_sliver_period_beside_floating_effects_is_offered_and_taken(&board, 3, |taken| {
        vec![SHIELD; 1 + taken]
    });
}

/// CR 615.8 + CR 609.7a: Circle of Protection: Red chose Mayhem Devil in the upkeep and the
/// Devil never deals damage to P0, so every frame of the Sliver period holds the same unused
/// effect naming an object, and the period is offered.
#[test]
fn a_chosen_source_effect_on_a_creature_that_stays_beside_a_sliver_period_is_offered_and_taken() {
    let db = shared_card_db().expect("the card fixture");
    let board = SliverBoard::new(db, Beside::Circle, PlayerId(1));
    a_sliver_period_beside_floating_effects_is_offered_and_taken(&board, 3, |_| {
        vec![CHOSEN_SOURCE]
    });
}

/// The Circle's effect on Mayhem Devil is the same in every frame while each period leaves one
/// more Noble Vestige shield unused, and the whole period is offered.
#[test]
fn a_chosen_source_effect_beside_a_shield_left_unused_each_period_is_offered_whole_and_taken() {
    let db = shared_card_db().expect("the card fixture");
    let board = SliverBoard::new(db, Beside::CircleAndShield, PlayerId(1));
    a_sliver_period_beside_floating_effects_is_offered_and_taken(&board, 3, |taken| {
        let mut stored = vec![CHOSEN_SOURCE];
        stored.resize(2 + taken, SHIELD);
        stored
    });
}

/// CR 609.7a + CR 111.7: Circle of Protection: Red chooses a Dragon made by Avatar Roku ("{8}:
/// Create a 4/4 red Dragon creature token with flying and firebending 4.") that is then
/// sacrificed, so its effect names a source only last-known information (CR 608.2h) describes,
/// and the period that installs it is not covered while the periods beside it are.
#[test]
fn a_chosen_source_effect_naming_a_departed_dragon_keeps_the_period_refused() {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let roku = scenario.add_real_card(P0, "Avatar Roku", Zone::Battlefield, db);
    let altar = scenario.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db);
    let circle = scenario.add_real_card(P0, "Circle of Protection: Red", Zone::Battlefield, db);
    // Four periods spend six mana each, the Circle one, and the last must still pay {8}.
    let forests = [(); 27].map(|()| scenario.add_basic_land(P0, ManaColor::Green));
    let answers = untargeted();
    let mut board = in_mode(scenario.build().state(), LoopDetectionMode::Off);
    for forest in forests {
        tap_for_mana(&mut board, forest);
        settle(&mut board, &answers);
    }
    let periods = |activated: bool| {
        let mut state = board.clone();
        let mut chosen = None;
        let frames = [false, activated, false, false].map(|activated| {
            activate_ability(&mut state, roku);
            settle(&mut state, &answers);
            let dragon = *tokens(&state, "Dragon").last().expect("a Dragon");
            if activated {
                activate_ability(&mut state, circle);
                let source = Answers {
                    aim: Some(TargetRef::Object(dragon)),
                    ..answers.clone()
                };
                settle(&mut state, &source);
                chosen = Some(dragon);
            }
            tap_for_mana(&mut state, altar);
            let sacrificed = Answers {
                cost: Some(dragon),
                ..answers.clone()
            };
            settle(&mut state, &sacrificed);
            state.clone()
        });
        (frames, chosen)
    };
    let covered = ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::History));

    let (frames, chosen) = periods(true);
    let dragon = chosen.expect("the chosen Dragon");
    for frame in &frames[1..] {
        assert_eq!(floating(frame), [CHOSEN_SOURCE]);
        let effect = &frame.pending_damage_replacements[0];
        assert!(matches!(
            &effect.damage_source_filter,
            Some(TargetFilter::And { filters })
                if filters.contains(&TargetFilter::SpecificObject { id: dragon })
        ));
        assert_eq!(effect.source_object, Some(circle));
        assert!(frame.lki_cache.contains_key(&dragon));
    }
    assert!(frames.iter().all(|frame| {
        frame.objects[&circle].zone == Zone::Battlefield && !frame.objects.contains_key(&dragon)
    }));
    let (plain, _) = periods(false);
    assert_eq!(
        [
            certified(&frames[..3], &[]),
            certified(&frames[1..], &[]),
            certified(&plain[..3], &[]),
        ],
        [
            ObjectGrowthVerdict::ResourceRecurrence(None),
            covered.clone(),
            covered,
        ]
    );
}

/// A token copy of Noble Vestige made by Cackling Counterpart ("Create a token that's a copy of
/// target creature you control.") under Madrush Cyclops ("Creatures you control have haste.")
/// shields P0 and is then sacrificed, so the shield's host has left and its last-known
/// information (CR 608.2h) could be asked for, and the periods that leave such shields stay
/// refused while the same periods without the activation are covered.
#[test]
fn a_shield_made_by_a_departed_token_keeps_the_period_refused() {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vestige = scenario.add_real_card(P0, "Noble Vestige", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Madrush Cyclops", Zone::Battlefield, db);
    let altar = scenario.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db);
    let counterparts =
        [(); 5].map(|()| scenario.add_real_card(P0, "Cackling Counterpart", Zone::Hand, db));
    let islands = [(); 15].map(|()| scenario.add_basic_land(P0, ManaColor::Blue));
    let answers = Answers {
        color: ManaType::Blue,
        ..untargeted()
    };
    let mut board = in_mode(scenario.build().state(), LoopDetectionMode::Off);
    for island in islands {
        tap_for_mana(&mut board, island);
        settle(&mut board, &answers);
    }
    // Casts a Counterpart on the nontoken Vestige; returns the token it makes.
    let copy = |state: &mut GameState, counterpart: ObjectId| {
        cast_on(state, counterpart, vestige);
        *tokens(state, "Noble Vestige")
            .last()
            .expect("a token Vestige")
    };
    let first = copy(&mut board, counterparts[0]);
    // Each frame with the tokens sacrificed up to it, oldest first.
    let periods = |shielded: bool| {
        let mut state = board.clone();
        let mut token = first;
        let mut sacrificed = Vec::new();
        let frames: Vec<GameState> = counterparts[1..]
            .iter()
            .map(|counterpart| {
                if shielded {
                    activate_ability(&mut state, token);
                    let shield = Answers {
                        aim: Some(TargetRef::Player(P0)),
                        ..answers.clone()
                    };
                    settle(&mut state, &shield);
                }
                let next = copy(&mut state, *counterpart);
                tap_for_mana(&mut state, altar);
                let paid = Answers {
                    cost: Some(token),
                    ..answers.clone()
                };
                settle(&mut state, &paid);
                assert!(has_priority(&state) && state.stack.is_empty());
                sacrificed.push(std::mem::replace(&mut token, next));
                state.clone()
            })
            .collect();
        (frames, sacrificed)
    };
    let refused = ObjectGrowthVerdict::ResourceRecurrence(None);
    let covered = ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::History));

    let (frames, hosts) = periods(true);
    for (period, frame) in frames.iter().enumerate() {
        assert_eq!(floating(frame), vec![SHIELD; period + 1]);
        let sources: Vec<Option<ObjectId>> = frame
            .pending_damage_replacements
            .iter()
            .map(|effect| effect.source_object)
            .collect();
        let departed: Vec<Option<ObjectId>> = hosts[..=period].iter().copied().map(Some).collect();
        assert_eq!(sources, departed);
        assert!(hosts[..=period]
            .iter()
            .all(|host| !frame.objects.contains_key(host)));
        assert_eq!(tokens(frame, "Noble Vestige").len(), 1);
    }
    let (plain, _) = periods(false);
    assert_eq!(
        [
            certified(&frames[..3], &counterparts),
            certified(&frames[1..], &counterparts),
            certified(&plain[..3], &counterparts),
            certified(&plain[1..], &counterparts),
        ],
        [refused.clone(), refused, covered.clone(), covered]
    );
}
