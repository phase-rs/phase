//! CR 732.2a: the confirmer's replay of a trace candidate, and the cover it certifies on.

use engine::analysis::decision_template::{IterationCount, PinnedDecision};
use engine::analysis::loop_check::{LoopCertificate, OfferRoad, ShortcutResponse, WinKind};
use engine::analysis::loop_states_equal_modulo_resources;
use engine::analysis::resource::{
    history_covers_for_tests, FodderCoverRefusal, ObjectGrowthVerdict, RecurrenceCover,
    ResourceAxis,
};
use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::effects::attach::{attach_to, attach_to_player};
use engine::game::engine::certify_object_growth_frames_for_tests;
use engine::game::functioning_abilities::active_trigger_definitions;
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, resolve_interaction_response,
    submit_interaction,
};
use engine::game::keywords::effective_foretell_cost;
use engine::game::log::resolve_log_entries;
use engine::game::perf_counters::play_trace_counters;
use engine::game::period_confirm::{confirm_for_tests, performed_for_tests, OfferRefusal};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zones::{add_to_zone, remove_from_zone};
use engine::game::{play_trace_view, NamedSpan, NamingCause, PeriodReach, SpanSource};
use engine::types::ability::{
    AbilityKind, CastingPermission, DelayedTriggerCondition, Effect, ResolvedAbility, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, GameState, LoopDetectionMode, ManaChoice, PersistedGameState, StackEntryKind,
    WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::interaction::{
    InteractionOpportunity, InteractionOpportunityResponse, InteractionResponse,
    InteractionResponseSpec, InteractionSessionId, InteractionShortcutDecision,
    InteractionShortcutPoint, InteractionShortcutPointKind, InteractionSubmission,
    ViewerInteraction,
};
use engine::types::keywords::Keyword;
use engine::types::log::LogSegment;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::food_chain_board::{self, BoardCMember};
use crate::play_trace::{
    ability, activate, altar_board, cast, chooses_color, is_offer, names, place, settle,
};
use crate::support::shared_card_db;

fn act(runner: &mut GameRunner, action: GameAction) {
    act_events(runner, action);
}

fn act_events(runner: &mut GameRunner, action: GameAction) -> Vec<GameEvent> {
    let shown = format!("{action:?}");
    runner
        .act(action)
        .unwrap_or_else(|error| panic!("{shown} rejected: {error:?}"))
        .events
}

fn activated_index(state: &GameState, id: ObjectId) -> usize {
    state.objects[&id]
        .abilities
        .iter()
        .position(|a| a.kind == AbilityKind::Activated)
        .expect("an activated ability")
}

/// Passes priority until the stack is empty, answering every target prompt with `target`.
fn resolve_stack(runner: &mut GameRunner, target: ObjectId) {
    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => act(runner, GameAction::PassPriority),
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => act(
                runner,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                },
            ),
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    panic!("the stack did not empty");
}

/// CR 701.21a: Kiki-Jiki's "Sacrifice it at the beginning of the next end step" sacrifices the
/// copy only while its creator controls it; Ray of Command's controller keeps it.
#[test]
fn a_control_changed_kiki_copy_survives_its_delayed_sacrifice() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kiki = scenario.add_real_card(P0, "Kiki-Jiki, Mirror Breaker", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let ray = scenario.add_real_card(P1, "Ray of Command", Zone::Hand, db);
    for _ in 0..4 {
        scenario.add_real_card(P1, "Island", Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    let index = activated_index(runner.state(), kiki);
    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: kiki,
            ability_index: index,
        },
    );
    resolve_stack(&mut runner, bears);
    let copy = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].is_token)
        .expect("Kiki-Jiki made a copy");
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    act(&mut runner, GameAction::PassPriority);
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    let card_id = runner.state().objects[&ray].card_id;
    act(
        &mut runner,
        GameAction::CastSpell {
            object_id: ray,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    );
    resolve_stack(&mut runner, copy);
    assert_eq!(runner.state().objects[&copy].controller, P1);

    for _ in 0..40 {
        let state = runner.state();
        if state.phase == Phase::End
            && state.delayed_triggers.is_empty()
            && !state
                .stack
                .iter()
                .any(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. }))
        {
            break;
        }
        let action = match state.waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            _ => GameAction::PassPriority,
        };
        act(&mut runner, action);
    }
    let state = runner.state();
    assert!(
        state.phase == Phase::End && state.delayed_triggers.is_empty() && state.stack.is_empty(),
        "reach: the delayed sacrifice resolved in the end step"
    );
    assert!(state.battlefield.contains(&copy));
    assert_eq!(state.objects[&copy].controller, P1);
    assert!(state.players.iter().all(|p| !p.graveyard.contains(&copy)));
}

/// The confirmer's verdict on the latest span the trace names at `state`.
fn latest_verdict(state: &GameState) -> Result<Vec<String>, OfferRefusal> {
    confirm_for_tests(state)
        .pop()
        .expect("the trace names a span")
        .1
}

/// Kiki-Jiki, Mirror Breaker ("{T}: Create a token that's a copy of target nonlegendary creature
/// you control, except it has haste. Sacrifice it at the beginning of the next end step.") beside
/// `copied`, whose copies' enters trigger untaps Kiki-Jiki.
fn kiki_copy_board(copied: &str) -> Option<(GameRunner, ObjectId, ObjectId)> {
    kiki_board(P0, copied, None)
}

/// [`kiki_copy_board`] on P0's turn with Kiki-Jiki and `copied` under `seat`'s control, and Soul
/// Warden ("Whenever another creature enters, you gain 1 life.") under `warden`'s.
fn kiki_board(
    seat: PlayerId,
    copied: &str,
    warden: Option<PlayerId>,
) -> Option<(GameRunner, ObjectId, ObjectId)> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kiki = scenario.add_real_card(seat, "Kiki-Jiki, Mirror Breaker", Zone::Battlefield, db);
    let copied = scenario.add_real_card(seat, copied, Zone::Battlefield, db);
    if let Some(warden) = warden {
        scenario.add_real_card(warden, "Soul Warden", Zone::Battlefield, db);
    }
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Mountain", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    Some((runner, kiki, copied))
}

/// Kiki-Jiki's answers: Kiki-Jiki untapped by the copy of `copied` it targets.
fn kiki_score(kiki: ObjectId, copied: ObjectId) -> impl Fn(&GameAction) -> i32 {
    move |action| match action {
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id)),
        } if *id == kiki => 3,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id)),
        } if *id == copied => 2,
        GameAction::SelectModes { indices } if indices == &[0] => 2,
        GameAction::DecideOptionalEffect { accept: true } => 2,
        // Pestermite's "tap or untap": the untap branch.
        GameAction::ChooseBranch { index: 1 } => 2,
        _ => 0,
    }
}

fn kiki_activates(runner: &mut GameRunner, kiki: ObjectId, score: &dyn Fn(&GameAction) -> i32) {
    let index = ability(runner.state(), kiki, false);
    activate(runner, kiki, index);
    settle(runner, score);
}

/// One Kiki-Jiki cycle, declining an offer it meets; what the offered period's replay performs.
fn kiki_copy_cycle(
    runner: &mut GameRunner,
    kiki: ObjectId,
    copied: ObjectId,
) -> Option<Result<Vec<String>, OfferRefusal>> {
    let score = kiki_score(kiki, copied);
    kiki_activates(runner, kiki, &score);
    let performed = is_offer(runner.state()).then(|| {
        let performed = performed_for_tests(runner.state()).expect("an offered span");
        act(runner, GameAction::DeclineShortcut);
        settle(runner, &score);
        performed
    });
    assert!(
        !runner.state().objects[&kiki].tapped,
        "reach: the copy's trigger untapped Kiki-Jiki"
    );
    performed
}

fn grown_not_inert(verdict: &Result<Vec<String>, OfferRefusal>) -> bool {
    matches!(
        verdict,
        Err(OfferRefusal::Cover(ObjectGrowthVerdict::FodderGrowth(pairs)))
            if pairs.iter().all(|refusals| refusals.contains(&FodderCoverRefusal::GrownNotInert))
    )
}

/// CR 702.8a + CR 702.10c: Kiki-Jiki copying Deceiver Exarch ("Flash … When this creature enters,
/// choose one — • Untap target permanent you control. …") confirms, its copies' flash and haste
/// and their delayed "sacrifice it" each grown; Pestermite's copies ("Flash Flying When this
/// creature enters, you may tap or untap target permanent.") carry flying and are not inert.
#[test]
fn kiki_jiki_confirms_with_deceiver_exarch_and_not_with_pestermite() {
    let Some((mut runner, kiki, exarch)) = kiki_copy_board("Deceiver Exarch") else {
        return;
    };
    let performed = (0..4).find_map(|_| kiki_copy_cycle(&mut runner, kiki, exarch));
    assert!(
        matches!(&performed, Some(Ok(performed)) if crate::loop_period_performs::same_up_to_rotation(
            performed,
            &["Deceiver Exarch"]
        )),
        "{performed:?}"
    );

    let Some((mut runner, kiki, pestermite)) = kiki_copy_board("Pestermite") else {
        return;
    };
    for cycle in 0..4 {
        assert_eq!(
            kiki_copy_cycle(&mut runner, kiki, pestermite),
            None,
            "cycle {cycle}"
        );
    }
    let verdict = latest_verdict(runner.state());
    assert!(grown_not_inert(&verdict), "{verdict:?}");
}

/// Phyrexian Altar recurring Gravecrawler beside Samwise Gamgee ("Whenever another nontoken
/// creature you control enters, create a Food token.") grows Foods, whose "{2}, {T}, Sacrifice
/// this token: You gain 3 life." keeps them from being inert.
#[test]
fn an_altar_loop_growing_foods_is_refused_at_the_cover() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(Some("Samwise Gamgee"), db);
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
    };
    for cycle in 0..3 {
        cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
        settle(&mut runner, &score);
        let index = ability(runner.state(), altar, true);
        activate(&mut runner, altar, index);
        settle(&mut runner, &score);
        assert!(!is_offer(runner.state()), "cycle {cycle}");
    }
    let foods = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].name == "Food")
        .count();
    assert_eq!(foods, 3, "reach: one Food per entry");
    let verdict = latest_verdict(runner.state());
    assert!(grown_not_inert(&verdict), "{verdict:?}");
}

/// Kiki-Jiki copying Deceiver Exarch, three cycles after the first: three priority frames.
fn kiki_exarch_frames() -> Option<(ObjectId, [GameState; 3])> {
    let (mut runner, kiki, exarch) = kiki_copy_board("Deceiver Exarch")?;
    let mut frames = Vec::new();
    for _ in 0..4 {
        kiki_copy_cycle(&mut runner, kiki, exarch);
        frames.push(runner.state().clone());
    }
    let [_, a, b, c]: [GameState; 4] = frames.try_into().ok()?;
    Some((kiki, [a, b, c]))
}

/// CR 603.7a + CR 603.7c: the newest "sacrifice it" delayed trigger is growth only while it acts
/// on the grown copy alone through a reference to its snapshotted target; each altered form stays
/// in the compared remainder.
#[test]
fn a_delayed_trigger_is_stripped_only_while_it_acts_on_grown_objects_alone() {
    use engine::types::ability::{Effect, QuantityExpr, TargetFilter, TypedFilter};

    let Some((kiki, frames)) = kiki_exarch_frames() else {
        return;
    };
    let verdict = |frames: &[GameState; 3]| {
        certify_object_growth_frames_for_tests([&frames[0], &frames[1], &frames[2]], &[], P0)
    };
    assert!(verdict(&frames).certifies(), "{:?}", verdict(&frames));

    let effect = |json: serde_json::Value| -> Effect { serde_json::from_value(json).unwrap() };
    type Alteration = Box<dyn Fn(&mut ResolvedAbility)>;
    let alterations: Vec<(&str, Alteration)> = vec![
        (
            "references Kiki-Jiki",
            Box::new(move |a| a.targets = vec![TargetRef::Object(kiki)]),
        ),
        ("references nothing", Box::new(|a| a.targets.clear())),
        (
            "puts it onto the battlefield",
            Box::new(move |a| {
                a.effect = effect(serde_json::json!({"type": "ChangeZone",
                    "destination": "Battlefield", "target": {"type": "LastCreated"}}))
            }),
        ),
        (
            "puts it into its library",
            Box::new(move |a| {
                a.effect = effect(serde_json::json!({"type": "Bounce",
                    "destination": "Library", "target": {"type": "LastCreated"}}))
            }),
        ),
        (
            "sacrifices through a filter",
            Box::new(|a| {
                a.effect = Effect::Sacrifice {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    count: QuantityExpr::Fixed { value: 1 },
                    min_count: 0,
                }
            }),
        ),
    ];
    for (what, alter) in alterations {
        let mut altered = frames.clone();
        let newest = altered[2]
            .delayed_triggers
            .last_mut()
            .expect("reach: the latest cycle's delayed sacrifice");
        assert!(matches!(newest.ability.effect, Effect::Sacrifice { .. }));
        alter(&mut newest.ability);
        let ObjectGrowthVerdict::FodderGrowth([first, second]) = verdict(&altered) else {
            panic!("{what}: {:?}", verdict(&altered));
        };
        assert!(first.is_empty(), "{what}: {first:?}");
        assert!(
            second.contains(&FodderCoverRefusal::NonObjectRemainder),
            "{what}: {second:?}"
        );
    }
}

/// CR 705.1 + CR 732.2a: Food Chain recasting Eternal Scourge beside Mirror March ("Whenever a
/// nontoken creature you control enters, flip a coin until you lose a flip. …") draws a random
/// outcome inside the period, so the replay refuses it.
#[test]
fn a_period_that_flips_a_coin_is_refused_as_random() {
    let Some(db) = shared_card_db() else { return };
    let member = BoardCMember::C1EternalScourge;
    let Some(mut board) = food_chain_board::build(member) else {
        return;
    };
    place(board.runner.state_mut(), P0, "Mirror March", db);
    board.runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    for _ in 0..2 {
        food_chain_board::exile_with_food_chain(
            &mut board.runner,
            board.food_chain,
            board.creature,
            member.mana(),
        );
        food_chain_board::cast_spell(&mut board.runner, board.creature).expect("cast from exile");
        settle(&mut board.runner, &|_| 0);
        assert_eq!(
            board.runner.state().objects[&board.creature].zone,
            Zone::Battlefield,
            "reach: Eternal Scourge resolved"
        );
    }
    let verdict = latest_verdict(board.runner.state());
    assert!(
        matches!(verdict, Err(OfferRefusal::Randomness)),
        "{verdict:?}"
    );
}

/// Phyrexian Altar ("Sacrifice a creature: Add one mana of any color.") beside `creature` in hand,
/// eight Swamps and Mountains, and `extra`; the creature is cast and sacrificed once for `color`.
fn altar_sacrifices_once(
    creature: &str,
    extra: Option<&str>,
    color: ManaType,
) -> Option<GameRunner> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    let creature = scenario.add_real_card(P0, creature, Zone::Hand, db);
    if let Some(extra) = extra {
        scenario.add_real_card(P0, extra, Zone::Battlefield, db);
    }
    for land in ["Swamp", "Mountain"] {
        for _ in 0..8 {
            scenario.add_real_card(P0, land, Zone::Battlefield, db);
        }
    }
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let score = |action: &GameAction| 2 * names(&[creature])(action) + chooses_color(color)(action);
    cast(&mut runner, creature, vec![], CastPaymentMode::Auto);
    settle(&mut runner, &score);
    let index = ability(runner.state(), altar, true);
    activate(&mut runner, altar, index);
    settle(&mut runner, &score);
    Some(runner)
}

/// CR 400.7: Hundred-Battle Veteran ("You may cast this card from your graveyard. If you do, it
/// enters with a finality counter on it.") reached the graveyard when sacrificed after a cast from
/// hand; the replay casts it from the graveyard, and the same sacrifice exiles it.
#[test]
fn a_sacrifice_arriving_elsewhere_than_recorded_is_refused() {
    let Some(runner) = altar_sacrifices_once("Hundred-Battle Veteran", None, ManaType::Black)
    else {
        return;
    };
    let veteran = runner.state().players[0]
        .graveyard
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Hundred-Battle Veteran");
    assert!(
        veteran.is_some(),
        "reach: the sacrifice put it into the graveyard"
    );
    let verdict = latest_verdict(runner.state());
    assert!(
        matches!(verdict, Err(OfferRefusal::ArrivalDiverged)),
        "{verdict:?}"
    );
}

/// CR 614.6: under Rest in Peace ("If a card or token would be put into a graveyard from anywhere,
/// exile it instead.") the sacrificed Squee, the Immortal ("You may cast this card from your
/// graveyard or from exile.") arrives in exile on both sides, so the replay reaches the cover.
#[test]
fn a_replaced_sacrifice_arriving_where_recorded_is_admitted() {
    let Some(runner) =
        altar_sacrifices_once("Squee, the Immortal", Some("Rest in Peace"), ManaType::Red)
    else {
        return;
    };
    let squee_exiled = runner
        .state()
        .objects
        .values()
        .any(|o| o.name == "Squee, the Immortal" && o.zone == Zone::Exile);
    assert!(
        squee_exiled,
        "reach: Rest in Peace exiled the sacrificed Squee"
    );
    let verdict = latest_verdict(runner.state());
    assert!(
        !matches!(
            verdict,
            Err(OfferRefusal::ArrivalDiverged
                | OfferRefusal::IllegalReplayedPlay
                | OfferRefusal::UnanswerablePrompt
                | OfferRefusal::NoRecurrence
                | OfferRefusal::Randomness
                | OfferRefusal::Fragmented { .. })
        ),
        "{verdict:?}"
    );
}

/// Basalt Monolith ("{T}: Add {C}{C}{C}. {3}: Untap this artifact.") enchanted by Power Artifact,
/// tapped and untapped `cycles` times beside Llanowar Elves, six Mountains, and five Mountains in
/// hand; with `pyromancy`, each cycle also activates Pyromancy at the Elves.
fn basalt_cycles(pyromancy: bool, cycles: usize) -> Option<GameRunner> {
    let db = shared_card_db()?;
    let mut rig = crate::loop_shortcut_mana_engine::setup(true, LoopDetectionMode::Interactive, db);
    let basalt = rig.basalt;
    let state = rig.runner.state_mut();
    let elves = place(state, P0, "Llanowar Elves", db);
    let pyromancy = pyromancy.then(|| place(state, P0, "Pyromancy", db));
    for _ in 0..6 {
        place(state, P0, "Mountain", db);
    }
    for _ in 0..5 {
        let face = db.get_face_by_name("Mountain").expect("card in fixture");
        let id = engine::game::deck_loading::create_object_from_card_face(state, face, P0);
        engine::game::zones::remove_from_zone(state, id, Zone::Library, P0);
        engine::game::zones::add_to_zone(state, id, Zone::Hand, P0);
        state.objects.get_mut(&id).expect("created").zone = Zone::Hand;
    }
    let runner = &mut rig.runner;
    let mana = ability(runner.state(), basalt, true);
    let untap = ability(runner.state(), basalt, false);
    for _ in 0..cycles {
        if is_offer(runner.state()) {
            break;
        }
        activate(runner, basalt, mana);
        settle(runner, &|_| 0);
        if let Some(pyromancy) = pyromancy {
            let index = ability(runner.state(), pyromancy, false);
            activate(runner, pyromancy, index);
            settle(runner, &names(&[elves]));
        }
        activate(runner, basalt, untap);
        settle(runner, &|_| 0);
    }
    Some(rig.runner)
}

/// CR 701.9b + CR 732.2a: a period that activates Pyromancy discards a card at random as its
/// cost, so the replay refuses it at that draw, before any cover; the same Basalt Monolith period
/// without it is offered.
#[test]
fn a_period_paying_a_random_discard_cost_is_refused_as_random() {
    let Some(plain) = basalt_cycles(false, 2) else {
        return;
    };
    assert!(
        is_offer(plain.state()),
        "reach: the plain Basalt Monolith period is offered"
    );
    let random = basalt_cycles(true, 2).expect("the fixture holds Pyromancy");
    let state = random.state();
    assert!(
        state.players[0].graveyard.len() >= 2,
        "reach: each Pyromancy activation discarded a card"
    );
    let verdicts = confirm_for_tests(state);
    assert!(
        verdicts
            .iter()
            .any(|(_, verdict)| matches!(verdict, Err(OfferRefusal::Randomness))),
        "{verdicts:?}"
    );
    assert!(!is_offer(state), "no offer");
}

/// Declares the standing offer at `count`, and every seat asked accepts it; the events emitted.
fn take(runner: &mut GameRunner, count: u32) -> Vec<GameEvent> {
    let mut events = act_events(
        runner,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(count),
            template: None,
        },
    );
    events.extend(accept(runner));
    events
}

/// Every seat asked accepts the standing proposal; the events emitted.
fn accept(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    while matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        events.extend(act_events(
            runner,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        ));
    }
    events
}

/// [`take`], then the passes to the step end.
fn take_to_step_end(runner: &mut GameRunner, count: u32) {
    take(runner, count);
    take_to_step_end_after_take(runner, count);
}

/// Every seat passes until the step ends, where a collapse prompt is answered `count`.
fn take_to_step_end_after_take(runner: &mut GameRunner, count: u32) {
    pass_to_step_end(runner);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::PayAmountChoice { .. }
    ) {
        act(runner, GameAction::SubmitPayAmount { amount: count });
    }
}

/// Every seat passes until the step ends or something other than priority is asked.
fn pass_to_step_end(runner: &mut GameRunner) {
    let phase = runner.state().phase;
    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        && runner.state().phase == phase
    {
        act(runner, GameAction::PassPriority);
    }
}

fn marks_tokens(state: &GameState, seat: PlayerId) -> bool {
    state
        .unbounded_resources
        .get(&seat)
        .is_some_and(|axes| axes.contains(&ResourceAxis::TokensCreated))
}

fn marks_mana(state: &GameState) -> bool {
    state.unbounded_resources.get(&P0).is_some_and(|axes| {
        axes.iter()
            .any(|axis| matches!(axis, ResourceAxis::Mana(_)))
    })
}

/// CR 106.6 + CR 732.2c: Food Chain's mana may be spent only to cast creature spells, so a take of
/// the Food Chain + Eternal Scourge period performs its cycles and leaves that restricted mana,
/// while Basalt Monolith's unrestricted period takes the ∞ mark.
#[test]
fn a_take_whose_period_adds_restricted_mana_performs_it() {
    let Some(mut board) = board_c_offered(BoardCMember::C1EternalScourge) else {
        return;
    };
    assert!(
        is_offer(board.runner.state()),
        "reach: the Food Chain period is offered"
    );
    let pool = |state: &GameState, restricted: bool| {
        state.players[0]
            .mana_pool
            .units()
            .filter(|unit| unit.restrictions.is_empty() != restricted)
            .count()
    };
    let before = pool(board.runner.state(), true);
    take(&mut board.runner, 3);
    let state = board.runner.state();
    assert!(
        pool(state, true) >= before + 3,
        "each performed cycle nets one restricted mana: {before} -> {}",
        pool(state, true)
    );
    assert_eq!(pool(state, false), 0, "no unrestricted mana is added");
    assert!(!marks_mana(state), "the restricted period takes no ∞ mark");

    let mut basalt = basalt_cycles(false, 2).expect("the fixture holds Basalt Monolith");
    assert!(
        is_offer(basalt.state()),
        "reach: the Basalt Monolith period is offered"
    );
    take(&mut basalt, 3);
    assert!(
        marks_mana(basalt.state()),
        "the unrestricted period takes the ∞ mark"
    );
}

/// Board C driven until its period is offered, at most three cycles.
pub(crate) fn board_c_offered(member: BoardCMember) -> Option<food_chain_board::FoodChainBoard> {
    let mut board = food_chain_board::build(member)?;
    board.runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    for _ in 0..3 {
        if is_offer(board.runner.state()) {
            break;
        }
        food_chain_board::exile_with_food_chain(
            &mut board.runner,
            board.food_chain,
            board.creature,
            member.mana(),
        );
        food_chain_board::cast_spell(&mut board.runner, board.creature).expect("cast from exile");
        settle(&mut board.runner, &|_| 0);
    }
    Some(board)
}

fn road(state: &GameState) -> Option<OfferRoad> {
    match &state.waiting_for {
        WaitingFor::LoopShortcut { road, .. } => Some(*road),
        _ => None,
    }
}

fn incarnations(state: &GameState, ids: &[ObjectId]) -> u64 {
    ids.iter().map(|id| state.objects[id].incarnation).sum()
}

/// CR 732.2a + CR 732.2c: each Board C member is offered on the recorded road, and a take of it
/// moves the creature through exile and back once per cycle (CR 400.7).
#[test]
fn board_c_members_are_offered_on_the_recorded_road_and_taken() {
    for member in [
        BoardCMember::C1EternalScourge,
        BoardCMember::C2SqueeTheImmortal,
    ] {
        let Some(mut board) = board_c_offered(member) else {
            return;
        };
        assert_eq!(
            road(board.runner.state()),
            Some(OfferRoad::RecordedPeriod),
            "{member:?}"
        );
        let before = incarnations(board.runner.state(), &[board.creature]);
        take(&mut board.runner, 3);
        let state = board.runner.state();
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "{member:?}: {}",
            state.waiting_for.variant_name()
        );
        let after = incarnations(state, &[board.creature]);
        assert!(
            after > before,
            "{member:?}: the take moved the creature ({before} -> {after})"
        );
    }
}

/// Board C's take: its per-cycle history work and pool walk do not grow with its count.
#[test]
fn board_c_take_work_is_flat_per_cycle() {
    use crate::loop_shortcut::{
        assert_take_history_work_is_flat, assert_take_pool_walk_is_flat, TakeHistoryVector,
    };

    let metered = |n: u32| {
        let mut board =
            board_c_offered(BoardCMember::C1EternalScourge).expect("the fixture holds Board C");
        assert!(is_offer(board.runner.state()), "reach: Board C is offered");
        engine::game::perf_counters::reset();
        take(&mut board.runner, n);
        board.runner.state().clone()
    };
    assert_take_history_work_is_flat(
        32,
        &[
            TakeHistoryVector::JournalEntries,
            TakeHistoryVector::ProducedMana,
            TakeHistoryVector::SpentMana,
            TakeHistoryVector::BattlefieldEntries,
        ],
        &[],
        metered,
    );
    assert_take_pool_walk_is_flat(32, metered);
}

fn tokens(state: &GameState, controller: PlayerId) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].is_token && state.objects[id].controller == controller)
        .collect()
}

/// Passes to the next turn, declaring no attackers and ordering triggers as they are listed.
fn pass_to_next_turn(runner: &mut GameRunner) {
    let turn = runner.state().turn_number;
    for _ in 0..80 {
        let action = match &runner.state().waiting_for {
            _ if runner.state().turn_number != turn => return,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            other => panic!("unexpected prompt {}", other.variant_name()),
        };
        act(runner, action);
    }
    panic!("the turn did not end");
}

/// CR 732.2a + CR 732.2c: Kiki-Jiki's first span refuses at its target prompt; the window after the
/// second activation offers the period on the recorded road, and the take makes what its cycles
/// would: each copy untapped with haste under its own "Sacrifice it at the beginning of the next
/// end step", its enters trigger already resolved, and none left after that step.
#[test]
fn kiki_jiki_is_refused_at_its_first_span_then_offered_and_taken() {
    let Some((mut runner, kiki, exarch)) = kiki_copy_board("Deceiver Exarch") else {
        return;
    };
    let score = kiki_score(kiki, exarch);
    kiki_activates(&mut runner, kiki, &score);
    assert!(!is_offer(runner.state()), "the first span is not offered");
    assert_eq!(
        latest_verdict(runner.state()),
        Err(OfferRefusal::UnanswerablePrompt)
    );
    kiki_activates(&mut runner, kiki, &score);
    assert_eq!(road(runner.state()), Some(OfferRoad::RecordedPeriod));
    let before = tokens(runner.state(), P0);
    take_to_step_end(&mut runner, 2);
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "no copy's enters trigger is owed once the step has ended: {}",
        state.waiting_for.variant_name()
    );
    let minted: Vec<ObjectId> = tokens(state, P0)
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    assert_eq!(minted.len(), 2, "the take made the copies it counted");
    for id in &minted {
        let copy = &state.objects[id];
        assert!(
            !copy.tapped && copy.keywords.contains(&Keyword::Haste),
            "{id:?}: tapped={} keywords={:?}",
            copy.tapped,
            copy.keywords
        );
        assert!(
            state.delayed_triggers.iter().any(|trigger| {
                trigger.condition == DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
                    && matches!(trigger.ability.effect, Effect::Sacrifice { .. })
                    && trigger.ability.targets == [TargetRef::Object(*id)]
            }),
            "{id:?} has no delayed sacrifice: {:?}",
            state.delayed_triggers
        );
    }
    pass_to_next_turn(&mut runner);
    assert_eq!(
        tokens(runner.state(), P0),
        Vec::<ObjectId>::new(),
        "no copy outlives the end step"
    );
}

/// CR 732.1b: Sprout Swarm's Saprolings carry no keyword and no delayed trigger, so the take of
/// its period stands on the ∞ mark.
#[test]
fn a_take_of_a_period_growing_bare_tokens_stands_on_the_mark() {
    let Some((mut runner, sprout, _, _)) = sprout_swarm_board() else {
        return;
    };
    for _ in 0..3 {
        runner.cast(sprout).accept_optional().commit();
        settle(&mut runner, &|_| 0);
    }
    assert!(is_offer(runner.state()), "reach: the recast is offered");
    take(&mut runner, 3);
    let marked = runner.state().unbounded_resources.get(&P0);
    assert!(
        marked.is_some_and(|axes| axes.contains(&ResourceAxis::TokensCreated)),
        "{marked:?}"
    );
}

/// CR 732.2c + CR 111.2: Forbidden Orchard ("Whenever you tap this land for mana, target opponent
/// creates a 1/1 colorless Spirit creature token.") untapped by Voyaging Satyr ("{T}: Untap target
/// land.") under Freed from the Real ("{U}: Untap enchanted creature.") grows Spirits its opponent
/// controls, so the take performs the period: the opponent gets each Spirit, and nothing is marked
/// or minted for the player who looped.
#[test]
fn a_take_of_a_period_growing_an_opponents_tokens_gives_them_to_that_opponent() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let orchard = scenario.add_real_card(P0, "Forbidden Orchard", Zone::Battlefield, db);
    let satyr = scenario.add_real_card(P0, "Voyaging Satyr", Zone::Battlefield, db);
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let freed = place(runner.state_mut(), P0, "Freed from the Real", db);
    attach_to(runner.state_mut(), freed, satyr);
    let untaps = runner.state().objects[&freed]
        .abilities
        .iter()
        .rposition(|ability| ability.kind == AbilityKind::Activated)
        .expect("Freed from the Real's untap ability");
    let score = |action: &GameAction| match action {
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id)),
        } if *id == orchard => 3,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        } => 3,
        other => chooses_color(ManaType::Blue)(other),
    };
    for cycle in 0.. {
        assert!(cycle < 4, "no offer in four cycles");
        let (taps, untaps_land) = (
            ability(runner.state(), orchard, true),
            ability(runner.state(), satyr, false),
        );
        for (source, index) in [(orchard, taps), (satyr, untaps_land), (freed, untaps)] {
            if !is_offer(runner.state()) {
                activate(&mut runner, source, index);
                settle(&mut runner, &score);
            }
        }
        if is_offer(runner.state()) {
            break;
        }
    }
    let spirits = tokens(runner.state(), P1).len();
    assert!(spirits > 0, "reach: the period made the opponent a Spirit");
    take(&mut runner, 3);
    assert!(!marks_tokens(runner.state(), P0), "at the take");
    take_to_step_end_after_take(&mut runner, 3);
    let state = runner.state();
    assert!(!marks_tokens(state, P0), "at the step end");
    assert_eq!(tokens(state, P1).len(), spirits + 3);
    assert_eq!(tokens(state, P0), Vec::<ObjectId>::new());
}

/// CR 732.2c + CR 111.2: Dragonlair Spider ("Whenever an opponent casts a spell, create a 1/1 green
/// Insect creature token.") makes its controller an Insect each time the Altar + Gravecrawler
/// period recasts, so the take performs the period and the Spider's controller gets each Insect.
#[test]
fn a_take_of_a_period_feeding_an_opponents_token_trigger_gives_them_its_tokens() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(None, db);
    place(runner.state_mut(), P1, "Dragonlair Spider", db);
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
    };
    for cycle in 0.. {
        assert!(cycle < 4, "no offer in four cycles");
        cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
        settle(&mut runner, &score);
        if is_offer(runner.state()) {
            break;
        }
        let index = ability(runner.state(), altar, true);
        activate(&mut runner, altar, index);
        settle(&mut runner, &score);
        if is_offer(runner.state()) {
            break;
        }
    }
    let insects = tokens(runner.state(), P1).len();
    assert!(insects > 0, "reach: the period made the opponent an Insect");
    take(&mut runner, 3);
    assert!(!marks_tokens(runner.state(), P0), "at the take");
    take_to_step_end_after_take(&mut runner, 3);
    let state = runner.state();
    assert!(!marks_tokens(state, P0), "at the step end");
    assert_eq!(tokens(state, P1).len(), insects + 3);
    assert_eq!(tokens(state, P0), Vec::<ObjectId>::new());
}

/// The Altar + Gravecrawler board with `payoffs` under P0, driven to its offer.
fn altar_payoffs_offered(payoffs: &[&str]) -> Option<GameRunner> {
    let db = shared_card_db()?;
    let (mut runner, altar, gravecrawler) = altar_board(None, db);
    for payoff in payoffs {
        place(runner.state_mut(), P0, payoff, db);
    }
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
    };
    for cycle in 0.. {
        assert!(cycle < 4, "{payoffs:?}: no offer in four cycles");
        cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
        settle(&mut runner, &score);
        if is_offer(runner.state()) {
            break;
        }
        let index = ability(runner.state(), altar, true);
        activate(&mut runner, altar, index);
        settle(&mut runner, &score);
        if is_offer(runner.state()) {
            break;
        }
    }
    Some(runner)
}

/// CR 732.2c: Aetherflux Reservoir ("Whenever you cast a spell, you gain 1 life for each spell
/// you've cast this turn.") observes the Altar + Gravecrawler + Genesis Chamber period's casts, so
/// its take performs the cycles where it is accepted: each Gravecrawler cast makes its Myr and
/// gains its life then, and nothing is left for the step end to collapse.
#[test]
fn a_take_whose_collapse_would_replay_performs_its_cycles_at_the_take() {
    let Some(mut runner) = altar_payoffs_offered(&["Genesis Chamber", "Aetherflux Reservoir"])
    else {
        return;
    };
    let state = runner.state();
    let (phase, myr, life) = (state.phase, tokens(state, P0).len(), state.players[0].life);
    let cast = state.spells_cast_this_turn_by_player[&P0].len() as i32;
    take(&mut runner, 3);
    let state = runner.state();
    assert_eq!(
        state.phase, phase,
        "reach: the take ends in the step it began"
    );
    assert_eq!(tokens(state, P0).len(), myr + 3, "the take makes its Myr");
    assert_eq!(
        state.players[0].life,
        life + (cast + 1) + (cast + 2) + (cast + 3),
        "each cast gains its life at the take"
    );
    assert!(!state.pending_unbounded_materialization.contains_key(&P0));
    pass_to_step_end(&mut runner);
    let state = runner.state();
    assert_ne!(state.phase, phase, "reach: the step ended");
    assert!(
        !matches!(state.waiting_for, WaitingFor::PayAmountChoice { .. }),
        "no collapse is named at the step end"
    );
    assert_eq!(tokens(state, P0).len(), myr + 3);
}

/// CR 732.1b: Genesis Chamber's Myr are bare tokens nothing observes, so the take of the Altar +
/// Gravecrawler + Genesis Chamber period stands on the ∞ mark and its count is named when the
/// step ends.
#[test]
fn a_take_whose_collapse_batches_names_its_count_at_the_step_end() {
    let Some(mut runner) = altar_payoffs_offered(&["Genesis Chamber"]) else {
        return;
    };
    let myr = tokens(runner.state(), P0).len();
    take(&mut runner, 3);
    let state = runner.state();
    assert!(marks_tokens(state, P0), "the take stands on the mark");
    assert_eq!(tokens(state, P0).len(), myr, "no Myr is made at the take");
    assert!(state.pending_unbounded_materialization.contains_key(&P0));
    pass_to_step_end(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayAmountChoice { .. }
        ),
        "the step end asks for the count: {}",
        runner.state().waiting_for.variant_name()
    );
    act(&mut runner, GameAction::SubmitPayAmount { amount: 3 });
    assert_eq!(
        tokens(runner.state(), P0).len(),
        myr + 3,
        "the step end makes the named Myr"
    );
}

/// CR 732.2a: P1's Kiki-Jiki period on P0's turn is offered to P1, at P1's own priority, and never
/// to P0, who makes none of its plays; P1's decline returns to priority, and P1's take makes P1's
/// copies and marks nothing for P0. P0's Soul Warden triggering inside the period changes none of
/// it.
#[test]
fn a_period_is_offered_only_to_the_seat_that_made_its_plays() {
    for warden in [None, Some(P0)] {
        let Some((mut runner, kiki, exarch)) = kiki_board(P1, "Deceiver Exarch", warden) else {
            return;
        };
        let score = kiki_score(kiki, exarch);
        for activation in 0.. {
            if is_offer(runner.state()) {
                break;
            }
            assert!(activation < 4, "{warden:?}: no offer in four activations");
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
                "{warden:?}: {}",
                runner.state().waiting_for.variant_name()
            );
            act(&mut runner, GameAction::PassPriority);
            if is_offer(runner.state()) {
                break;
            }
            kiki_activates(&mut runner, kiki, &score);
        }
        let WaitingFor::LoopShortcut { proposer, .. } = runner.state().waiting_for else {
            unreachable!("the drive ends on an offer");
        };
        assert_eq!(proposer, P1, "{warden:?}");

        let mut declined = runner.state().clone();
        engine::game::engine::apply(&mut declined, P1, GameAction::DeclineShortcut)
            .expect("the proposer declines");
        assert!(
            matches!(declined.waiting_for, WaitingFor::Priority { .. }),
            "{warden:?}: {}",
            declined.waiting_for.variant_name()
        );

        let before = tokens(runner.state(), P1).len();
        take(&mut runner, 3);
        let state = runner.state();
        assert_eq!(tokens(state, P1).len(), before + 3, "{warden:?}");
        assert_eq!(tokens(state, P0), Vec::<ObjectId>::new(), "{warden:?}");
        assert_eq!(state.unbounded_resources.get(&P0), None, "{warden:?}");
    }
}

/// Grand Architect ("{U}: Target artifact creature becomes blue until end of turn. Tap an untapped
/// blue creature you control: Add {C}{C}. …") and Pili-Pala ("{2}, {Q}: Add one mana of any
/// color."), with Pili-Pala recolored first when `blue`, cycled until an offer; whether one came.
pub(crate) fn grand_architect_pili_pala(
    db: &engine::database::card_db::CardDatabase,
    blue: bool,
) -> (GameRunner, ObjectId, bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let architect = scenario.add_real_card(P0, "Grand Architect", Zone::Battlefield, db);
    let pili = scenario.add_real_card(P0, "Pili-Pala", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let prefer_pili =
        |action: &GameAction| 2 * names(&[pili])(action) + chooses_color(ManaType::Blue)(action);
    if blue {
        let recolor = ability(runner.state(), architect, false);
        activate(&mut runner, architect, recolor);
        settle(&mut runner, &prefer_pili);
    }
    let (tap_blue, untap) = (
        ability(runner.state(), architect, true),
        ability(runner.state(), pili, true),
    );
    for _ in 0..3 {
        activate(&mut runner, architect, tap_blue);
        settle(&mut runner, &prefer_pili);
        if is_offer(runner.state()) {
            return (runner, pili, true);
        }
        let untapped = runner.act(GameAction::ActivateAbility {
            source_id: pili,
            ability_index: untap,
        });
        if untapped.is_err() {
            break;
        }
        settle(&mut runner, &prefer_pili);
    }
    (runner, pili, false)
}

/// Before the recolor Pili-Pala cannot be tapped for {C}{C}, so there is no cycle and no offer;
/// once it is blue, the window after the second tap offers the two-play period on the recorded
/// road, and the take adds the mana its cycles net.
#[test]
fn grand_architect_pili_pala_is_offered_only_once_pili_pala_is_blue() {
    let Some(db) = shared_card_db() else { return };
    let (runner, pili, offered) = grand_architect_pili_pala(db, false);
    assert!(!offered, "no offer before the recolor");
    assert!(
        !runner.state().objects[&pili].tapped,
        "Pili-Pala was never tapped"
    );

    let (mut runner, _, offered) = grand_architect_pili_pala(db, true);
    assert!(offered, "reach: the recolored board offers");
    assert_eq!(road(runner.state()), Some(OfferRoad::RecordedPeriod));
    let pool = |state: &GameState| state.players[0].mana_pool.units().count();
    let before = pool(runner.state());
    take(&mut runner, 3);
    assert!(
        pool(runner.state()) > before,
        "the take added the cycles' mana ({before} -> {})",
        pool(runner.state())
    );
}

/// CR 732.1b: on Food Chain + Eternal Scourge + Misthollow Griffin, the first window's span begins
/// at Food Chain's activation inside the Griffin's payment, which a replay from the priority frame
/// never makes, so the step ends first; the next cycle's window offers on the recorded road, and
/// the take moves the creatures through exile.
#[test]
fn griffin_board_is_refused_at_its_first_window_and_offered_at_the_next() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let food_chain = scenario.add_real_card(P0, "Food Chain", Zone::Battlefield, db);
    let scourge = scenario.add_real_card(P0, "Eternal Scourge", Zone::Battlefield, db);
    let griffin = scenario.add_real_card(P0, "Misthollow Griffin", Zone::Exile, db);
    for seat in [P0, P1, PlayerId(2), PlayerId(3)] {
        for _ in 0..8 {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let cast_paying_with_food_chain =
        |runner: &mut GameRunner, creature: ObjectId, other: ObjectId| {
            cast(runner, creature, vec![], CastPaymentMode::Manual);
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
    assert!(!is_offer(runner.state()));
    assert_eq!(
        latest_verdict(runner.state()),
        Err(OfferRefusal::NoRecurrence)
    );
    cast_paying_with_food_chain(&mut runner, scourge, griffin);
    assert_eq!(road(runner.state()), Some(OfferRoad::RecordedPeriod));
    let before = incarnations(runner.state(), &[griffin, scourge]);
    take(&mut runner, 2);
    let after = incarnations(runner.state(), &[griffin, scourge]);
    assert!(
        after > before,
        "the take moved the creatures ({before} -> {after})"
    );
}

/// CR 732.1b + CR 121.4 + CR 704.5a: Phyrexian Altar recurring Gravecrawler is refused by name:
/// with no payoff nothing grows; Moldervine Reclamation ("Whenever a creature you control dies,
/// you gain 1 life and draw a card.") and Liliana the Repentant ("Whenever another creature or
/// planeswalker you control enters, mill two cards.") deplete the caster's own library, beside
/// Altar of the Brood's mill or alone.
#[test]
fn an_altar_gravecrawler_period_is_refused_at_its_payoffs_stage() {
    let Some(db) = shared_card_db() else { return };
    let Some(board) = board_c_offered(BoardCMember::C2SqueeTheImmortal) else {
        return;
    };
    assert!(is_offer(board.runner.state()), "reach: Board C is offered");
    type Expected = fn(&Result<Vec<String>, OfferRefusal>) -> bool;
    fn at_cover(v: &Result<Vec<String>, OfferRefusal>) -> bool {
        matches!(
            v,
            Err(OfferRefusal::Cover(
                ObjectGrowthVerdict::ResourceRecurrence(None)
            ))
        )
    }
    // Per cycle: P0's hand, P0's library, P1's library.
    let refused: [(&[&str], [i64; 3], Expected); 4] = [
        (&[], [0, 0, 0], |v| *v == Err(OfferRefusal::NoAxis)),
        (
            &["Altar of the Brood", "Moldervine Reclamation"],
            [1, -1, -1],
            at_cover,
        ),
        (&["Liliana the Repentant"], [0, -2, 0], at_cover),
        (
            &["Altar of the Brood", "Liliana the Repentant"],
            [0, -2, -1],
            at_cover,
        ),
    ];
    let sizes = |state: &GameState| {
        [
            state.players[0].hand.len(),
            state.players[0].library.len(),
            state.players[1].library.len(),
        ]
        .map(|n| n as i64)
    };
    for (payoffs, per_cycle, expected) in refused {
        let (mut runner, altar, gravecrawler) = altar_board(payoffs.first().copied(), db);
        for payoff in payoffs.iter().skip(1) {
            place(runner.state_mut(), P0, payoff, db);
        }
        let score = |action: &GameAction| {
            2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
        };
        for cycle in 0..3 {
            let before = sizes(runner.state());
            cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
            settle(&mut runner, &score);
            let index = ability(runner.state(), altar, true);
            activate(&mut runner, altar, index);
            settle(&mut runner, &score);
            assert!(!is_offer(runner.state()), "{payoffs:?} cycle {cycle}");
            let after = sizes(runner.state());
            assert_eq!(
                [0, 1, 2].map(|i| after[i] - before[i]),
                per_cycle,
                "reach: {payoffs:?} cycle {cycle}"
            );
        }
        let verdict = latest_verdict(runner.state());
        assert!(expected(&verdict), "{payoffs:?}: {verdict:?}");
    }
}

/// CR 732.1b: once both rotations of the Altar + Gravecrawler period are named, each later cycle
/// asks and drives the same number of spans, every span a window names being confirmed afresh.
#[test]
fn an_altar_gravecrawler_meter_is_flat_once_both_rotations_are_named() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(None, db);
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
    };
    let mut per_cycle = Vec::new();
    for _ in 0..6 {
        let before = play_trace_counters();
        cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
        settle(&mut runner, &score);
        let index = ability(runner.state(), altar, true);
        activate(&mut runner, altar, index);
        settle(&mut runner, &score);
        let run = play_trace_counters().since(before);
        per_cycle.push((run.confirm_asks, run.confirm_drives));
    }
    let rotations = play_trace_view(runner.state())
        .expect("a trace")
        .named
        .iter()
        .filter(|span| span.cause == NamingCause::Repeat)
        .count();
    assert!(rotations >= 2, "reach: both rotations are named");
    assert!(per_cycle[0].1 > 0, "reach: the first span is driven");
    assert_eq!(per_cycle[2..], [(1, 1); 4], "{per_cycle:?}");
}

/// CR 732.2a: Aetherflux Reservoir ("Whenever you cast a spell, you gain 1 life for each spell
/// you've cast this turn.") against two Eidolon of the Great Revel ("Whenever a player casts a
/// spell with mana value 3 or less, Eidolon of the Great Revel deals 2 damage to that player.")
/// costs the Altar + Gravecrawler period life until the turn's fifth cast, so the period refused
/// for want of an axis is offered by the third cycle.
#[test]
fn a_period_refused_for_no_axis_is_offered_once_it_nets_life() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Walking Corpse", Zone::Battlefield, db);
    let gravecrawler = scenario.add_real_card(P0, "Gravecrawler", Zone::Graveyard, db);
    for card in ["Swamp", "Aetherflux Reservoir"] {
        scenario.add_real_card(P0, card, Zone::Battlefield, db);
    }
    for _ in 0..2 {
        scenario.add_real_card(P1, "Eidolon of the Great Revel", Zone::Battlefield, db);
    }
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
    };
    let mut refused_for_no_axis = false;
    for _ in 0..3 {
        cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
        settle(&mut runner, &score);
        if is_offer(runner.state()) {
            break;
        }
        let index = ability(runner.state(), altar, true);
        activate(&mut runner, altar, index);
        settle(&mut runner, &score);
        if is_offer(runner.state()) {
            break;
        }
        refused_for_no_axis |= latest_verdict(runner.state()) == Err(OfferRefusal::NoAxis);
    }
    assert!(
        refused_for_no_axis,
        "reach: the period was refused while it cost life"
    );
    assert_eq!(road(runner.state()), Some(OfferRoad::RecordedPeriod));
}

/// CR 732.2a + CR 614.1a: Burning-Tree Shaman ("Whenever a player activates an ability that isn't
/// a mana ability, this creature deals 1 damage to that player.") costs the Basalt Monolith +
/// Power Artifact period 1 life a cycle, until Angel's Grace ("... Until end of turn, damage that
/// would reduce your life total to less than 1 reduces it to 1 instead.") holds the total at 1; the
/// period refused while it costs life is offered from the first frame whose repeat costs none.
#[test]
fn a_period_costing_life_is_offered_once_a_floor_holds_the_life_total() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 5);
    let basalt = scenario.add_real_card(P0, "Basalt Monolith", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Plains", Zone::Battlefield, db);
    let grace = scenario.add_real_card(P0, "Angel's Grace", Zone::Hand, db);
    scenario.add_real_card(P1, "Burning-Tree Shaman", Zone::Battlefield, db);
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Wastes", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let power = place(runner.state_mut(), P0, "Power Artifact", db);
    attach_to(runner.state_mut(), power, basalt);
    cast(&mut runner, grace, vec![], CastPaymentMode::Auto);
    settle(&mut runner, &|_| 0);
    let mana = ability(runner.state(), basalt, true);
    let untap = ability(runner.state(), basalt, false);
    let life = |state: &GameState| state.players[0].life;
    let mut refused_at = Vec::new();
    for _ in 0..6 {
        activate(&mut runner, basalt, mana);
        settle(&mut runner, &|_| 0);
        if is_offer(runner.state()) {
            break;
        }
        activate(&mut runner, basalt, untap);
        settle(&mut runner, &|_| 0);
        if is_offer(runner.state()) {
            break;
        }
        if latest_verdict(runner.state()) == Err(OfferRefusal::NoAxis) {
            refused_at.push(life(runner.state()));
        }
    }
    assert_eq!(
        refused_at,
        [4, 3],
        "reach: the period was refused while it cost life"
    );
    assert_eq!(road(runner.state()), Some(OfferRoad::RecordedPeriod));
    assert_eq!(life(runner.state()), 2);
}

/// Cycles the Altar + Gravecrawler period at most `cycles` times, declining each offer `declines`
/// holds for, to the first offer it does not; each offer's P1 library size and certificate.
fn drive_altar_offers(
    runner: &mut GameRunner,
    (altar, gravecrawler): (ObjectId, ObjectId),
    cycles: usize,
    declines: impl Fn(&GameState) -> bool,
) -> Vec<(usize, LoopCertificate)> {
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
    };
    let mut offers = Vec::new();
    for _ in 0..cycles {
        for activates in [false, true] {
            if activates {
                let index = ability(runner.state(), altar, true);
                activate(runner, altar, index);
            } else {
                cast(runner, gravecrawler, vec![], CastPaymentMode::Auto);
            }
            settle(runner, &score);
            while let WaitingFor::LoopShortcut { certificate, .. } = &runner.state().waiting_for {
                offers.push((runner.state().players[1].library.len(), certificate.clone()));
                if !declines(runner.state()) {
                    return offers;
                }
                act(runner, GameAction::DeclineShortcut);
                settle(runner, &score);
            }
        }
    }
    let verdict = confirm_for_tests(runner.state())
        .pop()
        .map(|(_, verdict)| verdict);
    panic!("no standing offer in {cycles} cycles; declined {offers:?}; latest verdict {verdict:?}");
}

/// CR 701.17b + CR 732.2a: Altar of the Brood ("Whenever another permanent you control enters,
/// each opponent mills a card.") makes the Altar + Gravecrawler period mill P1 a card a cycle; it
/// is offered as an advantage on P1's library, and a take performs its cycles.
#[test]
fn an_altar_of_the_brood_period_is_offered_and_taken() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(Some("Altar of the Brood"), db);
    let offers = drive_altar_offers(&mut runner, (altar, gravecrawler), 3, |_| false);
    let (library, certificate) = offers.last().expect("an offer");
    assert_eq!(road(runner.state()), Some(OfferRoad::RecordedPeriod));
    assert_eq!(certificate.unbounded, [ResourceAxis::LibraryDelta(P1)]);
    assert_eq!(certificate.win_kind, WinKind::Advantage);
    take(&mut runner, 2);
    let state = runner.state();
    assert_eq!(state.players[1].library.len(), library - 2);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player: P0 }),
        "{}",
        state.waiting_for.variant_name()
    );
}

/// Phyrexian Altar and Gravecrawler beside Altar of the Brood and Soul Warden ("Whenever another
/// creature enters, you gain 1 life."), with three cards in P1's library.
fn soul_warden_mill_board(db: &CardDatabase) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Walking Corpse", Zone::Battlefield, db);
    let gravecrawler = scenario.add_real_card(P0, "Gravecrawler", Zone::Graveyard, db);
    for payoff in ["Swamp", "Altar of the Brood", "Soul Warden"] {
        scenario.add_real_card(P0, payoff, Zone::Battlefield, db);
    }
    for (seat, cards) in [(P0, 10), (P1, 3)] {
        for _ in 0..cards {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    (runner, altar, gravecrawler)
}

/// CR 701.17b + CR 732.2a: the Soul Warden board's period is offered while Altar of the Brood
/// still has P1's cards to mill, and once each offer is declined and the library is empty, Soul
/// Warden's life alone makes it worth repeating, so the same plays are offered again.
#[test]
fn an_altar_gravecrawler_period_is_offered_once_the_milled_library_is_empty() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = soul_warden_mill_board(db);
    let offers = drive_altar_offers(&mut runner, (altar, gravecrawler), 8, |state| {
        !state.players[1].library.is_empty()
    });
    let (first_library, first) = &offers[0];
    assert!(*first_library > 0, "{offers:?}");
    assert_eq!(
        first.unbounded,
        [ResourceAxis::Life(P0), ResourceAxis::LibraryDelta(P1)]
    );
    let (library, last) = offers.last().expect("an offer");
    assert_eq!(*library, 0);
    assert_eq!(last.unbounded, [ResourceAxis::Life(P0)]);
    assert_eq!(last.win_kind, WinKind::Advantage);
    assert_eq!(road(runner.state()), Some(OfferRoad::RecordedPeriod));
}

/// CR 701.17b + CR 732.2c: on the Soul Warden board, a take of the offer made at P1's last card
/// performs its cycles, milling that card and gaining P0 its life, and marks nothing.
#[test]
fn a_take_whose_period_mills_the_last_card_performs_it() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = soul_warden_mill_board(db);
    let offers = drive_altar_offers(&mut runner, (altar, gravecrawler), 8, |state| {
        state.players[1].library.len() > 1
    });
    assert!(
        offers[0]
            .1
            .unbounded
            .contains(&ResourceAxis::LibraryDelta(P1)),
        "reach: {offers:?}"
    );
    let (library, certificate) = offers.last().expect("an offer");
    assert_eq!(*library, 1, "reach");
    assert_eq!(certificate.unbounded, [ResourceAxis::Life(P0)]);
    let state = runner.state();
    let (graveyard, life) = (state.players[1].graveyard.len(), state.players[0].life);
    take(&mut runner, 2);
    let state = runner.state();
    assert_eq!(state.players[1].library.len(), 0);
    assert_eq!(state.players[1].graveyard.len(), graveyard + 1);
    assert_eq!(state.players[0].life, life + 2);
    assert!(!state
        .unbounded_resources
        .get(&P0)
        .is_some_and(|axes| axes.contains(&ResourceAxis::Life(P0))));
}

/// CR 701.17b + CR 603.6a: with Genesis Chamber ("Whenever a nontoken creature enters, if this
/// artifact is untapped, that creature's controller creates a 1/1 colorless Myr artifact creature
/// token.") beside Altar of the Brood, each Gravecrawler entry and each Myr entry mills P1 a card,
/// so a take of the offer made at P1's last two cards performs its cycle rather than standing on
/// the token mark.
#[test]
fn a_take_whose_minting_period_mills_the_last_cards_performs_them() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(Some("Altar of the Brood"), db);
    place(runner.state_mut(), P0, "Genesis Chamber", db);
    let offers = drive_altar_offers(&mut runner, (altar, gravecrawler), 8, |state| {
        state.players[1].library.len() > 2
    });
    assert!(
        offers[0]
            .1
            .unbounded
            .contains(&ResourceAxis::LibraryDelta(P1)),
        "reach: {offers:?}"
    );
    let (library, certificate) = offers.last().expect("an offer");
    assert_eq!(*library, 2, "reach");
    assert!(certificate.unbounded.contains(&ResourceAxis::TokensCreated));
    assert!(!certificate
        .unbounded
        .contains(&ResourceAxis::LibraryDelta(P1)));
    let state = runner.state();
    let (graveyard, myr) = (state.players[1].graveyard.len(), tokens(state, P0).len());
    take(&mut runner, 1);
    let state = runner.state();
    assert_eq!(state.players[1].library.len(), 0);
    assert_eq!(state.players[1].graveyard.len(), graveyard + 2);
    assert_eq!(tokens(state, P0).len(), myr + 1);
    assert!(!marks_tokens(state, P0));
    assert!(!state.pending_unbounded_materialization.contains_key(&P0));
}

/// CR 732.3: two seats each tapping and untapping their own Basalt Monolith under Power Artifact
/// ("Enchanted artifact's activated abilities cost {2} less to activate. …") name a span holding
/// both seats' plays, which is refused before any replay; nothing is offered.
#[test]
fn a_span_holding_two_seats_plays_is_refused_as_fragmented_without_a_drive() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let monoliths =
        [P0, P1].map(|seat| scenario.add_real_card(seat, "Basalt Monolith", Zone::Battlefield, db));
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Wastes", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    for (seat, monolith) in [P0, P1].into_iter().zip(monoliths) {
        let power = place(runner.state_mut(), seat, "Power Artifact", db);
        attach_to(runner.state_mut(), power, monolith);
    }
    let indices = monoliths.map(|monolith| {
        (
            ability(runner.state(), monolith, true),
            ability(runner.state(), monolith, false),
        )
    });
    let mut fragmented = None;
    for _ in 0..3 {
        for ((seat, monolith), (mana, untap)) in [P0, P1].into_iter().zip(monoliths).zip(indices) {
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == seat)
            );
            activate(&mut runner, monolith, mana);
            let before = play_trace_counters();
            activate(&mut runner, monolith, untap);
            let run = play_trace_counters().since(before);
            let last = play_trace_view(runner.state()).and_then(|view| view.named.last().copied());
            if let (Some(span), true, true) = (last, seat == P1, fragmented.is_none()) {
                let verdict = confirm_for_tests(runner.state())
                    .into_iter()
                    .find(|(named, _)| *named == span)
                    .map(|(_, verdict)| verdict);
                fragmented = Some((run.confirm_asks, run.confirm_drives, verdict));
            }
            act(&mut runner, GameAction::PassPriority);
        }
        settle(&mut runner, &|_| 0);
        assert!(!is_offer(runner.state()));
    }
    let view = play_trace_view(runner.state()).expect("a trace");
    assert!(
        view.named
            .iter()
            .any(|span| span.cause == NamingCause::Repeat),
        "reach: repeats name spans"
    );
    assert_eq!(
        fragmented,
        Some((
            1,
            0,
            Some(Err(OfferRefusal::Fragmented {
                seats: vec![P0, P1]
            }))
        ))
    );
}

/// Presence of Gond on Grizzly Bears with Intruder Alarm and Llanowar Elves; `extra` is a card in
/// P0's hand, or a Forest on the battlefield.
fn gond_board(
    db: &engine::database::card_db::CardDatabase,
    extra: Option<&str>,
) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Intruder Alarm", Zone::Battlefield, db);
    let elves = scenario.add_real_card(P0, "Llanowar Elves", Zone::Battlefield, db);
    let extra = extra.map(|name| {
        let zone = if name == "Forest" {
            Zone::Battlefield
        } else {
            Zone::Hand
        };
        scenario.add_real_card(P0, name, zone, db)
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
    (runner, bears, elves, extra)
}

/// One Gond cycle: Llanowar Elves taps for {G}, `between` runs, the Bears make an Elf, and the
/// stack settles. The meter's asks at the Bears' window.
fn gond_cycle(
    runner: &mut GameRunner,
    bears: ObjectId,
    elves: ObjectId,
    between: impl FnOnce(&mut GameRunner),
) -> u64 {
    let green = chooses_color(ManaType::Green);
    let tap = ability(runner.state(), elves, true);
    activate(runner, elves, tap);
    settle(runner, &green);
    between(runner);
    let before = play_trace_counters();
    let make = ability(runner.state(), bears, false);
    activate(runner, bears, make);
    settle(runner, &green);
    play_trace_counters().since(before).confirm_asks
}

/// CR 732.2a + CR 117.3c: the Gond board's first span, Llanowar Elves' tap and Mobilize ("Untap
/// all creatures you control."), is refused, and the retry offers the Bears' span at the same
/// window after two asks; without Mobilize the first span offers and nothing is retried.
#[test]
fn the_gond_board_offers_the_bears_span_after_refusing_the_first() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, bears, elves, mobilize) = gond_board(db, Some("Mobilize"));
    let mobilize = mobilize.expect("Mobilize");
    let asks = gond_cycle(&mut runner, bears, elves, |runner| {
        cast(runner, mobilize, vec![], CastPaymentMode::Auto);
        settle(runner, &|_| 0);
    });
    let view = play_trace_view(runner.state()).expect("a trace");
    let offered = view.offered.expect("the Bears' span is offered");
    let first = view
        .named
        .iter()
        .find(|span| span.end == offered.end)
        .copied()
        .expect("the window's first span");
    assert!(first.start < offered.start, "{:?}", view.named);
    assert_eq!(asks, 2);

    let (mut runner, bears, elves, _) = gond_board(db, None);
    let asks = gond_cycle(&mut runner, bears, elves, |_| {});
    assert!(
        is_offer(runner.state()),
        "reach: the board without Mobilize offers"
    );
    assert_eq!(asks, 1);
}

/// Hostiles on the Gond board without Mobilize: Giant Growth ("Target creature gets +3/+3 until
/// end of turn.") cast while the Bears' ability is on the stack is not replayable, so that window
/// offers nothing and the next cycle's does; a Forest tapped between the Elves and the Bears is
/// left out of the span the retry offers.
#[test]
fn the_gond_board_hostiles_are_offered_where_the_period_recurs() {
    let Some(db) = shared_card_db() else { return };
    let mut canary =
        crate::loop_shortcut_activation::setup(true, true, LoopDetectionMode::Interactive, db);
    let index =
        crate::loop_shortcut_activation::token_ability_index(canary.runner.state(), canary.host)
            .expect("Gond's granted ability");
    crate::loop_shortcut_activation::activate_and_drive(&mut canary.runner, canary.host, index);
    assert!(is_offer(canary.runner.state()), "reach: the canary offers");

    let (mut runner, bears, elves, growth) = gond_board(db, Some("Giant Growth"));
    let growth = growth.expect("Giant Growth");
    let green = chooses_color(ManaType::Green);
    let tap = ability(runner.state(), elves, true);
    activate(&mut runner, elves, tap);
    settle(&mut runner, &green);
    let make = ability(runner.state(), bears, false);
    activate(&mut runner, bears, make);
    cast(&mut runner, growth, vec![bears], CastPaymentMode::Auto);
    settle(&mut runner, &green);
    assert!(
        !is_offer(runner.state()),
        "the Giant Growth window offers nothing"
    );
    assert!(
        confirm_for_tests(runner.state())
            .iter()
            .all(|(_, verdict)| *verdict == Err(OfferRefusal::IllegalReplayedPlay)),
        "{:?}",
        confirm_for_tests(runner.state())
    );
    gond_cycle(&mut runner, bears, elves, |_| {});
    assert!(is_offer(runner.state()), "the next cycle's window offers");

    let (mut runner, bears, elves, forest) = gond_board(db, Some("Forest"));
    let forest = forest.expect("Forest");
    gond_cycle(&mut runner, bears, elves, |runner| {
        activate(runner, forest, 0);
        settle(runner, &green);
    });
    let view = play_trace_view(runner.state()).expect("a trace");
    let offered = view.offered.expect("the retry offers");
    assert!(view.named[0].start < offered.start, "{:?}", view.named);
}

/// Witherbloom, the Balancer ("Instant and sorcery spells you cast have affinity for creatures.")
/// with Sprout Swarm ("Convoke", "Buyback {3}", "Create a 1/1 green Saproling creature token.") in
/// hand and nine Forests; the opponent holds Murder ("Destroy target creature.") and three Swamps.
fn sprout_swarm_board() -> Option<(GameRunner, ObjectId, ObjectId, ObjectId)> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let witherbloom =
        scenario.add_real_card(P0, "Witherbloom, the Balancer", Zone::Battlefield, db);
    let sprout = scenario.add_real_card(P0, "Sprout Swarm", Zone::Hand, db);
    let murder = scenario.add_real_card(P1, "Murder", Zone::Hand, db);
    for _ in 0..9 {
        scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    }
    for _ in 0..3 {
        scenario.add_real_card(P1, "Swamp", Zone::Battlefield, db);
    }
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Forest", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    Some((runner, sprout, witherbloom, murder))
}

fn saprolings(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].name == "Saproling")
        .count()
}

/// CR 732.2a + CR 702.51a: three Sprout Swarm casts spend the Forests and leave three Saprolings,
/// and the window after the third offers the recast: its replay pays by convoke what the recorded
/// cast paid with Forests.
#[test]
fn a_sprout_swarm_ramp_up_is_offered_once_its_forests_are_spent() {
    let Some((mut runner, sprout, _, _)) = sprout_swarm_board() else {
        return;
    };
    for cast in 0..3 {
        assert!(!is_offer(runner.state()), "before cast {cast}");
        runner.cast(sprout).accept_optional().commit();
        settle(&mut runner, &|_| 0);
    }
    assert_eq!(saprolings(runner.state()), 3, "reach: each cast resolved");
    assert_eq!(road(runner.state()), Some(OfferRoad::RecordedPeriod));
}

/// CR 732.3: the same board with Murder cast in response to the third Sprout Swarm: the span
/// holds both seats' plays and is refused before any replay.
#[test]
fn an_opponents_response_fragments_the_sprout_swarm_span() {
    let Some((mut runner, sprout, witherbloom, murder)) = sprout_swarm_board() else {
        return;
    };
    for _ in 0..2 {
        runner.cast(sprout).accept_optional().commit();
        settle(&mut runner, &|_| 0);
        assert!(!is_offer(runner.state()));
    }
    assert_eq!(
        saprolings(runner.state()),
        2,
        "reach: each cast made a Saproling"
    );
    let before = play_trace_counters();
    runner.cast(sprout).accept_optional().commit();
    act(&mut runner, GameAction::PassPriority);
    cast(
        &mut runner,
        murder,
        vec![witherbloom],
        CastPaymentMode::Auto,
    );
    settle(&mut runner, &names(&[witherbloom]));
    let run = play_trace_counters().since(before);
    let state = runner.state();
    assert_eq!(
        state.objects[&witherbloom].zone,
        Zone::Graveyard,
        "reach: Murder resolved"
    );
    assert_eq!(saprolings(state), 3, "reach: Sprout Swarm resolved");
    assert!(!is_offer(state));
    assert_eq!(
        latest_verdict(state),
        Err(OfferRefusal::Fragmented {
            seats: vec![P0, P1]
        })
    );
    assert_eq!((run.confirm_asks, run.confirm_drives), (1, 0));

    let offered = board_c_offered(BoardCMember::C2SqueeTheImmortal).expect("Board C");
    assert!(
        is_offer(offered.runner.state()),
        "reach: Board C is offered"
    );
}

/// CR 732.2a: Food Chain ("Add X mana of any one color ... Spend this mana only to cast creature
/// spells.") making green cannot recast Squee, the Immortal ({1}{R}{R}) once the red in the pool
/// is spent, so the replay refuses; the same plays with red chosen are offered.
#[test]
fn a_replay_refused_for_want_of_red_mana_is_asked_again_once_food_chain_makes_red() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let food_chain = scenario.add_real_card(P0, "Food Chain", Zone::Battlefield, db);
    let squee = scenario.add_real_card(P0, "Squee, the Immortal", Zone::Battlefield, db);
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|unit| ManaUnit::new(ManaType::Red, ObjectId(9_900 + unit), false, Vec::new()))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let exile_for = |runner: &mut GameRunner, color: ManaType| {
        activate(runner, food_chain, 0);
        act(runner, GameAction::SelectCards { cards: vec![squee] });
        act(
            runner,
            GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(color),
                count: 1,
            },
        );
    };
    for _ in 0..2 {
        exile_for(&mut runner, ManaType::Green);
        assert!(!is_offer(runner.state()), "no offer while green is made");
        cast(&mut runner, squee, vec![], CastPaymentMode::Auto);
        settle(&mut runner, &|_| 0);
        assert!(!is_offer(runner.state()), "no offer while green is made");
    }
    assert_eq!(
        latest_verdict(runner.state()),
        Err(OfferRefusal::IllegalReplayedPlay),
        "reach: the green cycle was refused by its replay"
    );
    exile_for(&mut runner, ManaType::Red);
    assert_eq!(road(runner.state()), Some(OfferRoad::RecordedPeriod));
}

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// [`altar_board`]'s cards and libraries on `lives.len()` seats, each seat at its entry of `lives`:
/// only the seat count and the life totals differ from that board.
fn altar_board_at(
    payoff: &str,
    lives: &[i32],
    db: &CardDatabase,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(lives.len() as u8, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for (seat, life) in lives.iter().enumerate() {
        scenario.with_life(PlayerId(seat as u8), *life);
    }
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Walking Corpse", Zone::Battlefield, db);
    let gravecrawler = scenario.add_real_card(P0, "Gravecrawler", Zone::Graveyard, db);
    scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    scenario.add_real_card(P0, payoff, Zone::Battlefield, db);
    for seat in 0..lives.len() {
        for _ in 0..10 {
            scenario.add_real_card(PlayerId(seat as u8), "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    (runner, altar, gravecrawler)
}

fn lives(state: &GameState) -> Vec<i32> {
    state.players.iter().map(|p| p.life).collect()
}

/// The seats `events` eliminate, in order.
fn eliminations(events: &[GameEvent]) -> Vec<PlayerId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerEliminated { player_id } => Some(*player_id),
            _ => None,
        })
        .collect()
}

fn eliminated(state: &GameState) -> Vec<PlayerId> {
    state
        .players
        .iter()
        .filter(|p| p.is_eliminated)
        .map(|p| p.id)
        .collect()
}

/// Casts Gravecrawler and sacrifices it to the Altar, up to three cycles, until the engine offers
/// the loop; each seat in `drained` loses exactly 1 life per sacrifice, and a payoff's target
/// prompt is answered with P1.
fn drain_to_offer(
    runner: &mut GameRunner,
    altar: ObjectId,
    gravecrawler: ObjectId,
    drained: &[PlayerId],
) {
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action)
            + chooses_color(ManaType::Black)(action)
            + i32::from(matches!(
                action,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1))
                }
            ))
    };
    for cycle in 0..3 {
        cast(runner, gravecrawler, vec![], CastPaymentMode::Auto);
        settle(runner, &score);
        let before = lives(runner.state());
        let index = ability(runner.state(), altar, true);
        activate(runner, altar, index);
        settle(runner, &score);
        let after = lives(runner.state());
        for seat in drained {
            let seat = usize::from(seat.0);
            assert_eq!(after[seat], before[seat] - 1, "reach: cycle {cycle}");
        }
        if is_offer(runner.state()) {
            return;
        }
    }
}

/// The offer standing at `state`, asserted to be a recorded period offered exactly `count`
/// repetitions with that capacity; its certificate.
fn bounded_recorded_offer(state: &GameState, count: u32) -> &LoopCertificate {
    let WaitingFor::LoopShortcut {
        certificate,
        schema,
        road,
        ..
    } = &state.waiting_for
    else {
        panic!("no offer: {:?}", latest_verdict(state));
    };
    assert_eq!(*road, OfferRoad::RecordedPeriod);
    assert_eq!(schema.iteration_count, IterationCount::Fixed(count));
    assert_eq!(schema.deliverable_capacity, count);
    assert_eq!(certificate.win_kind, WinKind::LethalDamage);
    certificate
}

/// The board after `count` repetitions are declared and accepted on a clone of `state`.
fn taken(state: &GameState, count: u32) -> GameState {
    let mut runner = GameRunner::from_state(state.clone());
    take(&mut runner, count);
    runner.state().clone()
}

/// CR 732.2a: a count past the last predicted crossing is refused at its declaration, before any
/// seat is asked or any cycle performed.
fn assert_declaration_refused(state: &GameState, count: u32) {
    let mut runner = GameRunner::from_state(state.clone());
    act(
        &mut runner,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(count),
            template: None,
        },
    );
    let after = runner.state();
    assert_eq!(after.waiting_for, WaitingFor::Priority { player: P0 });
    assert_eq!(lives(after), lives(state));
    assert!(eliminated(after).is_empty());
}

/// CR 704.5a + CR 732.2a + CR 800.4a: Phyrexian Altar ("Sacrifice a creature: Add one mana of any
/// color.") recurring Gravecrawler beside Zulaport Cutthroat ("Whenever this creature or another
/// creature you control dies, each opponent loses 1 life and you gain 1 life.") or Blood Artist
/// ("Whenever this creature or another creature dies, target player loses 1 life and you gain 1
/// life.") is offered exactly as many repetitions as P1 has life, and a take performs the count it
/// declares.
#[test]
fn an_altar_zulaport_period_is_offered_a_bounded_shortcut_and_taken() {
    let Some(db) = shared_card_db() else { return };
    for payoff in ["Zulaport Cutthroat", "Blood Artist"] {
        let (mut runner, altar, gravecrawler) = altar_board(Some(payoff), db);
        drain_to_offer(&mut runner, altar, gravecrawler, &[P1]);
        let offered = runner.state().clone();
        let life = offered.players[1].life;
        let l = life as u32;
        let certificate = bounded_recorded_offer(&offered, l);
        let per_cycle = certificate
            .per_cycle
            .as_ref()
            .unwrap_or_else(|| panic!("{payoff}: a signed offer"));
        assert_eq!(
            per_cycle.delta.life,
            [(P0, 1), (P1, -1)].into_iter().collect(),
            "{payoff}"
        );
        assert!(
            engine::ai_support::candidate_actions(&offered)
                .iter()
                .any(|candidate| candidate.action
                    == GameAction::DeclareShortcut {
                        count: IterationCount::Fixed(l),
                        template: None,
                    }),
            "{payoff}: the AI proposer may declare the offer"
        );

        let three = taken(&offered, 3);
        assert_eq!(
            lives(&three),
            [offered.players[0].life + 3, life - 3],
            "{payoff}"
        );
        assert_eq!(three.waiting_for, WaitingFor::Priority { player: P0 });
        assert!(eliminated(&three).is_empty(), "{payoff}");

        let mut runner = GameRunner::from_state(offered.clone());
        let events = take(&mut runner, l);
        let all = runner.state();
        assert_eq!(
            all.waiting_for,
            WaitingFor::GameOver { winner: Some(P0) },
            "{payoff}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == GameEvent::GameOver { winner: Some(P0) })
                .count(),
            1,
            "{payoff}"
        );
        assert_eq!(eliminated(all), [P1], "{payoff}");

        assert_declaration_refused(&offered, l + 1);
    }
}

/// `state` reloaded through the persisted codec the WASM restore and server persistence use, then
/// through bare `GameState` serde.
pub(super) fn reload_both(state: &GameState) -> [Result<GameState, String>; 2] {
    let saved = serde_json::to_value(PersistedGameState::capture(state.clone())).expect("saves");
    let persisted = serde_json::from_value::<PersistedGameState>(saved)
        .map_err(|error| error.to_string())
        .and_then(|saved| saved.into_game_state().map_err(|error| error.to_string()));
    let bare = serde_json::from_value::<GameState>(serde_json::to_value(state).expect("saves"))
        .map_err(|error| error.to_string());
    [persisted, bare]
}

/// `state` reloaded through both ingresses, each asserted to restore the same decision.
pub(super) fn reloaded(state: &GameState) -> [GameState; 2] {
    reload_both(state).map(|restored| {
        let restored = restored.expect("an engine-minted state reloads");
        assert_eq!(restored.waiting_for, state.waiting_for);
        restored
    })
}

/// `state` bound to an interaction session, and the interaction it publishes to P0.
pub(super) fn published_to_p0(state: &GameState) -> (GameState, ViewerInteraction) {
    let mut bound = state.clone();
    bind_interaction_authority(&mut bound, InteractionSessionId("shortcut".into()))
        .expect("valid interaction authority binding");
    let filtered = filter_state_for_viewer(&bound, P0);
    let view = derive_viewer_interaction(&bound, &filtered, P0);
    (bound, view)
}

/// The opportunity among `view`'s that is a shortcut schema, and its points.
pub(super) fn shortcut_schema(
    view: &ViewerInteraction,
) -> Option<(&InteractionOpportunity, &[InteractionShortcutPoint])> {
    view.opportunities
        .iter()
        .find_map(|opportunity| match &opportunity.response {
            InteractionOpportunityResponse::Schema {
                spec: InteractionResponseSpec::Shortcut { points, .. },
                ..
            } => Some((opportunity, points.as_slice())),
            _ => None,
        })
}

/// Each ingress refuses `state` for carrying a confirmed period on the ring road.
fn assert_refused_on_reload(state: &GameState) {
    for restored in reload_both(state) {
        let message = restored.expect_err("a ring-road period is refused");
        assert!(message.contains("confirmed period"), "{message}");
    }
}

/// `state` with its offer's or proposal's road set to the ring road.
fn on_the_ring_road(state: &GameState) -> GameState {
    let mut state = state.clone();
    match &mut state.waiting_for {
        WaitingFor::LoopShortcut { road, .. } => *road = OfferRoad::Ring,
        WaitingFor::RespondToShortcut { proposal, .. } => proposal.road = OfferRoad::Ring,
        other => panic!("no offer or proposal: {other:?}"),
    }
    state
}

/// The Altar–Zulaport board standing on its bounded recorded-period offer, which carries its period.
fn bounded_recorded_offer_with_period(db: &CardDatabase) -> GameState {
    let (mut runner, altar, gravecrawler) = altar_board(Some("Zulaport Cutthroat"), db);
    drain_to_offer(&mut runner, altar, gravecrawler, &[P1]);
    let offered = runner.state().clone();
    bounded_recorded_offer(&offered, offered.players[1].life as u32);
    let WaitingFor::LoopShortcut { period, .. } = &offered.waiting_for else {
        unreachable!("asserted above")
    };
    assert!(!period.is_empty(), "the bounded offer carries its period");
    offered
}

/// `state` after the standing offer is declared at `count`, asserted to carry the offer's road and
/// period onto the proposal.
fn proposed(state: GameState, count: u32) -> GameState {
    let mut runner = GameRunner::from_state(state);
    act(
        &mut runner,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(count),
            template: None,
        },
    );
    let WaitingFor::RespondToShortcut { proposal, .. } = &runner.state().waiting_for else {
        panic!("no proposal: {:?}", runner.state().waiting_for);
    };
    assert_eq!(proposal.road, OfferRoad::RecordedPeriod);
    assert!(
        !proposal.period.is_empty(),
        "the proposal carries the period"
    );
    runner.state().clone()
}

/// CR 732.2a + CR 732.2c: a bounded recorded-period offer and the proposal declared against it
/// survive a save and reload, and the take after both reloads is the take without them.
#[test]
fn a_bounded_recorded_offer_and_its_proposal_survive_a_reload() {
    let Some(db) = shared_card_db() else { return };
    let offered = bounded_recorded_offer_with_period(db);
    let l = offered.players[1].life as u32;
    let expected = serde_json::to_value(taken(&offered, 3)).expect("saves");

    for offer in reloaded(&offered) {
        bounded_recorded_offer(&offer, l);
        for proposal in reloaded(&proposed(offer, 3)) {
            let mut runner = GameRunner::from_state(proposal);
            accept(&mut runner);
            assert_eq!(
                serde_json::to_value(runner.state()).expect("saves"),
                expected
            );
        }
    }
}

/// The same bounded offer, and the proposal declared against it, on the ring road: its take is the
/// ring drain, so the period it carries is refused at both ingresses.
#[test]
fn a_bounded_offer_or_proposal_on_the_ring_road_with_a_period_is_refused_on_reload() {
    let Some(db) = shared_card_db() else { return };
    let offered = bounded_recorded_offer_with_period(db);
    assert_refused_on_reload(&on_the_ring_road(&offered));
    assert_refused_on_reload(&on_the_ring_road(&proposed(offered, 3)));
}

/// CR 704.5a + CR 800.4a + CR 104.2a: on four seats the Zulaport drain drops P1 and P2 together at
/// the repetition both reach 0, and P3 at its own; a take stops at exactly what it declared.
#[test]
fn an_altar_zulaport_period_on_four_seats_drops_each_seat_at_its_crossing() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) =
        altar_board_at("Zulaport Cutthroat", &[20, 6, 6, 8], db);
    drain_to_offer(&mut runner, altar, gravecrawler, &[P1, P2, P3]);
    let offered = runner.state().clone();
    bounded_recorded_offer(&offered, 7);
    assert_eq!(lives(&offered)[1..], [5, 5, 7]);

    let mut runner = GameRunner::from_state(offered.clone());
    let events = take(&mut runner, 6);
    let six = runner.state();
    assert_eq!(eliminated(six), [P1, P2]);
    assert_eq!(six.players[3].life, 1);
    assert_eq!(six.waiting_for, WaitingFor::Priority { player: P0 });
    assert_eq!(eliminations(&events), [P1, P2]);
    let logged = resolve_log_entries(&events, &offered, six)
        .iter()
        .filter(|entry| {
            entry.segments.iter().any(
                |segment| matches!(segment, LogSegment::Text(text) if text == " is eliminated"),
            )
        })
        .count();
    assert_eq!(logged, 2);

    let seven = taken(&offered, 7);
    assert_eq!(seven.waiting_for, WaitingFor::GameOver { winner: Some(P0) });

    assert_declaration_refused(&offered, 8);
}

/// CR 704.3 + CR 800.4a: a take whose signature no longer charges P1 performs the repetitions
/// before P1's departure and stops there, rather than commit a departure it did not predict.
#[test]
fn a_replay_take_stops_before_a_departure_its_signature_did_not_predict() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board_at("Zulaport Cutthroat", &[20, 7, 9], db);
    drain_to_offer(&mut runner, altar, gravecrawler, &[P1, P2]);
    bounded_recorded_offer(runner.state(), 8);
    let p2 = runner.state().players[2].life;
    act(
        &mut runner,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(8),
            template: None,
        },
    );
    let WaitingFor::RespondToShortcut { proposal, .. } = &mut runner.state_mut().waiting_for else {
        panic!("the declaration opens the acceptance window");
    };
    proposal
        .per_cycle
        .as_mut()
        .expect("a signed proposal")
        .delta
        .life
        .remove(&P1);
    while matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        act(
            &mut runner,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        );
    }
    let state = runner.state();
    assert!(
        state.players[2].life < p2,
        "reach: the take performed repetitions"
    );
    assert!(eliminated(state).is_empty());
    assert_eq!(lives(state)[1..], [1, p2 - 5]);
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
}

/// Marvin, Murderous Mimic, Pili-Pala, Grove of the Burnwillows, Tainted Remedy, a Mountain and,
/// when `revolt`, Nature's Revolt, under P0 on `lives.len()` seats at `lives`, with ten Mountains
/// in every library; Marvin and the Mountain.
fn grove_board(lives: &[i32], revolt: bool, db: &CardDatabase) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(lives.len() as u8, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for (seat, life) in lives.iter().enumerate() {
        scenario.with_life(PlayerId(seat as u8), *life);
    }
    let marvin = scenario.add_real_card(P0, "Marvin, Murderous Mimic", Zone::Battlefield, db);
    let pili_pala = scenario.add_real_card(P0, "Pili-Pala", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Grove of the Burnwillows", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Tainted Remedy", Zone::Battlefield, db);
    if revolt {
        scenario.add_real_card(P0, "Nature's Revolt", Zone::Battlefield, db);
    }
    let mountain = scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    for seat in 0..lives.len() {
        for _ in 0..10 {
            scenario.add_real_card(PlayerId(seat as u8), "Mountain", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    // CR 302.6 + CR 107.6: Marvin and Pili-Pala have been under P0's control since P0's turn began.
    let turn = runner.state().turn_number;
    for creature in [marvin, pili_pala] {
        let object = runner
            .state_mut()
            .objects
            .get_mut(&creature)
            .expect("on the battlefield");
        object.summoning_sick = false;
        object.entered_battlefield_turn = Some(turn.saturating_sub(1));
    }
    (runner, marvin, mountain)
}

/// The index of `source`'s activated ability whose text contains `text`.
fn described(state: &GameState, source: ObjectId, text: &str) -> Option<usize> {
    state.objects[&source]
        .abilities
        .iter()
        .position(|a| a.description.as_deref().is_some_and(|d| d.contains(text)))
}

/// Floats the Mountain's {R}, then activates Marvin's Grove ability and its Pili-Pala ability, up
/// to three cycles, until the engine offers the loop; each seat in `drained` loses exactly 1 life
/// per Grove activation.
fn grove_to_offer(
    runner: &mut GameRunner,
    marvin: ObjectId,
    mountain: ObjectId,
    drained: &[PlayerId],
) {
    let score = chooses_color(ManaType::Red);
    let index = ability(runner.state(), mountain, true);
    activate(runner, mountain, index);
    settle(runner, &score);
    let tap =
        described(runner.state(), marvin, "Add {R} or {G}").expect("reach: the Grove ability");
    let untap =
        described(runner.state(), marvin, "{2}, {Q}").expect("reach: the Pili-Pala ability");
    for cycle in 0..3 {
        let before = lives(runner.state());
        activate(runner, marvin, tap);
        settle(runner, &score);
        let after = lives(runner.state());
        for seat in drained {
            let seat = usize::from(seat.0);
            assert_eq!(after[seat], before[seat] - 1, "reach: cycle {cycle}");
        }
        if is_offer(runner.state()) {
            return;
        }
        activate(runner, marvin, untap);
        settle(runner, &score);
        if is_offer(runner.state()) {
            return;
        }
    }
}

/// CR 605.1a + CR 605.3b + CR 614.1a + CR 704.5a: with Nature's Revolt making Grove of the
/// Burnwillows a creature (CR 613.1d), Marvin, Murderous Mimic has its "{T}: Add {R} or {G}. Each
/// opponent gains 1 life." and Pili-Pala's "{2}, {Q}: Add one mana of any color." (CR 613.1f), and
/// Tainted Remedy turns each gain into a loss: a period of mana abilities alone is offered as many
/// repetitions as P1 has life, and a take performs the count it declares.
#[test]
fn a_grove_marvin_period_is_offered_a_bounded_shortcut_and_taken() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, marvin, mountain) = grove_board(&[20, 20], true, db);
    grove_to_offer(&mut runner, marvin, mountain, &[P1]);
    let offered = runner.state().clone();
    let life = offered.players[1].life;
    let l = life as u32;
    let certificate = bounded_recorded_offer(&offered, l);
    assert_eq!(
        certificate.per_cycle.as_ref().map(|p| p.frames_per_period),
        Some(2)
    );
    assert!(offered.stack.is_empty());

    let three = taken(&offered, 3);
    assert_eq!(three.players[1].life, life - 3);
    assert_eq!(three.waiting_for, WaitingFor::Priority { player: P0 });

    let all = taken(&offered, l);
    assert_eq!(all.waiting_for, WaitingFor::GameOver { winner: Some(P0) });
    assert_eq!(eliminated(&all), [P1]);

    assert_declaration_refused(&offered, l + 1);

    let (mut runner, marvin, mountain) = grove_board(&[20, 7, 9], true, db);
    grove_to_offer(&mut runner, marvin, mountain, &[P1, P2]);
    let offered = runner.state().clone();
    bounded_recorded_offer(&offered, 8);
    assert_eq!(eliminated(&taken(&offered, 6)), [P1]);
    assert_eq!(
        taken(&offered, 8).waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    );
}

/// CR 613.1f: without Nature's Revolt the Grove is no creature, so Marvin has only Pili-Pala's
/// ability, no cycle exists, and nothing is offered.
#[test]
fn a_grove_marvin_board_without_natures_revolt_has_no_cycle() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, marvin, mountain) = grove_board(&[20, 20], false, db);
    let index = ability(runner.state(), mountain, true);
    activate(&mut runner, mountain, index);
    settle(&mut runner, &chooses_color(ManaType::Red));
    assert!(
        described(runner.state(), marvin, "{2}, {Q}").is_some(),
        "reach: Marvin reads Pili-Pala"
    );
    assert_eq!(described(runner.state(), marvin, "Add {R} or {G}"), None);
    assert!(!is_offer(runner.state()));
}

/// CR 732.2a: the Grove period asks Marvin for a mana color twice, once for the Grove's "{T}: Add
/// {R} or {G}. Each opponent gains 1 life." and once for Pili-Pala's "{2}, {Q}: Add one mana of
/// any color."; each is published to P0 as its own fixed point, and the response declares both and
/// is taken, Tainted Remedy turning each of the three gains into a loss of 1.
#[test]
fn a_repeated_mana_color_period_is_published_and_taken_at_the_interaction_ingress() {
    const COUNT: u32 = 3;
    let Some(db) = shared_card_db() else { return };
    let (mut runner, marvin, mountain) = grove_board(&[20, 20], true, db);
    grove_to_offer(&mut runner, marvin, mountain, &[P1]);
    let offer = runner.state().clone();
    let WaitingFor::LoopShortcut { period, .. } = &offer.waiting_for else {
        panic!("no offer: {:?}", latest_verdict(&offer));
    };
    let colors: Vec<&PinnedDecision> = period
        .choices()
        .iter()
        .filter(|pin| matches!(pin, PinnedDecision::ManaColor { .. }))
        .collect();
    let [first, second] = colors.as_slice() else {
        panic!("reach: the period chose a mana color twice; {colors:?}");
    };
    assert_eq!(first.slot().source, second.slot().source, "reach");
    assert_eq!(
        (first.slot().index, second.slot().index),
        (0, 1),
        "reach: each choice is its own occurrence on that source"
    );

    let (bound, view) = published_to_p0(&offer);
    let (opportunity, points) =
        shortcut_schema(&view).expect("the offer is published as a shortcut schema");
    assert_eq!(points.len(), colors.len());
    for point in points {
        assert!(
            point.kind == InteractionShortcutPointKind::ManaColor
                && point.read_only
                && point.candidate_ids.len() == 1,
            "each occurrence is a fixed point with its recorded color; got {point:?}"
        );
    }

    let submission = InteractionSubmission {
        interaction_id: opportunity.interaction_id.clone(),
        response: InteractionResponse::Shortcut {
            decision: InteractionShortcutDecision::Fixed { iterations: COUNT },
            pins: Vec::new(),
        },
    };
    let template = match resolve_interaction_response(&bound, P0, &submission) {
        Ok(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(COUNT),
            template: Some(template),
        }) => template,
        other => panic!(
            "the response mints a declaration of {COUNT}; got {:?}",
            other.map_err(|error| error.code)
        ),
    };
    for color in &colors {
        assert!(
            template.decisions.contains(color),
            "the declaration carries each recorded color"
        );
    }

    let mut declared = bound.clone();
    assert!(
        submit_interaction(&mut declared, P0, submission).is_ok(),
        "the response is accepted"
    );
    assert!(
        matches!(declared.waiting_for, WaitingFor::RespondToShortcut { .. }),
        "the declaration opens the response window; got {}",
        declared.waiting_for.variant_name()
    );
    let mut runner = GameRunner::from_state(declared);
    accept(&mut runner);
    let taken = runner.state();
    assert_eq!(taken.waiting_for, WaitingFor::Priority { player: P0 });
    assert_eq!(taken.players[1].life, offer.players[1].life - COUNT as i32);
}

/// P0's precombat main with `name` on P0's battlefield.
fn board_with(name: &str, db: &CardDatabase) -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"; 10]);
    scenario.with_library_top(P1, &["Island"; 10]);
    scenario.add_real_card(P0, name, Zone::Battlefield, db);
    scenario.build().state().clone()
}

/// P1's upkeep with Paradox Haze enchanting P1, its trigger fired; or with Grizzly Bears on P0's
/// battlefield instead.
fn haze_upkeep(haze: bool, db: &CardDatabase) -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"; 10]);
    scenario.with_library_top(P1, &["Island"; 10]);
    if !haze {
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    if haze {
        let aura = place(runner.state_mut(), P0, "Paradox Haze", db);
        assert_eq!(attach_to_player(runner.state_mut(), aura, P1), None);
    }
    runner.advance_to_upkeep();
    runner.state().clone()
}

/// CR 732.2a: a once-each-turn trigger's limit reads the grown fired-trigger ledger, so the
/// history cover refuses it beside Chance-Met Elves and admits it beside a vanilla creature.
#[test]
fn the_history_cover_refuses_a_grown_once_each_turn_ledger_a_live_trigger_reads() {
    let db = shared_card_db().expect("card db");
    let elves = board_with("Chance-Met Elves", db);
    let source = elves
        .battlefield
        .iter()
        .find(|id| elves.objects[id].name == "Chance-Met Elves")
        .expect("the Elves");
    let key = active_trigger_definitions(&elves, &elves.objects[source])
        .next()
        .expect("the Elves' trigger")
        .definition_ref;
    let mut fired = elves.clone();
    assert!(fired.triggers_fired_this_turn.insert(key.clone()));
    let bears = board_with("Grizzly Bears", db);
    let mut bears_fired = bears.clone();
    bears_fired.triggers_fired_this_turn.insert(key);

    assert!(
        !loop_states_equal_modulo_resources(&elves, &fired),
        "reach: the comparand refuses the grown ledger"
    );
    assert!(history_covers_for_tests(&bears, &bears_fired));
    assert!(!history_covers_for_tests(&elves, &fired));
}

/// CR 732.2a: Paradox Haze's first-upkeep limit reads the fire-count ledger its own upkeep firing
/// grew, so the history cover refuses that growth and admits it beside Grizzly Bears.
#[test]
fn the_history_cover_refuses_a_grown_fire_count_a_live_trigger_reads() {
    let db = shared_card_db().expect("card db");
    let fired = haze_upkeep(true, db);
    assert_eq!(
        fired.trigger_fire_counts_this_turn.len(),
        1,
        "reach: Haze fired"
    );
    let key = fired
        .trigger_fire_counts_this_turn
        .keys()
        .next()
        .expect("Haze's count")
        .clone();
    let mut unfired = fired.clone();
    unfired.trigger_fire_counts_this_turn.clear();
    let bears = haze_upkeep(false, db);
    let mut bears_counted = bears.clone();
    bears_counted.trigger_fire_counts_this_turn.insert(key, 1);

    assert!(
        !loop_states_equal_modulo_resources(&unfired, &fired),
        "reach: the comparand refuses the grown count"
    );
    assert!(history_covers_for_tests(&bears, &bears_counted));
    assert!(!history_covers_for_tests(&unfired, &fired));
}

/// P0's precombat main with Hellkite Charger, `mountains` Mountains, Bear Umbra on the Charger when
/// `umbra`, and `others` on P0's battlefield; P1 at 200 life.
fn charger_board(
    mountains: usize,
    umbra: bool,
    others: &[&str],
    mode: LoopDetectionMode,
    db: &CardDatabase,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 200);
    scenario.with_library_top(P0, &["Island"; 25]);
    scenario.with_library_top(P1, &["Island"; 25]);
    let charger = scenario.add_real_card(P0, "Hellkite Charger", Zone::Battlefield, db);
    for _ in 0..mountains {
        scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    }
    for name in others {
        scenario.add_real_card(P0, name, Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    if umbra {
        let aura = place(runner.state_mut(), P0, "Bear Umbra", db);
        attach_to(runner.state_mut(), aura, charger);
    }
    (runner, charger)
}

/// Hellkite Charger's period: attack with the Charger while `attacks` holds, order the paying
/// trigger to resolve last, pay {5}{R}{R}, and pass every priority.
fn charger_step(runner: &mut GameRunner, charger: ObjectId, attacks: bool) {
    let action = match &runner.state().waiting_for {
        WaitingFor::OrderTriggers { triggers, .. } => {
            let mut order: Vec<usize> = (0..triggers.len()).collect();
            order.sort_by_key(|&at| !triggers[at].description.contains("pay"));
            GameAction::OrderTriggers { order }
        }
        WaitingFor::OptionalEffectChoice { .. } => {
            GameAction::DecideOptionalEffect { accept: true }
        }
        WaitingFor::DeclareAttackers { player, .. } if *player == P0 && attacks => {
            GameAction::DeclareAttackers {
                attacks: vec![(charger, AttackTarget::Player(P1))],
                bands: vec![],
            }
        }
        WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        },
        WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
            assignments: vec![],
        },
        WaitingFor::ManaPayment { .. } | WaitingFor::Priority { .. } => GameAction::PassPriority,
        other => panic!("the Charger drive has no answer to {other:?}"),
    };
    act(runner, action);
}

/// Drives Hellkite Charger's period until `until` holds.
fn charger_until(
    runner: &mut GameRunner,
    charger: ObjectId,
    attacks: impl Fn(&GameState) -> bool,
    until: impl Fn(&GameState) -> bool,
) {
    for _ in 0..4000 {
        if until(runner.state()) {
            return;
        }
        let attacking = attacks(runner.state());
        charger_step(runner, charger, attacking);
    }
    panic!("the Charger drive did not reach its stop");
}

fn carried(state: &GameState) -> Vec<engine::game::CarriedView> {
    play_trace_view(state).map_or_else(Vec::new, |view| view.carried)
}

fn combats_begun(state: &GameState) -> u32 {
    state.steps_started_this_turn.count(Phase::BeginCombat)
}

/// CR 500.8: carrying a period costs one append per live carried span per recorded entry, with no
/// board scan, whatever else is on the battlefield; Engine B runs once for an unchanged face set.
#[test]
fn a_carried_combat_span_costs_one_append_per_entry_and_no_board_scan() {
    let db = shared_card_db().expect("card db");
    let mut runs = Vec::new();
    for bears in [0, 3] {
        let others = vec!["Grizzly Bears"; bears];
        let start = play_trace_counters();
        let (mut runner, charger) =
            charger_board(7, true, &others, LoopDetectionMode::Interactive, db);
        charger_until(
            &mut runner,
            charger,
            |_| true,
            |state| {
                combats_begun(state) == 2
                    && matches!(state.waiting_for, WaitingFor::OrderTriggers { .. })
            },
        );
        let live = carried(runner.state());
        assert_eq!(live.len(), 1, "reach: the Charger's span is carried");
        let before = play_trace_counters();
        charger_step(&mut runner, charger, true);
        let entry = play_trace_counters().since(before);
        let drive = play_trace_counters().since(start);
        runs.push((
            entry.carry_appends,
            entry.face_set_scans,
            drive.engine_b_rebuilds,
        ));
        assert!(
            drive.step_end_lookups > 0,
            "reach: step ends looked nodes up"
        );
    }
    assert_eq!(runs, [(1, 0, 1), (1, 0, 1)]);
}

/// CR 500.8: a combat span ends with its turn.
#[test]
fn a_carried_combat_span_does_not_outlive_its_turn() {
    let db = shared_card_db().expect("card db");
    let (mut runner, charger) = charger_board(7, true, &[], LoopDetectionMode::Interactive, db);
    charger_until(
        &mut runner,
        charger,
        |state| combats_begun(state) == 1,
        |state| state.phase == Phase::End && state.active_player == P0,
    );
    assert_eq!(
        carried(runner.state())
            .iter()
            .map(|span| span.reach)
            .collect::<Vec<_>>(),
        [PeriodReach::Combat],
        "reach: the span runs to the turn's end"
    );
    charger_until(
        &mut runner,
        charger,
        |_| false,
        |state| {
            state.active_player == P1 && matches!(state.waiting_for, WaitingFor::Priority { .. })
        },
    );
    assert_eq!(carried(runner.state()), []);
}

/// CR 500.7: an extra-turn span ends when another player's turn begins.
#[test]
fn a_carried_extra_turn_span_does_not_outlive_its_controllers_turn() {
    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"; 10]);
    scenario.with_library_top(P1, &["Island"; 10]);
    let sieve = scenario.add_real_card(P0, "Time Sieve", Zone::Battlefield, db);
    for _ in 0..5 {
        scenario.add_real_card(P0, "Ornithopter", Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let index = ability(runner.state(), sieve, false);
    activate(&mut runner, sieve, index);
    let pass_until = |runner: &mut GameRunner, until: &dyn Fn(&GameState) -> bool| {
        for _ in 0..2000 {
            if until(runner.state()) {
                return;
            }
            let action = match &runner.state().waiting_for {
                WaitingFor::PayCost { choices, count, .. } => GameAction::SelectCards {
                    cards: choices
                        .iter()
                        .copied()
                        .filter(|&id| id != sieve)
                        .take(*count)
                        .collect(),
                },
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                other => panic!("no answer to {other:?}"),
            };
            act(runner, action);
        }
        panic!("the drive did not reach its stop");
    };
    let turn = runner.state().turn_number;
    pass_until(&mut runner, &|state| {
        state.turn_number == turn + 1
            && state.phase == Phase::Upkeep
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
    });
    assert_eq!(
        runner.state().active_player,
        P0,
        "reach: the extra turn is P0's"
    );
    assert_eq!(
        carried(runner.state())
            .iter()
            .map(|span| span.reach)
            .collect::<Vec<_>>(),
        [PeriodReach::ExtraTurn],
        "reach: the Sieve's span is carried into the extra turn"
    );
    // P0's next turn begun with no window since this one, after `opponent` turns of P1's that
    // passed the same way, as a turn passed inside one action leaves them.
    for (opponent, survives) in [(0, true), (1, false)] {
        let mut drive = GameRunner::from_state(runner.state().clone());
        let state = drive.state_mut();
        state.turn_number += 1 + opponent;
        state.players[0].turns_taken += 1;
        state.players[1].turns_taken += opponent;
        act(&mut drive, GameAction::PassPriority);
        assert_eq!(
            carried(drive.state()).len(),
            usize::from(survives),
            "{opponent}"
        );
    }
    pass_until(&mut runner, &|state| {
        state.active_player == P1 && matches!(state.waiting_for, WaitingFor::Priority { .. })
    });
    assert_eq!(carried(runner.state()), []);
}

/// The offered span and the count the offer suggests.
fn offer_of(state: &GameState) -> (NamedSpan, IterationCount) {
    let WaitingFor::LoopShortcut { schema, .. } = &state.waiting_for else {
        panic!("no offer at {:?}", state.waiting_for);
    };
    let span = play_trace_view(state)
        .and_then(|view| view.offered)
        .expect("the offered span");
    (span, schema.iteration_count.clone())
}

fn fixed(count: &IterationCount) -> u32 {
    match count {
        IterationCount::Fixed(n) => *n,
        other => panic!("a fixed count, not {other:?}"),
    }
}

/// The confirmer's verdicts on the carried spans the trace names at `state`.
fn carried_verdicts(state: &GameState) -> Vec<Result<Vec<String>, OfferRefusal>> {
    confirm_for_tests(state)
        .into_iter()
        .filter(|(span, _)| matches!(span.source, SpanSource::Carried(_)))
        .map(|(_, verdict)| verdict)
        .collect()
}

/// `name` placed on every frame's battlefield under P0 as the same object, there since before
/// the first frame's turn began.
fn place_on_all(frames: &mut [GameState], name: &str, db: &CardDatabase) {
    let before = frames[0].turn_number.saturating_sub(1);
    let (earlier, last) = frames.split_at_mut(frames.len() - 1);
    let last = &mut last[0];
    let id = place(last, P0, name, db);
    for frame in earlier.iter_mut() {
        assert!(
            !frame.objects.contains_key(&id),
            "reach: {name}'s id is free in every earlier frame"
        );
        frame.objects.insert(id, last.objects[&id].clone());
        add_to_zone(frame, id, Zone::Battlefield, P0);
    }
    for frame in frames.iter_mut() {
        let object = frame.objects.get_mut(&id).expect("placed");
        object.entered_battlefield_turn = Some(before);
        object.summoning_sick = false;
    }
}

fn charger_trigger_top(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
        && state.phase == Phase::DeclareAttackers
        && state.stack.len() == 1
}

/// CR 500.8 + CR 732.2a: Hellkite Charger ("Whenever this creature attacks, you may pay
/// {5}{R}{R}. If you do, untap all attacking creatures and after this phase, there is an
/// additional combat phase.") wearing Bear Umbra, whose granted attack trigger untaps the Mountains
/// that pay it, is offered its carried combat span at the next combat's trigger, counted to P1's
/// lethal crossing, and a take ends the game.
#[test]
fn a_hellkite_charger_period_is_offered_at_its_next_combat_and_taken() {
    let db = shared_card_db().expect("card db");
    for mode in [LoopDetectionMode::Interactive, LoopDetectionMode::On] {
        let (mut runner, charger) = charger_board(7, true, &[], mode, db);
        charger_until(
            &mut runner,
            charger,
            |_| true,
            |state| is_offer(state) || combats_begun(state) > 6,
        );
        let state = runner.state();
        let (span, count) = offer_of(state);
        assert_eq!(
            (span.source, span.reach, span.cause),
            (
                SpanSource::Carried(0),
                PeriodReach::Combat,
                NamingCause::TriggerTop
            ),
            "{mode:?}"
        );
        assert_eq!(combats_begun(state), 2, "{mode:?}");
        let power = state.objects[&charger].power.expect("the Charger's power");
        let life = state.players[1].life;
        let n = fixed(&count);
        assert_eq!(n, ((life + power - 1) / power) as u32, "{mode:?}");

        let events = take(&mut runner, n);
        assert_eq!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P0) },
            "{mode:?}"
        );
        assert!(
            events.contains(&GameEvent::GameOver { winner: Some(P0) }),
            "{mode:?}"
        );
    }
}

/// CR 732.2a: without Bear Umbra the Charger's carried span spends Mountains nothing untaps, and
/// beside Moraug, Fury of Akoum ("Each creature you control gets +1/+0 for each time it has
/// attacked this turn.") each attack grows the trigger's source; neither is offered.
#[test]
fn hellkite_charger_boards_whose_period_does_not_recur_are_not_offered() {
    let db = shared_card_db().expect("card db");
    for (mountains, umbra, others) in [
        (14, false, &[][..]),
        (7, true, &["Moraug, Fury of Akoum"][..]),
    ] {
        let (mut runner, charger) =
            charger_board(mountains, umbra, others, LoopDetectionMode::Interactive, db);
        let mut verdicts = Vec::new();
        for combat in 2..=3 {
            charger_until(
                &mut runner,
                charger,
                |_| true,
                |state| {
                    is_offer(state)
                        || (combats_begun(state) == combat && charger_trigger_top(state))
                },
            );
            assert!(!is_offer(runner.state()), "{others:?}");
            verdicts.extend(carried_verdicts(runner.state()));
        }
        assert!(!verdicts.is_empty(), "reach: {others:?} carries the span");
        assert!(
            verdicts.iter().all(Result::is_err),
            "{others:?}: {verdicts:?}"
        );
        if !umbra {
            assert!(
                verdicts
                    .iter()
                    .all(|verdict| *verdict == Err(OfferRefusal::NoRecurrence)),
                "{verdicts:?}"
            );
        }
    }
}

/// Row 1's trigger-top frames at its second and third combats, with `other` on both battlefields.
fn charger_frames(other: &str, db: &CardDatabase) -> [GameState; 2] {
    let (mut runner, charger) = charger_board(7, true, &[], LoopDetectionMode::Off, db);
    let mut frame = |combat: u32| {
        charger_until(
            &mut runner,
            charger,
            |_| true,
            |state| combats_begun(state) == combat && charger_trigger_top(state),
        );
        runner.state().clone()
    };
    let mut frames = [frame(2), frame(3)];
    place_on_all(&mut frames, other, db);
    frames
}

/// CR 732.2a: the Charger's second combat grows the attack history Moraug's static reads, so the
/// history cover refuses that pair beside Moraug and admits it beside Grizzly Bears.
#[test]
fn the_history_cover_refuses_a_grown_attack_history_a_live_static_reads() {
    let db = shared_card_db().expect("card db");
    let [prior, current] = charger_frames("Grizzly Bears", db);
    assert!(
        !loop_states_equal_modulo_resources(&prior, &current),
        "reach: the comparand refuses the grown history"
    );
    assert!(history_covers_for_tests(&prior, &current));
    let [prior, current] = charger_frames("Moraug, Fury of Akoum", db);
    assert!(!history_covers_for_tests(&prior, &current));
}

const MYR: [&str; 5] = [
    "Gold Myr",
    "Silver Myr",
    "Leaden Myr",
    "Iron Myr",
    "Copper Myr",
];

/// P0's precombat main with Najeela, the Blade-Blossom, Sinister Monolith, the five Myr that tap
/// for Najeela's five colors and, when `urtet`, Urtet, Remnant of Memnarch; P1 at 20 life.
fn najeela_board(urtet: bool, mode: LoopDetectionMode, db: &CardDatabase) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 20);
    scenario.with_library_top(P0, &["Island"; 10]);
    scenario.with_library_top(P1, &["Island"; 10]);
    scenario.add_real_card(P0, "Najeela, the Blade-Blossom", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Sinister Monolith", Zone::Battlefield, db);
    if urtet {
        scenario.add_real_card(P0, "Urtet, Remnant of Memnarch", Zone::Battlefield, db);
    }
    let myr: Vec<ObjectId> = MYR
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Battlefield, db))
        .collect();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    // CR 302.6: the Myr have been under P0's control since P0's turn began.
    let turn = runner.state().turn_number;
    for id in myr {
        let object = runner.state_mut().objects.get_mut(&id).expect("a Myr");
        object.summoning_sick = false;
        object.entered_battlefield_turn = Some(turn.saturating_sub(1));
    }
    runner
}

/// Najeela's period at each beginning of combat: tap the five Myr, then activate Najeela's
/// "{W}{U}{B}{R}{G}: Untap all attacking creatures. ... After this phase, there is an additional
/// combat phase."; no attack, and every other priority passed.
fn najeela_step(runner: &mut GameRunner) {
    let state = runner.state();
    let action = match &state.waiting_for {
        WaitingFor::Priority { player }
            if *player == P0 && state.phase == Phase::BeginCombat && state.stack.is_empty() =>
        {
            let untapped = state.battlefield.iter().copied().find(|id| {
                let object = &state.objects[id];
                MYR.contains(&object.name.as_str()) && !object.tapped
            });
            let najeela =
                in_zone(state, &state.battlefield, "Najeela, the Blade-Blossom").expect("Najeela");
            match untapped {
                Some(myr) => GameAction::ActivateAbility {
                    source_id: myr,
                    ability_index: ability(state, myr, true),
                },
                None if state.players[0].mana_pool.total() >= 5 => GameAction::ActivateAbility {
                    source_id: najeela,
                    ability_index: ability(state, najeela, false),
                },
                None => GameAction::PassPriority,
            }
        }
        WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        },
        WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
            assignments: vec![],
        },
        WaitingFor::ManaPayment { .. } | WaitingFor::Priority { .. } => GameAction::PassPriority,
        WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
            order: (0..triggers.len()).collect(),
        },
        other => panic!("the Najeela drive has no answer to {other:?}"),
    };
    act(runner, action);
}

fn najeela_until(runner: &mut GameRunner, until: impl Fn(&GameState) -> bool) {
    for _ in 0..4000 {
        if until(runner.state()) {
            return;
        }
        najeela_step(runner);
    }
    panic!("the Najeela drive did not reach its stop");
}

/// CR 500.8 + CR 732.2a: Najeela's activation, paid by five Myr that Urtet ("At the beginning of
/// combat on your turn, untap each Myr you control.") untaps, repeats its combat while Sinister
/// Monolith drains P1 at each; the next beginning of combat offers the carried span and a take
/// ends the game.
#[test]
fn a_najeela_urtet_period_is_offered_at_its_next_combat_and_taken() {
    let db = shared_card_db().expect("card db");
    for mode in [LoopDetectionMode::Interactive, LoopDetectionMode::On] {
        let mut runner = najeela_board(true, mode, db);
        najeela_until(&mut runner, |state| {
            is_offer(state) || combats_begun(state) > 6
        });
        let state = runner.state();
        let (span, count) = offer_of(state);
        assert_eq!(
            (span.source, span.reach, span.cause),
            (
                SpanSource::Carried(0),
                PeriodReach::Combat,
                NamingCause::LegalNow
            ),
            "{mode:?}"
        );
        assert_eq!(
            (state.phase, combats_begun(state)),
            (Phase::BeginCombat, 2),
            "{mode:?}"
        );
        let n = fixed(&count);
        assert_eq!(n, state.players[1].life as u32, "{mode:?}");
        take(&mut runner, n);
        assert_eq!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P0) },
            "{mode:?}"
        );
    }
}

/// CR 302.6: without Urtet the Myr stay tapped after the first activation, so Najeela cannot be
/// activated at the second combat and nothing is offered.
#[test]
fn a_najeela_board_without_urtet_is_not_offered() {
    let db = shared_card_db().expect("card db");
    let mut runner = najeela_board(false, LoopDetectionMode::Interactive, db);
    najeela_until(&mut runner, |state| {
        is_offer(state) || state.phase == Phase::End
    });
    assert_eq!(
        combats_begun(runner.state()),
        2,
        "reach: Najeela was activated once"
    );
    assert!(!is_offer(runner.state()));
}

/// P0's precombat main with Archaeomancer ("When this creature enters, return target instant or
/// sorcery card from your graveyard to your hand."), Mnemonic Wall, eight Islands and `others` on
/// P0's battlefield, Time Warp and Ghostly Flicker in hand, and `library` Islands in P0's library.
fn warp_board(
    library: usize,
    others: &[&str],
    mode: LoopDetectionMode,
    db: &CardDatabase,
) -> GameRunner {
    warp_board_in(GameScenario::new(), library, others, mode, db)
}

/// [`warp_board`] in `scenario`.
fn warp_board_in(
    mut scenario: GameScenario,
    library: usize,
    others: &[&str],
    mode: LoopDetectionMode,
    db: &CardDatabase,
) -> GameRunner {
    scenario.at_phase(Phase::PreCombatMain);
    // Real Islands: a drawn card is a face Engine B reads.
    for _ in 0..library {
        scenario.add_real_card(P0, "Island", Zone::Library, db);
    }
    scenario.with_library_top(P1, &["Island"; 25]);
    for name in ["Archaeomancer", "Mnemonic Wall"]
        .into_iter()
        .chain(["Island"; 8])
        .chain(others.iter().copied())
    {
        scenario.add_real_card(P0, name, Zone::Battlefield, db);
    }
    scenario.add_real_card(P0, "Time Warp", Zone::Hand, db);
    scenario.add_real_card(P0, "Ghostly Flicker", Zone::Hand, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    runner
}

fn in_zone(state: &GameState, ids: &im::Vector<ObjectId>, name: &str) -> Option<ObjectId> {
    ids.iter()
        .copied()
        .find(|id| state.objects[id].name == name)
}

fn untapped_islands(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            let object = &state.objects[id];
            object.controller == P0 && object.name == "Island" && !object.tapped
        })
        .count()
}

/// The turn-cycle period at P0's precombat main: Time Warp on P0, Ghostly Flicker on
/// Archaeomancer and Mnemonic Wall, Archaeomancer returning Time Warp and the Wall returning
/// Flicker; a discard keeps the two spells, and every other priority passes.
fn warp_step(runner: &mut GameRunner) {
    let state = runner.state();
    let hand = &state.players[0].hand;
    let action = match &state.waiting_for {
        WaitingFor::Priority { player }
            if *player == P0
                && state.active_player == P0
                && state.phase == Phase::PreCombatMain
                && state.stack.is_empty() =>
        {
            let warp = in_zone(state, hand, "Time Warp").filter(|_| untapped_islands(state) >= 8);
            let flicker = in_zone(state, hand, "Ghostly Flicker").filter(|_| {
                untapped_islands(state) >= 3
                    && in_zone(state, &state.players[0].graveyard, "Time Warp").is_some()
            });
            match warp.or(flicker) {
                Some(spell) => GameAction::CastSpell {
                    object_id: spell,
                    card_id: state.objects[&spell].card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::default(),
                },
                None => GameAction::PassPriority,
            }
        }
        WaitingFor::TargetSelection { target_slots, .. } => {
            let mut targets: Vec<TargetRef> = Vec::new();
            for slot in target_slots {
                let pick = slot
                    .legal_targets
                    .iter()
                    .find(|target| **target == TargetRef::Player(P0))
                    .or_else(|| {
                        slot.legal_targets.iter().find(|target| {
                            matches!(target, TargetRef::Object(id)
                                if ["Archaeomancer", "Mnemonic Wall"].contains(&state.objects[id].name.as_str()))
                                && !targets.contains(target)
                        })
                    })
                    .cloned();
                targets.extend(pick);
            }
            GameAction::SelectTargets { targets }
        }
        WaitingFor::TriggerTargetSelection {
            target_slots,
            source_id,
            ..
        } => {
            let wanted = match source_id.map(|id| state.objects[&id].name.as_str()) {
                Some("Archaeomancer") => "Time Warp",
                _ => "Ghostly Flicker",
            };
            GameAction::SelectTargets {
                targets: target_slots
                    .iter()
                    .filter_map(|slot| {
                        slot.legal_targets
                            .iter()
                            .find(|target| matches!(target, TargetRef::Object(id) if state.objects[id].name == wanted))
                            .cloned()
                    })
                    .collect(),
            }
        }
        WaitingFor::OptionalEffectChoice { .. } => {
            GameAction::DecideOptionalEffect { accept: true }
        }
        WaitingFor::DiscardToHandSize { cards, count, .. } => GameAction::SelectCards {
            cards: cards
                .iter()
                .copied()
                .filter(|id| state.objects[id].name == "Island")
                .take(*count)
                .collect(),
        },
        WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        },
        WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
            assignments: vec![],
        },
        WaitingFor::ManaPayment { .. } | WaitingFor::Priority { .. } => GameAction::PassPriority,
        WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
            order: (0..triggers.len()).collect(),
        },
        other => panic!("the Time Warp drive has no answer to {other:?}"),
    };
    act(runner, action);
}

fn warp_until(runner: &mut GameRunner, until: impl Fn(&GameState) -> bool) {
    for _ in 0..8000 {
        if until(runner.state()) {
            return;
        }
        warp_step(runner);
    }
    panic!("the Time Warp drive did not reach its stop");
}

/// P0's precombat-main priority with an empty stack, before any play this turn.
fn turn_window(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
        && state.active_player == P0
        && state.phase == Phase::PreCombatMain
        && state.stack.is_empty()
        && state.players[0].hand.len() >= 2
        && in_zone(state, &state.players[0].hand, "Time Warp").is_some()
}

/// CR 402.2 + CR 514.1: the count before a repetition's cleanup would discard: each repetition
/// draws one card, and the window's turn reaches its cleanup holding the window's hand.
fn hand_term(state: &GameState) -> u32 {
    (8 - state.players[0].hand.len()) as u32
}

/// CR 504.1 + CR 704.5b: one draw a repetition, and a draw from an empty library loses.
fn library_term(state: &GameState) -> u32 {
    state.players[0].library.len() as u32 + 1
}

/// CR 500.7 + CR 732.2a: Time Warp on its caster, returned each turn by Archaeomancer through
/// Ghostly Flicker, is offered at the next turn's window for the carried extra-turn span, counted
/// to the cleanup that would first discard; a take performs that many extra turns and discards
/// nothing.
#[test]
fn an_archaeomancer_time_warp_period_is_offered_its_hand_bound_and_taken() {
    let db = shared_card_db().expect("card db");
    for mode in [LoopDetectionMode::Interactive, LoopDetectionMode::On] {
        let mut runner = warp_board(25, &[], mode, db);
        let turn = runner.state().turn_number;
        warp_until(&mut runner, |state| {
            is_offer(state) || state.turn_number > turn + 3
        });
        let state = runner.state().clone();
        let (span, count) = offer_of(&state);
        assert_eq!(
            (span.source, span.reach, span.cause),
            (
                SpanSource::Carried(0),
                PeriodReach::ExtraTurn,
                NamingCause::LegalNow
            ),
            "{mode:?}"
        );
        assert_eq!(state.turn_number, turn + 1, "{mode:?}");
        let n = fixed(&count);
        assert!(
            hand_term(&state) < library_term(&state),
            "reach: the hand binds"
        );
        assert_eq!(n, hand_term(&state), "{mode:?}");

        take(&mut runner, n);
        let after = runner.state();
        assert_eq!(
            (after.turn_number, after.active_player, after.phase),
            (state.turn_number + n, P0, Phase::PreCombatMain),
            "{mode:?}"
        );
        assert_eq!(
            after.players[0].hand.len(),
            state.players[0].hand.len() + n as usize
        );
    }
}

/// [`warp_step`], with Carpet of Flowers' trigger aimed at P1 and its mana taken as green.
fn carpet_warp_step(runner: &mut GameRunner) {
    let state = runner.state();
    let action = match &state.waiting_for {
        WaitingFor::TriggerTargetSelection {
            source_id: Some(source),
            ..
        } if state.objects[source].name == "Carpet of Flowers" => GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        },
        WaitingFor::ChooseManaColor { .. } => GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        },
        _ => return warp_step(runner),
    };
    act(runner, action);
}

fn carpet_warp_until(runner: &mut GameRunner, until: impl Fn(&GameState) -> bool) {
    for _ in 0..8000 {
        if until(runner.state()) {
            return;
        }
        carpet_warp_step(runner);
    }
    panic!("the Carpet of Flowers drive did not reach its stop");
}

type TurnPosition = (
    (u32, Phase, PlayerId),
    Vec<String>,
    Vec<String>,
    usize,
    Vec<String>,
    usize,
    Vec<i32>,
    Vec<(String, PlayerId, bool)>,
);

/// The turn, phase and active player, the stack's sources, P0's hand, library size, graveyard and
/// pool size, every life total, and each permanent with its controller and tapped state.
fn turn_position(state: &GameState) -> TurnPosition {
    let named = |ids: &mut dyn Iterator<Item = ObjectId>| {
        let mut names: Vec<String> = ids.map(|id| state.objects[&id].name.clone()).collect();
        names.sort();
        names
    };
    let mut battlefield: Vec<(String, PlayerId, bool)> = state
        .battlefield
        .iter()
        .map(|id| {
            let object = &state.objects[id];
            (object.name.clone(), object.controller, object.tapped)
        })
        .collect();
    battlefield.sort();
    let p0 = &state.players[0];
    (
        (state.turn_number, state.phase, state.active_player),
        named(&mut state.stack.iter().map(|entry| entry.source_id)),
        named(&mut p0.hand.iter().copied()),
        p0.library.len(),
        named(&mut p0.graveyard.iter().copied()),
        p0.mana_pool.total(),
        lives(state),
        battlefield,
    )
}

/// CR 500.7 + CR 732.2a: Carpet of Flowers ("At the beginning of each of your main phases, if you
/// haven't added mana with this ability this turn, you may add X mana of any one color, where X is
/// the number of Islands target opponent controls.") reads "this turn", so every extra turn asks it
/// at its first main phase alike: the Time Warp period beside it is offered, and a take is that
/// many turns played by hand. The board without Carpet of Flowers is offered and taken the same.
#[test]
fn an_archaeomancer_time_warp_period_with_carpet_of_flowers_is_offered_and_taken() {
    let db = shared_card_db().expect("card db");
    for others in [&["Carpet of Flowers"][..], &[]] {
        let board = |mode| {
            let mut scenario = GameScenario::new();
            for _ in 0..2 {
                scenario.add_real_card(P1, "Island", Zone::Battlefield, db);
            }
            warp_board_in(scenario, 25, others, mode, db)
        };
        let mut runner = board(LoopDetectionMode::Interactive);
        let turn = runner.state().turn_number;
        let carpet = !others.is_empty();
        if carpet {
            carpet_warp_until(&mut runner, |state| {
                turn_window(state) && state.turn_number == turn + 1
            });
            assert!(
                !runner
                    .state()
                    .triggered_abilities_added_mana_this_turn
                    .is_empty(),
                "reach: Carpet of Flowers added mana in the turn before the offer"
            );
        }
        carpet_warp_until(&mut runner, |state| {
            is_offer(state) || state.turn_number > turn + 3
        });
        let offered = runner.state().clone();
        let (span, count) = offer_of(&offered);
        assert_eq!(span.reach, PeriodReach::ExtraTurn, "{others:?}");
        if carpet {
            assert_eq!(offered.turn_number, turn + 2, "reach: the turn after");
        }
        let n = fixed(&count);

        let mut by_hand = board(LoopDetectionMode::Off);
        carpet_warp_until(&mut by_hand, |state| {
            state.turn_number == offered.turn_number + n
                && state.phase == Phase::PreCombatMain
                && state.stack.len() == offered.stack.len()
                && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
        });

        take(&mut runner, n);
        assert_eq!(
            turn_position(runner.state()),
            turn_position(by_hand.state()),
            "{others:?}"
        );
    }
}

/// CR 704.5b + CR 732.2a: with four cards in its library the caster's library binds the count, and
/// a take decks the caster at its last draw step.
#[test]
fn an_archaeomancer_time_warp_take_decks_its_caster_when_the_library_binds() {
    let db = shared_card_db().expect("card db");
    let mut runner = warp_board(4, &[], LoopDetectionMode::Interactive, db);
    let turn = runner.state().turn_number;
    warp_until(&mut runner, |state| {
        is_offer(state) || state.turn_number > turn + 3
    });
    let state = runner.state().clone();
    let n = fixed(&offer_of(&state).1);
    assert!(
        library_term(&state) < hand_term(&state),
        "reach: the library binds"
    );
    assert_eq!(n, library_term(&state));
    let events = take(&mut runner, n);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P1) }
    );
    assert!(events.contains(&GameEvent::GameOver { winner: Some(P1) }));
}

/// CR 704.5b + CR 800.4a: on three seats the same take decks its caster at its last draw step and
/// ends there, and the game goes on for the other two.
#[test]
fn an_archaeomancer_time_warp_take_on_three_seats_decks_its_caster_and_the_game_goes_on() {
    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.with_library_top(P2, &["Island"; 25]);
    let mut runner = warp_board_in(scenario, 4, &[], LoopDetectionMode::Interactive, db);
    let turn = runner.state().turn_number;
    warp_until(&mut runner, |state| {
        is_offer(state) || state.turn_number > turn + 3
    });
    let state = runner.state().clone();
    let n = fixed(&offer_of(&state).1);
    assert!(
        library_term(&state) < hand_term(&state),
        "reach: the library binds"
    );
    assert_eq!(n, library_term(&state));
    let events = take(&mut runner, n);
    let after = runner.state();
    assert_eq!(eliminated(after), [P0]);
    assert_eq!(eliminations(&events), [P0]);
    let departure = events
        .iter()
        .position(|event| matches!(event, GameEvent::PlayerEliminated { .. }))
        .expect("reach: the take emits the elimination");
    let departure_turn = events[..departure]
        .iter()
        .rev()
        .find_map(|event| match event {
            GameEvent::TurnStarted {
                player_id,
                turn_number,
            } => Some((*player_id, *turn_number)),
            _ => None,
        });
    assert_eq!(departure_turn.map(|(seat, _)| seat), Some(P0));
    assert_eq!(
        (Some(after.turn_number), after.phase),
        (departure_turn.map(|(_, turn)| turn), Phase::Draw)
    );
    assert!(!events[departure..]
        .iter()
        .any(|event| matches!(event, GameEvent::PriorityPassed { .. })));
    assert_eq!(
        after.players[0].turns_taken,
        state.players[0].turns_taken + n
    );
    assert_eq!(after.waiting_for, WaitingFor::Priority { player: P1 });
    warp_until(&mut runner, |state| state.active_player == P2);
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::GameOver { .. }
    ));
}

/// Drives the Time Warp board declining every offer until `turns` turns pass or the game ends; the
/// window state and offered count at each of P0's turn windows.
fn warp_windows(library: usize, turns: u32, db: &CardDatabase) -> Vec<(GameState, Option<u32>)> {
    let mut runner = warp_board(library, &[], LoopDetectionMode::Interactive, db);
    let start = runner.state().turn_number;
    let mut windows: Vec<(GameState, Option<u32>)> = Vec::new();
    for _ in 0..20_000 {
        let state = runner.state();
        if state.turn_number > start + turns
            || matches!(state.waiting_for, WaitingFor::GameOver { .. })
        {
            return windows;
        }
        let seen = windows
            .last()
            .is_some_and(|(window, _)| window.turn_number == state.turn_number);
        if is_offer(state) {
            let count = fixed(&offer_of(state).1);
            let mut window = state.clone();
            window.waiting_for = WaitingFor::Priority { player: P0 };
            windows.push((window, Some(count)));
            act(&mut runner, GameAction::DeclineShortcut);
            continue;
        }
        if turn_window(state) && !seen {
            windows.push((state.clone(), None));
        }
        warp_step(&mut runner);
    }
    panic!("the Time Warp drive did not end");
}

/// CR 514.1 + CR 704.5b + CR 732.2a: at every window of both libraries' drives an offer suggests
/// the lesser of the hand and library terms, and stands exactly where the two periods the
/// confirmer replays both end before a discard and a draw from an empty library.
#[test]
fn every_archaeomancer_window_is_offered_the_lesser_of_its_hand_and_library_terms() {
    let db = shared_card_db().expect("card db");
    for library in [25, 4] {
        let windows = warp_windows(library, 8, db);
        let offered = windows.iter().filter(|(_, count)| count.is_some()).count();
        assert!(offered >= 2, "reach: {library} offers more than once");
        for (state, count) in &windows[1..] {
            let replayable = hand_term(state).min(state.players[0].library.len() as u32);
            assert_eq!(
                *count,
                (replayable >= 2).then(|| hand_term(state).min(library_term(state))),
                "{library} cards, turn {}",
                state.turn_number
            );
        }
    }
}

/// A board whose turns come round naturally: Grizzly Bears and two Islands, or Time Sieve with five
/// Islands and Thopter Assembly in hand, too few Islands to cast it.
fn natural_board(sieve: bool, db: &CardDatabase) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..25 {
        scenario.add_real_card(P0, "Island", Zone::Library, db);
    }
    scenario.with_library_top(P1, &["Island"; 25]);
    let (permanents, islands) = if sieve {
        (["Time Sieve"].as_slice(), 5)
    } else {
        (["Grizzly Bears"].as_slice(), 2)
    };
    for name in permanents
        .iter()
        .copied()
        .chain(std::iter::repeat_n("Island", islands))
    {
        scenario.add_real_card(P0, name, Zone::Battlefield, db);
    }
    if sieve {
        scenario.add_real_card(P0, "Thopter Assembly", Zone::Hand, db);
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner
}

/// CR 500.7 + CR 732.2a: a board whose turns come round naturally is never offered, and a triple
/// of P0's precombat-main windows is refused by the cover, since the other player's turn and draw
/// came between its frames.
#[test]
fn natural_turn_boards_are_refused_at_every_window() {
    let db = shared_card_db().expect("card db");
    for sieve in [false, true] {
        let mut runner = natural_board(sieve, db);
        let start = runner.state().turn_number;
        let mut windows: Vec<GameState> = Vec::new();
        while runner.state().turn_number <= start + 6 {
            let state = runner.state();
            assert!(
                !is_offer(state),
                "{sieve}: offered at turn {}",
                state.turn_number
            );
            if state.active_player == P0
                && state.phase == Phase::PreCombatMain
                && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
                && windows
                    .last()
                    .is_none_or(|seen| seen.turn_number != state.turn_number)
            {
                windows.push(state.clone());
            }
            let action = match &runner.state().waiting_for {
                WaitingFor::DiscardToHandSize { cards, count, .. } => GameAction::SelectCards {
                    cards: cards.iter().copied().take(*count).collect(),
                },
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                _ => GameAction::PassPriority,
            };
            act(&mut runner, action);
        }
        assert!(windows.len() >= 3, "reach: {sieve} takes its turns");
        for frames in windows.windows(3) {
            let verdict = certify_object_growth_frames_for_tests(
                [&frames[0], &frames[1], &frames[2]],
                &[],
                P0,
            );
            assert!(!verdict.certifies(), "{sieve}: {verdict:?}");
        }
    }
}

/// The Time Warp board's turn windows after its first, with `others` on every battlefield.
fn warp_frames(others: &[&str], db: &CardDatabase) -> [GameState; 3] {
    let mut runner = warp_board(25, &[], LoopDetectionMode::Off, db);
    let turn = runner.state().turn_number;
    let mut frame = |turn: u32| {
        warp_until(&mut runner, |state| {
            state.turn_number == turn && turn_window(state)
        });
        runner.state().clone()
    };
    let mut frames = [frame(turn + 1), frame(turn + 2), frame(turn + 3)];
    for name in others {
        place_on_all(&mut frames, name, db);
    }
    frames
}

/// The object-growth certification of the Time Warp board's three frames, its two spells cast.
fn warp_verdict(frames: &[GameState; 3]) -> ObjectGrowthVerdict {
    let casts = ["Time Warp", "Ghostly Flicker"].map(|name| {
        frames[0]
            .objects
            .iter()
            .find(|(_, object)| object.name == name)
            .map(|(id, _)| *id)
            .expect(name)
    });
    certify_object_growth_frames_for_tests([&frames[0], &frames[1], &frames[2]], &casts, P0)
}

/// CR 500.7 + CR 732.2a: the turn the Time Warp period grows is read by Deathleaper, Terror
/// Weapon's "Creatures you control that entered this turn have double strike.", so the turn-cycle
/// cover refuses those frames beside it and admits them without.
#[test]
fn the_turn_cycle_cover_refuses_a_grown_turn_a_live_static_reads() {
    let db = shared_card_db().expect("card db");
    let frames = warp_frames(&[], db);
    assert!(
        !loop_states_equal_modulo_resources(&frames[0], &frames[1]),
        "reach: the comparand refuses the grown turn"
    );
    assert_eq!(
        warp_verdict(&frames),
        ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::TurnCycle))
    );
    let verdict = warp_verdict(&warp_frames(&["Deathleaper, Terror Weapon"], db));
    assert!(!verdict.certifies(), "{verdict:?}");
}

/// CR 400.7 + CR 732.2a: an object that entered during the first frame's turn and not since stands
/// in a different relation to the next frame's turn, so the turn-cycle cover refuses the frames.
#[test]
fn the_turn_cycle_cover_refuses_a_turn_stamp_whose_relation_moved() {
    let db = shared_card_db().expect("card db");
    let mut frames = warp_frames(&[], db);
    let turn = frames[0].turn_number;
    let archaeomancer = frames[0]
        .objects
        .iter()
        .find(|(_, object)| object.name == "Archaeomancer")
        .map(|(id, _)| *id)
        .expect("Archaeomancer");
    frames[0]
        .objects
        .get_mut(&archaeomancer)
        .expect("Archaeomancer")
        .entered_battlefield_turn = Some(turn);
    let verdict = warp_verdict(&frames);
    assert!(!verdict.certifies(), "{verdict:?}");
}

/// CR 702.143a + CR 732.2a: Cosmic Intervention foretold during the first frame's turn becomes
/// castable by the next frame's, so the turn-cycle cover refuses the frames; foretold a turn
/// earlier, it is castable in every frame and the frames are admitted.
#[test]
fn the_turn_cycle_cover_refuses_a_foretold_card_whose_castability_moved() {
    let db = shared_card_db().expect("card db");
    let verdict = |turns_before_first_frame: u32| {
        let mut frames = warp_frames(&["Cosmic Intervention"], db);
        let turn_foretold = frames[0].turn_number - turns_before_first_frame;
        let card = frames[0]
            .objects
            .iter()
            .find(|(_, object)| object.name == "Cosmic Intervention")
            .map(|(id, _)| *id)
            .expect("Cosmic Intervention");
        let cost = effective_foretell_cost(&frames[0], card).expect("foretell cost");
        for frame in &mut frames {
            remove_from_zone(frame, card, Zone::Battlefield, P0);
            add_to_zone(frame, card, Zone::Exile, P0);
            let object = frame.objects.get_mut(&card).expect("Cosmic Intervention");
            object.zone = Zone::Exile;
            object.entered_battlefield_turn = None;
            object.foretold = true;
            object.face_down = true;
            object.casting_permissions = vec![CastingPermission::Foretold {
                cost: cost.clone(),
                turn_foretold,
            }];
        }
        warp_verdict(&frames)
    };
    assert_eq!(
        verdict(1),
        ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::TurnCycle)),
        "reach: an unmoved castability is admitted"
    );
    let moved = verdict(0);
    assert!(!moved.certifies(), "{moved:?}");
}

/// CR 732.2a: Walking Ballista pinging P1 by removing its own +1/+1 counters spends a resource the
/// period never restores, so the confirmer refuses it on its loss axis and nothing is offered.
#[test]
fn a_walking_ballista_spending_its_counters_is_refused_on_its_loss_axis() {
    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ballista = scenario.add_real_card(P0, "Walking Ballista", Zone::Battlefield, db);
    scenario.with_counter(ballista, CounterType::Plus1Plus1, 8);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let ping = runner.state().objects[&ballista]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::DealDamage { .. }))
        .expect("the ping");
    for _ in 0..2 {
        if is_offer(runner.state()) {
            break;
        }
        act(
            &mut runner,
            GameAction::ActivateAbility {
                source_id: ballista,
                ability_index: ping,
            },
        );
        act(
            &mut runner,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            },
        );
        while !runner.state().stack.is_empty() && !is_offer(runner.state()) {
            act(&mut runner, GameAction::PassPriority);
        }
    }
    let state = runner.state();
    assert!(
        play_trace_view(state).is_some_and(|view| !view.named.is_empty() || view.offered.is_some()),
        "reach: a span is named"
    );
    assert!(!is_offer(state));
    assert_eq!(state.players[1].life, 18);
    assert_eq!(latest_verdict(state), Err(OfferRefusal::LossAxis));
}
