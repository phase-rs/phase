//! CR 732.2a: the play trace records every play and answer of the step, keyed by ability, and
//! names the candidate periods a shortcut may repeat (CR 104.4b, CR 732.1b).

use engine::ai_support::legal_actions;
use engine::database::card_db::CardDatabase;
use engine::game::casting::can_cast_object_now;
use engine::game::deck_loading::create_object_from_card_face;
use engine::game::effects::attach::attach_to;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::perf_counters::{play_trace_counters, PlayTraceCounters};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::{add_to_zone, remove_from_zone};
use engine::game::{
    play_trace_view, AnswerOptionality, EntryKind, NamedSpan, NamingCause, PlayLocus,
    PlayTraceView, TraceEntry,
};
use engine::types::ability::{AbilityCost, AbilityKind, TargetRef};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastPaymentMode, GameState, LoopDetectionMode, ManaChoice, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::food_chain_board::{self, BoardCMember};
use crate::loop_shortcut_mana_engine::{drive_one_period, mana_ability_index, untap_ability_index};
use crate::mana_ability_mana_payment_window::mana_ability_costing;
use crate::support::shared_card_db;

const BEAT_CAP: usize = 400;

/// One trace entry as a row compares it.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    Play(usize),
    Resolve(usize),
    Answer(PlayerId, &'static str),
}

fn trace_of(state: &GameState) -> PlayTraceView {
    play_trace_view(state).expect("the trace recorded this window")
}

fn steps(view: &PlayTraceView, span: &NamedSpan) -> Vec<Step> {
    entry_steps(view, span.start..span.end)
}

fn entry_steps(view: &PlayTraceView, range: std::ops::Range<usize>) -> Vec<Step> {
    view.entries[range]
        .iter()
        .map(|entry| match &entry.kind {
            EntryKind::Play { node, .. } => Step::Play(*node),
            EntryKind::Resolution { node } => Step::Resolve(*node),
            EntryKind::Answer { action, .. } => Step::Answer(entry.seat, action.into()),
        })
        .collect()
}

fn plays(view: &PlayTraceView, span: &NamedSpan) -> Vec<usize> {
    steps(view, span)
        .into_iter()
        .filter_map(|step| match step {
            Step::Play(node) => Some(node),
            _ => None,
        })
        .collect()
}

fn resolutions(view: &PlayTraceView, span: &NamedSpan) -> usize {
    steps(view, span)
        .iter()
        .filter(|step| matches!(step, Step::Resolve(_)))
        .count()
}

/// The node of the first play whose action `pick` accepts.
fn play_node(view: &PlayTraceView, pick: impl Fn(&GameAction) -> bool) -> usize {
    view.entries
        .iter()
        .find_map(|entry| match &entry.kind {
            EntryKind::Play { action, node, .. } if pick(action) => Some(*node),
            _ => None,
        })
        .expect("the trace holds the play")
}

fn activates(source: ObjectId) -> impl Fn(&GameAction) -> bool {
    move |action| matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == source)
}

fn activates_index(source: ObjectId, index: usize) -> impl Fn(&GameAction) -> bool {
    move |action| {
        matches!(action, GameAction::ActivateAbility { source_id, ability_index }
            if *source_id == source && *ability_index == index)
    }
}

fn casts(object: ObjectId) -> impl Fn(&GameAction) -> bool {
    move |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == object)
}

pub(crate) fn place(
    state: &mut GameState,
    player: PlayerId,
    name: &str,
    db: &CardDatabase,
) -> ObjectId {
    let face = db.get_face_by_name(name).expect("card in fixture");
    let id = create_object_from_card_face(state, face, player);
    remove_from_zone(state, id, Zone::Library, player);
    add_to_zone(state, id, Zone::Battlefield, player);
    state.objects.get_mut(&id).expect("placed").zone = Zone::Battlefield;
    id
}

pub(crate) fn ability(state: &GameState, id: ObjectId, mana: bool) -> usize {
    state.objects[&id]
        .abilities
        .iter()
        .position(|a| a.kind == AbilityKind::Activated && is_mana_ability(a) == mana)
        .expect("the ability")
}

fn act(runner: &mut GameRunner, action: GameAction) {
    let shown = format!("{action:?}");
    runner
        .act(action)
        .unwrap_or_else(|error| panic!("{shown} rejected: {error:?}"));
}

pub(crate) fn activate(runner: &mut GameRunner, source: ObjectId, index: usize) {
    act(
        runner,
        GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
        },
    );
}

pub(crate) fn cast(
    runner: &mut GameRunner,
    object: ObjectId,
    targets: Vec<ObjectId>,
    mode: CastPaymentMode,
) {
    let card_id = runner.state().objects[&object].card_id;
    act(
        runner,
        GameAction::CastSpell {
            object_id: object,
            card_id,
            targets,
            payment_mode: mode,
        },
    );
}

/// Answers a prompt with the highest-scoring non-pass legal action.
fn answer(runner: &mut GameRunner, score: &dyn Fn(&GameAction) -> i32) {
    let best = legal_actions(runner.state())
        .into_iter()
        .filter(|action| {
            !matches!(
                action,
                GameAction::PassPriority
                    | GameAction::CancelCast
                    | GameAction::DeclineShortcut
                    | GameAction::DeclareShortcut { .. }
                    | GameAction::UntapLandForMana { .. }
                    | GameAction::UnspendPoolMana { .. }
                    | GameAction::BackToManaPayment
                    | GameAction::Concede { .. }
            )
        })
        .enumerate()
        .max_by_key(|(at, action)| (score(action), std::cmp::Reverse(*at)))
        .map(|(_, action)| action)
        .unwrap_or(GameAction::PassPriority);
    act(runner, best);
}

/// Passes and answers until an empty-stack priority window or an offer.
pub(crate) fn settle(runner: &mut GameRunner, score: &dyn Fn(&GameAction) -> i32) {
    for _ in 0..BEAT_CAP {
        match &runner.state().waiting_for {
            WaitingFor::LoopShortcut { .. } | WaitingFor::GameOver { .. } => return,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => act(runner, GameAction::PassPriority),
            _ => answer(runner, score),
        }
    }
    panic!("the drive did not settle");
}

pub(crate) fn names(ids: &[ObjectId]) -> impl Fn(&GameAction) -> i32 + '_ {
    move |action| {
        let related = action.related_object_ids();
        ids.iter().filter(|id| related.contains(id)).count() as i32
    }
}

pub(crate) fn chooses_color(color: ManaType) -> impl Fn(&GameAction) -> i32 {
    move |action| {
        i32::from(
            matches!(action, GameAction::ChooseManaColor { choice: ManaChoice::SingleColor(c), .. } if *c == color),
        )
    }
}

pub(crate) fn is_offer(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::LoopShortcut { .. })
}

/// The span `runner`'s offer stands for, once it is declined and the drive settled; `None` with no
/// offer. Declining restarts the trace.
fn decline_offer(runner: &mut GameRunner, score: &dyn Fn(&GameAction) -> i32) -> Option<NamedSpan> {
    if !is_offer(runner.state()) {
        return None;
    }
    let span = trace_of(runner.state()).offered;
    act(runner, GameAction::DeclineShortcut);
    settle(runner, score);
    span
}

/// A play or resolution the base's recorded period names, for comparison up to rotation.
#[derive(Debug, Clone, PartialEq)]
enum PeriodStep {
    Cast(CardId),
    Activate(ObjectId, Option<usize>),
}

fn rotation_of(a: &[PeriodStep], b: &[PeriodStep]) -> bool {
    a.len() == b.len() && (a.is_empty() || (0..a.len()).any(|k| a[k..].iter().chain(&a[..k]).eq(b)))
}

/// Row 6's read: some named span's plays are the base's period `wanted` up to rotation, and a
/// period that `resolves` a trigger has one resolving in that span.
fn names_the_period(state: &GameState, wanted: &[PeriodStep], resolves: bool) -> bool {
    let Some(view) = play_trace_view(state) else {
        return false;
    };
    view.named.iter().any(|span| {
        let got: Vec<PeriodStep> = view.entries[span.start..span.end]
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Play { action, .. } => Some(match action {
                    GameAction::CastSpell { card_id, .. } => PeriodStep::Cast(*card_id),
                    GameAction::ActivateAbility {
                        source_id,
                        ability_index,
                    } => PeriodStep::Activate(*source_id, Some(*ability_index)),
                    GameAction::TapLandForMana { selection }
                    | GameAction::ActivateManaSource { selection } => {
                        PeriodStep::Activate(selection.source.object_id, selection.ability_index)
                    }
                    other => panic!("no period step for {other:?}"),
                }),
                _ => None,
            })
            .collect();
        rotation_of(wanted, &got) && (!resolves || resolutions(&view, span) > 0)
    })
}

// ---------------------------------------------------------------------------------------------
// Boards
// ---------------------------------------------------------------------------------------------

/// Basalt Monolith ("{T}: Add {C}{C}{C}." / "{3}: Untap this artifact.") with Power Artifact
/// ("Enchanted artifact's activated abilities cost {2} less to activate. This effect can't
/// reduce the mana in that cost to less than one mana.") attached, `wastes` Wastes, and
/// `bears` Grizzly Bears per seat.
fn basalt_board(wastes: usize, bears: usize, db: &CardDatabase) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let basalt = scenario.add_real_card(P0, "Basalt Monolith", Zone::Battlefield, db);
    for _ in 0..wastes {
        scenario.add_real_card(P0, "Wastes", Zone::Battlefield, db);
    }
    for seat in [P0, P1] {
        for _ in 0..bears {
            scenario.add_real_card(seat, "Grizzly Bears", Zone::Battlefield, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let power = place(runner.state_mut(), P0, "Power Artifact", db);
    attach_to(runner.state_mut(), power, basalt);
    (runner, basalt)
}

/// Kiki-Jiki, Mirror Breaker ("{T}: Create a token that's a copy of target nonlegendary creature
/// you control, except it has haste. Sacrifice it at the beginning of the next end step.") and
/// Deceiver Exarch ("When this creature enters, choose one — Untap target permanent you control;
/// or Tap target permanent an opponent controls.").
fn kiki_board(mode: LoopDetectionMode, db: &CardDatabase) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kiki = scenario.add_real_card(P0, "Kiki-Jiki, Mirror Breaker", Zone::Battlefield, db);
    let exarch = scenario.add_real_card(P0, "Deceiver Exarch", Zone::Battlefield, db);
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Mountain", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kiki, exarch)
}

/// One Kiki-Jiki cycle: copy the Exarch card, untap Kiki-Jiki with the copy's trigger.
fn kiki_cycle(runner: &mut GameRunner, kiki: ObjectId, exarch: ObjectId) {
    let index = ability(runner.state(), kiki, false);
    activate(runner, kiki, index);
    // Kiki-Jiki is legendary, so only the trigger's "untap target permanent" can name it.
    settle(runner, &|action| match action {
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id)),
        } if *id == kiki => 3,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id)),
        } if *id == exarch => 2,
        GameAction::SelectModes { indices } if indices == &[0] => 2,
        _ => 0,
    });
}

/// Phyrexian Altar ("Sacrifice a creature: Add one mana of any color."), Gravecrawler in the
/// graveyard ("You may cast this card from your graveyard as long as you control a Zombie."),
/// Walking Corpse (the Zombie), a Swamp, and `payoff`.
pub(crate) fn altar_board(
    payoff: Option<&str>,
    db: &CardDatabase,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Walking Corpse", Zone::Battlefield, db);
    let gravecrawler = scenario.add_real_card(P0, "Gravecrawler", Zone::Graveyard, db);
    scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    if let Some(payoff) = payoff {
        scenario.add_real_card(P0, payoff, Zone::Battlefield, db);
    }
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    (runner, altar, gravecrawler)
}

/// Grizzly Bears enchanted by Presence of Gond ("Enchanted creature has '{T}: Create a 1/1 green
/// Elf Warrior creature token.'") and Intruder Alarm ("Creatures don't untap during their
/// controllers' untap steps. Whenever a creature enters, untap all creatures.").
fn gond_board(
    with_elves: bool,
    db: &CardDatabase,
) -> (GameRunner, ObjectId, Option<(ObjectId, ObjectId)>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Intruder Alarm", Zone::Battlefield, db);
    // Llanowar Elves: "{T}: Add {G}." Mobilize: "Untap all creatures you control."
    let extra = with_elves.then(|| {
        (
            scenario.add_real_card(P0, "Llanowar Elves", Zone::Battlefield, db),
            scenario.add_real_card(P0, "Mobilize", Zone::Hand, db),
        )
    });
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Forest", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let gond = place(runner.state_mut(), P0, "Presence of Gond", db);
    attach_to(runner.state_mut(), gond, bears);
    (runner, bears, extra)
}

fn restore_dump(gz: &[u8]) -> GameState {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture inflates");
    let envelope: serde_json::Value = serde_json::from_str(&json).expect("dump parses");
    serde_json::from_value::<engine::types::game_state::PersistedGameState>(
        envelope["gameState"].clone(),
    )
    .expect("gameState deserializes")
    .into_game_state()
    .expect("dump restores")
}

/// Board A (Abdel Adrian, Gorion's Ward) and Board B (Preston, the Vanisher) driven by their
/// committed policies to their first offer.
#[derive(Clone, Copy, Debug)]
enum TriggerBoard {
    A,
    B,
}

type Policy = Box<dyn FnMut(&GameState) -> Option<GameAction>>;

struct TriggerDrive {
    runner: GameRunner,
    policy: Policy,
}

fn trigger_drive(board: TriggerBoard) -> Option<TriggerDrive> {
    const ACCEPTS: usize = 8;
    match board {
        TriggerBoard::A => {
            let built = crate::abdel_adrian_animate_dead_altar_board::build()?;
            let mut runner = built.runner;
            runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
            let returned = runner.state().objects[&built.animate_dead].card_id;
            cast(
                &mut runner,
                built.animate_dead,
                vec![built.abdel],
                CastPaymentMode::Auto,
            );
            let mut left = ACCEPTS;
            Some(TriggerDrive {
                runner,
                policy: Box::new(move |state| {
                    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
                        return None;
                    };
                    let cards = if left > 0 {
                        left -= 1;
                        cards
                            .iter()
                            .copied()
                            .filter(|id| state.objects[id].card_id == returned)
                            .collect()
                    } else {
                        Vec::new()
                    };
                    Some(GameAction::SelectCards { cards })
                }),
            })
        }
        TriggerBoard::B => {
            let built = crate::loop_period_trigger_driven_arming::build_board_b()?;
            let mut runner = built.runner;
            cast(
                &mut runner,
                built.animate_dead,
                vec![built.felidar],
                CastPaymentMode::Auto,
            );
            let felidar = built.felidar;
            let mut left = ACCEPTS;
            Some(TriggerDrive {
                runner,
                policy: Box::new(move |state| {
                    let legal = legal_actions(state);
                    if left > 0 {
                        if let Some(aimed) = legal.iter().find(|action| {
                            matches!(action, GameAction::ChooseTarget {
                                target: Some(TargetRef::Object(id)),
                            } if *id == felidar)
                        }) {
                            return Some(aimed.clone());
                        }
                    }
                    let accept = left > 0;
                    let decided = legal.iter().find(|action| {
                        matches!(action, GameAction::DecideOptionalEffect { accept: answered }
                            if *answered == accept)
                    })?;
                    if accept {
                        left -= 1;
                    }
                    Some(decided.clone())
                }),
            })
        }
    }
}

impl TriggerDrive {
    fn next_action(&mut self) -> GameAction {
        let state = self.runner.state();
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return GameAction::PassPriority;
        }
        crate::loop_period_accessor_answers::altar_resolves_first(state)
            .or_else(|| (self.policy)(state))
            .unwrap_or_else(|| {
                legal_actions(state)
                    .into_iter()
                    .find(|action| !matches!(action, GameAction::PassPriority))
                    .expect("the prompt offers a legal action")
            })
    }
}

// ---------------------------------------------------------------------------------------------
// Row 1 — the trace names each member's period
// ---------------------------------------------------------------------------------------------

#[test]
fn play_trace_names_each_members_period_basalt_power_artifact() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = crate::loop_shortcut_mana_engine::setup(true, LoopDetectionMode::Interactive, db);
    let mana = mana_ability_index(rig.runner.state(), rig.basalt).expect("{T}: Add {C}{C}{C}");
    let untap = untap_ability_index(rig.runner.state(), rig.basalt).expect("{3}: Untap");
    drive_one_period(&mut rig, mana, untap);
    assert!(
        is_offer(rig.runner.state()),
        "reach: the base offers for one period"
    );

    let view = trace_of(rig.runner.state());
    let (m, u) = (
        play_node(&view, activates_index(rig.basalt, mana)),
        play_node(&view, activates_index(rig.basalt, untap)),
    );
    let span = view
        .named
        .last()
        .expect("a candidate is named at the offer");
    assert_eq!(span.cause, NamingCause::Restored);
    assert_eq!(
        steps(&view, span),
        [
            Step::Play(m),
            Step::Play(u),
            Step::Answer(P0, "PassPriority"),
            Step::Answer(P1, "PassPriority"),
        ],
        "the span is one period, both seats' passes included"
    );
}

#[test]
fn play_trace_names_each_members_period_food_chain_c1_c2() {
    for member in [
        BoardCMember::C1EternalScourge,
        BoardCMember::C2SqueeTheImmortal,
    ] {
        let Some(mut board) = food_chain_board::build(member) else {
            return;
        };
        board.runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
        let mut previous_end = 0;
        for cycle in 0..2 {
            food_chain_board::exile_with_food_chain(
                &mut board.runner,
                board.food_chain,
                board.creature,
                member.mana(),
            );
            food_chain_board::cast_spell(&mut board.runner, board.creature)
                .expect("cast from exile");
            settle(&mut board.runner, &|_| 0);
            let state = board.runner.state();
            assert_eq!(
                state.objects[&board.creature].zone,
                Zone::Battlefield,
                "reach: {member:?} resolved"
            );

            let view = trace_of(state);
            let (fc, creature) = (
                play_node(&view, activates(board.food_chain)),
                play_node(&view, casts(board.creature)),
            );
            let span = view.named.last().expect("a candidate is named");
            assert_eq!(
                span.cause,
                NamingCause::Restored,
                "{member:?} cycle {cycle}"
            );
            assert_eq!(
                steps(&view, span),
                [
                    Step::Play(fc),
                    Step::Answer(P0, "SelectCards"),
                    Step::Answer(P0, "ChooseManaColor"),
                    Step::Play(creature),
                    Step::Answer(P0, "PassPriority"),
                    Step::Answer(P1, "PassPriority"),
                    Step::Answer(PlayerId(2), "PassPriority"),
                    Step::Answer(PlayerId(3), "PassPriority"),
                ],
                "{member:?} cycle {cycle}: activation, cast and every seat's answers"
            );
            assert_eq!(
                span.start, previous_end,
                "{member:?}: each cycle names its own period"
            );
            previous_end = span.end;
            // CR 732.2a: an offered cycle is declined, which restarts the trace.
            if decline_offer(&mut board.runner, &|_| 0).is_some() {
                previous_end = 0;
            }
        }
    }
}

#[test]
fn play_trace_names_each_members_period_altar_gravecrawler_payoffs() {
    let Some(db) = shared_card_db() else { return };
    for (payoff, resolved) in [
        (None, 0),
        // "Whenever this creature or another creature you control dies, each opponent loses 1
        // life and you gain 1 life."
        (Some("Zulaport Cutthroat"), 1),
        // "Whenever another permanent you control enters, each opponent mills a card."
        (Some("Altar of the Brood"), 1),
    ] {
        let (mut runner, altar, gravecrawler) = altar_board(payoff, db);
        let score = |action: &GameAction| {
            2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
        };
        cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
        settle(&mut runner, &score);
        let index = ability(runner.state(), altar, true);
        activate(&mut runner, altar, index);
        settle(&mut runner, &score);
        assert_eq!(
            runner.state().objects[&gravecrawler].zone,
            Zone::Graveyard,
            "reach: sacrificed"
        );

        let view = trace_of(runner.state());
        let (crawl, sac) = (
            play_node(&view, casts(gravecrawler)),
            play_node(&view, activates(altar)),
        );
        let span = view.named.last().expect("a candidate is named");
        assert_eq!(span.cause, NamingCause::Restored, "{payoff:?}");
        assert_eq!(
            plays(&view, span),
            [crawl, sac],
            "{payoff:?}: the cast and the sacrifice"
        );
        assert_eq!(
            resolutions(&view, span),
            resolved,
            "{payoff:?}: the payoff's resolution"
        );
        assert!(
            steps(&view, span).contains(&Step::Answer(P0, "SelectCards")),
            "{payoff:?}: the sacrifice answer"
        );
    }
}

#[test]
fn play_trace_names_each_members_period_kiki_exarch() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, kiki, exarch) = kiki_board(LoopDetectionMode::Interactive, db);
    let mut offers = Vec::new();
    for cycle in 0..3 {
        kiki_cycle(&mut runner, kiki, exarch);
        assert!(
            !runner.state().objects[&kiki].tapped,
            "reach: cycle {cycle} untapped Kiki-Jiki"
        );
        let view = trace_of(runner.state());
        let (k, x) = (play_node(&view, activates(kiki)), {
            view.entries
                .iter()
                .find_map(|entry| match entry.kind {
                    EntryKind::Resolution { node } => Some(node),
                    _ => None,
                })
                .expect("the Exarch copy's trigger resolved")
        });
        let span = view.named.last().expect("a candidate is named");
        assert_eq!(span.cause, NamingCause::Restored);
        // A single legal target is chosen without a prompt (CR 601.2c), so cycle 0 has no target
        // answer for Kiki-Jiki's ability; every later cycle has the copy as a second target.
        let target = (cycle > 0).then_some(Step::Answer(P0, "ChooseTarget"));
        let expected: Vec<Step> = [Step::Play(k)]
            .into_iter()
            .chain(target)
            .chain([
                Step::Answer(P0, "PassPriority"),
                Step::Answer(P1, "PassPriority"),
                Step::Answer(P0, "SelectModes"),
                Step::Answer(P0, "ChooseTarget"),
                Step::Answer(P0, "PassPriority"),
                Step::Answer(P1, "PassPriority"),
                Step::Resolve(x),
            ])
            .collect();
        assert_eq!(steps(&view, span), expected, "cycle {cycle}");
        if let Some(offered) = decline_offer(&mut runner, &|_| 0) {
            offers.push((cycle, offered.cause));
        }
    }
    // CR 117.3b/c: the repeated trigger's span is offered at the window after it; once that offer
    // is declined, the restored activation's span is offered.
    assert_eq!(
        offers,
        [(1, NamingCause::Repeat), (2, NamingCause::Restored)]
    );
}

#[test]
fn play_trace_names_each_members_period_trigger_boards() {
    for board in [TriggerBoard::A, TriggerBoard::B] {
        let Some(mut drive) = trigger_drive(board) else {
            return;
        };
        for _ in 0..BEAT_CAP {
            if is_offer(drive.runner.state()) {
                break;
            }
            let action = drive.next_action();
            act(&mut drive.runner, action);
        }
        let state = drive.runner.state();
        assert!(is_offer(state), "reach: {board:?} reaches its base offer");
        let view = trace_of(state);
        let span = view
            .named
            .iter()
            .find(|span| span.cause == NamingCause::TriggerTop)
            .unwrap_or_else(|| panic!("{board:?}: the trigger-top window named {:?}", view.named));
        assert!(
            matches!(view.entries[span.start].kind, EntryKind::Resolution { .. }),
            "{board:?}: the span starts at the trigger's previous resolution"
        );
        assert!(
            view.entries[span.start..span.end]
                .iter()
                .any(|entry| matches!(
                    entry.kind,
                    EntryKind::Answer {
                        optional: AnswerOptionality::Optional,
                        ..
                    }
                )),
            "{board:?}: the span holds the optional answer"
        );
        assert!(
            names_the_period(state, &[], true),
            "{board:?}: row 6 — the base's trigger-driven period is named"
        );
    }
}

#[test]
fn play_trace_names_each_members_period_grand_architect_pili_pala() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Grand Architect: "{U}: Target artifact creature becomes blue until end of turn." and "Tap an
    // untapped blue creature you control: Add {C}{C}. Spend this mana only to cast artifact
    // spells or activate abilities of artifacts." Pili-Pala: "{2}, {Q}: Add one mana of any
    // color."
    let architect = scenario.add_real_card(P0, "Grand Architect", Zone::Battlefield, db);
    let pili = scenario.add_real_card(P0, "Pili-Pala", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let prefer_pili =
        |action: &GameAction| 2 * names(&[pili])(action) + chooses_color(ManaType::Blue)(action);
    let blue = ability(runner.state(), architect, false);
    activate(&mut runner, architect, blue);
    settle(&mut runner, &prefer_pili);
    let (tap_blue, untap) = (
        ability(runner.state(), architect, true),
        ability(runner.state(), pili, true),
    );
    let mut offered = None;
    for cycle in 0..3 {
        activate(&mut runner, architect, tap_blue);
        settle(&mut runner, &prefer_pili);
        if is_offer(runner.state()) {
            offered = Some(cycle);
            break;
        }
        activate(&mut runner, pili, untap);
        settle(&mut runner, &prefer_pili);
        assert!(
            !is_offer(runner.state()),
            "cycle {cycle}: no offer after Pili-Pala"
        );
    }
    // CR 117.3c: the window after the second Grand Architect activation offers the period it and
    // Pili-Pala's activation repeat.
    assert_eq!(
        offered,
        Some(1),
        "reach: the second cycle's activation is offered"
    );
    let view = trace_of(runner.state());
    let span = view.offered.expect("the offered span");
    assert_eq!(span.cause, NamingCause::Repeat);
    assert_eq!(
        plays(&view, &span),
        [
            play_node(&view, |action| *action
                == GameAction::ActivateAbility {
                    source_id: architect,
                    ability_index: tap_blue,
                }),
            play_node(&view, activates(pili)),
        ],
        "the tap for {{C}}{{C}} and Pili-Pala's untap"
    );
}

#[test]
fn play_trace_names_each_members_period_food_chain_griffin_scourge() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Food Chain: "Exile a creature you control: Add X mana of any one color, where X is 1 plus
    // the exiled creature's mana value. Spend this mana only to cast creature spells."
    let food_chain = scenario.add_real_card(P0, "Food Chain", Zone::Battlefield, db);
    // Eternal Scourge / Misthollow Griffin: "You may cast this card from exile."
    let scourge = scenario.add_real_card(P0, "Eternal Scourge", Zone::Battlefield, db);
    let griffin = scenario.add_real_card(P0, "Misthollow Griffin", Zone::Exile, db);
    for seat in [P0, P1, PlayerId(2), PlayerId(3)] {
        for _ in 0..8 {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    // CR 601.2g + CR 605.3a: each creature's mana is paid by activating Food Chain inside the
    // cast's payment, exiling the other creature.
    let cast_paying_with_food_chain =
        |runner: &mut GameRunner, creature: ObjectId, other: ObjectId| {
            cast(runner, creature, vec![], CastPaymentMode::Manual);
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
                "reach: a payment prompt"
            );
            activate(runner, food_chain, 0);
            act(runner, GameAction::SelectCards { cards: vec![other] });
            act(
                runner,
                GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(ManaType::Blue),
                    count: 1,
                },
            );
            act(runner, GameAction::PassPriority);
            settle(runner, &|_| 0);
        };
    cast_paying_with_food_chain(&mut runner, griffin, scourge);
    let view = trace_of(runner.state());
    let fc = play_node(&view, activates(food_chain));
    let span = view
        .named
        .last()
        .expect("a candidate is named after the Griffin resolves");
    assert_eq!(span.cause, NamingCause::Restored);
    assert!(
        matches!(&view.entries[span.start].kind, EntryKind::Play { node, .. } if *node == fc),
        "the span starts at the in-payment Food Chain activation"
    );

    cast_paying_with_food_chain(&mut runner, scourge, griffin);
    let state = runner.state();
    assert!(
        can_cast_object_now(state, P0, griffin),
        "reach: the Griffin's exile cast is legal"
    );
    let view = trace_of(state);
    // The window's first span starts at the earliest restored play, the Griffin's cast.
    let span = view
        .named
        .iter()
        .find(|span| span.end == view.entries.len())
        .expect("a candidate is named after the Scourge resolves");
    assert_eq!(span.cause, NamingCause::Restored);
    let (g, s) = (
        play_node(&view, casts(griffin)),
        play_node(&view, casts(scourge)),
    );
    assert_eq!(
        plays(&view, span),
        [g, fc, s, fc],
        "two different casts, one Food Chain node"
    );
    assert_eq!(
        view.offered,
        Some(*span),
        "CR 732.2a: the cycle is offered at its window"
    );
}

// ---------------------------------------------------------------------------------------------
// Row 1 hostiles
// ---------------------------------------------------------------------------------------------

#[test]
fn play_trace_names_nothing_for_worldgorger_animate_dead() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Worldgorger Dragon: "When this creature enters, exile all other permanents you control.
    // When this creature leaves the battlefield, return the exiled cards to the battlefield
    // under their owners' control." Animate Dead returns it; nothing is activated.
    let dragon = scenario.add_real_card(P0, "Worldgorger Dragon", Zone::Graveyard, db);
    let animate = scenario.add_real_card(P0, "Animate Dead", Zone::Hand, db);
    for _ in 0..2 {
        scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    }
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    cast(&mut runner, animate, vec![dragon], CastPaymentMode::Auto);
    let mut top_trigger_window = false;
    for _ in 0..60 {
        let state = runner.state();
        match &state.waiting_for {
            WaitingFor::GameOver { .. } => break,
            WaitingFor::Priority { .. } if state.stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                top_trigger_window |= matches!(
                    state.stack.back().map(|e| &e.kind),
                    Some(StackEntryKind::TriggeredAbility { .. })
                ) && play_trace_view(state).is_some_and(|v| {
                    v.entries
                        .iter()
                        .any(|e| matches!(e.kind, EntryKind::Resolution { .. }))
                });
                act(&mut runner, GameAction::PassPriority);
            }
            WaitingFor::LoopShortcut { .. } => act(&mut runner, GameAction::DeclineShortcut),
            _ => answer(&mut runner, &names(&[dragon])),
        }
    }
    let view = trace_of(runner.state());
    let mut occurrences = vec![0usize; view.node_count];
    for entry in &view.entries {
        if let EntryKind::Resolution { node } = entry.kind {
            occurrences[node] += 1;
        }
    }
    assert!(
        top_trigger_window,
        "reach: a resolved trigger node stood on top at a priority window"
    );
    assert!(
        occurrences.iter().filter(|&&n| n >= 3).count() >= 2,
        "reach: the enters triggers recur over three cycles: {occurrences:?}"
    );
    assert_eq!(
        view.named,
        [],
        "a loop of mandatory nodes names nothing (CR 732.4)"
    );
}

#[test]
fn play_trace_keys_a_delayed_trigger_by_its_creator_across_cycles() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Animate Dead: "... When this Aura leaves the battlefield, that creature's controller
    // sacrifices it." Each cycle's enters trigger creates that delayed ability anew.
    let dragon = scenario.add_real_card(P0, "Worldgorger Dragon", Zone::Graveyard, db);
    let animate = scenario.add_real_card(P0, "Animate Dead", Zone::Hand, db);
    for _ in 0..2 {
        scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    }
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    cast(&mut runner, animate, vec![dragon], CastPaymentMode::Auto);
    let (mut delayed, mut animate_printed) = (Vec::new(), Vec::new());
    for _ in 0..60 {
        let state = runner.state();
        match &state.waiting_for {
            WaitingFor::GameOver { .. } => break,
            WaitingFor::Priority { .. } if state.stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                let top = match state.stack.back().map(|e| &e.kind) {
                    Some(StackEntryKind::TriggeredAbility { ability, .. })
                        if ability.source_id == animate =>
                    {
                        Some(ability.delayed_origin.is_some())
                    }
                    _ => None,
                };
                let before = trace_of(state).entries.len();
                act(&mut runner, GameAction::PassPriority);
                let resolved = trace_of(runner.state()).entries[before..]
                    .iter()
                    .find_map(|e| match e.kind {
                        EntryKind::Resolution { node } => Some(node),
                        _ => None,
                    });
                match (top, resolved) {
                    (Some(true), Some(node)) => delayed.push(node),
                    (Some(false), Some(node)) => animate_printed.push(node),
                    _ => {}
                }
            }
            WaitingFor::LoopShortcut { .. } => act(&mut runner, GameAction::DeclineShortcut),
            _ => answer(&mut runner, &names(&[dragon])),
        }
    }
    assert!(
        delayed.len() >= 3 && !animate_printed.is_empty(),
        "reach: three cycles of Animate Dead's delayed and enters triggers resolved: \
         {delayed:?} {animate_printed:?}"
    );
    assert!(
        delayed.iter().all(|&node| node == delayed[0]),
        "CR 603.7a: one creator's delayed ability is one node across cycles: {delayed:?}"
    );
    assert!(
        !animate_printed.contains(&delayed[0]),
        "the delayed ability is not its creator's printed trigger: {delayed:?} {animate_printed:?}"
    );
}

#[test]
fn play_trace_keys_activations_by_definition() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, basalt) = basalt_board(2, 0, db);
    let wastes: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].name == "Wastes")
        .collect();
    for &land in &wastes {
        let index = ability(runner.state(), land, true);
        activate(&mut runner, land, index);
    }
    let (mana, untap) = (
        ability(runner.state(), basalt, true),
        ability(runner.state(), basalt, false),
    );
    activate(&mut runner, basalt, mana);
    activate(&mut runner, basalt, untap);
    let view = trace_of(runner.state());
    assert_eq!(view.entries.len(), 4, "reach: four plays recorded");
    assert_eq!(
        play_node(&view, activates(wastes[0])),
        play_node(&view, activates(wastes[1])),
        "two Wastes' \"{{T}}: Add {{C}}.\" are one ability"
    );
    assert_ne!(
        play_node(&view, activates_index(basalt, mana)),
        play_node(&view, activates_index(basalt, untap)),
        "Basalt Monolith's two abilities are two nodes on one object"
    );
}

#[test]
fn play_trace_keys_a_copied_trigger_by_its_printed_card() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, kiki, exarch) = kiki_board(LoopDetectionMode::Interactive, db);
    // Declining an offer restarts the trace, so the drive runs until one trace holds two copies.
    for _ in 0..6 {
        kiki_cycle(&mut runner, kiki, exarch);
        let view = trace_of(runner.state());
        let resolved = view
            .entries
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Resolution { .. }))
            .count();
        if resolved >= 2 {
            assert_eq!(
                view.node_count, 2,
                "Kiki-Jiki's ability and Deceiver Exarch's trigger, over {resolved} copies"
            );
            return;
        }
        decline_offer(&mut runner, &|_| 0);
    }
    panic!("reach: no trace held two token copies' enters triggers");
}

#[test]
fn play_trace_window_is_the_step() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, basalt) = basalt_board(1, 0, db);
    let (mana, untap) = (
        ability(runner.state(), basalt, true),
        ability(runner.state(), basalt, false),
    );
    activate(&mut runner, basalt, mana);
    while runner.state().phase == Phase::PreCombatMain {
        act(&mut runner, GameAction::PassPriority);
    }
    assert_eq!(
        runner.state().phase,
        Phase::BeginCombat,
        "reach: the combat step's window"
    );
    let land = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Wastes")
        .expect("Wastes");
    let index = ability(runner.state(), land, true);
    activate(&mut runner, land, index);
    activate(&mut runner, basalt, untap);
    settle(&mut runner, &|_| 0);
    activate(&mut runner, basalt, mana);

    let view = trace_of(runner.state());
    assert!(
        matches!(&view.entries[0].kind, EntryKind::Play { action, .. } if activates(land)(action)),
        "the step's trace starts at the combat step's first play: {:?}",
        view.entries[0]
    );
    let span = view.named.last().expect("the in-step period is named");
    assert_eq!(
        plays(&view, span),
        [
            play_node(&view, activates_index(basalt, untap)),
            play_node(&view, activates_index(basalt, mana))
        ],
        "the untap, passes, then the tap — all inside the step"
    );
}

#[test]
fn play_trace_records_only_the_outermost_apply() {
    let Some(db) = shared_card_db() else { return };
    let mut state = crate::loop_shortcut_drain_boards::lethal_lifegain_loss_board();
    // A filter land ("{W/U}, {T}: Add {W}{W}, {W}{U}, or {U}{U}.") per seat: the finish's
    // castability check walks producer -> filter-land routes through nested applies.
    let seats: Vec<PlayerId> = state.players.iter().map(|p| p.id).collect();
    for seat in seats {
        place(&mut state, seat, "Mystic Gate", db);
    }
    let before = play_trace_counters();
    crate::loop_shortcut_drain_boards::drive_to_live_declarable_offer(&mut state);
    let run = play_trace_counters().since(before);
    assert!(
        run.nested_applies > 0,
        "reach: the finish re-enters the action boundary: {run:?}"
    );
    assert_eq!(
        run.actions_recorded, run.boundary_entries,
        "only outermost applies record: {run:?}"
    );
    assert!(
        run.windows <= run.boundary_entries,
        "only outermost applies name: {run:?}"
    );
}

#[test]
fn play_trace_records_nothing_inside_a_probe() {
    let mut state = restore_dump(include_bytes!(
        "../fixtures/kilo_freed_relic_pentad_4p.json.gz"
    ));
    state.loop_detection = LoopDetectionMode::Interactive;
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "reach: a priority window"
    );
    let before = play_trace_counters();
    legal_actions(&state);
    let run = play_trace_counters().since(before);
    assert!(
        run.probe_entries > 0,
        "reach: legality probes enter the action boundary: {run:?}"
    );
    assert_eq!(
        run.actions_recorded, run.boundary_entries,
        "probe applies record nothing: {run:?}"
    );
}

#[test]
fn play_trace_records_no_clone_drive_resolution() {
    let Some(mut drive) = trigger_drive(TriggerBoard::A) else {
        return;
    };
    let resolved = |state: &GameState| {
        play_trace_view(state).map_or(0, |view| {
            view.entries
                .iter()
                .filter(|entry| matches!(entry.kind, EntryKind::Resolution { .. }))
                .count()
        })
    };
    for _ in 0..BEAT_CAP {
        let action = drive.next_action();
        let (before, entries) = (play_trace_counters(), resolved(drive.runner.state()));
        act(&mut drive.runner, action);
        if is_offer(drive.runner.state()) {
            let run = play_trace_counters().since(before);
            assert!(
                run.resolution_hooks_in_probe > 0,
                "reach: the offer's clone drive resolved triggers: {run:?}"
            );
            let live = resolved(drive.runner.state()) - entries;
            assert_eq!(
                live, 1,
                "reach: the offer's apply resolved one trigger at the live frame"
            );
            assert_eq!(
                run.resolutions_recorded, 1,
                "clone-drive resolutions record nothing: {run:?}"
            );
            return;
        }
    }
    panic!("Board A reached no offer");
}

#[test]
fn play_trace_off_records_nothing() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, kiki, exarch) = kiki_board(LoopDetectionMode::Off, db);
    let index = ability(runner.state(), kiki, false);
    activate(&mut runner, kiki, index);
    let mut saw_trigger = false;
    for _ in 0..BEAT_CAP {
        let state = runner.state();
        saw_trigger |= state
            .stack
            .iter()
            .any(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. }));
        match &state.waiting_for {
            WaitingFor::Priority { .. } if state.stack.is_empty() => break,
            WaitingFor::Priority { .. } => act(&mut runner, GameAction::PassPriority),
            _ => answer(&mut runner, &|action| match action {
                GameAction::SelectModes { indices } if indices == &[0] => 2,
                _ => names(&[kiki, exarch])(action),
            }),
        }
    }
    assert!(
        saw_trigger && runner.state().stack.is_empty(),
        "reach: the Exarch copy's trigger resolved"
    );
    assert!(
        play_trace_view(runner.state()).is_none(),
        "Off records nothing"
    );
}

#[test]
fn play_trace_is_not_persisted_shown_or_kept_past_a_take() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = crate::loop_shortcut_mana_engine::setup(true, LoopDetectionMode::Interactive, db);
    let mana = mana_ability_index(rig.runner.state(), rig.basalt).expect("mana");
    let untap = untap_ability_index(rig.runner.state(), rig.basalt).expect("untap");
    drive_one_period(&mut rig, mana, untap);
    let state = rig.runner.state();
    assert!(
        play_trace_view(state).is_some(),
        "reach: the live state carries a trace"
    );

    let json = serde_json::to_string(state).expect("serializes");
    let loaded: GameState = serde_json::from_str(&json).expect("deserializes");
    assert!(
        play_trace_view(&loaded).is_none(),
        "a load starts with no trace"
    );
    for viewer in [P0, P1] {
        let shown = engine::game::visibility::filter_state_for_viewer(state, viewer);
        assert!(
            play_trace_view(&shown).is_none(),
            "{viewer:?}'s view carries no trace"
        );
    }

    act(
        &mut rig.runner,
        GameAction::DeclareShortcut {
            count: engine::analysis::decision_template::IterationCount::Fixed(3),
            template: None,
        },
    );
    while matches!(
        rig.runner.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        act(
            &mut rig.runner,
            GameAction::RespondToShortcut {
                response: engine::analysis::loop_check::ShortcutResponse::Accept,
            },
        );
    }
    assert!(
        matches!(rig.runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: the take ended"
    );
    assert!(
        play_trace_view(rig.runner.state()).is_none(),
        "a take clears the trace"
    );
}

// ---------------------------------------------------------------------------------------------
// Row 3 — no whole-state work per play
// ---------------------------------------------------------------------------------------------

#[test]
fn play_trace_cost_is_flat() {
    let Some(db) = shared_card_db() else { return };
    let run = |bears: usize| -> PlayTraceCounters {
        let (mut runner, basalt) = basalt_board(0, bears, db);
        let (mana, untap) = (
            ability(runner.state(), basalt, true),
            ability(runner.state(), basalt, false),
        );
        let before = play_trace_counters();
        activate(&mut runner, basalt, mana);
        settle(&mut runner, &|_| 0);
        activate(&mut runner, basalt, untap);
        settle(&mut runner, &|_| 0);
        assert!(
            is_offer(runner.state()),
            "reach: one period reaches the base offer ({bears} Bears)"
        );
        play_trace_counters().since(before)
    };
    let (small, large) = (run(0), run(60));
    let trace_work = |c: PlayTraceCounters| {
        (
            c.actions_recorded,
            c.resolutions_recorded,
            c.node_map_reads,
            c.node_map_writes,
            c.node_key_compares,
            c.windows,
        )
    };
    assert_eq!(
        small.trace_state_copies, 0,
        "the trace copies no state: {small:?}"
    );
    assert_eq!(
        large.trace_state_copies, 0,
        "the trace copies no state: {large:?}"
    );
    assert_eq!(
        trace_work(small),
        trace_work(large),
        "the trace's per-play work ignores board size"
    );
    assert!(
        small.legality_reads > 0 && small.legality_read_copies > 0,
        "reach: the naming read legality: {small:?}"
    );
    assert_eq!(
        (small.legality_reads, small.legality_read_copies),
        (large.legality_reads, large.legality_read_copies),
        "the naming's reads ignore board size"
    );
}

// ---------------------------------------------------------------------------------------------
// Row 6 — the base-window population is named no later than the base offers
// ---------------------------------------------------------------------------------------------

#[test]
fn play_trace_names_the_base_window_population_basalt() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = crate::loop_shortcut_mana_engine::setup(true, LoopDetectionMode::Interactive, db);
    let mana = mana_ability_index(rig.runner.state(), rig.basalt).expect("mana");
    let untap = untap_ability_index(rig.runner.state(), rig.basalt).expect("untap");
    drive_one_period(&mut rig, mana, untap);
    assert!(is_offer(rig.runner.state()), "reach: the base offers");
    assert!(
        names_the_period(
            rig.runner.state(),
            &[
                PeriodStep::Activate(rig.basalt, Some(mana)),
                PeriodStep::Activate(rig.basalt, Some(untap)),
            ],
            false,
        ),
        "the base's period is named at its offer"
    );
}

#[test]
fn play_trace_names_the_base_window_population_relic_kilo_freed() {
    let mut state = crate::kilo_live_offer_from_real_dump::load_migrated_dump();
    state.loop_detection = LoopDetectionMode::Interactive;
    crate::kilo_live_offer_from_real_dump::drive_one_live_cycle(
        &mut state,
        &crate::kilo_live_offer_from_real_dump::FIXTURE_IDS,
    );
    assert!(is_offer(&state), "reach: the base offers");
    assert!(
        names_the_period(
            &state,
            &[
                PeriodStep::Activate(
                    crate::kilo_live_offer_from_real_dump::RELIC,
                    Some(crate::kilo_live_offer_from_real_dump::RELIC_TAP_MANA),
                ),
                PeriodStep::Activate(
                    crate::kilo_live_offer_from_real_dump::FREED,
                    Some(crate::kilo_live_offer_from_real_dump::FREED_UNTAP),
                ),
            ],
            false,
        ),
        "the base's period is named at its offer"
    );
    // Relic of Legends: "Tap an untapped legendary creature you control: Add one mana of any
    // color." Freed from the Real: "{U}: Untap enchanted creature."
    let view = trace_of(&state);
    let relic = view
        .entries
        .iter()
        .position(|entry| matches!(&entry.kind, EntryKind::Play { action, .. } if activates(crate::kilo_live_offer_from_real_dump::RELIC)(action)))
        .expect("Relic of Legends' activation");
    assert!(
        view.named
            .iter()
            .any(|span| span.start == relic && span.cause == NamingCause::Restored),
        "Relic's mana ability is read legal again once Freed untaps Kilo: {:?}",
        view.named
    );
}

#[test]
fn play_trace_names_the_base_window_population_presence_of_gond() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, bears, _) = gond_board(false, db);
    let index = ability(runner.state(), bears, false);
    activate(&mut runner, bears, index);
    settle(&mut runner, &|_| 0);
    assert!(is_offer(runner.state()), "reach: the base offers");
    assert!(
        names_the_period(
            runner.state(),
            &[PeriodStep::Activate(bears, Some(index))],
            false
        ),
        "the base's period is named at its offer"
    );
}

/// Sprout Swarm ("Convoke", "Buyback {3}", "Create a 1/1 green Saproling creature token.") with
/// Bogwater Lumaret ("Whenever this creature or another creature you control enters, you gain 1
/// life.") on a real four-player board.
#[test]
fn play_trace_names_the_base_window_population_sprout_swarm_lumaret() {
    const SPROUT: ObjectId = ObjectId(64);
    const FODDER: ObjectId = ObjectId(421);
    let mut state = restore_dump(include_bytes!(
        "../fixtures/witherbloom_sprout_lumaret_4p.json.gz"
    ));
    state.loop_detection = LoopDetectionMode::On;
    let mut runner = GameRunner::from_state(state);
    let mut named_before = play_trace_view(runner.state()).map_or(0, |view| view.named.len());
    let mut commit = runner
        .cast(SPROUT)
        .accept_optional()
        .convoke_with(&[FODDER])
        .commit();
    let (mut quiet_with_trigger_on_top, mut quiet_with_spell_on_top) = (false, false);
    for _ in 0..BEAT_CAP {
        let state = commit.state();
        let named = play_trace_view(state).map_or(0, |view| view.named.len());
        match &state.waiting_for {
            WaitingFor::LoopShortcut { .. } => break,
            WaitingFor::Priority { .. } if state.stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                match state.stack.back().map(|e| &e.kind) {
                    Some(StackEntryKind::TriggeredAbility { .. }) => {
                        quiet_with_trigger_on_top = true;
                    }
                    Some(StackEntryKind::Spell { .. }) => quiet_with_spell_on_top = true,
                    _ => {}
                }
                assert_eq!(
                    named, named_before,
                    "nothing is named with the stack non-empty"
                );
                named_before = named;
                commit.act(GameAction::PassPriority).expect("pass");
            }
            _ => {
                let action = legal_actions(state)
                    .into_iter()
                    .find(|a| !matches!(a, GameAction::PassPriority))
                    .expect("an answer");
                commit.act(action).expect("answer");
            }
        }
    }
    let state = commit.state();
    assert!(
        quiet_with_trigger_on_top && quiet_with_spell_on_top,
        "reach: windows with Sprout Swarm and with Lumaret's trigger on top"
    );
    assert!(is_offer(state), "reach: the base offers");
    assert!(
        names_the_period(
            state,
            &[PeriodStep::Cast(state.objects[&SPROUT].card_id)],
            false
        ),
        "the base's period is named at its offer"
    );
}

/// The spans named at the current window, by the play each starts at.
fn window_starts(view: &PlayTraceView) -> Vec<usize> {
    view.named
        .iter()
        .filter(|span| span.end == view.entries.len())
        .map(|span| plays(view, span)[0])
        .collect()
}

#[test]
fn play_trace_names_the_base_window_population_in_last_occurrence_order() {
    let Some(db) = shared_card_db() else { return };
    let green = chooses_color(ManaType::Green);
    for retap in [false, true] {
        let (mut runner, bears, extra) = gond_board(true, db);
        let (elves, mobilize) = extra.expect("Llanowar Elves and Mobilize");
        let (tap_elves, make_elf) = (
            ability(runner.state(), elves, true),
            ability(runner.state(), bears, false),
        );
        activate(&mut runner, elves, tap_elves);
        settle(&mut runner, &green);
        cast(&mut runner, mobilize, vec![], CastPaymentMode::Auto);
        settle(&mut runner, &|_| 0);
        activate(&mut runner, bears, make_elf);
        if retap {
            // Llanowar Elves is tapped again while Intruder Alarm's trigger is on top, so its
            // last occurrence follows the Bears' though its first precedes it.
            while !matches!(
                runner.state().stack.back().map(|e| &e.kind),
                Some(StackEntryKind::TriggeredAbility { .. })
            ) {
                act(&mut runner, GameAction::PassPriority);
            }
            assert!(
                !runner.state().objects[&elves].tapped,
                "reach: Mobilize untapped Llanowar Elves"
            );
            activate(&mut runner, elves, tap_elves);
        }
        settle(&mut runner, &green);
        let view = trace_of(runner.state());
        let (bears_node, elves_node) = (
            play_node(&view, activates(bears)),
            play_node(&view, activates(elves)),
        );
        let expected = if retap {
            [bears_node, elves_node]
        } else {
            [elves_node, bears_node]
        };
        assert_eq!(
            window_starts(&view),
            expected,
            "retap {retap}: the window's spans in the order their plays were last made"
        );
    }
}

/// The trigger-wide provenance of a stack ability, for before/after comparison.
fn provenance(ability: &engine::types::ability::ResolvedAbility) -> String {
    format!(
        "{:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
        ability.trigger_definition_ref,
        ability.trigger_source,
        ability.source_incarnation,
        ability.original_controller,
        ability.controller,
        ability.may_trigger_origin,
        ability.scoped_player,
        ability.context.source_transformation_count,
    )
}

/// Every node of the chain carries the root's definition reference.
fn chain_keeps_ref(ability: &engine::types::ability::ResolvedAbility) -> bool {
    let root = &ability.trigger_definition_ref;
    let mut node = Some(ability);
    while let Some(current) = node {
        if &current.trigger_definition_ref != root {
            return false;
        }
        node = current.sub_ability.as_deref();
    }
    true
}

fn top_trigger(
    state: &GameState,
    source: ObjectId,
) -> Option<engine::types::ability::ResolvedAbility> {
    match state.stack.last().map(|entry| &entry.kind) {
        Some(StackEntryKind::TriggeredAbility {
            ability, source_id, ..
        }) if *source_id == source => Some(ability.as_ref().clone()),
        _ => None,
    }
}

/// Answers prompts until `source`'s trigger stands on the stack at priority, taking the
/// first mode; returns the trigger's ability when its mode was asked for and at priority.
fn drive_modal_trigger(
    runner: &mut GameRunner,
    source: impl Fn(&GameState) -> Option<ObjectId>,
) -> (
    Option<engine::types::ability::ResolvedAbility>,
    engine::types::ability::ResolvedAbility,
) {
    let mut at_mode_choice = None;
    for _ in 0..40 {
        let state = runner.state();
        let trigger = source(state).and_then(|id| top_trigger(state, id));
        match &state.waiting_for {
            WaitingFor::Priority { .. } if trigger.is_some() => {
                return (at_mode_choice, trigger.expect("checked"));
            }
            WaitingFor::Priority { .. } => act(runner, GameAction::PassPriority),
            WaitingFor::AbilityModeChoice { .. } => {
                at_mode_choice = trigger;
                act(runner, GameAction::SelectModes { indices: vec![0] });
            }
            _ => {
                runner
                    .choose_first_legal_target()
                    .expect("choose the first legal target");
            }
        }
    }
    panic!("the modal trigger never stood on the stack at priority");
}

/// CR 603.3c: a modal trigger's mode choice is part of putting that ability on the
/// stack, so the chosen modes keep the trigger's identity (Deceiver Exarch, "When this
/// creature enters, choose one — • Untap target permanent you control. • Tap target
/// permanent an opponent controls.", on the card and on a Kiki-Jiki token copy).
#[test]
fn modal_trigger_keeps_its_provenance_through_mode_choice() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kiki = scenario.add_real_card(P0, "Kiki-Jiki, Mirror Breaker", Zone::Battlefield, db);
    let exarch = scenario.add_real_card(P0, "Deceiver Exarch", Zone::Hand, db);
    for _ in 0..3 {
        scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    }
    scenario.add_real_card(P1, "Mountain", Zone::Battlefield, db);
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Mountain", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();

    cast(&mut runner, exarch, vec![], CastPaymentMode::Auto);
    let (asked, card) = drive_modal_trigger(&mut runner, |_| Some(exarch));
    let asked = asked.expect("the card's trigger asked for its mode");
    assert!(
        card.trigger_definition_ref.is_some(),
        "the card's trigger keeps its definition reference"
    );
    assert_eq!(provenance(&card), provenance(&asked), "the card's trigger");
    assert!(chain_keeps_ref(&card));
    settle(&mut runner, &|_| 0);

    let index = ability(runner.state(), kiki, false);
    activate(&mut runner, kiki, index);
    let (asked, copy) = drive_modal_trigger(&mut runner, |state| {
        state
            .battlefield
            .iter()
            .copied()
            .find(|id| *id != exarch && state.objects[id].name == "Deceiver Exarch")
    });
    let asked = asked.expect("the copy's trigger asked for its mode");
    assert!(
        copy.trigger_definition_ref.is_some(),
        "the copy's trigger keeps its definition reference"
    );
    assert_eq!(provenance(&copy), provenance(&asked), "the copy's trigger");
    assert!(chain_keeps_ref(&copy));
}

/// CR 603.3c + CR 700.2b: a mode the game picks at random keeps the trigger's identity
/// too (Summon: Magus Sisters, "I, II, III — Choose one at random —").
#[test]
fn random_modal_trigger_keeps_its_provenance() {
    const MAGUS_SISTERS: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI, II, III \u{2014} Choose one at random \u{2014}\n\u{2022} Combine Powers! \u{2014} Put three +1/+1 counters on target creature.\n\u{2022} Defense! \u{2014} Put a shield counter on target creature. You gain 3 life.\n\u{2022} Fight! \u{2014} This creature fights up to one target creature an opponent controls.\nHaste";
    let mut scenario = GameScenario::new_n_player(2, 0);
    scenario.at_phase(Phase::PreCombatMain);
    let saga = scenario
        .add_creature(P0, "Summon: Magus Sisters", 5, 5)
        .as_enchantment()
        .as_creature()
        .with_subtypes(vec!["Saga", "Faerie"])
        .from_oracle_text(MAGUS_SISTERS)
        .id();
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    scenario.add_creature(P1, "Hill Giant", 3, 3);
    scenario.with_library_top(P0, &["Plains"; 10]);
    scenario.with_library_top(P1, &["Plains"; 10]);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.active_player = P0;
        state.phase = Phase::Upkeep;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_phase(Phase::PreCombatMain);

    let (asked, chapter) = drive_modal_trigger(&mut runner, |_| Some(saga));
    assert!(asked.is_none(), "reach: the game chose the mode");
    assert!(
        !chapter.selected_mode_labels.is_empty(),
        "reach: a mode was chosen"
    );
    let definition_ref = chapter
        .trigger_definition_ref
        .as_ref()
        .expect("the chapter keeps its definition reference");
    assert_eq!(definition_ref.source.object_id, saga);
    assert!(chapter.trigger_source.is_some());
    assert!(chain_keeps_ref(&chapter));
}

// ---------------------------------------------------------------------------------------------
// CR 733.1 — a reversed play leaves the trace with its reversal
// ---------------------------------------------------------------------------------------------

/// Grizzly Bears in P0's hand and two Forests, sampled interactively.
fn bears_board(db: &CardDatabase) -> (GameRunner, ObjectId, [ObjectId; 2]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let forests = [
        scenario.add_real_card(P0, "Forest", Zone::Battlefield, db),
        scenario.add_real_card(P0, "Forest", Zone::Battlefield, db),
    ];
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    (runner, bears, forests)
}

/// Taps `land` for mana through the selection the engine authors for it at this prompt.
fn tap_for_mana(runner: &mut GameRunner, land: ObjectId) {
    let (_, _, grouped) = engine::ai_support::legal_actions_full(runner.state());
    let tap = grouped
        .get(&land)
        .into_iter()
        .flatten()
        .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
        .cloned()
        .expect("the engine authors the land's mana selection");
    act(runner, tap);
}

fn cancel(runner: &mut GameRunner, bears: ObjectId) {
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: the cast is in progress"
    );
    act(runner, GameAction::CancelCast);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0)
            && runner.state().objects[&bears].zone == Zone::Hand,
        "reach: the cancelled card is back in hand at priority"
    );
}

#[test]
fn play_trace_drops_a_cancelled_cast() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, bears, _) = bears_board(db);
    for _ in 0..2 {
        cast(&mut runner, bears, vec![], CastPaymentMode::Manual);
        cancel(&mut runner, bears);
    }
    let view = trace_of(runner.state());
    assert_eq!(
        view.entries,
        [],
        "neither the cancelled casts nor their cancels"
    );
    assert_eq!(view.named, [], "nothing is named from a reversed play");

    cast(&mut runner, bears, vec![], CastPaymentMode::Auto);
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == bears),
        "reach: the cast completed"
    );
    let view = trace_of(runner.state());
    assert_eq!(
        entry_steps(&view, 0..view.entries.len()),
        [Step::Play(play_node(&view, casts(bears)))],
        "a completed cast after cancelled ones is one play"
    );
}

#[test]
fn play_trace_keeps_the_mana_ability_a_cancelled_cast_did_not_reverse() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, bears, [forest, _]) = bears_board(db);
    cast(&mut runner, bears, vec![], CastPaymentMode::Manual);
    tap_for_mana(&mut runner, forest);
    cancel(&mut runner, bears);
    assert!(
        runner.state().objects[&forest].tapped,
        "reach: the cancel left the Forest's mana ability standing"
    );
    let view = trace_of(runner.state());
    assert!(
        matches!(
            view.entries.as_slice(),
            [TraceEntry { kind: EntryKind::Play { locus: PlayLocus::Mana(source, _), .. }, .. }]
                if *source == forest
        ),
        "only the Forest's mana ability stays: {:?}",
        view.entries
    );
}

#[test]
fn play_trace_drops_an_untapped_lands_mana_ability() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, _, [first, second]) = bears_board(db);
    tap_for_mana(&mut runner, first);
    tap_for_mana(&mut runner, second);
    act(
        &mut runner,
        GameAction::UntapLandForMana { object_id: first },
    );
    assert!(
        !runner.state().objects[&first].tapped && runner.state().objects[&second].tapped,
        "reach: only the first Forest was untapped"
    );
    let view = trace_of(runner.state());
    assert!(
        matches!(
            view.entries.as_slice(),
            [TraceEntry { kind: EntryKind::Play { locus: PlayLocus::Mana(source, _), .. }, .. }]
                if *source == second
        ),
        "the later tap stays, the reversed one and its untap leave: {:?}",
        view.entries
    );

    act(
        &mut runner,
        GameAction::UntapLandForMana { object_id: second },
    );
    assert!(
        !runner.state().objects[&second].tapped,
        "reach: the second Forest was untapped"
    );
    let view = trace_of(runner.state());
    assert_eq!(view.entries, [], "every tap was reversed");
    assert_eq!(view.named, [], "nothing is named from a reversed play");
}

#[test]
fn play_trace_keeps_what_stood_before_a_cancelled_resolution_cast() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let conduit = scenario.add_real_card(P0, "Conduit of Worlds", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let index = ability(runner.state(), conduit, false);
    activate(&mut runner, conduit, index);
    act(&mut runner, GameAction::PassPriority);
    act(&mut runner, GameAction::PassPriority);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::CastOffer { .. }),
        "reach: the resolved ability offers the cast"
    );
    act(
        &mut runner,
        GameAction::GraveyardPaidCastChoice {
            choice: CastChoice::Cast,
        },
    );
    tap_for_mana(&mut runner, forest);
    act(&mut runner, GameAction::CancelCast);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0)
            && runner.state().objects[&bears].zone == Zone::Graveyard
            && runner.state().objects[&forest].tapped,
        "reach: the cast is cancelled and the Forest's mana ability stands"
    );
    let view = trace_of(runner.state());
    let tap = play_node(&view, |action| {
        matches!(action, GameAction::TapLandForMana { .. })
    });
    assert_eq!(
        entry_steps(&view, 0..view.entries.len()),
        [
            Step::Play(play_node(&view, activates(conduit))),
            Step::Answer(P0, "PassPriority"),
            Step::Answer(P1, "PassPriority"),
            Step::Play(tap),
        ],
        "the passes that resolved the ability stand; the cast's own answer does not"
    );
}

#[test]
fn play_trace_drops_a_cancelled_activation() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cankerbloom = scenario.add_real_card(P0, "Cankerbloom", Zone::Battlefield, db);
    let copter = scenario.add_real_card(P0, "Smuggler's Copter", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;

    let index = ability(runner.state(), cankerbloom, false);
    activate(&mut runner, cankerbloom, index);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. }
        ),
        "reach: the activation waits on its mode"
    );
    act(&mut runner, GameAction::CancelCast);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: the activation is cancelled"
    );
    assert_eq!(
        trace_of(runner.state()).entries,
        [],
        "a cancelled activation"
    );

    act(
        &mut runner,
        GameAction::CrewVehicle {
            vehicle_id: copter,
            creature_ids: vec![],
        },
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::CrewVehicle { .. }),
        "reach: crew waits on its creatures"
    );
    act(&mut runner, GameAction::CancelCast);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: the crew activation is cancelled"
    );
    assert_eq!(trace_of(runner.state()).entries, [], "a cancelled crew");
}

#[test]
fn play_trace_keeps_a_mana_abilitys_choice_through_a_cancel() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let altar = scenario.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db);
    let fodder = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Llanowar Elves", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    cast(&mut runner, bears, vec![], CastPaymentMode::Manual);
    let index = ability(runner.state(), altar, true);
    activate(&mut runner, altar, index);
    act(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![fodder],
        },
    );
    cancel(&mut runner, bears);
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "reach: the Altar's sacrifice stands"
    );
    let view = trace_of(runner.state());
    assert_eq!(
        entry_steps(&view, 0..view.entries.len()),
        [
            Step::Play(play_node(&view, activates(altar))),
            Step::Answer(P0, "SelectCards"),
        ],
        "the mana ability and its sacrifice choice stand"
    );
}

#[test]
fn play_trace_drops_a_cancelled_spend_after_an_untapped_lands_mana_ability() {
    let Some(db) = shared_card_db() else { return };
    for untap_first in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let elves = scenario.add_real_card(P0, "Llanowar Elves", Zone::Battlefield, db);
        let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
        let index = ability(runner.state(), elves, true);
        activate(&mut runner, elves, index);
        cast(&mut runner, bears, vec![], CastPaymentMode::Manual);
        if untap_first {
            tap_for_mana(&mut runner, forest);
            act(
                &mut runner,
                GameAction::UntapLandForMana { object_id: forest },
            );
            assert!(
                !runner.state().objects[&forest].tapped,
                "reach: the Forest was untapped"
            );
        }
        let pip_id = runner.state().players[0]
            .mana_pool
            .units()
            .next()
            .expect("reach: the Elves' mana is in the pool")
            .pip_id;
        act(&mut runner, GameAction::SpendPoolMana { pip_id });
        let view = trace_of(runner.state());
        assert!(
            view.entries.len() >= 2,
            "reach: the spend was recorded: {:?}",
            view.entries
        );
        cancel(&mut runner, bears);
        let view = trace_of(runner.state());
        assert_eq!(
            entry_steps(&view, 0..view.entries.len()),
            [Step::Play(play_node(&view, activates(elves)))],
            "only the Elves' mana ability stands (untap first: {untap_first})"
        );
    }
}

/// One trace entry as the inner-cancel rows compare it: a play by the object and ability it
/// names, an answer by its action.
#[derive(Debug, PartialEq)]
enum Made {
    Play(PlayLocus),
    Answer(&'static str),
    Resolution,
}

fn mana(source: ObjectId, index: usize) -> Made {
    Made::Play(PlayLocus::Mana(source, Some(index)))
}

/// The prompt, the mana ability whose payment window stands, and the trace, compared together.
fn standing(state: &GameState) -> (&'static str, Option<ObjectId>, Vec<Made>) {
    let window = match &state.waiting_for {
        WaitingFor::ManaAbilityManaPayment {
            pending_mana_ability,
            ..
        } => Some(pending_mana_ability.source_id),
        _ => None,
    };
    let made = trace_of(state)
        .entries
        .iter()
        .map(|entry| match &entry.kind {
            EntryKind::Play { locus, .. } => Made::Play(*locus),
            EntryKind::Answer { action, .. } => Made::Answer(action.into()),
            EntryKind::Resolution { .. } => Made::Resolution,
        })
        .collect();
    (state.waiting_for.variant_name(), window, made)
}

const WINDOW: &str = "ManaAbilityManaPayment";

fn mana_alone(cost: &AbilityCost) -> bool {
    matches!(cost, AbilityCost::Mana { .. })
}

struct PrismiteCast {
    runner: GameRunner,
    bears: ObjectId,
    forest: ObjectId,
    prismite: ObjectId,
    /// Prismite's "{2}: Add one mana of any color."
    costed: usize,
}

/// Grizzly Bears in hand; Forest, Grand Architect and Prismite on the battlefield. Grand Architect
/// can tap itself for mana, so Prismite's {2} opens a payment window.
fn prismite_cast_board(db: &CardDatabase) -> PrismiteCast {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Grand Architect", Zone::Battlefield, db);
    let prismite = scenario.add_real_card(P0, "Prismite", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let costed = ability(runner.state(), prismite, true);
    PrismiteCast {
        runner,
        bears,
        forest,
        prismite,
        costed,
    }
}

fn on_stack(state: &GameState, object: ObjectId) -> bool {
    state.stack.iter().any(|entry| entry.id == object)
}

#[test]
fn play_trace_keeps_the_outer_cast_when_an_inner_mana_payment_is_cancelled() {
    let Some(db) = shared_card_db() else { return };
    let PrismiteCast {
        mut runner,
        bears,
        forest,
        prismite,
        costed,
    } = prismite_cast_board(db);
    let cast_of_bears = || Made::Play(PlayLocus::Cast(bears));
    let forest_tap = ability(runner.state(), forest, true);
    cast(&mut runner, bears, vec![], CastPaymentMode::Manual);
    activate(&mut runner, prismite, costed);
    assert_eq!(
        runner.state().waiting_for.variant_name(),
        WINDOW,
        "reach: Prismite's {{2}} opens its payment window"
    );
    tap_for_mana(&mut runner, forest);
    assert_eq!(
        standing(runner.state()),
        (
            WINDOW,
            Some(prismite),
            vec![
                cast_of_bears(),
                mana(prismite, costed),
                mana(forest, forest_tap)
            ]
        ),
        "reach: the Forest was tapped inside Prismite's window"
    );

    act(&mut runner, GameAction::CancelCast);
    assert!(
        on_stack(runner.state(), bears),
        "the cast is still in progress"
    );
    assert_eq!(
        standing(runner.state()),
        (
            "ManaPayment",
            None,
            vec![cast_of_bears(), mana(forest, forest_tap)]
        )
    );

    cancel(&mut runner, bears);
    assert_eq!(
        standing(runner.state()),
        ("Priority", None, vec![mana(forest, forest_tap)])
    );
}

/// Skyshroud Elf: "{T}: Add {G}." and "{1}: Add {R} or {W}."
#[test]
fn play_trace_drops_only_the_cancelled_one_of_two_nested_mana_activations() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let prismite = scenario.add_real_card(P0, "Prismite", Zone::Battlefield, db);
    let elf = scenario.add_real_card(P0, "Skyshroud Elf", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let outer = ability(runner.state(), prismite, true);
    let inner = mana_ability_costing(runner.state(), elf, mana_alone);
    activate(&mut runner, prismite, outer);
    activate(&mut runner, elf, inner);
    assert_eq!(
        standing(runner.state()),
        (
            WINDOW,
            Some(elf),
            vec![mana(prismite, outer), mana(elf, inner)]
        ),
        "reach: the Elf's window stands inside Prismite's"
    );

    act(&mut runner, GameAction::CancelCast);
    assert_eq!(
        standing(runner.state()),
        (WINDOW, Some(prismite), vec![mana(prismite, outer)])
    );

    act(&mut runner, GameAction::CancelCast);
    assert_eq!(standing(runner.state()), ("Priority", None, vec![]));
}

/// Calciform Pools: "{1}, Remove X storage counters from this land: Add X mana in any combination
/// of {W} and/or {U}." Its announced X is the activation's own answer.
#[test]
fn play_trace_drops_a_cancelled_mana_activations_own_answers() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let pools = scenario.add_real_card(P0, "Calciform Pools", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    {
        let pools = runner.state_mut().objects.get_mut(&pools).unwrap();
        pools.tapped = true;
        pools
            .counters
            .insert(CounterType::Generic("storage".to_string()), 2);
    }
    let storage = mana_ability_costing(runner.state(), pools, |cost| {
        matches!(cost, AbilityCost::Composite { .. })
    });
    let cast_of_bears = || Made::Play(PlayLocus::Cast(bears));
    cast(&mut runner, bears, vec![], CastPaymentMode::Manual);
    activate(&mut runner, pools, storage);
    act(&mut runner, GameAction::SubmitPayAmount { amount: 1 });
    assert_eq!(
        standing(runner.state()),
        (
            WINDOW,
            Some(pools),
            vec![
                cast_of_bears(),
                mana(pools, storage),
                Made::Answer("SubmitPayAmount")
            ]
        ),
        "reach: the Pools' window stands with its X announced"
    );

    act(&mut runner, GameAction::CancelCast);
    assert!(
        on_stack(runner.state(), bears),
        "the cast is still in progress"
    );
    assert_eq!(
        standing(runner.state()),
        ("ManaPayment", None, vec![cast_of_bears()])
    );
}

/// Grand Architect: "Tap an untapped blue creature you control: Add {C}{C}. Spend this mana only
/// to cast artifact spells or activate abilities of artifacts."
#[test]
fn play_trace_keeps_the_outer_activation_when_a_second_prismites_payment_is_cancelled() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let architect = scenario.add_real_card(P0, "Grand Architect", Zone::Battlefield, db);
    let first = scenario.add_real_card(P0, "Prismite", Zone::Battlefield, db);
    let second = scenario.add_real_card(P0, "Prismite", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let costed = ability(runner.state(), first, true);
    let tap_blue = ability(runner.state(), architect, true);
    activate(&mut runner, first, costed);
    assert_eq!(
        standing(runner.state()),
        (WINDOW, Some(first), vec![mana(first, costed)]),
        "reach: the first Prismite's window"
    );
    activate(&mut runner, second, costed);
    assert_eq!(
        standing(runner.state()),
        (
            WINDOW,
            Some(second),
            vec![mana(first, costed), mana(second, costed)]
        ),
        "reach: the second Prismite's window stands inside the first's"
    );

    act(&mut runner, GameAction::CancelCast);
    assert_eq!(
        standing(runner.state()),
        (WINDOW, Some(first), vec![mana(first, costed)])
    );

    activate(&mut runner, architect, tap_blue);
    act(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![architect],
        },
    );
    act(
        &mut runner,
        GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Red),
            count: 1,
        },
    );
    assert!(
        runner.state().objects[&architect].tapped
            && runner.state().players[0].mana_pool.total() == 1,
        "the first Prismite's ability resolved on Grand Architect's mana"
    );
    assert_eq!(
        standing(runner.state()),
        (
            "Priority",
            None,
            vec![
                mana(first, costed),
                mana(architect, tap_blue),
                Made::Answer("SelectCards"),
                Made::Answer("ChooseManaColor"),
            ]
        )
    );
}

/// Whether the engine still tracks `land`'s tap as one P0 may take back.
fn tracked(state: &GameState, land: ObjectId) -> bool {
    state
        .lands_tapped_for_mana
        .get(&P0)
        .is_some_and(|lands| lands.contains(&land))
}

/// CR 602.2b + CR 605.3b: withdrawing Prismite's activation at its payment window ends neither
/// the cast it was paying for nor the Forest's resolved mana ability.
#[test]
fn an_inner_cancel_keeps_the_outer_plays_tapped_land_untappable() {
    let Some(db) = shared_card_db() else { return };
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Line {
        TappedForTheCast,
        TappedInsideTheWindow,
        NoCast,
    }
    for line in [
        Line::TappedForTheCast,
        Line::TappedInsideTheWindow,
        Line::NoCast,
    ] {
        let PrismiteCast {
            mut runner,
            bears,
            forest,
            prismite,
            costed,
        } = prismite_cast_board(db);
        let casting = line != Line::NoCast;
        if casting {
            cast(&mut runner, bears, vec![], CastPaymentMode::Manual);
        }
        if line != Line::TappedInsideTheWindow {
            tap_for_mana(&mut runner, forest);
        }
        activate(&mut runner, prismite, costed);
        if line == Line::TappedInsideTheWindow {
            tap_for_mana(&mut runner, forest);
        }
        let state = runner.state();
        assert!(
            state.waiting_for.variant_name() == WINDOW
                && state.objects[&forest].tapped
                && tracked(state, forest),
            "reach ({line:?}): Prismite's window stands over a tracked Forest tap: {:?}",
            state.waiting_for
        );

        act(&mut runner, GameAction::CancelCast);
        let state = runner.state();
        assert_eq!(
            (state.waiting_for.variant_name(), on_stack(state, bears)),
            (if casting { "ManaPayment" } else { "Priority" }, casting),
            "reach ({line:?}): only Prismite's activation was withdrawn"
        );

        let mut outer_cancelled = GameRunner::from_state(state.clone());
        act(
            &mut runner,
            GameAction::UntapLandForMana { object_id: forest },
        );
        let state = runner.state();
        assert!(
            !state.objects[&forest].tapped && state.players[0].mana_pool.total() == 0,
            "{line:?}: the Forest's tap was taken back"
        );
        assert_eq!(on_stack(state, bears), casting, "{line:?}");

        // Cancelling the cast itself still ends the window in which its taps can be taken back.
        if casting {
            cancel(&mut outer_cancelled, bears);
            assert!(
                outer_cancelled
                    .act(GameAction::UntapLandForMana { object_id: forest })
                    .is_err(),
                "{line:?}"
            );
        }
    }
}

/// Marvin, Murderous Mimic: "Marvin has all activated abilities of creatures you control that
/// don't have the same name as this creature." With Skyshroud Elf and Prismite it has "{T}: Add
/// {G}." and "{2}: Add one mana of any color.": two mana abilities of one object, one resolved
/// and one withdrawn.
#[test]
fn play_trace_keeps_a_completed_ability_of_the_cancelled_abilitys_own_object() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let elf = scenario.add_real_card(P0, "Skyshroud Elf", Zone::Battlefield, db);
    let prismite = scenario.add_real_card(P0, "Prismite", Zone::Battlefield, db);
    let marvin = scenario.add_real_card(P0, "Marvin, Murderous Mimic", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner.state_mut().objects.get_mut(&elf).unwrap().tapped = true;
    engine::game::layers::evaluate_layers(runner.state_mut());
    let state = runner.state();
    let prismites = state.objects[&prismite].abilities[ability(state, prismite, true)]
        .cost
        .clone();
    let costed = mana_ability_costing(state, marvin, |cost| Some(cost) == prismites.as_ref());
    let tap = mana_ability_costing(state, marvin, |cost| *cost == AbilityCost::Tap);
    activate(&mut runner, marvin, costed);
    assert_eq!(
        standing(runner.state()),
        (WINDOW, Some(marvin), vec![mana(marvin, costed)]),
        "reach: Marvin's {{2}} opens its payment window"
    );
    activate(&mut runner, marvin, tap);
    let green = |state: &GameState| state.players[0].mana_pool.count_color(ManaType::Green);
    assert_eq!(
        (standing(runner.state()), green(runner.state())),
        (
            (
                WINDOW,
                Some(marvin),
                vec![mana(marvin, costed), mana(marvin, tap)]
            ),
            1
        ),
        "reach: Marvin's {{T}} resolved inside its own {{2}} window"
    );

    act(&mut runner, GameAction::CancelCast);
    assert_eq!(
        (standing(runner.state()), green(runner.state())),
        (("Priority", None, vec![mana(marvin, tap)]), 1)
    );
}

#[test]
fn play_trace_names_a_trigger_repeated_across_land_drops() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Scute Swarm", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Exploration", Zone::Battlefield, db);
    let forests = [
        scenario.add_real_card(P0, "Forest", Zone::Hand, db),
        scenario.add_real_card(P0, "Forest", Zone::Hand, db),
    ];
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    for forest in forests {
        let card_id = runner.state().objects[&forest].card_id;
        act(
            &mut runner,
            GameAction::PlayLand {
                object_id: forest,
                card_id,
            },
        );
        act(&mut runner, GameAction::PassPriority);
        act(&mut runner, GameAction::PassPriority);
    }
    let view = trace_of(runner.state());
    let resolved: Vec<usize> = view
        .entries
        .iter()
        .filter_map(|entry| match entry.kind {
            EntryKind::Resolution { node } => Some(node),
            _ => None,
        })
        .collect();
    assert!(
        runner.state().stack.is_empty()
            && forests
                .iter()
                .all(|forest| runner.state().objects[forest].zone == Zone::Battlefield)
            && resolved.len() == 2
            && resolved[0] == resolved[1],
        "reach: both land drops resolved the landfall trigger: {:?}",
        view.entries
    );
    let span = view
        .named
        .iter()
        .find(|span| span.cause == NamingCause::Repeat)
        .expect("the trigger repeated with a land drop between");
    assert_eq!(
        steps(&view, span),
        [
            Step::Resolve(resolved[0]),
            Step::Answer(P0, "PlayLand"),
            Step::Answer(P0, "PassPriority"),
            Step::Answer(P1, "PassPriority"),
        ]
    );
}
