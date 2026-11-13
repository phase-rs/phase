//! CR 732.2a: the confirmer's replay of a trace candidate, and the cover it certifies on.

use engine::analysis::decision_template::{IterationCount, PinnedDecision, ReplayMode};
use engine::analysis::loop_check::{LoopCertificate, ShortcutResponse, WinKind};
use engine::analysis::loop_states_equal_modulo_resources;
use engine::analysis::resource::{
    history_covers_for_tests, FodderCoverRefusal, ObjectGrowthVerdict, PeriodSignature,
    RecurrenceCover, ResourceAxis,
};
use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::effects::attach::{attach_to, attach_to_player};
use engine::game::engine::{certify_object_growth_frames_for_tests, EngineError};
use engine::game::functioning_abilities::active_trigger_definitions;
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, resolve_interaction_response,
    submit_interaction,
};
use engine::game::keywords::effective_foretell_cost;
use engine::game::log::resolve_log_entries;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::perf_counters::play_trace_counters;
use engine::game::period_confirm::{
    confirm_for_tests, performed_for_tests, OfferRefusal, PeriodPerformance,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zones::{add_to_zone, remove_from_zone};
use engine::game::{play_trace_view, EntryKind, NamedSpan, NamingCause, PeriodReach, SpanSource};
use engine::types::ability::{
    AbilityKind, CastingPermission, ChosenAttribute, ContinuousModification,
    DelayedTriggerCondition, Effect, ResolvedAbility, TargetRef,
};
use engine::types::actions::{GameAction, ResolveAllConsentDecision, ResolveAllScope};
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, LossReason};
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    AutoMayChoice, CastPaymentMode, GameEnd, GameState, LoopDetectionMode, ManaChoice,
    MayTriggerAutoChoiceSelector, PersistedGameState, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::interaction::{
    InteractionOpportunity, InteractionOpportunityResponse, InteractionResponse,
    InteractionResponseSpec, InteractionSessionId, InteractionShortcutDecision,
    InteractionShortcutPin, InteractionShortcutPoint, InteractionShortcutPointKind,
    InteractionShortcutPreviewFamily, InteractionSubmission, ViewerInteraction,
};
use engine::types::keywords::Keyword;
use engine::types::log::LogSegment;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::match_config::{MatchConfig, MatchPhase, MatchType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::food_chain_board::{self, BoardCMember};
use crate::loop_period_remade_token_source::{markers, take_recorded};
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
            payment_mode: CastPaymentMode::Auto,
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

/// The seat `state` hands priority to, when it waits on priority and publishes that seat.
fn handed_to(state: &GameState) -> Option<PlayerId> {
    match state.waiting_for {
        WaitingFor::Priority { player } if state.priority_player == player => Some(player),
        _ => None,
    }
}

/// CR 732.2a + CR 800.4a: a shortcut that is not taken (declined, its declaration rejected, or its
/// accepted proposal refused because its proposer conceded, CR 104.3a) hands priority back to the
/// proposer, or to the next player in turn order still in the game; an accepted take ends where
/// the active player receives priority (CR 117.3b). P1 holds priority on P0's turn with
/// Kiki-Jiki, Mirror Breaker ("Haste / {T}: Create a token that's a copy of target nonlegendary
/// creature you control, except it has haste. Sacrifice it at the beginning of the next end
/// step.") and Deceiver Exarch ("Flash (You may cast this spell any time you could cast an
/// instant.) / When this creature enters, choose one — / • Untap target permanent you control. /
/// • Tap target permanent an opponent controls.").
#[test]
fn a_shortcut_not_taken_hands_priority_back_to_its_proposer() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let p2 = PlayerId(2);
    let mut state = crate::loop_shortcut::restore_dump(&crate::loop_shortcut::gunzip_dump(
        include_bytes!("../fixtures/kilo_freed_relic_pentad_4p.json.gz"),
    ));
    let kiki = place(&mut state, P1, "Kiki-Jiki, Mirror Breaker", db);
    let exarch = place(&mut state, P1, "Deceiver Exarch", db);
    let score = kiki_score(kiki, exarch);
    let mut runner = GameRunner::from_state(state);
    let mut plain_priority = None;
    for _ in 0..40 {
        if is_offer(runner.state()) {
            break;
        }
        match runner.state().waiting_for {
            WaitingFor::Priority { player } if player == P1 => {
                plain_priority.get_or_insert_with(|| runner.state().clone());
                kiki_activates(&mut runner, kiki, &score);
            }
            WaitingFor::Priority { .. } => act(&mut runner, GameAction::PassPriority),
            ref other => panic!("unexpected {other:?}"),
        }
    }
    let offer = runner.state().clone();
    assert!(
        matches!(offer.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P1)
            && offer.active_player == P0,
        "reach: P1's loop is offered on P0's turn: {:?}",
        offer.waiting_for
    );
    let declare = |count| GameAction::DeclareShortcut {
        count,
        template: None,
    };
    let accept_all = |runner: &mut GameRunner| {
        let mut last_responder = None;
        while matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ) {
            last_responder = Some(runner.state().priority_player);
            act(
                runner,
                GameAction::RespondToShortcut {
                    response: ShortcutResponse::Accept,
                },
            );
        }
        last_responder
    };

    let mut conceded = GameRunner::from_state(plain_priority.expect("P1 held priority"));
    act(&mut conceded, GameAction::Concede { player_id: P1 });
    assert_eq!(
        handed_to(conceded.state()),
        Some(p2),
        "control: a concession at plain priority"
    );

    let mut declined = GameRunner::from_state(offer.clone());
    act(&mut declined, GameAction::DeclineShortcut);

    let mut rejected = GameRunner::from_state(offer.clone());
    act(&mut rejected, declare(IterationCount::Fixed(u32::MAX)));
    assert!(
        !matches!(
            rejected.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ),
        "reach: the over-cap declaration opens no window"
    );

    let mut refused = GameRunner::from_state(offer.clone());
    act(&mut refused, declare(IterationCount::Fixed(3)));
    assert!(
        matches!(refused.state().waiting_for, WaitingFor::RespondToShortcut { player, .. } if player == p2),
        "reach: the declaration polls P2: {:?}",
        refused.state().waiting_for
    );
    act(&mut refused, GameAction::Concede { player_id: P1 });
    assert_eq!(
        accept_all(&mut refused),
        Some(P0),
        "hostile: the last polled responder holds `priority_player` at the refusal"
    );
    assert!(!engine::game::players::is_alive(refused.state(), P1));

    assert_eq!(
        [declined.state(), rejected.state(), refused.state()].map(handed_to),
        [Some(P1), Some(P1), Some(p2)],
        "decline, rejected declaration, refused proposal of a departed proposer"
    );

    let mut taken = GameRunner::from_state(offer);
    act(&mut taken, declare(IterationCount::Fixed(3)));
    accept_all(&mut taken);
    assert_eq!(
        handed_to(taken.state()),
        Some(P0),
        "control: the accepted take: {:?}",
        taken.state().waiting_for
    );
    assert!(taken.state().stack.is_empty());
}

/// CR 702.8a + CR 702.10c: Kiki-Jiki copying Deceiver Exarch ("Flash … When this creature enters,
/// choose one — • Untap target permanent you control. …") confirms, its copies' flash and haste
/// and their delayed "sacrifice it" each grown. CR 702.9b + CR 732.2a: Pestermite's copies ("Flash
/// Flying When this creature enters, you may tap or untap target permanent.") carry flying, which
/// only restricts blocks; the period declares no block and nothing reads the copies' abilities,
/// so it confirms, is offered at the engine's cap (nothing the cards spend bounds it) and taken.
#[test]
fn kiki_jiki_confirms_with_deceiver_exarch_and_with_pestermite() {
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
    let score = kiki_score(kiki, pestermite);
    kiki_activates(&mut runner, kiki, &score);
    kiki_activates(&mut runner, kiki, &score);
    assert!(
        is_offer(runner.state()),
        "{:?}",
        latest_verdict(runner.state())
    );
    let WaitingFor::LoopShortcut { schema, .. } = &runner.state().waiting_for else {
        unreachable!();
    };
    assert_eq!(schema.iteration_count, IterationCount::Fixed(1_000));
    assert_eq!(
        performed_for_tests(runner.state()),
        Some(Ok(vec!["Pestermite".to_string()]))
    );
    let before = tokens(runner.state(), P0);
    take_to_step_end(&mut runner, 2);
    let state = runner.state();
    let minted: Vec<ObjectId> = tokens(state, P0)
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    assert_eq!(minted.len(), 2, "the take made the copies it counted");
    for id in &minted {
        let copy = &state.objects[id];
        assert!(
            !copy.tapped
                && copy.keywords.contains(&Keyword::Flying)
                && copy.keywords.contains(&Keyword::Haste),
            "{id:?}: tapped={} keywords={:?}",
            copy.tapped,
            copy.keywords
        );
    }
}

/// CR 602.2 + CR 732.2a: Phyrexian Altar recurring Gravecrawler beside Samwise Gamgee ("Whenever
/// another nontoken creature you control enters, create a Food token.") grows Foods whose "{2},
/// {T}, Sacrifice this token: You gain 3 life." the period never activates, so it is offered.
#[test]
fn an_altar_loop_growing_foods_is_offered() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(Some("Samwise Gamgee"), db);
    let offers = drive_altar_offers(&mut runner, (altar, gravecrawler), 3, |_| false);
    assert_eq!(offers.len(), 1);
    let state = runner.state();
    let foods = state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].name == "Food")
        .count();
    assert_eq!(foods, 1, "reach: Gravecrawler's entry made a Food");
    assert!(matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }));
}

/// CR 121.1 + CR 732.2a: Phyrexian Altar ("Sacrifice a creature: Add one mana of any color.")
/// recurring Gravecrawler beside Samwise Gamgee ("Whenever another nontoken creature you control
/// enters, create a Food token.") and Moldervine Reclamation ("Whenever a creature you control
/// dies, you gain 1 life and draw a card.") grows a Food and draws P0 a card each cycle; the cover
/// puts the draw back beside the Food, so the period is offered.
#[test]
fn an_altar_loop_growing_foods_and_drawing_passes_the_cover() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(Some("Samwise Gamgee"), db);
    place(runner.state_mut(), P0, "Moldervine Reclamation", db);
    let counts = |state: &GameState| {
        let foods = state
            .battlefield
            .iter()
            .filter(|id| state.objects[id].name == "Food")
            .count();
        let p0 = &state.players[0];
        (foods, p0.library.len(), p0.hand.len())
    };
    for cycle in 0..3 {
        let (foods, library, hand) = counts(runner.state());
        altar_cycle(&mut runner, altar, gravecrawler);
        assert_eq!(
            counts(runner.state()),
            (foods + 1, library - 1, hand + 1),
            "reach: cycle {cycle}"
        );
    }
    drive_altar_offers(&mut runner, (altar, gravecrawler), 3, |_| false);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    ));
}

/// CR 121.1 + CR 701.17a: beside Liliana the Repentant ("Whenever another creature or
/// planeswalker you control enters, mill two cards.") and Moldervine Reclamation ("Whenever a
/// creature you control dies, you gain 1 life and draw a card."), each cycle mills P0 two cards
/// and then draws the card under them, which the cover puts back as the top once they had left, so
/// the period is offered.
#[test]
fn a_draw_after_a_mill_passes_the_cover() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(Some("Liliana the Repentant"), db);
    let state = runner.state_mut();
    place(state, P0, "Moldervine Reclamation", db);
    let swamp = db.get_face_by_name("Swamp").expect("card in fixture");
    for _ in 0..30 {
        engine::game::deck_loading::create_object_from_card_face(state, swamp, P0);
    }
    let counts = |state: &GameState| {
        let p0 = &state.players[0];
        (p0.library.len(), p0.graveyard.len(), p0.hand.len())
    };
    for cycle in 0..3 {
        let (library, graveyard, hand) = counts(runner.state());
        altar_cycle(&mut runner, altar, gravecrawler);
        assert_eq!(
            counts(runner.state()),
            (library - 3, graveyard + 2, hand + 1),
            "reach: cycle {cycle} mills two and draws one"
        );
    }
    drive_altar_offers(&mut runner, (altar, gravecrawler), 3, |_| false);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    ));
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

fn performance(state: &GameState) -> Option<PeriodPerformance> {
    match &state.waiting_for {
        WaitingFor::LoopShortcut { period, .. } => period.performance(),
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
        assert!(
            matches!(
                board.runner.state().waiting_for,
                WaitingFor::LoopShortcut { .. }
            ),
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
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    ));
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
            payment_mode: CastPaymentMode::Auto,
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
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    ));
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
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    ));
    let before = incarnations(runner.state(), &[griffin, scourge]);
    take(&mut runner, 2);
    let after = incarnations(runner.state(), &[griffin, scourge]);
    assert!(
        after > before,
        "the take moved the creatures ({before} -> {after})"
    );
}

/// CR 732.1b: Phyrexian Altar recurring Gravecrawler with no payoff moves nothing toward anyone,
/// so the period has no axis.
#[test]
fn an_altar_gravecrawler_period_without_a_payoff_has_no_axis() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(None, db);
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
    assert_eq!(latest_verdict(runner.state()), Err(OfferRefusal::NoAxis));
}

/// The Altar + Gravecrawler period's next cycle, its offers declined.
fn altar_cycle(runner: &mut GameRunner, altar: ObjectId, gravecrawler: ObjectId) {
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
    };
    for activates in [false, true] {
        if activates {
            let index = ability(runner.state(), altar, true);
            activate(runner, altar, index);
        } else {
            cast(runner, gravecrawler, vec![], CastPaymentMode::Auto);
        }
        settle(runner, &score);
        while is_offer(runner.state()) {
            act(runner, GameAction::DeclineShortcut);
            settle(runner, &score);
        }
    }
}

/// CR 121.4 + CR 701.17b + CR 732.2a: beside Altar of the Brood ("Whenever another permanent you
/// control enters, each opponent mills a card."), Moldervine Reclamation ("Whenever a creature you
/// control dies, you gain 1 life and draw a card.") draws P0 a card a cycle while P1 mills one.
/// P0's draws count the period; P1's shorter library caps nothing, since milling loses no game.
#[test]
fn a_moldervine_altar_of_the_brood_period_is_counted_to_the_casters_library() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(Some("Altar of the Brood"), db);
    let state = runner.state_mut();
    place(state, P0, "Moldervine Reclamation", db);
    let cut: Vec<ObjectId> = state.players[1].library.iter().skip(3).copied().collect();
    for id in cut {
        remove_from_zone(state, id, Zone::Library, P1);
        state.objects.remove(&id);
    }
    drive_altar_offers(&mut runner, (altar, gravecrawler), 1, |_| false);
    let state = runner.state();
    assert_eq!(
        [0, 1].map(|seat| state.players[seat].library.len()),
        [9, 2],
        "reach: the offer's libraries"
    );
    let WaitingFor::LoopShortcut { schema, .. } = &state.waiting_for else {
        panic!("an offer");
    };
    assert_eq!(schema.iteration_count, IterationCount::Fixed(9));
    assert_eq!(schema.deliverable_capacity, 9);
    let after = taken(state, 9);
    assert_eq!(
        [0, 1].map(|seat| after.players[seat].library.len()),
        [0, 0],
        "the take draws P0's whole library"
    );
    assert!(after.game_end.is_none() && eliminated(&after).is_empty());
}

/// CR 121.1 + CR 704.5b + CR 732.2a: the same period with one card left in P0's library at the
/// offer draws it once, so one repetition is offered, and taking it leaves P0 in the game.
#[test]
fn a_moldervine_altar_of_the_brood_period_with_one_card_left_is_offered_one_repetition() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(Some("Altar of the Brood"), db);
    let state = runner.state_mut();
    place(state, P0, "Moldervine Reclamation", db);
    for (seat, keep) in [(P0, state.players[0].library.len() - 8), (P1, 3)] {
        let cut: Vec<ObjectId> = state.players[seat.0 as usize]
            .library
            .iter()
            .skip(keep)
            .copied()
            .collect();
        for id in cut {
            remove_from_zone(state, id, Zone::Library, seat);
            state.objects.remove(&id);
        }
    }
    drive_altar_offers(&mut runner, (altar, gravecrawler), 1, |_| false);
    let state = runner.state();
    assert_eq!(
        [0, 1].map(|seat| state.players[seat].library.len()),
        [1, 2],
        "reach: the offer's libraries"
    );
    let WaitingFor::LoopShortcut { schema, .. } = &state.waiting_for else {
        panic!("an offer");
    };
    assert_eq!(schema.iteration_count, IterationCount::Fixed(1));
    let after = taken(state, 1);
    assert_eq!(after.players[0].library.len(), 0);
    assert!(after.game_end.is_none() && eliminated(&after).is_empty());
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
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    ));
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
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    ));
    assert_eq!(certificate.unbounded, [ResourceAxis::LibraryDelta(P1)]);
    assert_eq!(certificate.win_kind, Some(WinKind::Advantage));
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
    assert_eq!(last.win_kind, Some(WinKind::Advantage));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    ));
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
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    ));
    assert_eq!(
        performance(runner.state()),
        Some(PeriodPerformance::UsesTheStack)
    );
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
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    ));
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

/// Each seat `events` record as losing, with its reasons, in order.
fn losses_in(events: &[GameEvent]) -> Vec<(PlayerId, BTreeSet<LossReason>)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerLost { player_id, reasons } => Some((*player_id, reasons.clone())),
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
        ..
    } = &state.waiting_for
    else {
        panic!("no offer: {:?}", latest_verdict(state));
    };
    assert_eq!(schema.iteration_count, IterationCount::Fixed(count));
    assert_eq!(schema.deliverable_capacity, count);
    assert_eq!(certificate.win_kind, Some(WinKind::LethalDamage));
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
            .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
            .unwrap_or_else(|| panic!("{payoff}: a signed offer"));
        assert_eq!(
            per_cycle.delta.life,
            [(P0, 1), (P1, -1)].into_iter().collect(),
            "{payoff}"
        );
        let WaitingFor::LoopShortcut { declaration, .. } = &offered.waiting_for else {
            panic!("{payoff}: an offer");
        };
        // The offer's own declaration carries the period's recorded answers, Phyrexian Altar's
        // colour among them.
        assert!(
            engine::ai_support::candidate_actions(&offered)
                .iter()
                .any(|candidate| candidate.action
                    == GameAction::DeclareShortcut {
                        count: IterationCount::Fixed(l),
                        template: declaration.clone(),
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

/// Drives P0's Phyrexian Altar + Gravecrawler loop on four seats, with `payoffs` beside Walking
/// Corpse, each of `placements` on its seat, `enchanting`'s Aura under its seat attached to the
/// first placement, and each of `starting` at its life, every trigger aimed at `aim`, to the offer.
fn four_seat_altar_offer(
    db: &CardDatabase,
    payoffs: &[&str],
    placements: &[(PlayerId, &str)],
    enchanting: Option<(PlayerId, &str)>,
    starting: &[(PlayerId, i32)],
    aim: PlayerId,
) -> GameRunner {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    for card in payoffs.iter().chain(&["Walking Corpse"]) {
        scenario.add_real_card(P0, card, Zone::Battlefield, db);
    }
    let placed: Vec<ObjectId> = placements
        .iter()
        .map(|&(seat, card)| scenario.add_real_card(seat, card, Zone::Battlefield, db))
        .collect();
    let aura =
        enchanting.map(|(seat, card)| (seat, scenario.add_real_card(seat, card, Zone::Hand, db)));
    let gravecrawler = scenario.add_real_card(P0, "Gravecrawler", Zone::Battlefield, db);
    for seat in 0..4 {
        for _ in 0..50 {
            scenario.add_real_card(PlayerId(seat), "Swamp", Zone::Library, db);
        }
    }
    for &(seat, life) in starting {
        scenario.with_life(seat, life);
    }
    let mut runner = scenario.build();
    if let Some((seat, aura)) = aura {
        let state = runner.state_mut();
        remove_from_zone(state, aura, Zone::Hand, seat);
        add_to_zone(state, aura, Zone::Battlefield, seat);
        state.objects.get_mut(&aura).expect("the Aura exists").zone = Zone::Battlefield;
        attach_to(state, aura, placed[0]);
        state.layers_dirty.mark_full();
    }
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action)
            + chooses_color(ManaType::Black)(action)
            + i32::from(
                matches!(
                    action,
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(seat))
                    } if *seat == aim
                ) || matches!(action, GameAction::DecideOptionalEffect { accept: true }),
            )
    };
    for _ in 0..6 {
        let index = ability(runner.state(), altar, true);
        activate(&mut runner, altar, index);
        settle(&mut runner, &score);
        if is_offer(runner.state()) {
            break;
        }
        cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
        settle(&mut runner, &score);
        if is_offer(runner.state()) {
            break;
        }
    }
    runner
}

/// CR 603.3d + CR 601.2c + CR 800.4a + CR 104.2a: on four seats, P0 sacrifices Gravecrawler to
/// Phyrexian Altar and recasts it beside a death trigger aimed at one seat: Blood Artist ("Whenever
/// this creature or another creature dies, target player loses 1 life and you gain 1 life."),
/// Spiteful Prankster ("Whenever another creature dies, this creature deals 1 damage to target
/// player or planeswalker."), Blood Artist beside Extractor Demon ("Whenever another creature
/// leaves the battlefield, you may have target player mill two cards."), or Blood Artist beside
/// Spiteful Prankster, with or without an opponent's Energy Field ("Prevent all damage that would
/// be dealt to you by sources you don't control." / "When a card is put into your graveyard from
/// anywhere, sacrifice this enchantment.") or Fiendish Duo ("First strike (This creature deals
/// combat damage before creatures without first strike.)" / "If a source would deal damage to an
/// opponent, it deals double that damage to that player instead."). Each trigger's target is
/// chosen as it goes on the stack, so the period re-aims at each seat left, and P0 wins. With every
/// slot taking life, P1 leaves at repetition 19, P2 at 39 and P3 at 59, or at 9, 19 and 29 when
/// Blood Artist and Spiteful Prankster both take 1; aimed at P2 or P3 instead, both taking 1 still
/// win at 29. A slot that
/// took nothing at P1 (Extractor Demon's mill, or Prankster's damage under Energy Field, CR 615.6)
/// keeps P1, so P2 leaves at 21, P3 at 41 and P1 at 59. Prankster measured taking nothing at Energy
/// Field's controller stays there while Blood Artist drains the others, so with the record aimed at
/// P2 and the Field under P1, or aimed at P1 and the Field under P3, P0 wins at 57. Under P1's
/// Fiendish Duo, P1 is not its own opponent and takes 2 a repetition: 29; under P2's, P1 takes 3 a
/// repetition until it leaves in repetition 6, and P2 leaves with the Duo (CR 800.4a) in 16: 26.
/// Under P1's Duo with the record aimed at P2 at 17, Prankster takes 1 at P2 once P1 has left with
/// the Duo, so P1 leaves at 11, P2 at 18 and P3 at 28: 28.
/// Palisade Giant ("All damage that would be dealt to you and other permanents you control is
/// dealt to this creature instead.") takes Prankster's damage in its controller's place until a
/// seventh destroys it (CR 614.1a + CR 704.5g), so Prankster leaves the Giant's seat after the hits
/// it survives while Blood Artist drains that seat first: 32 under P3 with the record aimed at P1,
/// and 32 under P1 or P2 aimed there, the Giant already marked once. Beside P1's Fiendish Duo,
/// Prankster's damage at P3 meets the Duo's doubling and the Giant's redirection and P3 orders them
/// (CR 616.1): the offer names the Giant first, which reaches 32 where the Duo first reaches 31, and
/// P3 may shorten where it would order them otherwise (CR 732.2b). With P1 at 12 and the Giant under
/// P3, P1 leaves at 15, before the Giant's seat, and Prankster moves on when P1 leaves: 28.
/// The record aims at one seat alone, so its own declaration reaches that seat's departure: 19, 9
/// when both slots take 1, 6 when P1 or P2 takes 3, or 5 when the Giant shares that seat.
#[test]
fn a_single_player_death_trigger_on_four_seats_re_aims_at_each_seat_left() {
    let Some(db) = shared_card_db() else { return };
    let prankster_artist = &["Blood Artist", "Spiteful Prankster"][..];
    for (payoffs, placements, aim, frame, count, own, end) in [
        (
            &["Blood Artist"][..],
            &[][..],
            P1,
            [21, 19, 20, 20],
            59,
            19,
            [80, 0, 0, 0],
        ),
        (
            &["Spiteful Prankster"][..],
            &[][..],
            P1,
            [20, 19, 20, 20],
            59,
            19,
            [20, 0, 0, 0],
        ),
        (
            &["Blood Artist", "Extractor Demon"][..],
            &[][..],
            P1,
            [21, 19, 20, 20],
            59,
            19,
            [80, 0, 0, 0],
        ),
        (
            prankster_artist,
            &[(P1, "Energy Field")][..],
            P1,
            [21, 19, 20, 20],
            59,
            19,
            [80, 0, 0, 0],
        ),
        (
            prankster_artist,
            &[][..],
            P1,
            [21, 18, 20, 20],
            29,
            9,
            [50, 0, 0, 0],
        ),
        (
            prankster_artist,
            &[(P1, "Energy Field")][..],
            P2,
            [21, 20, 18, 20],
            57,
            9,
            [78, 0, 0, 0],
        ),
        (
            prankster_artist,
            &[(P3, "Energy Field")][..],
            P1,
            [21, 18, 20, 20],
            57,
            9,
            [78, 0, 0, 0],
        ),
        (
            prankster_artist,
            &[][..],
            P2,
            [21, 20, 18, 20],
            29,
            9,
            [50, 0, 0, 0],
        ),
        (
            prankster_artist,
            &[][..],
            P3,
            [21, 20, 20, 18],
            29,
            9,
            [50, 0, 0, 0],
        ),
        (
            prankster_artist,
            &[(P1, "Fiendish Duo")][..],
            P1,
            [21, 18, 20, 20],
            29,
            9,
            [50, 0, 0, 0],
        ),
        (
            prankster_artist,
            &[(P2, "Fiendish Duo")][..],
            P1,
            [21, 17, 20, 20],
            26,
            6,
            [46, 0, 0, 0],
        ),
        (
            prankster_artist,
            &[(P1, "Fiendish Duo")][..],
            P2,
            [21, 20, 17, 20],
            28,
            6,
            [49, 0, 0, 0],
        ),
    ] {
        let mut runner = four_seat_altar_offer(db, payoffs, placements, None, &[], aim);
        assert_suggestion_wins(
            &mut runner,
            &format!("{payoffs:?} {placements:?} {aim:?}"),
            payoffs.len(),
            frame,
            count,
            Some(engine::analysis::decision_template::RepetitionBound::Measured),
            own,
            end,
        );
    }
    let giant_first: &[(&str, u8)] = &[("Palisade Giant", 0), ("Fiendish Duo", 1)];
    for (placements, starting, aim, frame, count, own, end, orders) in [
        (
            &[(P3, "Palisade Giant")][..],
            &[][..],
            P1,
            [21, 18, 20, 20],
            32,
            9,
            [53, 0, 0, 0],
            &[][..],
        ),
        (
            &[(P1, "Palisade Giant")][..],
            &[][..],
            P1,
            [21, 19, 20, 20],
            32,
            5,
            [53, 0, 0, 0],
            &[][..],
        ),
        (
            &[(P2, "Palisade Giant")][..],
            &[][..],
            P2,
            [21, 20, 19, 20],
            32,
            5,
            [53, 0, 0, 0],
            &[][..],
        ),
        (
            &[(P3, "Palisade Giant"), (P1, "Fiendish Duo")][..],
            &[][..],
            P1,
            [21, 18, 20, 20],
            32,
            9,
            [53, 0, 0, 0],
            giant_first,
        ),
        (
            &[(P3, "Palisade Giant")][..],
            &[(P1, 12)][..],
            P1,
            [21, 10, 20, 20],
            28,
            5,
            [49, 0, 0, 0],
            &[][..],
        ),
    ] {
        let label = format!("{placements:?} {starting:?}, aim {aim:?}");
        let mut runner =
            four_seat_altar_offer(db, prankster_artist, placements, None, starting, aim);
        let WaitingFor::LoopShortcut { schema, .. } = &runner.state().waiting_for else {
            panic!("{label}: no offer: {:?}", latest_verdict(runner.state()));
        };
        let positions = orders.len() as u8;
        assert_eq!(
            replacement_places(
                runner.state(),
                schema
                    .suggested_declaration
                    .as_ref()
                    .map_or(&[][..], |suggestion| &suggestion.decisions),
                &schema.points,
            ),
            (
                orders
                    .iter()
                    .map(|&(name, pos)| (name.to_string(), pos))
                    .collect(),
                orders
                    .iter()
                    .map(|&(name, pos)| (name.to_string(), pos, positions))
                    .collect(),
            ),
            "{label}: the suggestion's replacement order and its published points"
        );
        assert_suggestion_wins(&mut runner, &label, 2, frame, count, None, own, end);
    }
}

/// Each CR 616.1 replacement-order pin among `pins`, as its effect source's name and place, and each
/// published `Order` point among `points`, as its source's name, place and number of places.
#[allow(clippy::type_complexity)]
fn replacement_places(
    state: &GameState,
    pins: &[PinnedDecision],
    points: &[engine::analysis::decision_template::DecisionPoint],
) -> (Vec<(String, u8)>, Vec<(String, u8, u8)>) {
    use engine::analysis::decision_template::{ChoicePoint, DecisionPointKind};
    use engine::types::game_state::YieldTarget;
    let name = |source: &YieldTarget| {
        let YieldTarget::ThisObject { source_id, .. } = source else {
            panic!("an object source, got {source:?}");
        };
        state.objects[source_id].name.clone()
    };
    (
        pins.iter()
            .filter_map(|pin| match pin {
                PinnedDecision::Order { slot, pos }
                    if slot.point == ChoicePoint::ReplacementOrder =>
                {
                    Some((name(&slot.source), *pos))
                }
                _ => None,
            })
            .collect(),
        points
            .iter()
            .filter_map(|point| match point.kind {
                DecisionPointKind::Order { pos, positions } => {
                    Some((name(&point.slot.source), pos, positions))
                }
                _ => None,
            })
            .collect(),
    )
}

/// Asserts the offer standing in `runner` (`label`) at its frame — `stack` triggers, lives `frame`
/// — publishes `count` with P0 winning beside its own declaration's `own`, with `bound` as its
/// repetition bound where given, and that taking its suggestion at `count` ends the game
/// once with P0 the winner at lives `end`.
#[allow(clippy::too_many_arguments)]
fn assert_suggestion_wins(
    runner: &mut GameRunner,
    label: &str,
    stack: usize,
    frame: [i32; 4],
    count: u32,
    bound: Option<engine::analysis::decision_template::RepetitionBound>,
    own: u32,
    end: [i32; 4],
) {
    let offered = runner.state().clone();
    assert_eq!(
        (offered.stack.len(), lives(&offered)),
        (stack, frame.to_vec()),
        "reach {label}: the triggers stand at the offer frame"
    );
    let WaitingFor::LoopShortcut {
        schema,
        predicted_winner,
        declaration,
        ..
    } = &offered.waiting_for
    else {
        panic!("{label}: no offer: {:?}", latest_verdict(&offered));
    };
    assert_eq!(
        (&schema.iteration_count, *predicted_winner),
        (&IterationCount::Fixed(count), Some(P0)),
        "{label}"
    );
    if let Some(bound) = bound {
        assert_eq!(schema.repetition_bound, bound, "{label}");
    }
    bounded_recorded_offer(&offered, count);
    assert_eq!(
        declaration.as_ref().map(|own| own.replay.clone()),
        Some(ReplayMode::Scheduled {
            count: IterationCount::Fixed(own)
        }),
        "{label}"
    );
    let suggestion = schema
        .suggested_declaration
        .clone()
        .unwrap_or_else(|| panic!("{label}: a suggestion"));
    assert_eq!(
        suggestion.replay,
        ReplayMode::Scheduled {
            count: IterationCount::Fixed(count)
        },
        "{label}"
    );

    let mut events = act_events(
        runner,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(count),
            template: Some(suggestion),
        },
    );
    events.extend(accept(runner));
    let taken = runner.state();
    assert_eq!(
        taken.waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "{label}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::GameOver { .. }))
            .count(),
        1,
        "{label}"
    );
    assert_eq!(
        (lives(taken), eliminated(taken)),
        (end.to_vec(), vec![P1, P2, P3]),
        "{label}"
    );
}

/// Takes the offer standing in `runner` at its published count with its suggestion, and returns
/// the events.
fn take_suggestion(runner: &mut GameRunner) -> Vec<GameEvent> {
    let WaitingFor::LoopShortcut { schema, .. } = &runner.state().waiting_for else {
        panic!("an offer stands, got {:?}", runner.state().waiting_for);
    };
    let count = schema.iteration_count.clone();
    let template = schema.suggested_declaration.clone();
    let mut events = act_events(runner, GameAction::DeclareShortcut { count, template });
    events.extend(accept(runner));
    events
}

/// P0's four-seat Altar loop with Blood Artist and Spiteful Prankster, Palisade Giant under P3 and
/// Fiendish Duo under P1, every trigger aimed at P1, at its offer.
fn giant_and_duo_offer(db: &CardDatabase) -> GameRunner {
    four_seat_altar_offer(
        db,
        &["Blood Artist", "Spiteful Prankster"],
        &[(P3, "Palisade Giant"), (P1, "Fiendish Duo")],
        None,
        &[],
        P1,
    )
}

/// The object on `state`'s battlefield named `name` under `controller`, as a published candidate
/// names it.
fn object_named(state: &GameState, name: &str, controller: PlayerId) -> (String, String) {
    let (id, _) = state
        .objects
        .iter()
        .find(|(_, object)| object.name == name && object.controller == controller)
        .unwrap_or_else(|| panic!("{name} under {controller:?}"));
    (id.0.to_string(), name.to_string())
}

/// Each published `Order` point among `points`, as whether it is read-only and the objects its
/// candidates name among `candidates`, in order.
fn order_statements(
    points: &[InteractionShortcutPoint],
    candidates: &[engine::types::interaction::InteractionChoice],
) -> Vec<(bool, Vec<(String, String)>)> {
    use engine::types::interaction::InteractionPresentationSurface;
    points
        .iter()
        .filter(|point| point.kind == InteractionShortcutPointKind::Order)
        .map(|point| {
            let named = point
                .candidate_ids
                .iter()
                .map(|id| {
                    candidates
                        .iter()
                        .find(|choice| choice.id == *id)
                        .and_then(|choice| {
                            choice.surfaces.iter().find_map(|surface| match surface {
                                InteractionPresentationSurface::Object {
                                    reference,
                                    name: Some(name),
                                    ..
                                } => Some((reference.clone(), name.clone())),
                                _ => None,
                            })
                        })
                        .unwrap_or_else(|| panic!("candidate {id:?} names an object"))
                })
                .collect();
            (point.read_only, named)
        })
        .collect()
}

/// The candidates an interaction opportunity publishes.
fn opportunity_candidates(
    opportunity: &InteractionOpportunity,
) -> &[engine::types::interaction::InteractionChoice] {
    match &opportunity.response {
        InteractionOpportunityResponse::Schema { candidates, .. } => candidates,
        other => panic!("a schema opportunity, got {other:?}"),
    }
}

/// CR 616.1 + CR 732.2a: Spiteful Prankster's damage at P3 meets Fiendish Duo's "If a source would
/// deal damage to an opponent, it deals double that damage to that player instead." and Palisade
/// Giant's "All damage that would be dealt to you and other permanents you control is dealt to this
/// creature instead.", an order the record never made. The offer publishes that order as read-only
/// points, so accepting the suggestion through the interaction response declares it from the
/// confirmed period and the take wins at 32.
#[test]
fn a_replacement_order_the_record_never_made_is_declared_through_the_ingress() {
    use engine::analysis::decision_template::{
        AnnouncementSubject, DecisionPointKind, TargetPin, TargetSchedule,
    };
    use engine::types::interaction::AmountAssignment;
    let Some(db) = shared_card_db() else { return };
    let offered = giant_and_duo_offer(db).state().clone();
    assert_eq!(
        (offered.stack.len(), lives(&offered)),
        (2, vec![21, 18, 20, 20]),
        "reach: the triggers stand at the offer frame"
    );
    let WaitingFor::LoopShortcut { schema, .. } = &offered.waiting_for else {
        panic!("no offer: {:?}", latest_verdict(&offered));
    };
    let IterationCount::Fixed(count) = schema.iteration_count else {
        panic!("a counted offer, got {:?}", schema.iteration_count);
    };
    assert_eq!(count, 32);
    let suggestion = schema.suggested_declaration.clone().expect("a suggestion");
    let giant = object_named(&offered, "Palisade Giant", P3);
    let duo = object_named(&offered, "Fiendish Duo", P1);

    let (bound, view) = published_to_p0(&offered);
    let (opportunity, points) = shortcut_schema(&view).expect("a shortcut schema");
    assert_eq!(
        order_statements(points, opportunity_candidates(opportunity)),
        vec![(true, vec![giant, duo]), (true, Vec::new())],
        "the place-0 point states the order; the place-1 point states nothing"
    );
    // The suggestion's own announcement sequences, named through the published candidates.
    let pins: Vec<InteractionShortcutPin> = points
        .iter()
        .zip(&schema.points)
        .filter_map(|(point, published)| {
            let DecisionPointKind::Targets { legal_targets, .. } = &published.kind else {
                return None;
            };
            let targets = suggestion
                .decisions
                .iter()
                .find_map(|decision| match decision {
                    PinnedDecision::Targets { slot, targets } if *slot == published.slot => {
                        Some(targets)
                    }
                    _ => None,
                })?;
            let [TargetPin::Scheduled(TargetSchedule::Piecewise(segments))] = targets.as_slice()
            else {
                panic!("a piecewise suggestion, got {targets:?}");
            };
            let named: Vec<AmountAssignment> = segments
                .iter()
                .enumerate()
                .filter_map(|(k, (start, ranking))| {
                    let end = segments.get(k + 1).map_or(count, |(next, _)| *next);
                    let amount = count.min(end).saturating_sub(*start);
                    let AnnouncementSubject::Seat(seat) = ranking.head() else {
                        panic!("a seat segment, got {ranking:?}");
                    };
                    let at = legal_targets
                        .iter()
                        .position(|target| *target == TargetRef::Player(*seat))?;
                    (amount > 0).then(|| AmountAssignment {
                        choice_id: point.candidate_ids[at].clone(),
                        amount,
                    })
                })
                .collect();
            Some(InteractionShortcutPin {
                group: point.group,
                choice_ids: named.iter().map(|part| part.choice_id.clone()).collect(),
                amounts: named,
            })
        })
        .collect();
    assert_eq!(
        pins.len(),
        2,
        "reach: both announced-target points are answered"
    );
    let submission = InteractionSubmission {
        interaction_id: opportunity.interaction_id.clone(),
        response: InteractionResponse::Shortcut {
            decision: InteractionShortcutDecision::AcceptSuggested,
            pins,
        },
    };
    let template = match resolve_interaction_response(&bound, P0, &submission) {
        Ok(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(32),
            template: Some(template),
        }) => template,
        other => panic!(
            "the response mints a declaration of 32; got {:?}",
            other.map_err(|error| error.code)
        ),
    };
    let orders = |decisions: &[PinnedDecision]| -> Vec<PinnedDecision> {
        decisions
            .iter()
            .filter(|decision| matches!(decision, PinnedDecision::Order { .. }))
            .cloned()
            .collect()
    };
    let suggested = orders(&suggestion.decisions);
    assert_eq!(
        suggested.len(),
        2,
        "reach: the suggestion declares the order"
    );
    assert_eq!(orders(&template.decisions), suggested);

    let mut declared = bound.clone();
    assert!(submit_interaction(&mut declared, P0, submission).is_ok());
    let mut runner = GameRunner::from_state(declared);
    accept(&mut runner);
    let taken = runner.state();
    assert_eq!(taken.waiting_for, WaitingFor::GameOver { winner: Some(P0) });
    assert_eq!(lives(taken), [53, 0, 0, 0]);
}

/// CR 616.1 + CR 732.2a: a second Palisade Giant under P2 gives P2 the order the offer names, and
/// none for P3. Its suggestion reaches 28 with no winner: P2 leaves at 21 and P1 at 22, and a 29th
/// Prankster hit would destroy P3's Giant and break the period (CR 704.5g). Re-aimed at P3 while P1
/// lives, Prankster's damage meets Fiendish Duo and P3's Giant, an order the declaration names for
/// P2 alone; it describes no choice of P3's, so nothing is performed and the declaration is handed
/// back with P3's Giant unmarked.
#[test]
fn a_second_seats_order_the_offer_does_not_name_is_never_answered_from_the_first() {
    use engine::analysis::decision_template::{
        AnnouncementSubject, Ranking, TargetPin, TargetSchedule,
    };
    use engine::types::game_state::YieldTarget;
    let Some(db) = shared_card_db() else { return };
    let mut runner = four_seat_altar_offer(
        db,
        &["Blood Artist", "Spiteful Prankster"],
        &[
            (P3, "Palisade Giant"),
            (P1, "Fiendish Duo"),
            (P2, "Palisade Giant"),
        ],
        None,
        &[],
        P1,
    );
    let offered = runner.state().clone();
    assert_eq!(
        (offered.stack.len(), lives(&offered)),
        (2, vec![21, 18, 20, 20]),
        "reach: the triggers stand at the offer frame"
    );
    let WaitingFor::LoopShortcut {
        schema,
        predicted_winner,
        declaration,
        ..
    } = &offered.waiting_for
    else {
        panic!("no offer: {:?}", latest_verdict(&offered));
    };
    assert_eq!(
        (&schema.iteration_count, *predicted_winner),
        (&IterationCount::Fixed(28), None)
    );
    assert_eq!(
        declaration.as_ref().map(|own| own.replay.clone()),
        Some(ReplayMode::Scheduled {
            count: IterationCount::Fixed(9)
        })
    );
    let suggestion = schema.suggested_declaration.clone().expect("a suggestion");
    let p2_giant = object_named(&offered, "Palisade Giant", P2).0;
    let duo = object_named(&offered, "Fiendish Duo", P1).0;
    let places: Vec<(String, u8)> = suggestion
        .decisions
        .iter()
        .filter_map(|decision| match decision {
            PinnedDecision::Order { slot, pos } => match &slot.source {
                YieldTarget::ThisObject { source_id, .. } => Some((source_id.0.to_string(), *pos)),
                other => panic!("an object source, got {other:?}"),
            },
            _ => None,
        })
        .collect();
    assert_eq!(places, [(p2_giant, 0), (duo, 1)], "P2's order, Giant first");

    let events = take_suggestion(&mut runner);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::GameOver { .. })),
        "the take ends no game"
    );
    let taken = runner.state();
    assert_eq!(
        (lives(taken), eliminated(taken)),
        (vec![49, 0, 0, 14], vec![P1, P2])
    );

    let prankster = offered
        .objects
        .iter()
        .find(|(_, object)| object.name == "Spiteful Prankster")
        .map(|(id, _)| *id)
        .expect("Spiteful Prankster");
    let p3_giant = offered
        .objects
        .iter()
        .find(|(_, object)| object.name == "Palisade Giant" && object.controller == P3)
        .map(|(id, _)| *id)
        .expect("P3's Palisade Giant");
    let mut partial = suggestion.clone();
    let mut re_aimed = 0;
    for decision in &mut partial.decisions {
        if let PinnedDecision::Targets { slot, targets } = decision {
            if matches!(slot.source, YieldTarget::ThisObject { source_id, .. } if source_id == prankster)
            {
                *targets = vec![TargetPin::Scheduled(TargetSchedule::Constant(
                    Ranking::one(AnnouncementSubject::Seat(P3)),
                ))];
                re_aimed += 1;
            }
        }
    }
    assert_eq!(
        re_aimed, 1,
        "reach: Prankster's announced target is re-aimed at P3"
    );
    let mut runner = GameRunner::from_state(offered.clone());
    act(
        &mut runner,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(3),
            template: Some(partial),
        },
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "the declaration is handed back, no reply window"
    );
    let handed_back = runner.state();
    assert_eq!(lives(handed_back), [21, 18, 20, 20]);
    assert_eq!(handed_back.objects[&p3_giant].damage_marked, 0);
}

/// CR 732.2b: P3, whose order Spiteful Prankster's damage meets (Fiendish Duo's "If a source would
/// deal damage to an opponent, it deals double that damage to that player instead." and Palisade
/// Giant's "All damage that would be dealt to you and other permanents you control is dealt to this
/// creature instead."), needs the order the proposal names for it to shorten where it would choose
/// otherwise. It is public proposal text, so each responder, chooser or not, is shown Palisade Giant
/// then Fiendish Duo, as the proposer's offer states it.
#[test]
fn a_declared_replacement_order_is_stated_to_every_responder() {
    let Some(db) = shared_card_db() else { return };
    let mut runner = giant_and_duo_offer(db);
    let offered = runner.state().clone();
    assert_eq!(
        (offered.stack.len(), lives(&offered)),
        (2, vec![21, 18, 20, 20]),
        "reach: the triggers stand at the offer frame"
    );
    let stated = vec![
        object_named(&offered, "Palisade Giant", P3),
        object_named(&offered, "Fiendish Duo", P1),
    ];
    let (_, view) = published_to_p0(&offered);
    let (opportunity, points) = shortcut_schema(&view).expect("a shortcut schema");
    assert_eq!(
        order_statements(points, opportunity_candidates(opportunity)),
        vec![(true, stated.clone()), (true, Vec::new())],
        "the proposer's offer states the order at place 0"
    );

    let WaitingFor::LoopShortcut { schema, .. } = &offered.waiting_for else {
        panic!("no offer: {:?}", latest_verdict(&offered));
    };
    let count = schema.iteration_count.clone();
    let template = schema.suggested_declaration.clone();
    act(&mut runner, GameAction::DeclareShortcut { count, template });
    let mut responders = Vec::new();
    while let WaitingFor::RespondToShortcut { player, .. } = runner.state().waiting_for.clone() {
        let mut bound = runner.state().clone();
        bind_interaction_authority(&mut bound, InteractionSessionId("respond".into()))
            .expect("valid interaction authority binding");
        let filtered = filter_state_for_viewer(&bound, player);
        let view = derive_viewer_interaction(&bound, &filtered, player);
        let (opportunity, points) = view
            .opportunities
            .iter()
            .find_map(|opportunity| match &opportunity.response {
                InteractionOpportunityResponse::Schema {
                    spec: InteractionResponseSpec::ShortcutReply { points, .. },
                    ..
                } => Some((opportunity, points.as_slice())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{player:?} is asked to reply"));
        assert_eq!(
            points
                .iter()
                .filter(|point| point.kind == InteractionShortcutPointKind::Targets)
                .count(),
            2,
            "reach: {player:?} is shown both announced-target decisions"
        );
        assert_eq!(
            order_statements(points, opportunity_candidates(opportunity)),
            vec![(true, stated.clone())],
            "{player:?} is shown the order once"
        );
        responders.push(player);
        act(
            &mut runner,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        );
    }
    assert_eq!(responders, [P1, P2, P3]);
    let taken = runner.state();
    assert_eq!(taken.waiting_for, WaitingFor::GameOver { winner: Some(P0) });
    assert_eq!(lives(taken), [53, 0, 0, 0]);
}

/// CR 613.1b + CR 800.4a: P2's Confiscate ("Enchant permanent / You control enchanted
/// permanent.") on P1's Energy Field ("Prevent all damage that would be dealt to you by sources you
/// don't control." / "When a card is put into your graveyard from anywhere, sacrifice this
/// enchantment.") makes the Field protect P2 until P2 leaves, when the control effect ends and the
/// Field protects P1 again. So Spiteful Prankster stays on P2 while Blood Artist drains the others
/// and P2, then moves to P1 while Blood Artist drains P1: with the record aimed at P2, every
/// repetition takes exactly 1, 59; aimed at P1 or P3, the first repetition takes 2 there, 57.
#[test]
fn a_zero_that_moves_when_its_controller_leaves_follows_it() {
    let Some(db) = shared_card_db() else { return };
    for (aim, frame, count, own, end) in [
        (P2, [21, 20, 19, 20], 59, 19, [80, 0, 0, 0]),
        (P1, [21, 18, 20, 20], 57, 9, [78, 0, 0, 0]),
        (P3, [21, 20, 20, 18], 57, 9, [78, 0, 0, 0]),
    ] {
        let mut runner = four_seat_altar_offer(
            db,
            &["Blood Artist", "Spiteful Prankster"],
            &[(P1, "Energy Field")],
            Some((P2, "Confiscate")),
            &[],
            aim,
        );
        let field = runner
            .state()
            .objects
            .values()
            .find(|object| object.name == "Energy Field")
            .expect("Energy Field is on the battlefield");
        assert_eq!(
            (field.owner, field.controller),
            (P1, P2),
            "aim {aim:?}: Confiscate gives P2 the Field"
        );
        assert_suggestion_wins(
            &mut runner,
            &format!("Confiscate, aim {aim:?}"),
            2,
            frame,
            count,
            Some(engine::analysis::decision_template::RepetitionBound::Measured),
            own,
            end,
        );
    }
}

/// CR 117.4 + CR 616.1: with P1's Energy Field ("Prevent all damage that would be dealt to you by
/// sources you don't control.") and P2's Fiendish Duo ("If a source would deal damage to an
/// opponent, it deals double that damage to that player instead.") both applying to Spiteful
/// Prankster's damage to P1, the record holds P1's choice of which applies first; a repetition
/// whose Prankster does not hit P1 with both still applying never asks it. Either order deals 0,
/// so Prankster stays on P1 while Blood Artist drains P2, P3 and P1, every repetition taking
/// exactly 1: 59.
#[test]
fn a_recorded_order_a_repetition_never_asks_does_not_stop_it() {
    let Some(db) = shared_card_db() else { return };
    let mut runner = four_seat_altar_offer(
        db,
        &["Blood Artist", "Spiteful Prankster"],
        &[(P1, "Energy Field"), (P2, "Fiendish Duo")],
        None,
        &[],
        P1,
    );
    assert_suggestion_wins(
        &mut runner,
        "Energy Field under P1, Fiendish Duo under P2",
        2,
        [21, 19, 20, 20],
        59,
        Some(engine::analysis::decision_template::RepetitionBound::Measured),
        19,
        [80, 0, 0, 0],
    );
}

/// CR 603.3b + CR 704.5a + CR 104.2a: with no negator, P1 at 1 life and the record aimed at P2 at
/// 18, Blood Artist ("Whenever this creature or another creature dies, target player loses 1 life
/// and you gain 1 life.") and Spiteful Prankster ("Whenever another creature dies, this creature
/// deals 1 damage to target player or planeswalker.") trigger on one death and go on the stack
/// together, so in repetition 2 Prankster's 1 kills P1 while Blood Artist drains P2, which leaves
/// in repetition 10 as Blood Artist turns to P3. P3 then takes 1 there and 2 a repetition until
/// Prankster's 1 in repetition 20 ends the game, Blood Artist having resolved 19 times: 20.
#[test]
fn a_segment_its_seat_leaves_before_the_slot_resolves_there_is_dropped() {
    let Some(db) = shared_card_db() else { return };
    let mut runner = four_seat_altar_offer(
        db,
        &["Blood Artist", "Spiteful Prankster"],
        &[],
        None,
        &[(P1, 1)],
        P2,
    );
    let offered = runner.state().clone();
    assert_eq!(
        (offered.stack.len(), lives(&offered)),
        (2, vec![21, 1, 18, 20]),
        "the triggers stand at the offer frame"
    );
    let WaitingFor::LoopShortcut {
        schema,
        predicted_winner,
        declaration,
        ..
    } = &offered.waiting_for
    else {
        panic!("no offer: {:?}", latest_verdict(&offered));
    };
    assert_eq!(
        (
            &schema.iteration_count,
            schema.repetition_bound,
            *predicted_winner
        ),
        (
            &IterationCount::Fixed(20),
            engine::analysis::decision_template::RepetitionBound::Measured,
            Some(P0)
        )
    );
    assert_eq!(
        declaration.as_ref().map(|own| own.replay.clone()),
        Some(ReplayMode::Scheduled {
            count: IterationCount::Fixed(9)
        })
    );
    assert_eq!(
        schema
            .suggested_declaration
            .as_ref()
            .map(|suggestion| suggestion.replay.clone()),
        Some(ReplayMode::Scheduled {
            count: IterationCount::Fixed(20)
        })
    );
    let events = take_suggestion(&mut runner);
    let taken = runner.state();
    assert_eq!(taken.waiting_for, WaitingFor::GameOver { winner: Some(P0) });
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::GameOver { .. }))
            .count(),
        1
    );
    assert_eq!(
        (lives(taken), eliminated(taken)),
        (vec![40, 0, 0, 0], vec![P1, P2, P3])
    );
}

fn in_a_best_of_three(runner: &mut GameRunner) {
    runner.state_mut().set_match_config(MatchConfig {
        match_type: MatchType::Bo3,
        loop_detection: LoopDetectionMode::Interactive,
    });
}

/// CR 704.5a + CR 104.2a + CR 100.4: in a best-of-three, the Altar period taken as many times as
/// P1 has life ends the game with its last repetition, and the players go on to sideboard.
#[test]
fn an_altar_period_taken_to_lethal_in_a_best_of_three_reaches_sideboarding() {
    let Some(db) = shared_card_db() else { return };
    for payoff in ["Zulaport Cutthroat", "Blood Artist"] {
        let (mut runner, altar, gravecrawler) = altar_board(Some(payoff), db);
        in_a_best_of_three(&mut runner);
        drain_to_offer(&mut runner, altar, gravecrawler, &[P1]);
        let l = runner.state().players[1].life as u32;
        bounded_recorded_offer(runner.state(), l);

        let events = take(&mut runner, l);
        let state = runner.state();
        assert_eq!(
            state.game_end,
            Some(GameEnd { winner: Some(P0) }),
            "{payoff}"
        );
        assert_eq!(eliminated(state), [P1], "{payoff}");
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == GameEvent::GameOver { winner: Some(P0) })
                .count(),
            1,
            "{payoff}"
        );
        assert_eq!(state.match_phase, MatchPhase::BetweenGames, "{payoff}");
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::BetweenGamesSideboard {
                    player: P0,
                    game_number: 2,
                    ..
                }
            ),
            "{payoff}: {:?}",
            state.waiting_for
        );
    }
}

/// CR 104.2a + CR 732.2a: a period whose second repetition ends a best-of-three game is counted to
/// that ending with P0 its winner, as in a single game; with one more life it is counted to three.
#[test]
fn a_period_whose_replay_ends_a_best_of_three_game_is_counted_to_its_ending() {
    let Some(db) = shared_card_db() else { return };
    let one_cycle_from = |p1_life: i32| {
        let (mut runner, altar, gravecrawler) =
            altar_board_at("Zulaport Cutthroat", &[20, p1_life], db);
        in_a_best_of_three(&mut runner);
        let score = |action: &GameAction| {
            2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
        };
        cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
        settle(&mut runner, &score);
        let index = ability(runner.state(), altar, true);
        activate(&mut runner, altar, index);
        settle(&mut runner, &score);
        runner
    };

    let counted = |p1_life: i32| {
        let runner = one_cycle_from(p1_life);
        let WaitingFor::LoopShortcut {
            schema,
            predicted_winner,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("an offer, got {:?}", runner.state().waiting_for);
        };
        (
            lives(runner.state()),
            schema.iteration_count.clone(),
            *predicted_winner,
        )
    };
    assert_eq!(
        counted(3),
        (vec![21, 2], IterationCount::Fixed(2), Some(P0))
    );
    assert_eq!(
        counted(4),
        (vec![21, 3], IterationCount::Fixed(3), Some(P0))
    );
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

/// `state` after the standing offer is declared at `count`, asserted to carry the offer's period
/// onto the proposal.
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

/// `state` reloaded through both ingresses with `"road": stamp` spliced into the `waiting_for`
/// carrier at the JSON pointer `carrier`, once per ingress.
fn reload_with_road(
    state: &GameState,
    carrier: &str,
    stamp: Option<&str>,
) -> [Result<GameState, String>; 2] {
    fn splice(value: &mut serde_json::Value, carrier: &str, stamp: &str) -> usize {
        let here = value
            .get_mut("waiting_for")
            .and_then(|waiting_for| waiting_for.pointer_mut(carrier))
            .and_then(serde_json::Value::as_object_mut)
            .map(|carrier| carrier.insert("road".to_string(), stamp.into()))
            .is_some();
        usize::from(here)
            + match value {
                serde_json::Value::Object(object) => object
                    .values_mut()
                    .map(|child| splice(child, carrier, stamp))
                    .sum(),
                serde_json::Value::Array(array) => array
                    .iter_mut()
                    .map(|child| splice(child, carrier, stamp))
                    .sum(),
                _ => 0,
            }
    }
    let mut persisted =
        serde_json::to_value(PersistedGameState::capture(state.clone())).expect("saves");
    let mut bare = serde_json::to_value(state).expect("saves");
    if let Some(stamp) = stamp {
        for saved in [&mut persisted, &mut bare] {
            assert_eq!(
                splice(saved, carrier, stamp),
                1,
                "reach: one {carrier} carrier"
            );
        }
    }
    let persisted = serde_json::from_value::<PersistedGameState>(persisted)
        .map_err(|error| error.to_string())
        .and_then(|saved| saved.into_game_state().map_err(|error| error.to_string()));
    let bare = serde_json::from_value::<GameState>(bare).map_err(|error| error.to_string());
    [persisted, bare]
}

/// CR 732.2a: a saved offer or proposal carrying a `road` key decodes as the unspliced one, and
/// the spliced proposal's take is the take of the offer's period.
#[test]
fn a_saved_offer_or_proposal_carrying_a_road_loads_and_takes_its_period() {
    let Some(db) = shared_card_db() else { return };
    let offered = bounded_recorded_offer_with_period(db);
    let expected = serde_json::to_value(taken(&offered, 3)).expect("saves");
    let proposal = proposed(offered.clone(), 3);
    for stamp in [Some("Ring"), Some("RecordedPeriod"), None] {
        for (state, carrier) in [(&offered, "/data"), (&proposal, "/data/proposal")] {
            for restored in reload_with_road(state, carrier, stamp) {
                let restored =
                    restored.unwrap_or_else(|error| panic!("[{stamp:?} {carrier}] {error}"));
                assert_eq!(
                    restored.waiting_for, state.waiting_for,
                    "[{stamp:?} {carrier}]"
                );
                if matches!(restored.waiting_for, WaitingFor::RespondToShortcut { .. }) {
                    let mut runner = GameRunner::from_state(restored);
                    accept(&mut runner);
                    assert_eq!(
                        serde_json::to_value(runner.state()).expect("saves"),
                        expected,
                        "[{stamp:?}]"
                    );
                }
            }
        }
    }
}

/// Whether `seat` has a play in the trace at `state`.
fn plays_in_trace(state: &GameState, seat: PlayerId) -> bool {
    play_trace_view(state).is_some_and(|view| {
        view.entries
            .iter()
            .any(|entry| entry.seat == seat && matches!(entry.kind, EntryKind::Play { .. }))
    })
}

/// CR 732.2a: a proposal refused when it is taken is declined, so the proposer's plays leave the
/// trace. The Altar–Zulaport proposal, its count raised past what a take may drive after the
/// declaration, is refused once P1 accepts.
#[test]
fn a_proposal_refused_at_its_take_leaves_the_proposers_plays_out_of_the_trace() {
    let Some(db) = shared_card_db() else { return };
    let mut proposal = proposed(bounded_recorded_offer_with_period(db), 3);
    assert!(
        plays_in_trace(&proposal, P0),
        "reach: P0's plays made the period"
    );
    let WaitingFor::RespondToShortcut {
        proposal: shortcut, ..
    } = &mut proposal.waiting_for
    else {
        unreachable!("`proposed` asserted the proposal")
    };
    shortcut.count = IterationCount::Fixed(u32::MAX);
    let before = lives(&proposal);
    let mut runner = GameRunner::from_state(proposal);
    accept(&mut runner);
    let state = runner.state();
    assert_eq!(lives(state), before, "reach: the take is refused");
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
    assert!(!plays_in_trace(state, P0));
}

/// CR 704.5a + CR 800.4a + CR 104.2a: on four seats the Zulaport drain drops P1 and P2 together at
/// the repetition both reach 0, and P3 at its own; a take stops at exactly what it declared. Each
/// repetition casts Gravecrawler from the graveyard (Walking Corpse is the Zombie) and sacrifices
/// it to Phyrexian Altar, and Zulaport Cutthroat makes each opponent lose 1: from [5, 5, 7] P1 and
/// P2 leave in repetition 5 of six, which the take's record elides but for their departures.
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
    let result = take_recorded(runner.state_mut(), 6);
    let events = &result.events;
    let six = runner.state();
    assert_eq!(eliminated(six), [P1, P2]);
    assert_eq!(six.players[3].life, 1);
    assert_eq!(six.waiting_for, WaitingFor::Priority { player: P0 });
    assert_eq!(eliminations(events), [P1, P2]);
    let logged = resolve_log_entries(events, &offered, six)
        .iter()
        .filter(|entry| {
            entry.segments.iter().any(
                |segment| matches!(segment, LogSegment::Text(text) if text == " is eliminated"),
            )
        })
        .count();
    assert_eq!(logged, 2);
    assert_eq!(markers(events), [(P0, 1), (P0, 6)]);
    let sixth = events
        .iter()
        .position(|event| {
            *event
                == GameEvent::ShortcutRepetition {
                    controller: P0,
                    repetition: 6,
                }
        })
        .expect("marker 6");
    for seat in [P1, P2] {
        for departure in [
            GameEvent::PlayerLost {
                player_id: seat,
                reasons: BTreeSet::from([LossReason::LifeTotal]),
            },
            GameEvent::PlayerEliminated { player_id: seat },
        ] {
            assert!(
                events[..sixth].contains(&departure),
                "{departure:?} before marker 6"
            );
        }
    }
    let line = |wanted: &str| {
        result
            .log_entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry
                    .segments
                    .iter()
                    .any(|segment| matches!(segment, LogSegment::Text(text) if text == wanted))
            })
            .map(|(index, entry)| (index, entry.segments.last().cloned()))
            .collect::<Vec<_>>()
    };
    let eliminated_lines = line(" is eliminated");
    let marker_lines = line(" repeats the loop — repetition ");
    assert_eq!(eliminated_lines.len(), 2);
    assert_eq!(
        marker_lines
            .iter()
            .map(|(_, last)| last.clone())
            .collect::<Vec<_>>(),
        [Some(LogSegment::Number(1)), Some(LogSegment::Number(6))]
    );
    assert!(eliminated_lines
        .iter()
        .all(|(index, _)| *index < marker_lines[1].0));

    let seven = taken(&offered, 7);
    assert_eq!(seven.waiting_for, WaitingFor::GameOver { winner: Some(P0) });

    assert_declaration_refused(&offered, 8);
}

/// CR 732.2c + CR 800.4a: a take whose record no longer predicts P1's departure performs the
/// repetitions before it and stops there, rather than commit a departure it did not predict.
#[test]
fn a_replay_take_stops_before_a_departure_its_signature_did_not_predict() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board_at("Zulaport Cutthroat", &[20, 7, 9], db);
    drain_to_offer(&mut runner, altar, gravecrawler, &[P1, P2]);
    bounded_recorded_offer(runner.state(), 8);
    let p2 = runner.state().players[2].life;
    let (recorded, state) =
        crate::loop_shortcut::take_with_edited_departures(runner.state(), |entries| {
            entries.retain(|entry| entry["seats"] != serde_json::json!([1]));
        });
    assert!(
        recorded.iter().any(|(_, seats)| *seats == [P1]),
        "reach: the record predicted P1's departure: {recorded:?}"
    );
    assert!(
        state.players[2].life < p2,
        "reach: the take performed repetitions"
    );
    assert!(eliminated(&state).is_empty());
    assert_eq!(lives(&state)[1..], [1, p2 - 5]);
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
    bounded_recorded_offer(&offered, l);
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
    charger_board_on(2, mountains, umbra, others, mode, db)
}

/// `charger_board` on `seats` seats, every opponent at 200 life.
fn charger_board_on(
    seats: u8,
    mountains: usize,
    umbra: bool,
    others: &[&str],
    mode: LoopDetectionMode,
    db: &CardDatabase,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(seats, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in (0..seats).map(PlayerId) {
        if seat != P0 {
            scenario.with_life(seat, 200);
        }
        scenario.with_library_top(seat, &["Island"; 25]);
    }
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
    let action = charger_action(runner.state(), charger, attacks);
    act(runner, action);
}

/// The Charger drive's next action at `state`.
fn charger_action(state: &GameState, charger: ObjectId, attacks: bool) -> GameAction {
    match &state.waiting_for {
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
    }
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
    let mode = LoopDetectionMode::Interactive;
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

/// CR 506.1 + CR 508.1a + CR 613.4c + CR 704.5a + CR 732.2a: beside Moraug, Fury of Akoum ("Each
/// creature you control gets +1/+0 for each time it has attacked this turn."), Hellkite Charger
/// wearing Bear Umbra deals one more damage each combat its period adds, so the period is offered
/// at the first count whose growing damage reaches P1's life, with P0 the predicted winner, and a
/// take of that count ends the game.
#[test]
fn a_hellkite_charger_period_beside_moraug_is_counted_over_its_growing_damage_and_taken() {
    let db = shared_card_db().expect("card db");
    let mode = LoopDetectionMode::Interactive;
    let (mut runner, charger) = charger_board(7, true, &["Moraug, Fury of Akoum"], mode, db);
    charger_until(
        &mut runner,
        charger,
        |_| true,
        |state| is_offer(state) || combats_begun(state) > 6,
    );
    let state = runner.state();
    let (span, count) = offer_of(state);
    assert_eq!(
        (span.reach, span.cause),
        (PeriodReach::Combat, NamingCause::TriggerTop),
        "{mode:?}"
    );
    // The Charger has attacked in each combat begun: 5 printed, 2 from Bear Umbra, and one
    // per attack from Moraug.
    let first = 7 + combats_begun(state) as i32;
    assert_eq!(state.objects[&charger].power, Some(first), "{mode:?}");
    let life = state.players[1].life;
    let n = (1..)
        .find(|&n: &i32| (0..n).map(|k| first + k).sum::<i32>() >= life)
        .expect("the damage reaches P1's life") as u32;
    assert_eq!(fixed(&count), n, "{mode:?}");
    let WaitingFor::LoopShortcut {
        predicted_winner, ..
    } = &state.waiting_for
    else {
        unreachable!("offer_of read the offer");
    };
    assert_eq!(*predicted_winner, Some(P0), "{mode:?}");

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

/// The Charger period beside Moraug on `seats` seats, driven to its first offer.
fn charger_moraug_offer(seats: u8, db: &CardDatabase) -> (GameRunner, ObjectId) {
    let (mut runner, charger) = charger_board_on(
        seats,
        7,
        true,
        &["Moraug, Fury of Akoum"],
        LoopDetectionMode::Interactive,
        db,
    );
    charger_until(
        &mut runner,
        charger,
        |_| true,
        |state| is_offer(state) || combats_begun(state) > 6,
    );
    (runner, charger)
}

/// CR 508.1b + CR 800.4a + CR 104.2a: on three seats, once P1 leaves, P2 is the one player
/// Hellkite Charger can attack, so its period beside Moraug is counted on past P1's departure. From
/// the second combat, repetition r deals 8 + r (CR 510.1a + CR 613.4c): P1 at 192 leaves at 13
/// and P2 at 200 at 21, ending the game.
#[test]
fn a_hellkite_charger_period_beside_moraug_on_three_seats_attacks_the_last_defender() {
    let db = shared_card_db().expect("card db");
    let (mut runner, charger) = charger_moraug_offer(3, db);
    let state = runner.state();
    assert_eq!(combats_begun(state), 2);
    assert_eq!(state.objects[&charger].power, Some(9));
    assert_eq!(lives(state), [20, 192, 200]);
    let WaitingFor::LoopShortcut {
        schema,
        predicted_winner,
        ..
    } = &state.waiting_for
    else {
        panic!("no offer at {:?}", state.waiting_for);
    };
    assert_eq!(
        (
            schema.iteration_count.clone(),
            schema.repetition_bound,
            *predicted_winner
        ),
        (
            IterationCount::Fixed(21),
            engine::analysis::decision_template::RepetitionBound::Measured,
            Some(P0)
        )
    );

    let events = take(&mut runner, 21);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    );
    assert_eq!(lives(runner.state()), [20, -3, -4]);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::GameOver { .. }))
            .count(),
        1
    );
}

/// CR 508.1b: on four seats, once P1 leaves, which of P2 and P3 the Charger attacks is a choice
/// its record does not hold, so the replay makes no attack on either.
#[test]
fn a_hellkite_charger_period_beside_moraug_on_four_seats_chooses_no_defender_for_its_proposer() {
    let db = shared_card_db().expect("card db");
    let (mut runner, charger) = charger_moraug_offer(4, db);
    let state = runner.state();
    assert_eq!(combats_begun(state), 2);
    assert_eq!(state.objects[&charger].power, Some(9));
    assert_eq!(lives(state), [20, 192, 200, 200]);
    let (_, count) = offer_of(state);

    take(&mut runner, fixed(&count));
    assert_eq!(eliminated(runner.state()), [P1]);
    assert_eq!(lives(runner.state())[2..], [200, 200]);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

/// CR 508.1a + CR 510.1a + CR 613.4c: Moraug, Fury of Akoum's "+1/+0 for each time it has
/// attacked this turn" counts the attack being declared (Scryfall ruling: "The first time a
/// creature attacks in a turn, the +1/+0 bonus will apply to the combat damage it deals during
/// that combat."), so Hellkite Charger wearing Bear Umbra deals 8, 9 and 10 in its first three
/// combats.
#[test]
fn moraug_counts_the_attack_being_declared_in_the_damage_it_deals() {
    let db = shared_card_db().expect("card db");
    let (mut runner, charger) = charger_board(
        7,
        true,
        &["Moraug, Fury of Akoum"],
        LoopDetectionMode::Off,
        db,
    );
    let mut lives = vec![runner.state().players[1].life];
    for combat in 2..=4 {
        charger_until(
            &mut runner,
            charger,
            |_| true,
            |state| combats_begun(state) == combat && charger_trigger_top(state),
        );
        lives.push(runner.state().players[1].life);
    }
    assert_eq!(lives, [200, 192, 183, 173]);
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

/// CR 601.3 + CR 702.170d + CR 732.2a: the history cover reads a casting-permission grant by its
/// permission. Ramirez DePietro, Pillager's "You may cast that card for as long as it remains
/// exiled." reads no history, so the Charger's frames and the Time Warp board's turn frames are
/// admitted beside it. Kellan Joins Up's "it becomes plotted" (CR 702.170c) reads the turn, which
/// the Charger's frames share and the turn frames grow.
#[test]
fn the_history_cover_reads_a_casting_permission_grant_by_its_permission() {
    let db = shared_card_db().expect("card db");
    for (other, turn_cover) in [
        (
            "Ramirez DePietro, Pillager",
            Some(RecurrenceCover::TurnCycle),
        ),
        ("Kellan Joins Up", None),
    ] {
        let [prior, current] = charger_frames(other, db);
        assert!(
            !loop_states_equal_modulo_resources(&prior, &current),
            "reach: the comparand refuses the grown history beside {other}"
        );
        assert!(history_covers_for_tests(&prior, &current), "{other}");
        assert_eq!(
            warp_verdict(&warp_frames(&[other], db)),
            ObjectGrowthVerdict::ResourceRecurrence(turn_cover),
            "{other}"
        );
    }
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
                    payment_mode: CastPaymentMode::Auto,
                },
                None if state.players[0].mana_pool.total() >= 5 => GameAction::ActivateAbility {
                    source_id: najeela,
                    ability_index: ability(state, najeela, false),
                    payment_mode: CastPaymentMode::Auto,
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
    let mode = LoopDetectionMode::Interactive;
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

/// CR 504.1 + CR 704.5b: one draw a repetition, so the caster survives one per card left.
fn library_term(state: &GameState) -> u32 {
    state.players[0].library.len() as u32
}

/// CR 500.7 + CR 732.2a: Time Warp on its caster, returned each turn by Archaeomancer through
/// Ghostly Flicker, is offered at the next turn's window for the carried extra-turn span, counted
/// to the cleanup that would first discard; a take performs that many extra turns and discards
/// nothing.
#[test]
fn an_archaeomancer_time_warp_period_is_offered_its_hand_bound_and_taken() {
    let db = shared_card_db().expect("card db");
    let mode = LoopDetectionMode::Interactive;
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

/// CR 504.1 + CR 704.5b: when the library binds, the offer stops at the caster's last card, because
/// a further repetition would lose the game by drawing from an empty library.
#[test]
fn an_archaeomancer_time_warp_take_stops_at_its_casters_last_card_when_the_library_binds() {
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
    let after = runner.state();
    assert!(eliminated(after).is_empty());
    assert!(after.players[0].library.is_empty());
    assert_eq!(
        after.players[0].turns_taken,
        state.players[0].turns_taken + n
    );
    assert!(!matches!(after.waiting_for, WaitingFor::GameOver { .. }));
    assert!(!events
        .iter()
        .any(|event| matches!(event, GameEvent::GameOver { .. })));
}

/// CR 504.1 + CR 704.5b: on three seats the offer also stops at the caster's last card, because a
/// further repetition would lose the game by drawing from an empty library.
#[test]
fn an_archaeomancer_time_warp_take_on_three_seats_stops_at_its_casters_last_card() {
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
    assert!(eliminated(after).is_empty());
    assert!(eliminations(&events).is_empty());
    assert!(after.players[0].library.is_empty());
    assert_eq!(
        after.players[0].turns_taken,
        state.players[0].turns_taken + n
    );
    assert!(!matches!(after.waiting_for, WaitingFor::GameOver { .. }));
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

/// How many cards named `name` P0 holds.
fn in_hand(state: &GameState, name: &str) -> u32 {
    state.players[0]
        .hand
        .iter()
        .filter(|id| state.objects[id].name == name)
        .count() as u32
}

/// CR 514.1 + CR 704.5b + CR 732.2a: at every window of both libraries' drives an offer suggests
/// the lesser of the hand and library terms, and stands wherever that is at least one: the first
/// period the confirmer replays ends before a discard and a draw from an empty library.
/// Once the span records a cleanup discard the hand term is the Islands in hand, since a further
/// discard would name a card still face down in the library (CR 401.2).
#[test]
fn every_archaeomancer_window_is_offered_the_lesser_of_its_hand_and_library_terms() {
    let db = shared_card_db().expect("card db");
    let mut discard_windows = Vec::new();
    for library in [25, 4] {
        let windows = warp_windows(library, 8, db);
        let offered = windows.iter().filter(|(_, count)| count.is_some()).count();
        assert!(offered >= 2, "reach: {library} offers more than once");
        for (state, count) in &windows[1..] {
            // An Island reaches P0's graveyard on this board only by a cleanup discard.
            let discarded = state.players[0]
                .graveyard
                .iter()
                .any(|id| state.objects[id].name == "Island");
            let known = if discarded {
                discard_windows.push((in_hand(state, "Island"), library_term(state), *count));
                in_hand(state, "Island")
            } else {
                hand_term(state)
            };
            let replayable = known.min(library_term(state));
            assert_eq!(
                *count,
                (replayable >= 1).then_some(replayable),
                "{library} cards, turn {}",
                state.turn_number
            );
        }
    }
    // The first window after a discard holds Time Warp, Ghostly Flicker and six Islands, with 18
    // cards left in the library.
    assert_eq!(discard_windows.first(), Some(&(6, 18, Some(6))));
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

/// The Altar + Gravecrawler + Genesis Chamber board in `mode` with Mayhem Devil ("Whenever a
/// player sacrifices a permanent, this creature deals 1 damage to any target.") and `damaged`
/// under `controller`, driven with the Devil aimed at `damaged` until its period is offered.
fn altar_devil_offered(
    mode: LoopDetectionMode,
    controller: PlayerId,
    damaged: &str,
    db: &CardDatabase,
) -> (GameRunner, ObjectId) {
    let (mut runner, altar, gravecrawler) = altar_board(None, db);
    runner.state_mut().loop_detection = mode;
    place(runner.state_mut(), P0, "Genesis Chamber", db);
    place(runner.state_mut(), P0, "Mayhem Devil", db);
    let damaged = place(runner.state_mut(), controller, damaged, db);
    let aims = |action: &GameAction| {
        i32::from(matches!(
            action,
            GameAction::ChooseTarget { target: Some(TargetRef::Object(id)) } if *id == damaged
        ))
    };
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action)
            + chooses_color(ManaType::Black)(action)
            + 8 * aims(action)
    };
    for _ in 0..4 {
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
    assert!(
        is_offer(runner.state()),
        "{mode:?}: no offer with the Devil aimed at {}",
        runner.state().objects[&damaged].name
    );
    (runner, damaged)
}

/// CR 702.12b + CR 120.3e: the damage Mayhem Devil marks each cycle on the proposer's own
/// Darksteel Myr (indestructible) never destroys it, so the period is offered, and its take
/// performs the cycles, marking the damage with the Myr tokens it makes.
#[test]
fn a_period_damaging_an_indestructible_permanent_is_offered_and_performed() {
    let db = shared_card_db().expect("card db");
    let mode = LoopDetectionMode::Interactive;
    let (mut runner, myr) = altar_devil_offered(mode, P0, "Darksteel Myr", db);
    let state = runner.state();
    assert!(state.objects[&myr].has_keyword(&Keyword::Indestructible));
    let (damage, made) = (state.objects[&myr].damage_marked, tokens(state, P0).len());
    assert!(damage > 0, "{mode:?}: the hand-driven cycle marked damage");
    take(&mut runner, 3);
    let state = runner.state();
    assert_eq!(state.objects[&myr].damage_marked, damage + 3, "{mode:?}");
    assert_eq!(tokens(state, P0).len(), made + 3, "{mode:?}");
    assert!(!marks_tokens(state, P0), "{mode:?}");
}

/// CR 120.3e + CR 120.6: the damage Mayhem Devil marks each cycle on an opponent's Steel Wall
/// (0/4) is delivered by the take with the Myr tokens, short of the wall's toughness.
#[test]
fn a_period_damaging_an_opponents_permanent_is_performed() {
    let db = shared_card_db().expect("card db");
    let mode = LoopDetectionMode::Interactive;
    let (mut runner, wall) = altar_devil_offered(mode, P1, "Steel Wall", db);
    let state = runner.state();
    let made = tokens(state, P0).len();
    assert_eq!(state.objects[&wall].damage_marked, 1, "{mode:?}");
    take(&mut runner, 2);
    let state = runner.state();
    assert_eq!(state.objects[&wall].damage_marked, 3, "{mode:?}");
    assert_eq!(state.objects[&wall].zone, Zone::Battlefield, "{mode:?}");
    assert_eq!(tokens(state, P0).len(), made + 2, "{mode:?}");
}

fn treasures(state: &GameState) -> usize {
    tokens(state, P0)
        .into_iter()
        .filter(|id| state.objects[id].name == "Treasure")
        .count()
}

/// [`charger_board`] with no Mountains, Old Gnawbone ("Whenever a creature you control deals
/// combat damage to a player, create that many Treasure tokens.") able to attack, and seven
/// Treasures; the Charger and Old Gnawbone.
fn charger_gnawbone_board(
    umbra: bool,
    mode: LoopDetectionMode,
    db: &CardDatabase,
) -> (GameRunner, ObjectId, ObjectId) {
    let (mut runner, charger) =
        charger_board(0, umbra, &["Old Gnawbone"], LoopDetectionMode::Off, db);
    let state = runner.state_mut();
    let gnawbone = state
        .battlefield
        .iter()
        .copied()
        .find(|id| state.objects[id].name == "Old Gnawbone")
        .expect("Old Gnawbone");
    let before = state.turn_number.saturating_sub(1);
    let object = state.objects.get_mut(&gnawbone).expect("Old Gnawbone");
    object.entered_battlefield_turn = Some(before);
    object.summoning_sick = false;
    crate::loop_period_remade_token_source::make_treasures(state, P0, 7);
    state.loop_detection = mode;
    (runner, charger, gnawbone)
}

/// CR 500.8 + CR 111.7: Hellkite Charger wearing Bear Umbra deals 7, so Old Gnawbone makes again
/// the seven Treasures that paid its trigger, and the carried combat span is offered at the next
/// combat's trigger, counted to P1's lethal crossing, and taken.
#[test]
fn a_hellkite_charger_period_paid_by_remade_treasures_is_offered_and_taken() {
    let db = shared_card_db().expect("card db");
    let mode = LoopDetectionMode::Interactive;
    let (mut runner, charger, _) = charger_gnawbone_board(true, mode, db);
    charger_until(
        &mut runner,
        charger,
        |_| true,
        |state| is_offer(state) || combats_begun(state) > 6,
    );
    let state = runner.state();
    let (span, count) = offer_of(state);
    assert_eq!(
        (span.source, span.reach),
        (SpanSource::Carried(0), PeriodReach::Combat),
        "{mode:?}"
    );
    assert_eq!(combats_begun(state), 2, "{mode:?}");
    let life = state.players[1].life;
    assert_eq!(fixed(&count), ((life + 6) / 7) as u32, "{mode:?}");
    assert_eq!(treasures(state), 7, "{mode:?}");

    take(&mut runner, 3);
    let state = runner.state();
    assert_eq!(state.players[1].life, life - 21, "{mode:?}");
    assert_eq!(treasures(state), 7, "{mode:?}");
    assert_eq!(combats_begun(state), 5, "{mode:?}");
}

/// CR 602.2 + CR 732.2a: without Bear Umbra and with Old Gnawbone attacking beside it, each combat
/// makes more Treasures than it spends. Set apart from the Treasures it makes again, the ones it
/// nets are never activated in the period and the combat history it grows is read by no live
/// surface, so the carried combat span is offered.
#[test]
fn a_carried_period_that_nets_treasures_is_offered() {
    let db = shared_card_db().expect("card db");
    let (mut runner, charger, gnawbone) =
        charger_gnawbone_board(false, LoopDetectionMode::Interactive, db);
    for _ in 0..4000 {
        let state = runner.state();
        if is_offer(state) || combats_begun(state) > 5 {
            break;
        }
        match &state.waiting_for {
            WaitingFor::DeclareAttackers { player, .. } if *player == P0 => act(
                &mut runner,
                GameAction::DeclareAttackers {
                    attacks: [charger, gnawbone]
                        .map(|attacker| (attacker, AttackTarget::Player(P1)))
                        .to_vec(),
                    bands: vec![],
                },
            ),
            _ => charger_step(&mut runner, charger, true),
        }
    }
    let state = runner.state();
    assert!(is_offer(state), "{:?}", carried_verdicts(state));
    assert_eq!(
        (combats_begun(state), treasures(state)),
        (2, 12),
        "reach: offered in the second combat"
    );
}

/// The mana Aggravated Assault's {3}{R}{R} costs.
const ASSAULT_COST: usize = 5;

/// How the Aggravated Assault drive pays {3}{R}{R}.
#[derive(Clone, Copy, Debug, PartialEq)]
enum AssaultPay {
    /// The engine pays at the mana payment.
    Engine,
    /// Mana sources are activated one at a time until the pool pays.
    Hand,
}

/// What stands beside Aggravated Assault on P0's battlefield; `aura` is on `attacker`.
#[derive(Clone, Copy, Debug)]
struct AssaultBoard<'a> {
    others: &'a [&'a str],
    attacker: &'a str,
    aura: Option<&'a str>,
    mountains: usize,
    treasures: u32,
}

/// The Aggravated Assault drive: what it plays with, and the activations it has made.
#[derive(Clone, Copy, Debug)]
struct AssaultDrive {
    assault: ObjectId,
    attacker: ObjectId,
    pay: AssaultPay,
    /// The combats begun when Aggravated Assault was last activated.
    activated_at: Option<u32>,
    activations: u32,
}

/// P0 at `phase` with Aggravated Assault ("{3}{R}{R}: Untap all creatures you control. After this
/// main phase, there is an additional combat phase followed by an additional main phase. Activate
/// only as a sorcery.") and `board`, each permanent there since before the turn; P1 at 200 life
/// with no creatures. Detection is off until the board stands.
fn assault_board(
    board: AssaultBoard,
    phase: Phase,
    pay: AssaultPay,
    mode: LoopDetectionMode,
    db: &CardDatabase,
) -> (GameRunner, AssaultDrive) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    scenario.with_life(P1, 200);
    scenario.with_library_top(P0, &["Island"; 25]);
    scenario.with_library_top(P1, &["Island"; 25]);
    let assault = scenario.add_real_card(P0, "Aggravated Assault", Zone::Battlefield, db);
    let mut permanents: Vec<ObjectId> = board
        .others
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Battlefield, db))
        .collect();
    let attacker = scenario.add_real_card(P0, board.attacker, Zone::Battlefield, db);
    permanents.push(attacker);
    for _ in 0..board.mountains {
        scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    let state = runner.state_mut();
    let before = state.turn_number.saturating_sub(1);
    for id in permanents {
        let object = state.objects.get_mut(&id).expect("placed");
        object.entered_battlefield_turn = Some(before);
        object.summoning_sick = false;
    }
    if let Some(aura) = board.aura {
        let aura = place(state, P0, aura, db);
        attach_to(state, aura, attacker);
    }
    if board.treasures > 0 {
        crate::loop_period_remade_token_source::make_treasures(state, P0, board.treasures);
    }
    state.loop_detection = mode;
    let drive = AssaultDrive {
        assault,
        attacker,
        pay,
        activated_at: None,
        activations: 0,
    };
    (runner, drive)
}

/// P0's priority in a main phase of their own turn with an empty stack.
fn assault_window(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
        && state.active_player == P0
        && matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
        && state.stack.is_empty()
}

fn untapped_mana_sources(state: &GameState) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &state.objects[id];
            object.controller == P0
                && !object.tapped
                && object.abilities.iter().any(is_mana_ability)
        })
        .collect()
}

/// One action of Aggravated Assault's period: activate it once per main phase when P0's untapped
/// mana sources and pool cover its cost, attack P1 with the attacker whenever it is untapped,
/// take every mana as red, keep trigger order, and pass every other priority.
fn assault_step(runner: &mut GameRunner, drive: &mut AssaultDrive) {
    let action = assault_action(runner.state(), drive);
    act(runner, action);
}

/// The Assault drive's next action at `state`.
fn assault_action(state: &GameState, drive: &mut AssaultDrive) -> GameAction {
    match &state.waiting_for {
        WaitingFor::Priority { .. } if assault_window(state) => {
            let combats = combats_begun(state);
            let sources = untapped_mana_sources(state);
            let pool = state.players[0].mana_pool.total();
            if drive.activated_at == Some(combats) || sources.len() + pool < ASSAULT_COST {
                GameAction::PassPriority
            } else if drive.pay == AssaultPay::Hand && pool < ASSAULT_COST {
                GameAction::ActivateAbility {
                    source_id: sources[0],
                    ability_index: ability(state, sources[0], true),
                    payment_mode: CastPaymentMode::Auto,
                }
            } else {
                drive.activated_at = Some(combats);
                drive.activations += 1;
                GameAction::ActivateAbility {
                    source_id: drive.assault,
                    ability_index: ability(state, drive.assault, false),
                    payment_mode: CastPaymentMode::Auto,
                }
            }
        }
        WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
        WaitingFor::ChooseManaColor { .. } => GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Red),
            count: 1,
        },
        WaitingFor::DeclareAttackers { player, .. }
            if *player == P0 && !state.objects[&drive.attacker].tapped =>
        {
            GameAction::DeclareAttackers {
                attacks: vec![(drive.attacker, AttackTarget::Player(P1))],
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
        WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
            order: (0..triggers.len()).collect(),
        },
        other => panic!("the Assault drive has no answer to {other:?}"),
    }
}

/// Drives Aggravated Assault's period until `until` holds.
fn assault_until(
    runner: &mut GameRunner,
    drive: &mut AssaultDrive,
    until: impl Fn(&GameState) -> bool,
) {
    for _ in 0..4000 {
        if until(runner.state()) {
            return;
        }
        assault_step(runner, drive);
    }
    panic!("the Assault drive did not reach its stop");
}

/// What a cycle of Aggravated Assault's period moves or leaves as it found it: P1's life, P0's
/// Treasures, the phase, the combats begun, P0's pool, the stack, and the tapped permanents.
type AssaultFrame = (i32, usize, Phase, u32, usize, usize, Vec<String>);

fn assault_frame(state: &GameState) -> AssaultFrame {
    let mut tapped: Vec<String> = state
        .battlefield
        .iter()
        .filter(|id| state.objects[*id].tapped)
        .map(|id| state.objects[id].name.clone())
        .collect();
    tapped.sort();
    (
        state.players[1].life,
        treasures(state),
        state.phase,
        combats_begun(state),
        state.players[0].mana_pool.total(),
        state.stack.len(),
        tapped,
    )
}

/// The offer standing at `state` is the carried combat span of a play legal again, on the
/// recorded road; the count it suggests.
fn carried_combat_offer(state: &GameState, case: &str) -> u32 {
    let (span, count) = offer_of(state);
    assert_eq!(
        (span.source, span.reach, span.cause),
        (
            SpanSource::Carried(0),
            PeriodReach::Combat,
            NamingCause::LegalNow
        ),
        "{case}"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }),
        "{case}"
    );
    fixed(&count)
}

/// CR 500.8 + CR 120.2a + CR 732.2a: in the combat Aggravated Assault adds, Spined Wurm deals 5
/// combat damage and Old Gnawbone ("Whenever a creature you control deals combat damage to a
/// player, create that many Treasure tokens.") makes again the five Treasures ("{T}, Sacrifice this
/// token: Add one mana of any color.") the activation spent. The carried span is offered once the
/// cycle has come round, counted to P1's lethal crossing (CR 704.5a), and a take leaves what its
/// cycles driven by hand leave.
#[test]
fn an_aggravated_assault_period_paid_by_the_treasures_its_combat_makes_is_offered_and_taken() {
    let db = shared_card_db().expect("card db");
    let board = AssaultBoard {
        others: &["Old Gnawbone"],
        attacker: "Spined Wurm",
        aura: None,
        mountains: 0,
        treasures: 5,
    };
    let mode = LoopDetectionMode::Interactive;
    for pay in [AssaultPay::Engine, AssaultPay::Hand] {
        // CR 505.1a: every added main phase is a postcombat main, so a period begun in the
        // precombat main first comes round to the phase it resumes in one cycle later.
        for (phase, combats) in [(Phase::PreCombatMain, 2), (Phase::PostCombatMain, 1)] {
            let case = format!("{mode:?}, {pay:?}, from {phase:?}");
            let (mut runner, mut drive) = assault_board(board, phase, pay, mode, db);
            assault_until(&mut runner, &mut drive, |state| {
                is_offer(state) || combats_begun(state) > 6
            });
            let state = runner.state().clone();
            let n = carried_combat_offer(&state, &case);
            let life = state.players[1].life;
            assert_eq!(
                assault_frame(&state),
                (
                    life,
                    5,
                    Phase::PostCombatMain,
                    combats,
                    0,
                    0,
                    vec!["Spined Wurm".to_string()]
                ),
                "{case}"
            );
            assert_eq!(n, ((life + 4) / 5) as u32, "{case}");

            let cycles = assault_frame(&taken(&state, 3));
            assert_eq!(
                cycles,
                (
                    life - 15,
                    5,
                    Phase::PostCombatMain,
                    combats + 3,
                    0,
                    0,
                    vec!["Spined Wurm".to_string()]
                ),
                "{case}"
            );
            let mut by_hand = GameRunner::from_state(state);
            act(&mut by_hand, GameAction::DeclineShortcut);
            by_hand.state_mut().loop_detection = LoopDetectionMode::Off;
            let mut hand_drive = AssaultDrive {
                activated_at: None,
                ..drive
            };
            assault_until(&mut by_hand, &mut hand_drive, |state| {
                combats_begun(state) == combats + 3 && assault_window(state)
            });
            assert_eq!(cycles, assault_frame(by_hand.state()), "{case}");

            let events = take(&mut runner, n);
            assert_eq!(
                runner.state().waiting_for,
                WaitingFor::GameOver { winner: Some(P0) },
                "{case}"
            );
            assert!(
                events.contains(&GameEvent::GameOver { winner: Some(P0) }),
                "{case}"
            );
        }
    }
}

/// CR 500.8 + CR 701.26b + CR 732.2a: Aggravated Assault paid by a population untapped each
/// period — five Mountains by Bear Umbra's granted trigger ("Whenever this creature attacks, untap
/// all lands you control."), or five Birds of Paradise ("{T}: Add one mana of any color.") by
/// Aggravated Assault itself — is offered at its second combat, counted to P1's lethal crossing
/// (CR 704.5a), and a take ends the game.
#[test]
fn an_aggravated_assault_period_paid_by_a_population_it_untaps_is_offered_and_taken() {
    let db = shared_card_db().expect("card db");
    let boards = [
        (
            AssaultBoard {
                others: &[],
                attacker: "Spined Wurm",
                aura: Some("Bear Umbra"),
                mountains: 5,
                treasures: 0,
            },
            7,
        ),
        (
            AssaultBoard {
                others: &["Birds of Paradise"; 5],
                attacker: "Spined Wurm",
                aura: None,
                mountains: 0,
                treasures: 0,
            },
            5,
        ),
    ];
    for (board, power) in boards {
        let mode = LoopDetectionMode::Interactive;
        for pay in [AssaultPay::Engine, AssaultPay::Hand] {
            let case = format!("{board:?}, {mode:?}, {pay:?}");
            let (mut runner, mut drive) = assault_board(board, Phase::PreCombatMain, pay, mode, db);
            assault_until(&mut runner, &mut drive, |state| {
                is_offer(state) || combats_begun(state) > 6
            });
            let state = runner.state();
            let n = carried_combat_offer(state, &case);
            assert_eq!(combats_begun(state), 2, "{case}");
            assert_eq!(state.objects[&drive.attacker].power, Some(power), "{case}");
            assert_eq!(
                n,
                ((state.players[1].life + power - 1) / power) as u32,
                "{case}"
            );

            let events = take(&mut runner, n);
            assert_eq!(
                runner.state().waiting_for,
                WaitingFor::GameOver { winner: Some(P0) },
                "{case}"
            );
            assert!(
                events.contains(&GameEvent::GameOver { winner: Some(P0) }),
                "{case}"
            );
        }
    }
}

/// Adds to `verdicts` the confirmer's verdicts on the carried spans at `state`, at the first
/// main-phase window of each combat that has any.
fn gather_carried_verdicts(
    state: &GameState,
    gathered_at: &mut u32,
    verdicts: &mut Vec<Result<Vec<String>, OfferRefusal>>,
) {
    if assault_window(state) && combats_begun(state) > *gathered_at {
        let at_window = carried_verdicts(state);
        if !at_window.is_empty() {
            *gathered_at = combats_begun(state);
            verdicts.extend(at_window);
        }
    }
}

/// A card of P0's that the period never uses: its name, its zone, and whether it stands tapped.
#[derive(Clone, Copy, Debug)]
struct Idle(&'static str, Zone, bool);

/// Puts `card` in its zone under P0, a permanent there since before the turn.
fn place_idle(state: &mut GameState, Idle(name, zone, tapped): Idle, db: &CardDatabase) {
    let before = state.turn_number.saturating_sub(1);
    let id = place(state, P0, name, db);
    if zone != Zone::Battlefield {
        remove_from_zone(state, id, Zone::Battlefield, P0);
        add_to_zone(state, id, zone, P0);
    }
    let object = state.objects.get_mut(&id).expect("placed");
    object.zone = zone;
    object.tapped = tapped;
    object.entered_battlefield_turn = (zone == Zone::Battlefield).then_some(before);
    object.summoning_sick = false;
}

/// Where an offer stands and for how many cycles: the combats begun, the phase, P1's life and
/// the count.
type OfferMoment = (u32, Phase, i32, u32);

/// The carried span offered on the recorded road at `state`, and its moment.
fn carried_offer(state: &GameState, case: &str) -> (NamedSpan, OfferMoment) {
    assert!(
        is_offer(state),
        "{case}: no offer, carrying {:?}",
        carried(state)
    );
    let (span, count) = offer_of(state);
    assert!(
        matches!(span.source, SpanSource::Carried(_)),
        "{case}: {span:?}"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }),
        "{case}"
    );
    let moment = (
        combats_begun(state),
        state.phase,
        state.players[1].life,
        fixed(&count),
    );
    (span, moment)
}

/// Old Gnawbone makes again the five Treasures a Spined Wurm's combat damage pays for.
const GNAWBONE_BOARD: AssaultBoard<'static> = AssaultBoard {
    others: &["Old Gnawbone"],
    attacker: "Spined Wurm",
    aura: None,
    mountains: 0,
    treasures: 5,
};

/// Bear Umbra on the attacking Spined Wurm untaps the five Mountains that pay.
const UMBRA_BOARD: AssaultBoard<'static> = AssaultBoard {
    others: &[],
    attacker: "Spined Wurm",
    aura: Some("Bear Umbra"),
    mountains: 5,
    treasures: 0,
};

/// `board` beside `idle`, driven from P0's precombat main until an offer stands, the turn ends or
/// a seventh combat begins.
fn assault_beside(
    board: AssaultBoard,
    pay: AssaultPay,
    idle: Option<Idle>,
    mode: LoopDetectionMode,
    db: &CardDatabase,
) -> (GameRunner, AssaultDrive) {
    let (mut runner, mut drive) = assault_board(board, Phase::PreCombatMain, pay, mode, db);
    if let Some(idle) = idle {
        place_idle(runner.state_mut(), idle, db);
    }
    assault_until(&mut runner, &mut drive, |state| {
        is_offer(state) || state.active_player != P0 || combats_begun(state) > 6
    });
    (runner, drive)
}

/// The top-level serialized fields in which a take of `cycles` from the offer at `offered`
/// differs from that many cycles driven by hand once the offer is declined.
fn assault_take_differs(
    offered: &GameState,
    drive: AssaultDrive,
    cycles: u32,
) -> std::collections::BTreeSet<String> {
    let combats = combats_begun(offered);
    let taken = taken(offered, cycles);
    let mut by_hand = GameRunner::from_state(offered.clone());
    act(&mut by_hand, GameAction::DeclineShortcut);
    by_hand.state_mut().loop_detection = LoopDetectionMode::Off;
    let mut hand_drive = AssaultDrive {
        activated_at: None,
        ..drive
    };
    assault_until(&mut by_hand, &mut hand_drive, |state| {
        combats_begun(state) == combats + cycles && assault_window(state)
    });
    crate::loop_period_remade_token_source::differing_fields(&taken, by_hand.state())
}

/// CR 732.2a: an Aggravated Assault period beside a card it never uses is offered where, and for
/// as many cycles, as the same board without the card, counted to P1's lethal crossing
/// (CR 704.5a), and an engine-paid take leaves what its cycles driven by hand leave. The cards: a
/// tapped Mana Confluence or Forgotten Monument; Kain, Traitorous Dragoon, Cecil, Dark Knight and
/// Namazu Trader, which neither attack nor enter; Carnival of Souls and Davvol, Evincar of Rath,
/// with no creature entering; Ramirez DePietro, Pillager, with no Pirate dealing damage; and an
/// instant or sorcery left in a hand or a graveyard. Time Warp held beside Mountains reaches a
/// turn (CR 500.7), since an added turn would untap them.
#[test]
fn a_period_beside_cards_it_never_uses_is_offered_as_it_is_without_them() {
    let db = shared_card_db().expect("card db");
    let untapped = |name| Idle(name, Zone::Battlefield, false);
    let tapped = |name| Idle(name, Zone::Battlefield, true);
    let in_hand = |name| Idle(name, Zone::Hand, false);
    let in_graveyard = |name| Idle(name, Zone::Graveyard, false);
    let (combat, turn) = (PeriodReach::Combat, PeriodReach::ExtraTurn);
    let groups = [
        (
            GNAWBONE_BOARD,
            AssaultPay::Engine,
            vec![
                (tapped("Mana Confluence"), combat),
                (untapped("Kain, Traitorous Dragoon"), combat),
                (untapped("Carnival of Souls"), combat),
                (untapped("Davvol, Evincar of Rath"), combat),
                (untapped("Namazu Trader"), combat),
                (tapped("Forgotten Monument"), combat),
                (in_graveyard("World at War"), combat),
                (in_hand("Jeska's Will"), combat),
                (in_hand("Time Warp"), combat),
                (untapped("Ramirez DePietro, Pillager"), combat),
            ],
        ),
        (
            GNAWBONE_BOARD,
            AssaultPay::Hand,
            vec![(tapped("Mana Confluence"), combat)],
        ),
        (
            UMBRA_BOARD,
            AssaultPay::Engine,
            vec![
                (tapped("Mana Confluence"), combat),
                (untapped("Cecil, Dark Knight"), combat),
                (untapped("Kain, Traitorous Dragoon"), combat),
                (in_graveyard("Act of Treason"), combat),
                (in_hand("Time Warp"), turn),
            ],
        ),
    ];
    let mode = LoopDetectionMode::Interactive;
    for (board, pay, beside) in &groups {
        let control = format!("{mode:?}, {pay:?}, {board:?}");
        let (runner, drive) = assault_beside(*board, *pay, None, mode, db);
        let state = runner.state();
        let (_, without @ (_, _, life, count)) = carried_offer(state, &control);
        let power = state.objects[&drive.attacker]
            .power
            .expect("the attacker's power");
        assert_eq!(count, ((life + power - 1) / power) as u32, "{control}");
        for &(idle, reach) in beside {
            let case = format!("{control} beside {idle:?}");
            let (runner, drive) = assault_beside(*board, *pay, Some(idle), mode, db);
            let state = runner.state();
            let (span, moment) = carried_offer(state, &case);
            assert_eq!(moment, without, "{case}");
            assert_eq!(
                (span.reach, span.cause),
                (reach, NamingCause::LegalNow),
                "{case}"
            );
            assert_eq!(state.players[0].life, 20, "{case}");
            // A hand-paid take leaves other activation records than its cycles driven by hand.
            if *pay == AssaultPay::Engine {
                let differs = assault_take_differs(state, drive, 3);
                assert!(differs.is_empty(), "{case}: {differs:?}");
            }
        }
    }
}

/// CR 500.8 + CR 732.2a: Hellkite Charger wearing Bear Umbra with seven Mountains, beside World
/// at War in the graveyard, an Aggravated Assault never activated, a tapped Mana Confluence or
/// Time Warp in hand, is offered where, and for as many cycles, as without the card, counted to
/// P1's lethal crossing (CR 704.5a), and a take leaves what its cycles driven by hand leave.
#[test]
fn a_hellkite_charger_period_beside_cards_it_never_uses_is_offered_as_it_is_without_them() {
    let db = shared_card_db().expect("card db");
    let offered = |idle: Option<Idle>, mode| {
        let (mut runner, charger) = charger_board(7, true, &[], mode, db);
        if let Some(idle) = idle {
            place_idle(runner.state_mut(), idle, db);
        }
        charger_until(
            &mut runner,
            charger,
            |_| true,
            |state| is_offer(state) || state.active_player != P0 || combats_begun(state) > 6,
        );
        (runner, charger)
    };
    let mode = LoopDetectionMode::Interactive;
    let control = format!("{mode:?}");
    let (runner, charger) = offered(None, mode);
    let state = runner.state();
    let (_, without @ (_, _, life, count)) = carried_offer(state, &control);
    let power = state.objects[&charger].power.expect("the Charger's power");
    assert_eq!(count, ((life + power - 1) / power) as u32, "{control}");
    for idle in [
        Idle("World at War", Zone::Graveyard, false),
        Idle("Aggravated Assault", Zone::Battlefield, false),
        Idle("Mana Confluence", Zone::Battlefield, true),
        Idle("Time Warp", Zone::Hand, false),
    ] {
        let case = format!("{mode:?} beside {idle:?}");
        let (runner, charger) = offered(Some(idle), mode);
        let state = runner.state();
        let (span, moment) = carried_offer(state, &case);
        assert_eq!(moment, without, "{case}");
        assert_eq!(
            (span.reach, span.cause),
            (PeriodReach::Combat, NamingCause::TriggerTop),
            "{case}"
        );
        assert_eq!(state.players[0].life, 20, "{case}");

        let combats = combats_begun(state);
        let taken = taken(state, 3);
        let mut by_hand = GameRunner::from_state(state.clone());
        act(&mut by_hand, GameAction::DeclineShortcut);
        by_hand.state_mut().loop_detection = LoopDetectionMode::Off;
        charger_until(
            &mut by_hand,
            charger,
            |_| true,
            |state| combats_begun(state) == combats + 3 && charger_trigger_top(state),
        );
        let differs =
            crate::loop_period_remade_token_source::differing_fields(&taken, by_hand.state());
        assert!(differs.is_empty(), "{case}: {differs:?}");
    }
}

/// CR 119.4 + CR 119.3 + CR 732.2a: Aggravated Assault paid by four Mountains and Mana Confluence
/// ("{T}, Pay 1 life: Add one mana of any color.") beside Sinister Monolith ("At the beginning of
/// combat on your turn, each opponent loses 1 life and you gain 1 life."): each cycle P0 pays 1
/// life and gains 1, and P1 loses the attacker's combat damage and 1 more. The period is offered
/// at its second combat, counted to P1's lethal crossing (CR 704.5a), and a take leaves what its
/// cycles driven by hand leave.
#[test]
fn a_period_paying_life_its_own_trigger_regains_is_offered_and_taken() {
    let db = shared_card_db().expect("card db");
    let board = AssaultBoard {
        others: &["Mana Confluence", "Sinister Monolith"],
        attacker: "Spined Wurm",
        aura: Some("Bear Umbra"),
        mountains: 4,
        treasures: 0,
    };
    let mode = LoopDetectionMode::Interactive;
    let case = format!("{mode:?}");
    let (runner, drive) = assault_beside(board, AssaultPay::Engine, None, mode, db);
    let state = runner.state();
    let (span, (combats, phase, life, count)) = carried_offer(state, &case);
    assert_eq!(
        (combats, phase, span.reach, span.cause),
        (
            2,
            Phase::PostCombatMain,
            PeriodReach::Combat,
            NamingCause::LegalNow
        ),
        "{case}"
    );
    assert_eq!(state.players[0].life, 20, "{case}");
    let lost_each_cycle = state.objects[&drive.attacker]
        .power
        .expect("the attacker's power")
        + 1;
    assert_eq!(
        count,
        ((life + lost_each_cycle - 1) / lost_each_cycle) as u32,
        "{case}"
    );
    let differs = assault_take_differs(state, drive, 3);
    assert!(differs.is_empty(), "{case}: {differs:?}");
}

/// CR 505.1a + CR 732.2a: Neheb, the Eternal ("At the beginning of each of your postcombat main
/// phases, add {R} for each 1 life your opponents have lost this turn.") attacking beside five
/// Mountains, with Aggravated Assault first activated in the first postcombat main, is offered at
/// the second combat's postcombat main, counted to P1's lethal crossing (CR 704.5a), with or
/// without a Hill Giant that stays home, and a take leaves what its cycles driven by hand leave.
#[test]
fn an_aggravated_assault_period_paid_by_neheb_and_lands_is_offered_and_taken() {
    let db = shared_card_db().expect("card db");
    for others in [&[][..], &["Hill Giant"][..]] {
        let board = AssaultBoard {
            others,
            attacker: "Neheb, the Eternal",
            aura: None,
            mountains: 5,
            treasures: 0,
        };
        let mode = LoopDetectionMode::Interactive;
        let case = format!("{others:?}, {mode:?}");
        let (mut runner, mut drive) =
            assault_board(board, Phase::PreCombatMain, AssaultPay::Engine, mode, db);
        // The precombat main passes without an activation.
        drive.activated_at = Some(0);
        assault_until(&mut runner, &mut drive, |state| {
            is_offer(state) || state.active_player != P0 || combats_begun(state) > 6
        });
        let state = runner.state();
        let (span, (combats, phase, life, count)) = carried_offer(state, &case);
        assert_eq!(
            (combats, phase, span.reach, span.cause),
            (
                2,
                Phase::PostCombatMain,
                PeriodReach::Combat,
                NamingCause::LegalNow
            ),
            "{case}"
        );
        let power = state.objects[&drive.attacker].power.expect("Neheb's power");
        assert_eq!(count, ((life + power - 1) / power) as u32, "{case}");
        let differs = assault_take_differs(state, drive, 2);
        assert!(differs.is_empty(), "{case}: {differs:?}");
    }
}

/// Whether an object of P0's outside its library has a mana ability: one it functions with, one
/// it is printed with, or one a static it is printed with grants.
fn mana_ability_outside_library(state: &GameState) -> bool {
    state.objects.values().any(|object| {
        (object.owner == P0 || object.controller == P0)
            && object.zone != Zone::Library
            && (object.abilities.iter().any(is_mana_ability)
                || object.base_abilities.iter().any(is_mana_ability)
                || object
                    .base_static_definitions
                    .iter()
                    .flat_map(|statik| &statik.modifications)
                    .any(|modification| {
                        matches!(
                            modification,
                            ContinuousModification::GrantAbility { definition }
                                if is_mana_ability(definition)
                        )
                    }))
    })
}

/// CR 505.1a + CR 500.8 + CR 732.2a: Aggravated Assault paid by Neheb, the Eternal alone ("At the
/// beginning of each of your postcombat main phases, add {R} for each 1 life your opponents have
/// lost this turn."), the main phase it adds being a postcombat main phase. A Spined Wurm attacks
/// beside Neheb, or Neheb attacks and one Treasure pays the fifth mana of the first activation;
/// at the offer nothing of P0's outside its library has a mana ability. The period is offered at
/// the second combat's postcombat main, counted to P1's lethal crossing (CR 704.5a); a take
/// leaves what its cycles driven by hand leave, and the whole count ends the game. A mana ability
/// held anywhere there would be seen: a Mountain's or Llanowar Elves' ("{T}: Add {G}."), also
/// under Frogify ("Enchanted creature loses all abilities …"); the one Cryptolith Rite grants
/// ("Creatures you control have "{T}: Add one mana of any color.""); and the one Urborg, Tomb of
/// Yawgmoth has on the battlefield for being a Swamp (CR 305.6).
#[test]
fn an_aggravated_assault_period_paid_by_neheb_alone_is_offered_and_taken() {
    let db = shared_card_db().expect("card db");
    let boards = [
        AssaultBoard {
            others: &["Neheb, the Eternal"],
            attacker: "Spined Wurm",
            aura: None,
            mountains: 0,
            treasures: 0,
        },
        AssaultBoard {
            others: &[],
            attacker: "Neheb, the Eternal",
            aura: None,
            mountains: 0,
            treasures: 1,
        },
    ];
    for board in boards {
        let mode = LoopDetectionMode::Interactive;
        for pay in [AssaultPay::Engine, AssaultPay::Hand] {
            let case = format!("{board:?}, {mode:?}, {pay:?}");
            let (mut runner, mut drive) = assault_board(board, Phase::PreCombatMain, pay, mode, db);
            assault_until(&mut runner, &mut drive, |state| {
                is_offer(state) || state.active_player != P0 || combats_begun(state) > 6
            });
            let state = runner.state();
            let (span, (combats, phase, life, count)) = carried_offer(state, &case);
            assert_eq!(
                (combats, phase, span.reach, span.cause),
                (
                    2,
                    Phase::PostCombatMain,
                    PeriodReach::Combat,
                    NamingCause::LegalNow
                ),
                "{case}"
            );
            assert!(!mana_ability_outside_library(state), "{case}");
            let power = state.objects[&drive.attacker].power.expect("a power");
            assert_eq!(count, ((life + power - 1) / power) as u32, "{case}");
            let differs = assault_take_differs(state, drive, 3);
            assert!(differs.is_empty(), "{case}: {differs:?}");

            let events = take(&mut runner, count);
            assert_eq!(
                runner.state().waiting_for,
                WaitingFor::GameOver { winner: Some(P0) },
                "{case}"
            );
            assert!(
                events.contains(&GameEvent::GameOver { winner: Some(P0) }),
                "{case}"
            );
        }
    }
    for (name, zones) in [
        ("Mountain", &[Zone::Hand][..]),
        (
            "Llanowar Elves",
            &[Zone::Hand, Zone::Graveyard, Zone::Exile][..],
        ),
        (
            "Cryptolith Rite",
            &[Zone::Battlefield, Zone::Hand, Zone::Graveyard][..],
        ),
        (RETRACE_LAND, &[Zone::Battlefield][..]),
    ] {
        for zone in zones {
            let idle = Some(Idle(name, *zone, false));
            let mode = LoopDetectionMode::Interactive;
            let (runner, _) = assault_beside(boards[0], AssaultPay::Engine, idle, mode, db);
            assert!(
                mana_ability_outside_library(runner.state()),
                "{name} in {zone:?}"
            );
        }
    }
    let (mut runner, mut drive) = assault_board(
        boards[0],
        Phase::PreCombatMain,
        AssaultPay::Engine,
        LoopDetectionMode::Interactive,
        db,
    );
    let state = runner.state_mut();
    place_idle(state, Idle("Llanowar Elves", Zone::Battlefield, false), db);
    let elves = in_zone(state, &state.battlefield, "Llanowar Elves").expect("placed");
    let frogify = place(state, P0, "Frogify", db);
    attach_to(state, frogify, elves);
    assault_until(&mut runner, &mut drive, |state| {
        is_offer(state) || state.active_player != P0 || combats_begun(state) > 6
    });
    let state = runner.state();
    assert!(
        !state.objects[&elves].abilities.iter().any(is_mana_ability),
        "reach: Frogify takes the Elves' ability"
    );
    assert!(mana_ability_outside_library(state), "Elves under Frogify");
}

/// The land a retraced Waves of Aggression discards. It has no printed mana ability and only
/// moves between hand and graveyard, so it never has the one a land on the battlefield has for
/// the Swamp type it gives (CR 305.6).
const RETRACE_LAND: &str = "Urborg, Tomb of Yawgmoth";

/// One action of a retraced Waves of Aggression period: cast it from P0's graveyard once per
/// main phase when the land is in hand and the pool holds its five mana, discard the land for it,
/// attack P1 with `attacker` whenever it is untapped, and pass every other priority.
fn waves_step(runner: &mut GameRunner, attacker: ObjectId, cast_at: &mut Option<u32>) {
    let state = runner.state();
    let action = match &state.waiting_for {
        WaitingFor::Priority { .. } if assault_window(state) => {
            let combats = combats_begun(state);
            let waves = in_zone(state, &state.players[0].graveyard, "Waves of Aggression");
            let land = in_zone(state, &state.players[0].hand, RETRACE_LAND);
            match (waves, land) {
                (Some(waves), Some(_))
                    if *cast_at != Some(combats)
                        && state.players[0].mana_pool.total() >= ASSAULT_COST =>
                {
                    *cast_at = Some(combats);
                    GameAction::CastSpell {
                        object_id: waves,
                        card_id: state.objects[&waves].card_id,
                        targets: vec![],
                        payment_mode: CastPaymentMode::Auto,
                    }
                }
                _ => GameAction::PassPriority,
            }
        }
        WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
        WaitingFor::PayCost { choices, .. } => GameAction::SelectCards {
            cards: choices.clone(),
        },
        WaitingFor::DeclareAttackers { player, .. }
            if *player == P0 && !state.objects[&attacker].tapped =>
        {
            GameAction::DeclareAttackers {
                attacks: vec![(attacker, AttackTarget::Player(P1))],
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
        WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
            order: (0..triggers.len()).collect(),
        },
        other => panic!("the Waves drive has no answer to {other:?}"),
    };
    act(runner, action);
}

/// CR 505.1a + CR 500.8 + CR 702.81a + CR 732.2a: Waves of Aggression ("Untap all creatures that
/// attacked this turn. After this main phase, there is an additional combat phase followed by an
/// additional main phase. Retrace") cast from the graveyard each postcombat main with the mana
/// Neheb, the Eternal adds there, discarding the land Kirri, Talented Sprout ("At the beginning of
/// each of your postcombat main phases, return target Plant, Treefolk, or land card from your
/// graveyard to your hand.") returns there, while a Spined Wurm attacks. Nothing of P0's outside
/// its library taps for mana. The period is offered at the second combat's postcombat main,
/// counted to P1's lethal crossing (CR 704.5a); a take leaves what its cycles driven by hand
/// leave, and the whole count ends the game. Without Kirri the land stays in the graveyard and
/// nothing is offered.
#[test]
fn a_retraced_waves_of_aggression_period_paid_by_neheb_is_offered_and_taken() {
    let db = shared_card_db().expect("card db");
    let boards = [
        (&["Kirri, Talented Sprout", "Neheb, the Eternal"][..], true),
        (&["Neheb, the Eternal"][..], false),
    ];
    for (others, recurs) in boards {
        let mode = LoopDetectionMode::Interactive;
        let case = format!("{others:?}, {mode:?}");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P1, 200);
        scenario.with_library_top(P0, &["Island"; 25]);
        scenario.with_library_top(P1, &["Island"; 25]);
        let mut permanents: Vec<ObjectId> = others
            .iter()
            .map(|name| scenario.add_real_card(P0, name, Zone::Battlefield, db))
            .collect();
        let attacker = scenario.add_real_card(P0, "Spined Wurm", Zone::Battlefield, db);
        permanents.push(attacker);
        scenario.add_real_card(P0, "Waves of Aggression", Zone::Graveyard, db);
        scenario.add_real_card(P0, RETRACE_LAND, Zone::Hand, db);
        let mut runner = scenario.build();
        let state = runner.state_mut();
        let before = state.turn_number.saturating_sub(1);
        for id in permanents {
            let object = state.objects.get_mut(&id).expect("placed");
            object.entered_battlefield_turn = Some(before);
            object.summoning_sick = false;
        }
        state.loop_detection = mode;
        let mut cast_at = None;
        let mut casts = 0;
        for _ in 0..4000 {
            let state = runner.state();
            if is_offer(state) || state.active_player != P0 || combats_begun(state) > 6 {
                break;
            }
            let before = cast_at;
            waves_step(&mut runner, attacker, &mut cast_at);
            casts += u32::from(cast_at != before);
        }
        let state = runner.state().clone();
        assert!(!mana_ability_outside_library(&state), "{case}");
        if !recurs {
            assert!(!is_offer(&state), "{case}");
            assert_eq!(casts, 1, "reach: {case} casts Waves of Aggression once");
            continue;
        }
        let (span, (combats, phase, life, count)) = carried_offer(&state, &case);
        assert_eq!(
            (combats, phase, span.reach, span.cause),
            (
                2,
                Phase::PostCombatMain,
                PeriodReach::Combat,
                NamingCause::LegalNow
            ),
            "{case}"
        );
        let power = state.objects[&attacker].power.expect("a power");
        assert_eq!(count, ((life + power - 1) / power) as u32, "{case}");

        let taken = taken(&state, 3);
        let mut by_hand = GameRunner::from_state(state.clone());
        act(&mut by_hand, GameAction::DeclineShortcut);
        by_hand.state_mut().loop_detection = LoopDetectionMode::Off;
        let mut hand_cast_at = None;
        for _ in 0..4000 {
            let state = by_hand.state();
            if combats_begun(state) == combats + 3
                && assault_window(state)
                && in_zone(state, &state.players[0].hand, RETRACE_LAND).is_some()
            {
                break;
            }
            waves_step(&mut by_hand, attacker, &mut hand_cast_at);
        }
        let differs =
            crate::loop_period_remade_token_source::differing_fields(&taken, by_hand.state());
        assert!(differs.is_empty(), "{case}: {differs:?}");

        let events = take(&mut runner, count);
        assert_eq!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P0) },
            "{case}"
        );
        assert!(
            events.contains(&GameEvent::GameOver { winner: Some(P0) }),
            "{case}"
        );
    }
}

/// Aggravated Assault, an attacking Spined Wurm and `others` on P0's battlefield with no land,
/// each Frontier Siege there with Khans chosen as it entered.
fn siege_board(
    others: &[&str],
    mode: LoopDetectionMode,
    db: &CardDatabase,
) -> (GameRunner, AssaultDrive) {
    let board = AssaultBoard {
        others,
        attacker: "Spined Wurm",
        aura: None,
        mountains: 0,
        treasures: 0,
    };
    let (mut runner, drive) =
        assault_board(board, Phase::PreCombatMain, AssaultPay::Hand, mode, db);
    for (_, object) in runner.state_mut().objects.iter_mut() {
        if object.name == "Frontier Siege" {
            object
                .chosen_attributes
                .push(ChosenAttribute::Label("Khans".to_string()));
        }
    }
    (runner, drive)
}

/// What an Aggravated Assault drive through P0's turn carried: the confirmer's verdicts on the
/// carried spans, and each carried key's face and reach.
type Carrying = (
    Vec<Result<Vec<String>, OfferRefusal>>,
    Vec<(Option<String>, PeriodReach)>,
);

/// Drives `drive` through P0's turn, which makes no offer.
fn carried_through_turn(runner: &mut GameRunner, drive: &mut AssaultDrive, case: &str) -> Carrying {
    let (mut verdicts, mut gathered_at, mut faces) = (Vec::new(), 0, Vec::new());
    for _ in 0..4000 {
        let state = runner.state();
        assert!(!is_offer(state), "{case}");
        if state.active_player != P0 || combats_begun(state) > 8 {
            break;
        }
        gather_carried_verdicts(state, &mut gathered_at, &mut verdicts);
        for span in carried(state) {
            let key = (span.face, span.reach);
            if !faces.contains(&key) {
                faces.push(key);
            }
        }
        assault_step(runner, drive);
    }
    assert_eq!(runner.state().active_player, P1, "{case}");
    verdicts.dedup();
    (verdicts, faces)
}

/// CR 505.1a + CR 500.8: what an added main phase does not feed is not carried, and what it
/// feeds is. Glóin the Mighty ("At the beginning of your first main phase, add {R}{R}.") beside
/// Aggravated Assault pays one activation with three Treasures, whoever taps them, and adds
/// nothing in the main phase that activation adds; the combat phase Najeela, the Blade-Blossom
/// would add ("{W}{U}{B}{R}{G}: … After this phase, there is an additional combat phase. Activate
/// only during combat.") brings no main phase for Neheb, the Eternal, while a Hill Giant attacks.
/// Neheb attacking alone beside Aggravated Assault adds four mana for its five: its trigger is
/// carried as far as a combat and the period never comes round; with Time Warp ("Target player
/// takes an extra turn after this one.") in hand it is carried as far as a turn (CR 500.7).
/// Frontier Siege with Khans chosen ("At the beginning of each of your main phases, add {G}{G}.")
/// beside Aggravated Assault adds two mana for its five: its trigger is carried as far as a
/// combat.
#[test]
fn a_trigger_an_added_main_phase_does_not_feed_is_not_carried() {
    let db = shared_card_db().expect("card db");
    let mode = LoopDetectionMode::Interactive;
    let board = AssaultBoard {
        others: &["Glóin the Mighty"],
        attacker: "Spined Wurm",
        aura: None,
        mountains: 0,
        treasures: 3,
    };
    for pay in [AssaultPay::Engine, AssaultPay::Hand] {
        let case = format!("Glóin, {mode:?}, {pay:?}");
        let (mut runner, mut drive) = assault_board(board, Phase::Upkeep, pay, mode, db);
        let (verdicts, faces) = carried_through_turn(&mut runner, &mut drive, &case);
        assert_eq!(
            drive.activations, 1,
            "reach: {case}: Glóin's mana pays once"
        );
        assert_eq!(runner.state().players[1].life, 195, "reach: {case}");
        assert_eq!((verdicts, faces), (vec![], vec![]), "{case}");
    }

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"; 25]);
    scenario.with_library_top(P1, &["Island"; 25]);
    let mut runner = scenario.build();
    let state = runner.state_mut();
    for name in [
        "Najeela, the Blade-Blossom",
        "Neheb, the Eternal",
        "Hill Giant",
    ] {
        place_idle(state, Idle(name, Zone::Battlefield, false), db);
    }
    state.loop_detection = mode;
    let on_battlefield = |name| in_zone(state, &state.battlefield, name).expect("placed");
    // Najeela's ability is never activated: Neheb adds three mana, and no Warrior attacks.
    let mut drive = AssaultDrive {
        assault: on_battlefield("Najeela, the Blade-Blossom"),
        attacker: on_battlefield("Hill Giant"),
        pay: AssaultPay::Engine,
        activated_at: None,
        activations: 0,
    };
    let (verdicts, faces) = carried_through_turn(&mut runner, &mut drive, "Najeela");
    assert_eq!(drive.activations, 0, "{mode:?}");
    assert_eq!(
        runner.state().players[1].life,
        17,
        "reach: {mode:?}: Neheb's trigger had three mana to add"
    );
    assert_eq!((verdicts, faces), (vec![], vec![]), "{mode:?}");

    let board = AssaultBoard {
        others: &[],
        attacker: "Neheb, the Eternal",
        aura: None,
        mountains: 0,
        treasures: 0,
    };
    for (held, reach) in [
        (None, PeriodReach::Combat),
        (Some("Time Warp"), PeriodReach::ExtraTurn),
    ] {
        let (mut runner, mut drive) =
            assault_board(board, Phase::PreCombatMain, AssaultPay::Engine, mode, db);
        if let Some(held) = held {
            place_idle(runner.state_mut(), Idle(held, Zone::Hand, false), db);
        }
        let (verdicts, faces) = carried_through_turn(&mut runner, &mut drive, "Neheb alone");
        assert_eq!(drive.activations, 0, "{mode:?}");
        assert_eq!(
            (verdicts, faces),
            (
                vec![],
                vec![(Some("Neheb, the Eternal".to_string()), reach)]
            ),
            "{held:?}, {mode:?}"
        );
    }

    let (mut runner, mut drive) = siege_board(&["Frontier Siege"], mode, db);
    let (verdicts, faces) = carried_through_turn(&mut runner, &mut drive, "Frontier Siege");
    assert_eq!(drive.activations, 0, "{mode:?}");
    assert_eq!(runner.state().players[1].life, 195, "reach: {mode:?}");
    assert_eq!(
        (verdicts, faces),
        (
            vec![],
            vec![(Some("Frontier Siege".to_string()), PeriodReach::Combat)]
        ),
        "{mode:?}"
    );
}

/// CR 505.1a + CR 732.2a: an added main phase whose trigger pays too little is carried and
/// refused by the confirmer's replay, which cannot pay for Aggravated Assault a third time from
/// ten Treasures, whoever taps them. Belbe, Corrupted Observer ("At the beginning of each
/// postcombat main phase, the active player adds {C}{C} for each of your opponents who lost life
/// this turn.") adds two colorless mana for a Spined Wurm's attack; Neheb, the Eternal adds none
/// while an Ornithopter attacks.
#[test]
fn an_added_main_phase_that_pays_too_little_is_carried_and_refused() {
    let db = shared_card_db().expect("card db");
    let boards = [
        ("Belbe, Corrupted Observer", "Spined Wurm", 190),
        ("Neheb, the Eternal", "Ornithopter", 200),
    ];
    for (payer, attacker, life) in boards {
        let mode = LoopDetectionMode::Interactive;
        for pay in [AssaultPay::Engine, AssaultPay::Hand] {
            let case = format!("{payer}, {mode:?}, {pay:?}");
            let board = AssaultBoard {
                others: &[payer],
                attacker,
                aura: None,
                mountains: 0,
                treasures: 10,
            };
            let (mut runner, mut drive) = assault_board(board, Phase::PreCombatMain, pay, mode, db);
            let (verdicts, faces) = carried_through_turn(&mut runner, &mut drive, &case);
            assert_eq!(drive.activations, 2, "reach: {case}");
            assert_eq!(runner.state().players[1].life, life, "reach: {case}");
            assert!(
                faces.contains(&(Some(payer.to_string()), PeriodReach::Combat)),
                "{case}: {faces:?}"
            );
            assert_eq!(
                verdicts,
                vec![Err(OfferRefusal::IllegalReplayedPlay)],
                "{case}"
            );
        }
    }
}
use crate::loop_period_remade_token_source::differing_fields;
use std::collections::BTreeSet;

type HandStep = dyn FnMut(&mut GameRunner) -> Result<(), EngineError>;

/// A board played by hand one action at a time: its runner, the action, the frame a period ends
/// on, and the periods made so far.
struct HandBoard {
    runner: GameRunner,
    /// `Err` is the reducer refusing the board's next action.
    step: Box<HandStep>,
    window: fn(&GameState) -> bool,
    periods: Box<dyn Fn(&GameState) -> u32>,
}

/// Case name, board, P1's permanents, (P0, P1) life, (count, suggested), where the hand game stops.
type AssaultRow = (
    &'static str,
    AssaultBoard<'static>,
    &'static [&'static str],
    (i32, i32),
    (u32, u32),
    HandStop,
);
/// Case name, card, lands, (P0, P1) life, the ability picked, (count, suggested), where the hand game stops.
type InStepRow = (
    &'static str,
    &'static str,
    &'static [(&'static str, usize)],
    (i32, i32),
    fn(&GameState, ObjectId) -> usize,
    (u32, u32),
    HandStop,
);
type MakeHand = Box<dyn Fn(LoopDetectionMode) -> HandBoard>;

/// What stops the hand game a counted offer is measured against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandStop {
    /// The period's resource runs out with the game still on.
    Spent,
    /// The last period the offer counts ends the game (CR 104.3b, CR 704.5a).
    GameEnds,
    /// The hand game goes on past the offer, which stands at the replay's cap.
    PastTheCap,
}

fn game_over(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::GameOver { .. })
}

/// CR 732.2a + CR 118.3: drives `make` in each mode to its first offer and asserts that the offer
/// states `count` repetitions and suggests `suggested`; that it performed nothing, and declining
/// it leaves what the same actions leave with detection off, on the whole serialized state; that
/// from the declined frame the hand game makes `count` periods, stopping as `stop` says; that a
/// take of one, of half, of one fewer and of the whole count each leaves what that many hand
/// periods leave; and that a declaration of one more performs nothing. Returns each mode's offer.
fn assert_counted_offer(
    case: &str,
    make: &dyn Fn(LoopDetectionMode) -> HandBoard,
    (count, suggested): (u32, u32),
    stop: HandStop,
) -> Vec<GameState> {
    let mut offers = Vec::new();
    let mode = LoopDetectionMode::Interactive;
    let case = format!("{case}, {mode:?}");
    let mut board = make(mode);
    let mut actions = 0;
    while !is_offer(board.runner.state()) {
        let state = board.runner.state();
        assert!(
            actions < 2000 && state.active_player == P0 && !game_over(state),
            "{case}: not offered"
        );
        (board.step)(&mut board.runner).expect("the board's next action");
        actions += 1;
    }
    let offered = board.runner.state().clone();
    let WaitingFor::LoopShortcut { schema, .. } = &offered.waiting_for else {
        unreachable!("is_offer")
    };
    assert_eq!(
        (schema.deliverable_capacity, fixed(&schema.iteration_count)),
        (count, suggested),
        "{case}"
    );

    let mut undetected = make(LoopDetectionMode::Off);
    for _ in 0..actions {
        (undetected.step)(&mut undetected.runner).expect("the board's next action");
    }
    assert_eq!(
        differing_fields(&offered, undetected.runner.state()),
        BTreeSet::from(["waiting_for".to_string()]),
        "{case}: the offer performed nothing"
    );
    let mut hand = GameRunner::from_state(offered.clone());
    act(&mut hand, GameAction::DeclineShortcut);
    let declined = hand.state().clone();
    assert!(
        differing_fields(&declined, undetected.runner.state()).is_empty(),
        "{case}: declining leaves the hand-played state"
    );

    hand.state_mut().loop_detection = LoopDetectionMode::Off;
    let start = (board.periods)(&offered);
    let mut frames: Vec<GameState> = Vec::new();
    let mut ended = false;
    for _ in 0..6000 {
        let state = hand.state();
        if game_over(state) {
            ended = true;
            break;
        }
        if state.active_player != P0 || frames.len() as u32 > count {
            break;
        }
        if (board.window)(state) && (board.periods)(state) == start + frames.len() as u32 + 1 {
            frames.push(state.clone());
        }
        // The hand game ends where the reducer refuses its next play.
        let mut trial = GameRunner::from_state(state.clone());
        if (board.step)(&mut trial).is_err() {
            break;
        }
        hand = trial;
    }
    let made = frames.len() as u32 + u32::from(ended);
    match stop {
        HandStop::Spent | HandStop::GameEnds => assert_eq!(
            (made, ended),
            (count, stop == HandStop::GameEnds),
            "{case}: the hand count"
        ),
        HandStop::PastTheCap => assert!(made > count, "{case}: {made} by hand"),
    }

    let mut takes = vec![1, count / 2, count - 1, count];
    takes.retain(|k| *k >= 1);
    takes.dedup();
    for k in takes {
        let reference = frames.get(k as usize - 1).unwrap_or(hand.state());
        let differs = differing_fields(&taken(&offered, k), reference);
        assert!(
            differs.is_empty(),
            "{case}: a take of {k} differs in {differs:?}"
        );
    }
    let mut over = GameRunner::from_state(offered.clone());
    act(
        &mut over,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(count + 1),
            template: None,
        },
    );
    let mut declined_off = declined.clone();
    declined_off.loop_detection = LoopDetectionMode::Off;
    assert!(
        differing_fields(over.state(), &declined_off).is_empty(),
        "{case}: a declaration of {} performed something",
        count + 1
    );
    offers.push(offered);
    offers
}

/// The Aggravated Assault board with `lives` (0 keeps the board's), engine-paid; a period is an
/// activation and the combat it adds.
fn assault_hand(
    board: AssaultBoard<'static>,
    lives: (i32, i32),
) -> impl Fn(LoopDetectionMode) -> HandBoard {
    move |mode| {
        let db = shared_card_db().expect("card db");
        let (mut runner, mut drive) =
            assault_board(board, Phase::PreCombatMain, AssaultPay::Engine, mode, db);
        for (seat, life) in [(0, lives.0), (1, lives.1)] {
            if life > 0 {
                runner.state_mut().players[seat].life = life;
            }
        }
        let first = std::rc::Rc::new(std::cell::Cell::new((0u32, None::<u32>)));
        let seen = first.clone();
        HandBoard {
            runner,
            step: Box::new(move |runner| {
                let combats = combats_begun(runner.state());
                let action = assault_action(runner.state(), &mut drive);
                runner.act(action)?;
                let at = first.get().1.or((drive.activations > 0).then_some(combats));
                first.set((drive.activations, at));
                Ok(())
            }),
            window: assault_window,
            periods: Box::new(move |state| {
                let (made, first) = seen.get();
                combats_begun(state)
                    .saturating_sub(first.unwrap_or(0))
                    .min(made)
            }),
        }
    }
}

/// The Hellkite Charger board; a period is a combat.
fn charger_hand(
    mountains: usize,
    umbra: bool,
    others: &'static [&'static str],
) -> impl Fn(LoopDetectionMode) -> HandBoard {
    move |mode| {
        let db = shared_card_db().expect("card db");
        let (runner, charger) = charger_board(mountains, umbra, others, mode, db);
        HandBoard {
            runner,
            step: Box::new(move |runner| {
                let action = charger_action(runner.state(), charger, true);
                runner.act(action).map(drop)
            }),
            window: |state| {
                matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
                    && charger_trigger_top(state)
            },
            periods: Box::new(combats_begun),
        }
    }
}

/// P0 in their first main phase with `card` carrying `counters` +1/+1 counters and `lands`, at
/// `lives`; a period is one activation of `card`'s ability `pick` chooses, aimed at P1.
fn in_step_hand(
    card: &'static str,
    counters: u32,
    lands: &'static [(&'static str, usize)],
    lives: (i32, i32),
    pick: fn(&GameState, ObjectId) -> usize,
) -> impl Fn(LoopDetectionMode) -> HandBoard {
    move |mode| {
        let db = shared_card_db().expect("card db");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, lives.0);
        scenario.with_life(P1, lives.1);
        scenario.with_library_top(P0, &["Island"; 25]);
        scenario.with_library_top(P1, &["Island"; 25]);
        let source = scenario.add_real_card(P0, card, Zone::Battlefield, db);
        if counters > 0 {
            scenario.with_counter(source, CounterType::Plus1Plus1, counters);
        }
        for (land, n) in lands {
            for _ in 0..*n {
                scenario.add_real_card(P0, land, Zone::Battlefield, db);
            }
        }
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = mode;
        let index = pick(runner.state(), source);
        let made = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let seen = made.clone();
        HandBoard {
            runner,
            step: Box::new(move |runner| {
                in_step_activation(runner, source, index, &made);
                Ok(())
            }),
            window: |state| {
                matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
                    && state.stack.is_empty()
            },
            periods: Box::new(move |_| seen.get()),
        }
    }
}

/// One action of an in-step board: `source`'s ability `index` activated when legal and settled
/// with P1 named at every target prompt, or the step's pass when it is not.
fn in_step_activation(
    runner: &mut GameRunner,
    source: ObjectId,
    index: usize,
    made: &std::cell::Cell<u32>,
) {
    let mut trial = GameRunner::from_state(runner.state().clone());
    let activation = GameAction::ActivateAbility {
        source_id: source,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    };
    if trial.act(activation).is_err() {
        let pass = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            _ => GameAction::PassPriority,
        };
        act(runner, pass);
        return;
    }
    *runner = trial;
    made.set(made.get() + 1);
    for _ in 0..40 {
        let state = runner.state();
        let action = match &state.waiting_for {
            WaitingFor::LoopShortcut { .. } | WaitingFor::GameOver { .. } => return,
            WaitingFor::Priority { player } if *player == P0 && state.stack.is_empty() => return,
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                GameAction::PassPriority
            }
            _ => GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            },
        };
        act(runner, action);
    }
    panic!("the activation did not settle");
}

fn ping_ability(state: &GameState, id: ObjectId) -> usize {
    state.objects[&id]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::DealDamage { .. }))
        .expect("the ping")
}

fn mana_ability(state: &GameState, id: ObjectId) -> usize {
    ability(state, id, true)
}

fn other_ability(state: &GameState, id: ObjectId) -> usize {
    ability(state, id, false)
}

const UMBRA_FOUR: AssaultBoard<'static> = AssaultBoard {
    others: &[],
    attacker: "Spined Wurm",
    aura: Some("Bear Umbra"),
    mountains: 4,
    treasures: 0,
};

const fn on_treasures(
    others: &'static [&'static str],
    attacker: &'static str,
) -> AssaultBoard<'static> {
    AssaultBoard {
        others,
        attacker,
        aura: None,
        mountains: 0,
        treasures: 20,
    }
}

/// CR 732.2a + CR 118.3 + CR 701.21a + CR 701.26a: Aggravated Assault ("{3}{R}{R}: Untap all
/// creatures you control. After this main phase, there is an additional combat phase followed by
/// an additional main phase.") paid by Treasures ("{T}, Sacrifice this token: Add one mana of any
/// color.") or Mountains that nothing replaces in kind. Grim Hireling ("Whenever one or more
/// creatures you control deal combat damage to a player, create two Treasure tokens.") and Old
/// Gnawbone ("Whenever a creature you control deals combat damage to a player, create that many
/// Treasure tokens.") behind Grizzly Bears make two of the five spent; behind Ornithopter, and
/// beside Virtue of Courage or Smaug the Impenetrable, combat makes none; Bear Umbra untaps four
/// Mountains and a Treasure pays the fifth. Each is offered at the periods its Treasures or lands
/// pay for from the offer frame.
#[test]
fn a_period_spending_its_controllers_treasures_or_lands_is_offered_at_its_hand_count() {
    let boards = [
        (on_treasures(&["Grim Hireling"], "Spined Wurm"), 4),
        (on_treasures(&["Old Gnawbone"], "Grizzly Bears"), 4),
        (on_treasures(&["Old Gnawbone"], "Ornithopter"), 2),
        (on_treasures(&["Virtue of Courage"], "Spined Wurm"), 2),
        (on_treasures(&["Smaug the Impenetrable"], "Spined Wurm"), 2),
        (on_treasures(&[], "Spined Wurm"), 2),
        (
            AssaultBoard {
                treasures: 6,
                ..UMBRA_FOUR
            },
            4,
        ),
        (
            AssaultBoard {
                mountains: 20,
                aura: None,
                ..UMBRA_FOUR
            },
            2,
        ),
    ];
    for (board, count) in boards {
        let case = format!("{board:?}");
        assert_counted_offer(
            &case,
            &assault_hand(board, (0, 0)),
            (count, count),
            HandStop::Spent,
        );
    }
}

/// CR 704.5a + CR 732.2a: Bear Umbra and six Treasures with P1 at 22 life, where P1's life binds
/// before the Treasures, and at 43, where the Treasures bind first: the offer is what the hand
/// game performs in each order, the repetition that ends the game counted.
#[test]
fn a_spend_and_a_seat_threshold_offer_whichever_binds_first() {
    let six = AssaultBoard {
        treasures: 6,
        ..UMBRA_FOUR
    };
    assert_counted_offer(
        "P1 at 22",
        &assault_hand(six, (0, 22)),
        (2, 2),
        HandStop::GameEnds,
    );
    assert_counted_offer(
        "P1 at 43",
        &assault_hand(six, (0, 43)),
        (4, 4),
        HandStop::Spent,
    );
}

/// CR 118.3 + CR 119.4 + CR 732.2a: a period paying its controller's life is counted by the
/// repetitions it can pay for. Mana Confluence ("{T}, Pay 1 life: Add one mana of any color.")
/// pays the fifth mana of Bear Umbra's four Mountains, also with three Treasures beside it, and
/// with Energy Field ("Prevent all damage that would be dealt to you by sources you don't
/// control.") under P1, where nothing moves but P0's life; Hall of the Bandit Lord ("{T}, Pay 3
/// life: Add {C}.") from 17 life pays only while 3 life is left; Treasonous Ogre's "Pay 3 life:
/// Add {R}." is made by hand inside one step; Underworld Coinsmith's "{W}{B}, Pay 1 life: Each
/// opponent loses 1 life." spends its lands and life. Where the last payment leaves P0 at 0 the
/// count includes it and suggests one fewer.
#[test]
fn a_period_paying_its_controllers_life_is_offered_at_its_hand_count() {
    let confluence = AssaultBoard {
        others: &["Mana Confluence"],
        ..UMBRA_FOUR
    };
    let assault: [AssaultRow; 4] = [
        (
            "Mana Confluence",
            confluence,
            &[],
            (0, 0),
            (18, 17),
            HandStop::GameEnds,
        ),
        (
            "Energy Field",
            confluence,
            &["Energy Field"],
            (0, 0),
            (18, 17),
            HandStop::GameEnds,
        ),
        (
            "three Treasures",
            AssaultBoard {
                treasures: 3,
                ..confluence
            },
            &[],
            (0, 0),
            (18, 17),
            HandStop::GameEnds,
        ),
        (
            "Hall of the Bandit Lord",
            AssaultBoard {
                others: &["Hall of the Bandit Lord"],
                ..UMBRA_FOUR
            },
            &[],
            (17, 0),
            (4, 4),
            HandStop::Spent,
        ),
    ];
    for (case, board, theirs, lives, offer, stop) in assault {
        let make = move |mode| {
            let mut hand = assault_hand(board, lives)(mode);
            let db = shared_card_db().expect("card db");
            for name in theirs {
                place(hand.runner.state_mut(), P1, name, db);
            }
            hand
        };
        assert_counted_offer(case, &make, offer, stop);
    }
    assert_counted_offer(
        "Hellkite Charger through Mana Confluence",
        &charger_hand(6, true, &["Mana Confluence"]),
        (19, 18),
        HandStop::GameEnds,
    );
    let coinsmith_lands: &'static [(&'static str, usize)] = &[("Plains", 4), ("Swamp", 4)];
    let in_step: [InStepRow; 5] = [
        (
            "Ogre from 20",
            "Treasonous Ogre",
            &[],
            (20, 20),
            mana_ability,
            (5, 5),
            HandStop::Spent,
        ),
        (
            "Ogre from 21",
            "Treasonous Ogre",
            &[],
            (21, 20),
            mana_ability,
            (6, 5),
            HandStop::GameEnds,
        ),
        (
            "Coinsmith",
            "Underworld Coinsmith",
            coinsmith_lands,
            (20, 20),
            other_ability,
            (3, 3),
            HandStop::Spent,
        ),
        (
            "Coinsmith, P1 at 3",
            "Underworld Coinsmith",
            coinsmith_lands,
            (20, 3),
            other_ability,
            (2, 2),
            HandStop::GameEnds,
        ),
        (
            "Coinsmith, P0 at 3",
            "Underworld Coinsmith",
            coinsmith_lands,
            (3, 20),
            other_ability,
            (2, 1),
            HandStop::GameEnds,
        ),
    ];
    let mut performances = Vec::new();
    for (case, card, lands, lives, pick, offer, stop) in in_step {
        let offers = assert_counted_offer(
            case,
            &in_step_hand(card, 0, lands, lives, pick),
            offer,
            stop,
        );
        performances.push((case, offers.iter().map(performance).collect::<Vec<_>>()));
    }
    // CR 605.3a + CR 603.3: the Ogre's repetition is its mana ability alone; the Coinsmith's
    // activates a non-mana ability.
    let mana_only = vec![Some(PeriodPerformance::ManaAbilitiesOnly)];
    let stack = vec![Some(PeriodPerformance::UsesTheStack)];
    assert_eq!(
        performances,
        [
            ("Ogre from 20", mana_only.clone()),
            ("Ogre from 21", mana_only),
            ("Coinsmith", stack.clone()),
            ("Coinsmith, P1 at 3", stack.clone()),
            ("Coinsmith, P0 at 3", stack),
        ]
    );
}

/// CR 104.3b + CR 704.5a + CR 732.2a: the Mana Confluence period with P0 at 19 life at the offer
/// frame is offered 19 repetitions, the nineteenth P0's own loss; a take of 19 ends the game where
/// the hand game ends it, and a declaration of 20 performs nothing.
#[test]
fn a_take_of_nineteen_at_nineteen_life_ends_the_game_where_the_hand_game_does() {
    let confluence = AssaultBoard {
        others: &["Mana Confluence"],
        ..UMBRA_FOUR
    };
    for offered in assert_counted_offer(
        "P0 at 21",
        &assault_hand(confluence, (21, 0)),
        (19, 18),
        HandStop::GameEnds,
    ) {
        assert_eq!(offered.players[0].life, 19);
        assert!(game_over(&taken(&offered, 19)));
    }
}

/// CR 118.3 + CR 122.1 + CR 732.2a: Walking Ballista's "Remove a +1/+1 counter from this
/// creature: It deals 1 damage to any target." spends counters nothing returns, and Aether Hub's
/// "{T}, Pay {E}: Add one mana of any color." spends energy; each is offered at the repetitions
/// its counters or energy pay for, P1's life binding first at 5. P1's own energy takes no part in
/// the period of the Hubs P0 taps (CR 107.14).
#[test]
fn a_period_spending_its_counters_or_energy_is_offered_at_its_hand_count() {
    assert_counted_offer(
        "Walking Ballista, 8 counters",
        &in_step_hand("Walking Ballista", 8, &[], (20, 20), ping_ability),
        (7, 7),
        HandStop::Spent,
    );
    assert_counted_offer(
        "Walking Ballista, P1 at 5",
        &in_step_hand("Walking Ballista", 30, &[], (20, 5), ping_ability),
        (4, 4),
        HandStop::GameEnds,
    );
    let hub = AssaultBoard {
        others: &["Aether Hub", "Aether Hub", "Aether Hub", "Aether Hub"],
        mountains: 1,
        ..UMBRA_FOUR
    };
    let make = move |mode| {
        let mut hand = assault_hand(hub, (0, 0))(mode);
        hand.runner.state_mut().players[0].energy = 4;
        hand.runner.state_mut().players[1].energy = 1;
        hand
    };
    for offered in assert_counted_offer("Aether Hub, 4 energy", &make, (2, 2), HandStop::Spent) {
        let WaitingFor::LoopShortcut { certificate, .. } = &offered.waiting_for else {
            unreachable!("assert_counted_offer returns offers")
        };
        let (PeriodSignature::EveryRepetition(period) | PeriodSignature::OneRepetition(period)) =
            certificate
                .per_cycle
                .as_ref()
                .expect("the offer carries its period");
        let energy = &period.delta.energy;
        assert_eq!(energy.keys().copied().collect::<Vec<_>>(), vec![P0]);
        assert!(energy[&P0] < 0, "{energy:?}");
    }
}

/// CR 704.5a + CR 732.2a: Walking Ballista's pings take P1 from 5 one life at a time; the take of
/// the four offered ends the game in the counting loop, which the offer names from that last
/// repetition, and records P1's reason.
#[test]
fn a_counted_take_ending_the_game_records_its_reason() {
    let mut board = in_step_hand("Walking Ballista", 30, &[], (20, 5), ping_ability)(
        LoopDetectionMode::Interactive,
    );
    for _ in 0..2000 {
        if is_offer(board.runner.state()) {
            break;
        }
        (board.step)(&mut board.runner).expect("the board's next action");
    }
    let (count, _, certificate) = offered_parts(&board.runner);
    assert_eq!(count, IterationCount::Fixed(4), "reach");
    assert_eq!(
        (certificate.win_kind, certificate.win_kind_at),
        (Some(WinKind::LethalDamage), Some(4))
    );
    let events = take(&mut board.runner, 4);
    assert_eq!(
        losses_in(&events),
        [(P1, BTreeSet::from([LossReason::LifeTotal]))]
    );
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    ));
}

/// CR 732.2a: a count stops at the confirmer's cap, which the replay performed, however much
/// longer the hand game goes on: the Mana Confluence period from 40 life, and Walking Ballista
/// with sixty counters against P1 at 200.
#[test]
fn a_counted_offer_stands_at_the_cap() {
    let confluence = AssaultBoard {
        others: &["Mana Confluence"],
        ..UMBRA_FOUR
    };
    assert_counted_offer(
        "Mana Confluence from 40",
        &assault_hand(confluence, (40, 0)),
        (25, 25),
        HandStop::PastTheCap,
    );
    assert_counted_offer(
        "Walking Ballista, 60 counters",
        &in_step_hand("Walking Ballista", 60, &[], (20, 200), ping_ability),
        (25, 25),
        HandStop::PastTheCap,
    );
}

/// CR 732.2a: a period whose spend changes from cycle to cycle is counted by what the reducer
/// performs, never by one period. Mana Confluence beside three Treasures pays life once the
/// Treasures are gone; Old Gnawbone's Treasures behind Wicker Warcrawler ("Whenever this creature
/// attacks or blocks, put a -1/-1 counter on it at end of combat.") shrink each combat; behind
/// Predator Ooze ("Whenever this creature attacks, put a +1/+1 counter on it.") they grow, and
/// P1's life binds at the seventeenth.
#[test]
fn a_period_whose_spend_changes_each_cycle_is_offered_at_its_hand_count() {
    let gnawbone = |attacker, treasures| AssaultBoard {
        others: &["Old Gnawbone"],
        attacker,
        aura: None,
        mountains: 0,
        treasures,
    };
    assert_counted_offer(
        "Wicker Warcrawler",
        &assault_hand(gnawbone("Wicker Warcrawler", 5), (0, 0)),
        (2, 2),
        HandStop::Spent,
    );
    assert_counted_offer(
        "Predator Ooze",
        &assault_hand(gnawbone("Predator Ooze", 20), (0, 0)),
        (17, 17),
        HandStop::GameEnds,
    );
}

/// CR 732.2a: Hellkite Charger ("Whenever this creature attacks, you may pay {5}{R}{R}. If you
/// do, untap all attacking creatures and after this phase, there is an additional combat phase.")
/// without Bear Umbra pays from fourteen Mountains nothing untaps, so one repetition is left at
/// its second combat and it is offered at one.
#[test]
fn a_hellkite_charger_period_with_one_repetition_left_is_offered_at_one() {
    assert_counted_offer(
        "fourteen Mountains",
        &charger_hand(14, false, &[]),
        (1, 1),
        HandStop::Spent,
    );
}

/// `offered` with every serialized `deliverable_capacity` raised to `capacity`.
fn with_capacity(offered: &GameState, capacity: u32) -> GameState {
    fn raise(value: &mut serde_json::Value, capacity: u32) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(field) = map.get_mut("deliverable_capacity") {
                    *field = serde_json::json!(capacity);
                }
                map.values_mut().for_each(|child| raise(child, capacity));
            }
            serde_json::Value::Array(items) => {
                items.iter_mut().for_each(|child| raise(child, capacity))
            }
            _ => {}
        }
    }
    let mut json = serde_json::to_value(offered).expect("serializes");
    raise(&mut json, capacity);
    serde_json::from_value(json).expect("loads")
}

/// CR 732.2a: a counted take answers to the replay. A declaration the reducer cannot perform in
/// full is refused whole, never delivered short, even against an offer whose serialized capacity
/// was raised: Walking Ballista declared one past its counters, and past the cap.
#[test]
fn a_counted_declaration_the_replay_cannot_perform_is_refused_whole() {
    for (counters, life, count) in [(8, 20, 7), (60, 200, 25)] {
        let make = in_step_hand("Walking Ballista", counters, &[], (20, life), ping_ability);
        let stop = if count == 25 {
            HandStop::PastTheCap
        } else {
            HandStop::Spent
        };
        for offered in assert_counted_offer("Walking Ballista", &make, (count, count), stop) {
            let raised = with_capacity(&offered, count + 1);
            let mut declined = GameRunner::from_state(raised.clone());
            act(&mut declined, GameAction::DeclineShortcut);
            let mut over = GameRunner::from_state(raised);
            act(
                &mut over,
                GameAction::DeclareShortcut {
                    count: IterationCount::Fixed(count + 1),
                    template: None,
                },
            );
            while matches!(
                over.state().waiting_for,
                WaitingFor::RespondToShortcut { .. }
            ) {
                act(
                    &mut over,
                    GameAction::RespondToShortcut {
                        response: ShortcutResponse::Accept,
                    },
                );
            }
            let mut declined = declined.state().clone();
            declined.loop_detection = LoopDetectionMode::Off;
            let differs = differing_fields(over.state(), &declined);
            assert!(differs.is_empty(), "{counters} counters: {differs:?}");
        }
    }
}

/// CR 732.2a: an offer follows one performed period, and a declined counted offer is offered again
/// after the next one, at the count left: Walking Ballista, Bear Umbra's six Treasures, and
/// Treasonous Ogre by hand.
#[test]
fn a_declined_counted_offer_is_offered_again_at_the_count_left() {
    let six = AssaultBoard {
        treasures: 6,
        ..UMBRA_FOUR
    };
    let boards: [(&str, MakeHand, u32); 3] = [
        (
            "Walking Ballista",
            Box::new(in_step_hand(
                "Walking Ballista",
                8,
                &[],
                (20, 20),
                ping_ability,
            )),
            7,
        ),
        ("six Treasures", Box::new(assault_hand(six, (0, 0))), 4),
        (
            "Treasonous Ogre",
            Box::new(in_step_hand(
                "Treasonous Ogre",
                0,
                &[],
                (20, 20),
                mana_ability,
            )),
            5,
        ),
    ];
    for (case, make, count) in boards {
        let mode = LoopDetectionMode::Interactive;
        let mut board = make(mode);
        let mut counts = Vec::new();
        for _ in 0..4000 {
            let state = board.runner.state();
            if game_over(state) || state.active_player != P0 {
                break;
            }
            if let WaitingFor::LoopShortcut { schema, .. } = &state.waiting_for {
                counts.push(schema.deliverable_capacity);
                act(&mut board.runner, GameAction::DeclineShortcut);
            } else {
                (board.step)(&mut board.runner).expect("the board's next action");
            }
        }
        assert_eq!(
            counts,
            (1..=count).rev().collect::<Vec<_>>(),
            "{case}, {mode:?}"
        );
    }
}

/// CR 732.2a: drives `make` in each mode until it leaves P0's turn, asserting it is never offered,
/// that it ends where the same actions end with detection off, and that the confirmer's
/// refusals on the way are `refusals`, each first seen in that order.
fn assert_refused(
    case: &str,
    make: &dyn Fn(LoopDetectionMode) -> HandBoard,
    refusals: &[OfferRefusal],
) {
    let mode = LoopDetectionMode::Interactive;
    let mut board = make(mode);
    let mut seen: Vec<OfferRefusal> = Vec::new();
    let mut actions = 0;
    while actions < 1500 {
        let state = board.runner.state();
        assert!(!is_offer(state), "{case}, {mode:?}: offered");
        if game_over(state) || state.active_player != P0 {
            break;
        }
        if matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0) {
            for (_, verdict) in confirm_for_tests(state) {
                if let Err(refusal) = verdict {
                    if !seen.contains(&refusal) {
                        seen.push(refusal);
                    }
                }
            }
        }
        (board.step)(&mut board.runner).expect("the board's next action");
        actions += 1;
    }
    assert_eq!(seen, refusals, "{case}, {mode:?}");
    let mut undetected = make(LoopDetectionMode::Off);
    for _ in 0..actions {
        (undetected.step)(&mut undetected.runner).expect("the board's next action");
    }
    let differs = differing_fields(board.runner.state(), undetected.runner.state());
    assert!(differs.is_empty(), "{case}, {mode:?}: {differs:?}");
}

/// CR 732.2a: periods the counted road does not take. Ingris Stingerquill's "{4}: Create a 2/2
/// colorless Wizard Soldier creature token named Cadet. Then creatures you control gain haste
/// until end of turn." mints a token class, so the reproduced-token arm answers its pairs and
/// refuses both on the board; Shivan Dragon's "{R}: This creature gets +1/+0 until end of turn."
/// spends Mountains toward no axis.
#[test]
fn periods_the_counted_road_does_not_take_stay_refused() {
    assert_refused(
        "Ingris Stingerquill",
        &in_step_hand(
            "Ingris Stingerquill",
            0,
            &[("Mountain", 12)],
            (20, 20),
            other_ability,
        ),
        &[
            OfferRefusal::Cover(ObjectGrowthVerdict::FodderGrowth([
                vec![FodderCoverRefusal::BoardCover],
                vec![FodderCoverRefusal::BoardCover],
            ])),
            OfferRefusal::IllegalReplayedPlay,
        ],
    );
    assert_refused(
        "Shivan Dragon",
        &in_step_hand(
            "Shivan Dragon",
            0,
            &[("Mountain", 5)],
            (20, 20),
            other_ability,
        ),
        &[OfferRefusal::NoAxis, OfferRefusal::IllegalReplayedPlay],
    );
}

/// CR 732.2a + CR 614.1a: Burning-Tree Shaman ("Whenever a player activates an ability that isn't
/// a mana ability, this creature deals 1 damage to that player.") costs the Basalt Monolith +
/// Power Artifact period 1 life a cycle until Angel's Grace ("... Until end of turn, damage that
/// would reduce your life total to less than 1 reduces it to 1 instead.") holds the total at 1.
/// While a cycle costs life the period is counted, and since the floor never lets the count end
/// it stands at the cap; from the first frame whose cycles cost nothing the period repeats
/// without bound, whether the counted offers were declined or the first was taken.
#[test]
fn a_period_costing_life_is_counted_until_a_floor_holds_the_life_total() {
    let Some(db) = shared_card_db() else { return };
    for take_first in [false, true] {
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
        let mut offers = Vec::new();
        for _ in 0..4 {
            for index in [mana, untap] {
                activate(&mut runner, basalt, index);
                settle(&mut runner, &|_| 0);
                let state = runner.state();
                if let WaitingFor::LoopShortcut { schema, .. } = &state.waiting_for {
                    let capacity = schema.deliverable_capacity;
                    offers.push((
                        state.players[0].life,
                        schema.is_bounded().then_some(capacity),
                    ));
                    if take_first && offers.len() == 1 {
                        take(&mut runner, capacity);
                        assert_eq!(runner.state().players[0].life, 1);
                    } else {
                        act(&mut runner, GameAction::DeclineShortcut);
                        // An offer can stand at a window with the untap still on the stack.
                        settle(&mut runner, &|_| 0);
                    }
                }
            }
        }
        let expected: &[(i32, Option<u32>)] = if take_first {
            &[(4, Some(25)), (1, None), (1, None), (1, None)]
        } else {
            &[(4, Some(25)), (3, Some(25)), (2, None), (1, None)]
        };
        assert_eq!(offers[..4], *expected, "take first: {take_first}");
    }
}

/// CR 119.4 + CR 732.2a: Aggravated Assault paid by hand through Treasonous Ogre ("Pay 3 life:
/// Add {R}.") beside Sinister Monolith ("At the beginning of combat on your turn, each opponent
/// loses 1 life and you gain 1 life."). Each Ogre activation after the first inside a main phase
/// is a period of its own, offered at the payments P0's life leaves; the carried combat span,
/// whose hand activations the replay would make in a later step, is never offered. Without the
/// Monolith nothing returns life and nothing is carried.
#[test]
fn a_period_fed_by_an_each_opponent_drain_is_offered_inside_its_step() {
    let db = shared_card_db().expect("card db");
    let boards = [
        (&["Treasonous Ogre", "Sinister Monolith"][..], true),
        (&["Treasonous Ogre"][..], false),
    ];
    for (others, carries) in boards {
        let board = AssaultBoard {
            others,
            attacker: "Spined Wurm",
            aura: None,
            mountains: 0,
            treasures: 0,
        };
        let mode = LoopDetectionMode::Interactive;
        let case = format!("{others:?}, {mode:?}");
        let (mut runner, mut drive) =
            assault_board(board, Phase::PreCombatMain, AssaultPay::Hand, mode, db);
        runner.state_mut().players[0].life = 80;
        let state = runner.state();
        let ogre = state
            .battlefield
            .iter()
            .copied()
            .find(|id| state.objects[id].name == "Treasonous Ogre")
            .expect("the Ogre");
        let (mut offers, mut most_carried) = (Vec::new(), 0);
        for _ in 0..4000 {
            let state = runner.state();
            if state.active_player != P0 || game_over(state) {
                break;
            }
            most_carried = most_carried.max(carried(state).len());
            if let WaitingFor::LoopShortcut { schema, .. } = &state.waiting_for {
                assert_eq!(
                    play_trace_view(state)
                        .and_then(|view| view.offered)
                        .map(|span| span.source),
                    Some(SpanSource::Window),
                    "{case}"
                );
                let life = i64::from(state.players[0].life);
                offers.push((life, schema.deliverable_capacity));
                act(&mut runner, GameAction::DeclineShortcut);
                continue;
            }
            if assault_window(state)
                && drive.activated_at != Some(combats_begun(state))
                && state.players[0].mana_pool.total() < ASSAULT_COST
                && state.players[0].life > 3
            {
                let ability_index = ability(state, ogre, true);
                act(
                    &mut runner,
                    GameAction::ActivateAbility {
                        source_id: ogre,
                        ability_index,
                        payment_mode: CastPaymentMode::Auto,
                    },
                );
            } else {
                assault_step(&mut runner, &mut drive);
            }
        }
        assert!(drive.activations >= 2, "reach: {case} activates twice");
        assert_eq!(most_carried > 0, carries, "{case} carries {most_carried}");
        assert!(!offers.is_empty(), "{case}");
        for (life, count) in offers {
            // CR 119.4: three life is paid while three is left.
            assert_eq!(i64::from(count), (life / 3).min(25), "{case} at {life}");
        }
    }
}

/// What a take changed on one preview family and seat, read off the boards before and after it.
fn take_change(
    offered: &GameState,
    after: &GameState,
    family: InteractionShortcutPreviewFamily,
    player: Option<u8>,
) -> i64 {
    let counters = |state: &GameState| -> i64 {
        state
            .battlefield
            .iter()
            .map(|id| {
                state.objects[id]
                    .counters
                    .values()
                    .map(|n| i64::from(*n))
                    .sum::<i64>()
            })
            .sum()
    };
    match (family, player) {
        (InteractionShortcutPreviewFamily::Life, Some(seat)) => i64::from(
            after.players[usize::from(seat)].life - offered.players[usize::from(seat)].life,
        ),
        (InteractionShortcutPreviewFamily::Combats, None) => {
            i64::from(combats_begun(after)) - i64::from(combats_begun(offered))
        }
        (InteractionShortcutPreviewFamily::Counters, None) => counters(after) - counters(offered),
        other => panic!("a preview family this row does not read: {other:?}"),
    }
}

/// CR 732.2a: the count picker states no magnitude for a counted offer's counts. Predator Ooze ("Whenever this creature attacks, put a +1/+1 counter on it.") deals
/// one more each period and Mana Confluence's last payment ends the game before its combat, so a
/// count's consequence is no multiple of one period's.
#[test]
fn a_counted_offer_previews_only_what_its_take_performs() {
    let gnawbone = |attacker, treasures| AssaultBoard {
        others: &["Old Gnawbone"],
        attacker,
        aura: None,
        mountains: 0,
        treasures,
    };
    let confluence = AssaultBoard {
        others: &["Mana Confluence"],
        ..UMBRA_FOUR
    };
    let boards: [(&str, MakeHand); 2] = [
        (
            "Predator Ooze",
            Box::new(assault_hand(gnawbone("Predator Ooze", 20), (0, 0))),
        ),
        (
            "Mana Confluence",
            Box::new(assault_hand(confluence, (0, 0))),
        ),
    ];
    for (case, make) in boards {
        let mode = LoopDetectionMode::Interactive;
        let mut board = make(mode);
        for _ in 0..2000 {
            if is_offer(board.runner.state()) {
                break;
            }
            (board.step)(&mut board.runner).expect("the board's next action");
        }
        let offered = board.runner.state().clone();
        let WaitingFor::LoopShortcut { schema, .. } = &offered.waiting_for else {
            panic!("{case}, {mode:?}: not offered");
        };
        let capacity = schema.deliverable_capacity;
        assert_ne!(
            take_change(
                &offered,
                &taken(&offered, capacity),
                InteractionShortcutPreviewFamily::Life,
                Some(1),
            ),
            0,
            "{case}, {mode:?}: the take moves P1's life, a magnitude a preview could state"
        );
        let mut bound = offered.clone();
        bind_interaction_authority(&mut bound, InteractionSessionId("preview".to_string()))
            .expect("bind");
        let view = derive_viewer_interaction(&bound, &filter_state_for_viewer(&bound, P0), P0);
        let Some(InteractionOpportunityResponse::Schema {
            spec: InteractionResponseSpec::Shortcut { preview, .. },
            ..
        }) = view.opportunities.first().map(|o| &o.response)
        else {
            panic!("{case}, {mode:?}: no shortcut opportunity");
        };
        assert!(
            !preview.is_empty(),
            "{case}, {mode:?}: elements are published"
        );
        for element in preview {
            assert!(
                element.entries.is_empty(),
                "{case}, {mode:?}: a take of {} is previewed {:?}",
                element.count,
                element.entries
            );
        }
    }
}

/// `make` played by hand in `mode` to its first offer.
fn hand_offer(make: &dyn Fn(LoopDetectionMode) -> HandBoard, mode: LoopDetectionMode) -> GameState {
    let mut board = make(mode);
    for _ in 0..2000 {
        if is_offer(board.runner.state()) {
            return board.runner.state().clone();
        }
        (board.step)(&mut board.runner).expect("the board's next action");
    }
    panic!("{mode:?}: not offered");
}

/// CR 104.3b + CR 732.2a: Walking Ballista ("Remove a +1/+1 counter from this creature: It deals 1
/// damage to any target.") with eight counters, aiming each ping at P1 at 3 life under Platinum
/// Angel ("You can't lose the game and your opponents can't win the game."), is declared through
/// the interaction response at its whole count, and the take is the one the engine performs.
#[test]
fn a_counted_declaration_aiming_at_a_seat_is_accepted_through_the_response_path() {
    let make = |mode| {
        let mut hand = in_step_hand("Walking Ballista", 8, &[], (20, 3), ping_ability)(mode);
        let db = shared_card_db().expect("card db");
        place(hand.runner.state_mut(), P1, "Platinum Angel", db);
        hand
    };
    let mode = LoopDetectionMode::Interactive;
    let offered = hand_offer(&make, mode);
    let WaitingFor::LoopShortcut { schema, .. } = &offered.waiting_for else {
        unreachable!("hand_offer")
    };
    assert_eq!(schema.deliverable_capacity, 7, "{mode:?}");
    let (bound, view) = published_to_p0(&offered);
    let (opportunity, points) = shortcut_schema(&view).expect("a shortcut schema");
    let submission = InteractionSubmission {
        interaction_id: opportunity.interaction_id.clone(),
        response: InteractionResponse::Shortcut {
            decision: InteractionShortcutDecision::Fixed { iterations: 7 },
            pins: points
                .iter()
                .map(|point| InteractionShortcutPin {
                    group: point.group,
                    choice_ids: point.candidate_ids.clone(),
                    amounts: Vec::new(),
                })
                .collect(),
        },
    };
    assert!(
        matches!(
            resolve_interaction_response(&bound, P0, &submission),
            Ok(GameAction::DeclareShortcut {
                template: Some(_),
                ..
            })
        ),
        "{mode:?}: the response mints a templated declaration"
    );
    let mut declared = bound.clone();
    assert!(
        submit_interaction(&mut declared, P0, submission).is_ok(),
        "{mode:?}: the response is accepted"
    );
    let mut runner = GameRunner::from_state(declared);
    accept(&mut runner);
    let after = runner.state();
    assert!(
        after.players[1].life < 0 && eliminated(after).is_empty(),
        "{mode:?}: reach: P1 at {} under the Angel",
        after.players[1].life
    );
    let mut reference = taken(&bound, 7);
    reference.loop_detection = LoopDetectionMode::Off;
    let differs = differing_fields(after, &reference);
    assert!(differs.is_empty(), "{mode:?}: differs in {differs:?}");
}

/// CR 104.2a + CR 732.2a: a counted offer names how the game ends only at the count whose take
/// ends it with its proposer the winner. Twenty Treasures and Treasonous Ogre from 20 run out with
/// the game on, and Mana Confluence's last payment ends it with P0 the loser, so none names an
/// outcome; Bear Umbra and six Treasures against P1 at 22 end it at their second repetition, named
/// at that count on the offer and on the proposal declaring it.
#[test]
fn a_counted_offer_states_no_outcome_its_take_does_not_produce() {
    let six = AssaultBoard {
        treasures: 6,
        ..UMBRA_FOUR
    };
    let confluence = AssaultBoard {
        others: &["Mana Confluence"],
        ..UMBRA_FOUR
    };
    let boards: [(&str, MakeHand, u32, bool, Option<WinKind>); 4] = [
        (
            "twenty Treasures",
            Box::new(assault_hand(on_treasures(&[], "Spined Wurm"), (0, 0))),
            2,
            false,
            None,
        ),
        (
            "Ogre from 20",
            Box::new(in_step_hand(
                "Treasonous Ogre",
                0,
                &[],
                (20, 20),
                mana_ability,
            )),
            5,
            false,
            None,
        ),
        (
            "Mana Confluence",
            Box::new(assault_hand(confluence, (0, 0))),
            18,
            true,
            None,
        ),
        (
            "Bear Umbra, P1 at 22",
            Box::new(assault_hand(six, (0, 22))),
            2,
            true,
            Some(WinKind::LethalDamage),
        ),
    ];
    for (case, make, count, ends, ending) in boards {
        let mode = LoopDetectionMode::Interactive;
        let case = format!("{case}, {mode:?}");
        let offered = hand_offer(&*make, mode);
        let WaitingFor::LoopShortcut {
            certificate,
            schema,
            ..
        } = &offered.waiting_for
        else {
            unreachable!("hand_offer")
        };
        assert_eq!(schema.deliverable_capacity, count, "{case}");
        assert_eq!(
            (
                game_over(&taken(&offered, count)),
                game_over(&taken(&offered, count - 1))
            ),
            (ends, false),
            "{case}: reach"
        );
        let named = (ending, ending.map(|_| count));
        assert_eq!(
            (certificate.win_kind, certificate.win_kind_at),
            named,
            "{case}"
        );
        assert!(certificate.unbounded.is_empty(), "{case}");
        let declared = proposed(offered.clone(), count);
        let WaitingFor::RespondToShortcut { proposal, .. } = &declared.waiting_for else {
            unreachable!("proposed")
        };
        assert_eq!((proposal.win_kind, proposal.win_kind_at), named, "{case}");
    }
}

/// CR 104.2a + CR 704.5a + CR 704.5c + CR 732.2a: Kilo, Apogee Mind ("Whenever Kilo becomes
/// tapped, proliferate.") tapped by Relic of Legends and untapped by Freed from the Real, with
/// Scheming Aspirant ("Whenever you proliferate, each opponent loses 2 life and you gain 2
/// life."): the counted offer names the reason P1 leaves for. Offered at 2 life and 8 poison, life
/// reaches 0 at 9 poison; offered at 3 and 8, poison reaches 10 at 1; offered at 2 and 9, poison
/// reaches 10 before the drain resolves.
#[test]
fn a_counted_offer_names_the_threshold_its_ending_crosses() {
    let Some(db) = shared_card_db() else { return };
    for (offered_at, repetitions, expected, reason) in [
        ((2, 8), 1, WinKind::LethalDamage, LossReason::LifeTotal),
        ((3, 8), 2, WinKind::PoisonLoss, LossReason::Poison),
        ((2, 9), 1, WinKind::PoisonLoss, LossReason::Poison),
    ] {
        let mut runner = kilo_offer(GameScenario::new(), &[(22, 0), offered_at], &[P1], db);
        let (count, _, certificate) = offered_parts(&runner);
        assert_eq!(count, IterationCount::Fixed(repetitions), "{offered_at:?}");
        assert_eq!(predicted_winner(&runner), Some(P0), "{offered_at:?}");
        assert_eq!(certificate.win_kind, Some(expected), "{offered_at:?}");
        let events = take(&mut runner, repetitions);
        assert_eq!(
            losses_in(&events),
            [(P1, BTreeSet::from([reason]))],
            "{offered_at:?}"
        );
    }
}

/// Phyrexian Altar recurring Gravecrawler as `altar_board_at` builds it, with Ms. Bumbleflower
/// ("Whenever you cast a spell, target opponent draws a card. …") and `victim`'s library at three
/// cards; every other library holds ten.
fn bumbleflower_board(
    mut scenario: GameScenario,
    lives: &[i32],
    victim: PlayerId,
    db: &CardDatabase,
) -> (GameRunner, ObjectId, ObjectId) {
    scenario.at_phase(Phase::PreCombatMain);
    for (seat, life) in lives.iter().enumerate() {
        scenario.with_life(PlayerId(seat as u8), *life);
    }
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Walking Corpse", Zone::Battlefield, db);
    let gravecrawler = scenario.add_real_card(P0, "Gravecrawler", Zone::Graveyard, db);
    scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Ms. Bumbleflower", Zone::Battlefield, db);
    for seat in 0..lives.len() {
        let seat = PlayerId(seat as u8);
        let cards = if seat == victim { 3 } else { 10 };
        for _ in 0..cards {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    (runner, altar, gravecrawler)
}

/// Casts Gravecrawler and sacrifices it to the Altar, Bumbleflower aiming its draw at `victim`,
/// until the engine offers the loop.
fn deck_to_offer(
    runner: &mut GameRunner,
    altar: ObjectId,
    gravecrawler: ObjectId,
    victim: PlayerId,
) {
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action)
            + chooses_color(ManaType::Black)(action)
            + 4 * i32::from(matches!(
                action,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(seat))
                } if *seat == victim
            ))
    };
    for _ in 0..4 {
        cast(runner, gravecrawler, vec![], CastPaymentMode::Auto);
        settle(runner, &score);
        if is_offer(runner.state()) {
            return;
        }
        let index = ability(runner.state(), altar, true);
        activate(runner, altar, index);
        settle(runner, &score);
        if is_offer(runner.state()) {
            return;
        }
    }
    panic!("not offered: {:?}", latest_verdict(runner.state()));
}

/// CR 704.5b: Bumbleflower makes the victim draw from an empty library, and the offer names that
/// decking; CR 810.8a: in Two-Headed Giant the victim's teammate loses with it, carrying its reason.
#[test]
fn a_decking_take_records_its_reason_for_each_leaving_seat() {
    let Some(db) = shared_card_db() else { return };
    let decked = BTreeSet::from([LossReason::DrewFromEmptyLibrary]);
    for (case, scenario, lives, victim, expected) in [
        (
            "two players",
            GameScenario::new(),
            &[20, 20][..],
            P1,
            vec![(P1, decked.clone())],
        ),
        (
            "Two-Headed Giant",
            GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42),
            &[30, -3, 15, 15][..],
            P2,
            vec![(P2, decked.clone()), (P3, decked.clone())],
        ),
    ] {
        let (mut runner, altar, gravecrawler) = bumbleflower_board(scenario, lives, victim, db);
        deck_to_offer(&mut runner, altar, gravecrawler, victim);
        let (count, _, certificate) = offered_parts(&runner);
        assert_eq!(count, IterationCount::Fixed(1), "{case}: reach");
        assert_eq!(certificate.win_kind, Some(WinKind::Decking), "{case}");
        assert_eq!(predicted_winner(&runner), Some(P0), "{case}");
        let events = take(&mut runner, 1);
        assert_eq!(losses_in(&events), expected, "{case}");
    }
}

/// Cycles the Altar + Gravecrawler period like [`drive_altar_offers`], noting P0's library at each
/// offer.
fn drive_altar_offers_noting_p0(
    runner: &mut GameRunner,
    pair: (ObjectId, ObjectId),
    cycles: usize,
    declines: impl Fn(&GameState) -> bool,
) -> Vec<(usize, LoopCertificate)> {
    let libraries = std::cell::RefCell::new(Vec::new());
    let offers = drive_altar_offers(runner, pair, cycles, |state| {
        libraries.borrow_mut().push(state.players[0].library.len());
        declines(state)
    });
    libraries
        .into_inner()
        .into_iter()
        .zip(offers.into_iter().map(|(_, certificate)| certificate))
        .collect()
}

/// CR 701.17a + CR 701.17b + CR 704.5b + CR 732.2a: Liliana the Repentant ("Whenever another
/// creature or planeswalker you control enters, mill two cards.") mills P0 two cards each time
/// Gravecrawler enters. Milling never loses the game, so the period is offered without end while
/// the library holds a card, its last two included, and has no axis once it is empty.
#[test]
fn a_liliana_self_mill_period_is_offered_without_end() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(Some("Liliana the Repentant"), db);
    let offers = drive_altar_offers_noting_p0(&mut runner, (altar, gravecrawler), 8, |state| {
        state.players[0].library.len() > 2
    });
    let libraries: Vec<usize> = offers.iter().map(|(library, _)| *library).collect();
    assert_eq!(libraries, [8, 6, 4, 2], "reach: two cards a cycle");
    for (library, certificate) in &offers {
        assert_eq!(
            certificate.unbounded,
            [ResourceAxis::LibraryDelta(P0)],
            "library {library}"
        );
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    ));
    act(&mut runner, GameAction::DeclineShortcut);
    settle(&mut runner, &|action| 2 * names(&[gravecrawler])(action));
    for cycle in 0..2 {
        altar_cycle(&mut runner, altar, gravecrawler);
        assert!(!is_offer(runner.state()), "cycle {cycle}");
    }
    assert_eq!(runner.state().players[0].library.len(), 0);
    assert_eq!(latest_verdict(runner.state()), Err(OfferRefusal::NoAxis));
}

/// CR 701.17b: a take of more repetitions than the library lasts mills what it holds and stops at
/// P0's priority.
#[test]
fn a_take_of_a_liliana_self_mill_period_mills_what_the_library_holds() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(Some("Liliana the Repentant"), db);
    drive_altar_offers(&mut runner, (altar, gravecrawler), 8, |_| false);
    let state = runner.state();
    let (library, graveyard) = (
        state.players[0].library.len(),
        state.players[0].graveyard.len(),
    );
    assert!(library > 0, "reach: the offer's library holds cards");
    take(&mut runner, (library / 2 + 2) as u32);
    let state = runner.state();
    assert_eq!(state.players[0].library.len(), 0);
    assert_eq!(state.players[0].graveyard.len(), graveyard + library);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player: P0 }),
        "{}",
        state.waiting_for.variant_name()
    );
}

/// CR 701.17b: Altar of the Brood's period is offered at P1's last card, whose first repetition
/// mills it and whose later ones mill none; at an empty library it has no axis.
#[test]
fn an_altar_of_the_brood_period_is_offered_at_the_last_card() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(Some("Altar of the Brood"), db);
    let offers = drive_altar_offers(&mut runner, (altar, gravecrawler), 12, |state| {
        state.players[1].library.len() > 1
    });
    let (library, certificate) = offers.last().expect("an offer");
    assert_eq!(*library, 1);
    assert_eq!(certificate.unbounded, [ResourceAxis::LibraryDelta(P1)]);
    act(&mut runner, GameAction::DeclineShortcut);
    settle(&mut runner, &|action| 2 * names(&[gravecrawler])(action));
    altar_cycle(&mut runner, altar, gravecrawler);
    assert!(!is_offer(runner.state()));
    assert_eq!(runner.state().players[1].library.len(), 0);
    assert_eq!(latest_verdict(runner.state()), Err(OfferRefusal::NoAxis));
}

/// CR 701.17b + CR 732.2a: beside Altar of the Brood, Liliana the Repentant's period mills both
/// libraries; it is offered on both while P0's holds a card, and on P1's once P0's is empty.
#[test]
fn a_liliana_beside_altar_of_the_brood_is_offered_before_and_after_the_casters_library_empties() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board(Some("Liliana the Repentant"), db);
    place(runner.state_mut(), P0, "Altar of the Brood", db);
    let offers = drive_altar_offers_noting_p0(&mut runner, (altar, gravecrawler), 12, |state| {
        !state.players[0].library.is_empty()
    });
    let (first_library, first) = &offers[0];
    assert!(*first_library > 0, "{offers:?}");
    assert_eq!(
        first.unbounded,
        [
            ResourceAxis::LibraryDelta(P0),
            ResourceAxis::LibraryDelta(P1)
        ]
    );
    let (library, last) = offers.last().expect("an offer");
    assert_eq!(*library, 0);
    assert_eq!(last.unbounded, [ResourceAxis::LibraryDelta(P1)]);
    assert_eq!(last.win_kind, Some(WinKind::Advantage));
}

/// CR 118.3 + CR 701.17b: Millikin ("{T}, Mill a card: Add {C}.") untapped by Intruder Alarm
/// ("Whenever a creature enters, untap all creatures.") pays a mill each cycle, which can't be
/// paid from an empty library, so the period is counted to P0's library.
#[test]
fn a_period_whose_cost_mills_is_counted_to_the_library() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Walking Corpse", Zone::Battlefield, db);
    let gravecrawler = scenario.add_real_card(P0, "Gravecrawler", Zone::Graveyard, db);
    let millikin = scenario.add_real_card(P0, "Millikin", Zone::Battlefield, db);
    for card in ["Intruder Alarm", "Swamp"] {
        scenario.add_real_card(P0, card, Zone::Battlefield, db);
    }
    for seat in [P0, P1] {
        for _ in 0..8 {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
    };
    'cycles: for _ in 0..6 {
        for play in 0..3 {
            match play {
                0 => {
                    // CR 605.1a: a cost that mills makes Millikin's ability not a mana ability.
                    let index = ability(runner.state(), millikin, false);
                    activate(&mut runner, millikin, index);
                }
                1 => cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto),
                _ => {
                    let index = ability(runner.state(), altar, true);
                    activate(&mut runner, altar, index);
                }
            }
            settle(&mut runner, &score);
            if is_offer(runner.state()) {
                break 'cycles;
            }
        }
    }
    let state = runner.state();
    let library = state.players[0].library.len();
    let WaitingFor::LoopShortcut {
        schema,
        certificate,
        ..
    } = &state.waiting_for
    else {
        panic!("an offer: {:?}", latest_verdict(state));
    };
    assert!(library > 0, "reach: the offer's library holds cards");
    assert_eq!(
        schema.iteration_count,
        IterationCount::Fixed(library as u32)
    );
    assert_eq!(schema.deliverable_capacity, library as u32);
    assert!(
        certificate.unbounded.is_empty(),
        "{:?}",
        certificate.unbounded
    );
    take(&mut runner, library as u32);
    let state = runner.state();
    assert_eq!(state.players[0].library.len(), 0);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player: P0 }),
        "{}",
        state.waiting_for.variant_name()
    );
}

/// CR 111.10a + CR 602.2: Temur Sabertooth ("{1}{G}: You may return another creature you control
/// to its owner's hand. If you do, this creature gains indestructible until end of turn.") returns
/// Dockside Extortionist ("When this creature enters, create X Treasure tokens, where X is the
/// number of artifacts and enchantments your opponents control.") to be cast again; against six
/// Ornithopters each cycle makes six Treasures, more than the period spends, and the ones it nets
/// go unused, so it is offered.
#[test]
fn dockside_and_sabertooth_are_offered() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sabertooth = scenario.add_real_card(P0, "Temur Sabertooth", Zone::Battlefield, db);
    let dockside = scenario.add_real_card(P0, "Dockside Extortionist", Zone::Battlefield, db);
    for land in ["Forest", "Forest", "Mountain", "Mountain"] {
        scenario.add_real_card(P0, land, Zone::Battlefield, db);
    }
    for _ in 0..6 {
        scenario.add_real_card(P1, "Ornithopter", Zone::Battlefield, db);
    }
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Forest", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let score = |action: &GameAction| {
        2 * names(&[dockside])(action)
            + i32::from(matches!(
                action,
                GameAction::DecideOptionalEffect { accept: true }
            ))
    };
    'cycles: for _ in 0..4 {
        for recast in [false, true] {
            if recast {
                cast(&mut runner, dockside, vec![], CastPaymentMode::Auto);
            } else {
                let index = ability(runner.state(), sabertooth, false);
                activate(&mut runner, sabertooth, index);
            }
            settle(&mut runner, &score);
            if is_offer(runner.state()) {
                break 'cycles;
            }
        }
    }
    let state = runner.state();
    assert!(is_offer(state), "{:?}", latest_verdict(state));
    assert_eq!(
        treasures(state),
        6,
        "reach: Dockside's entry made a Treasure per Ornithopter"
    );
    assert!(matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }));
}

/// Magda, Brazen Outlaw ("Other Dwarves you control get +1/+0. / Whenever a Dwarf you control
/// becomes tapped, create a Treasure token. ...") beside Universal Automaton (Changeling), Clock of
/// Omens ("Tap two untapped artifacts you control: Untap target artifact.") and Battered Golem
/// ("This creature doesn't untap during your untap step. / Whenever an artifact enters, you may
/// untap this creature."), whose "may" P0 has answered `golem` for the card; the board, the
/// Automaton, the Clock and the Golem.
fn magda_board(
    golem: AutoMayChoice,
    db: &CardDatabase,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Magda, Brazen Outlaw", Zone::Battlefield, db);
    let automaton = scenario.add_real_card(P0, "Universal Automaton", Zone::Battlefield, db);
    let clock = scenario.add_real_card(P0, "Clock of Omens", Zone::Battlefield, db);
    let battered = scenario.add_real_card(P0, "Battered Golem", Zone::Battlefield, db);
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Island", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.loop_detection = LoopDetectionMode::Interactive;
    // CR 205.3m: the creature types the card pool holds, which Changeling makes the Automaton.
    state.all_creature_types = ["Berserker", "Dwarf", "Golem", "Shapeshifter"]
        .map(String::from)
        .to_vec();
    let printed_ref = state.objects[&battered]
        .printed_ref
        .clone()
        .expect("a printed card");
    state.set_may_trigger_auto_choice_selector(
        MayTriggerAutoChoiceSelector::SameCard {
            player: P0,
            printed_ref,
            printed_occurrence: 0,
        },
        golem,
    );
    (runner, automaton, clock, battered)
}

/// One activation of Clock of Omens tapping the Automaton and `other`, untapping the Automaton.
fn magda_cycle(runner: &mut GameRunner, (automaton, clock): (ObjectId, ObjectId), other: ObjectId) {
    let score = |action: &GameAction| 3 * names(&[automaton])(action) + names(&[other])(action);
    let index = activated_index(runner.state(), clock);
    activate(runner, clock, index);
    settle(runner, &score);
}

/// CR 602.2 + CR 732.2a: each activation of Clock of Omens taps Universal Automaton, the board's
/// only Dwarf, so Magda makes a Treasure, whose entry untaps Battered Golem; the period never
/// activates its Treasures, so it is offered.
#[test]
fn a_magda_period_growing_unused_treasures_is_offered() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, automaton, clock, battered) = magda_board(AutoMayChoice::Accept, db);
    let mut treasures_at = Vec::new();
    for _ in 0..2 {
        magda_cycle(&mut runner, (automaton, clock), battered);
        assert!(
            is_offer(runner.state()),
            "{:?}",
            latest_verdict(runner.state())
        );
        treasures_at.push(treasures(runner.state()));
        act(&mut runner, GameAction::DeclineShortcut);
        settle(&mut runner, &|_| 0);
    }
    assert_eq!(treasures_at, [1, 2], "reach: one Treasure a cycle");
}

/// CR 602.2: tapping a Treasure the period made to pay Clock of Omens uses it, so the grown
/// Treasures are not inert.
#[test]
fn a_magda_period_tapping_its_treasures_is_refused_at_the_cover() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, automaton, clock, battered) = magda_board(AutoMayChoice::Accept, db);
    magda_cycle(&mut runner, (automaton, clock), battered);
    assert!(
        is_offer(runner.state()),
        "reach: the untapped period is offered"
    );
    act(&mut runner, GameAction::DeclineShortcut);
    settle(&mut runner, &|_| 0);
    for cycle in 0..3 {
        let state = runner.state();
        let untapped = tokens(state, P0)
            .into_iter()
            .find(|id| !state.objects[id].tapped)
            .expect("an untapped Treasure");
        magda_cycle(&mut runner, (automaton, clock), untapped);
        assert!(
            runner.state().objects[&untapped].tapped,
            "reach: cycle {cycle} tapped a Treasure"
        );
        assert!(!is_offer(runner.state()), "cycle {cycle}");
    }
    let verdict = latest_verdict(runner.state());
    assert!(grown_not_inert(&verdict), "{verdict:?}");
}

/// CR 603.5 + CR 118.3: with Battered Golem's "may" declined it stays tapped, so the next
/// activation of Clock of Omens cannot tap the same two artifacts and the period's own cost answer
/// cannot be given again.
#[test]
fn a_magda_period_declining_the_golem_does_not_recur() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, automaton, clock, battered) = magda_board(AutoMayChoice::Decline, db);
    magda_cycle(&mut runner, (automaton, clock), battered);
    let state = runner.state();
    assert!(
        state.objects[&battered].tapped,
        "reach: the Golem stays tapped"
    );
    assert!(!is_offer(state));
    assert_eq!(latest_verdict(state), Err(OfferRefusal::UnanswerablePrompt));
}

/// CR 732.2a: Resolve All proposes a shortcut over priority passes, not a game choice, so Altar of
/// the Brood's period resolved through it is the same period, and is offered alike for either
/// scope.
#[test]
fn an_altar_of_the_brood_period_resolved_all_is_offered() {
    let Some(db) = shared_card_db() else { return };
    for scope in [ResolveAllScope::Own, ResolveAllScope::Shared] {
        let (mut runner, altar, gravecrawler) = altar_board(Some("Altar of the Brood"), db);
        let score = |action: &GameAction| {
            2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
        };
        let library = runner.state().players[1].library.len();
        cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
        act(
            &mut runner,
            GameAction::BeginResolveAll {
                max_resolutions: 10,
                scope,
            },
        );
        if scope == ResolveAllScope::Shared {
            let WaitingFor::ResolveAllConsent { epoch, .. } = runner.state().waiting_for else {
                panic!(
                    "a shared Resolve All asks P1: {:?}",
                    runner.state().waiting_for
                );
            };
            act(
                &mut runner,
                GameAction::RespondResolveAllConsent {
                    epoch,
                    decision: ResolveAllConsentDecision::Grant,
                },
            );
        }
        settle(&mut runner, &score);
        if !is_offer(runner.state()) {
            let index = ability(runner.state(), altar, true);
            activate(&mut runner, altar, index);
            settle(&mut runner, &score);
        }
        let state = runner.state();
        let WaitingFor::LoopShortcut { certificate, .. } = &state.waiting_for else {
            panic!("{scope:?}: {:?}", latest_verdict(state));
        };
        assert_eq!(
            state.players[1].library.len(),
            library - 1,
            "{scope:?}: reach"
        );
        assert_eq!(
            certificate.unbounded,
            [ResourceAxis::LibraryDelta(P1)],
            "{scope:?}"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }),
            "{scope:?}"
        );
    }
}

/// The offer standing on `runner`, as its schema's published count and the certificate's.
fn offered_parts(runner: &GameRunner) -> (IterationCount, u32, LoopCertificate) {
    let WaitingFor::LoopShortcut {
        schema,
        certificate,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("an offer, got {:?}", runner.state().waiting_for);
    };
    (
        schema.iteration_count.clone(),
        schema.deliverable_capacity,
        certificate.clone(),
    )
}

/// CR 104.2a + CR 704.5a + CR 732.2a: with P1 at 2, the Altar–Zulaport period's second replayed
/// repetition ends the game with P0 the winner, so the counting replay counts it and the take
/// ends the game there.
#[test]
fn a_period_whose_replay_ends_the_game_for_its_holder_is_counted() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board_at("Zulaport Cutthroat", &[20, 3], db);
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
    };
    cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
    settle(&mut runner, &score);
    let index = ability(runner.state(), altar, true);
    activate(&mut runner, altar, index);
    settle(&mut runner, &score);
    assert_eq!(lives(runner.state()), [21, 2]);
    let (count, _, certificate) = offered_parts(&runner);
    assert_eq!(count, IterationCount::Fixed(2));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::LoopShortcut {
            predicted_winner: Some(P0),
            ..
        }
    ));
    assert_eq!(certificate.win_kind, Some(WinKind::LethalDamage));
    assert_eq!(certificate.win_kind_at, Some(2));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    ));
    let events = take_recorded(runner.state_mut(), 2).events;
    assert_eq!(markers(&events).len(), 2);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::GameOver {
            winner: Some(P0),
            ..
        }
    ));
}

/// CR 121.1 + CR 704.5b + CR 732.2a: a period that grows a Food and draws P0 a card each
/// repetition has no CR 704 threshold but its own library, so it is offered at the draws that
/// library holds; milling two first (Liliana the Repentant) leaves a third of them.
#[test]
fn a_drawing_advantage_period_is_offered_at_its_library() {
    let Some(db) = shared_card_db() else { return };
    for (payoff, per_repetition) in [("Samwise Gamgee", 1), ("Liliana the Repentant", 3)] {
        let (mut runner, altar, gravecrawler) = altar_board(Some(payoff), db);
        place(runner.state_mut(), P0, "Moldervine Reclamation", db);
        drive_altar_offers(&mut runner, (altar, gravecrawler), 3, |_| false);
        let library = runner.state().players[0].library.len() as u32;
        let (count, _, _) = offered_parts(&runner);
        assert_eq!(
            count,
            IterationCount::Fixed(library / per_repetition),
            "{payoff}: library {library}"
        );
    }
}

/// CR 119.3: Spike Feeder ("Remove a +1/+1 counter from this creature: You gain 2 life.") beside
/// Archangel of Thune ("Whenever you gain life, put a +1/+1 counter on each creature you
/// control.") gains P0 2 life a repetition, so an offer near the largest life total the engine
/// represents is held to the repetitions that stay representable, and its take completes.
#[test]
fn a_life_gaining_period_is_offered_within_the_representable_life_total() {
    let Some(db) = shared_card_db() else { return };
    let offered_at = |life: i32| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let feeder = scenario.add_real_card(P0, "Spike Feeder", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Archangel of Thune", Zone::Battlefield, db);
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.loop_detection = LoopDetectionMode::Interactive;
        state.players[0].life = life;
        state
            .objects
            .get_mut(&feeder)
            .expect("Spike Feeder")
            .counters
            .insert(CounterType::Plus1Plus1, 2);
        let index = runner.state().objects[&feeder]
            .abilities
            .iter()
            .position(|a| {
                a.kind == AbilityKind::Activated && matches!(*a.effect, Effect::GainLife { .. })
            })
            .expect("Spike Feeder's life gain");
        for _ in 0..8 {
            activate(&mut runner, feeder, index);
            settle(&mut runner, &|_| 0);
            if is_offer(runner.state()) {
                return runner;
            }
        }
        panic!("reach: the period is offered");
    };
    let headroom = 200;
    let mut runner = offered_at(i32::MAX - headroom);
    let life = runner.state().players[0].life;
    let (_, capacity, _) = offered_parts(&runner);
    assert_eq!(capacity, ((i32::MAX - life) / 2) as u32);
    take_recorded(runner.state_mut(), capacity);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().players[0].life, life + 2 * capacity as i32);

    let (_, uncapped, _) = offered_parts(&offered_at(20));
    assert!(uncapped > capacity, "{uncapped}");
}

/// CR 701.17a + CR 701.17b + CR 121.1: beside Altar of the Brood and Moldervine Reclamation the
/// period mills P1 one card and draws P0 one, so its preview mills P1 no more than its library
/// holds, states P0's draws, and its certificate names no decking win; with P1's library as long
/// as P0's, the take performs every repetition offered.
#[test]
fn a_moldervine_altar_of_the_brood_period_previews_its_mills_and_draws() {
    let Some(db) = shared_card_db() else { return };
    for p1_library in [3, 10] {
        let (mut runner, altar, gravecrawler) = altar_board(Some("Altar of the Brood"), db);
        let state = runner.state_mut();
        place(state, P0, "Moldervine Reclamation", db);
        let cut: Vec<ObjectId> = state.players[1]
            .library
            .iter()
            .skip(p1_library)
            .copied()
            .collect();
        for id in cut {
            remove_from_zone(state, id, Zone::Library, P1);
            state.objects.remove(&id);
        }
        drive_altar_offers(&mut runner, (altar, gravecrawler), 1, |_| false);
        let offered = runner.state().clone();
        let p1_held = i32::try_from(offered.players[1].library.len()).expect("small");
        let (count, _, certificate) = offered_parts(&runner);
        assert_ne!(certificate.win_kind, Some(WinKind::Decking), "{p1_library}");
        let IterationCount::Fixed(n) = count else {
            panic!("a fixed count");
        };
        let (_, view) = published_to_p0(&offered);
        let Some(InteractionOpportunityResponse::Schema {
            spec: InteractionResponseSpec::Shortcut { preview, .. },
            ..
        }) = view.opportunities.first().map(|o| &o.response)
        else {
            panic!("no shortcut opportunity");
        };
        let element = preview
            .iter()
            .find(|element| element.count == n)
            .expect("the suggested count is previewed");
        let amount = |family, seat: PlayerId| {
            element
                .entries
                .iter()
                .find(|entry| entry.family == family && entry.player == Some(seat.0))
                .map(|entry| entry.amount)
        };
        assert_eq!(
            amount(InteractionShortcutPreviewFamily::Mill, P1),
            Some(-(n as i32).min(p1_held)),
            "{p1_library}"
        );
        assert_eq!(
            amount(InteractionShortcutPreviewFamily::Cards, P0),
            Some(n as i32),
            "{p1_library}"
        );
        if p1_library == 10 {
            let events = take_recorded(runner.state_mut(), n).events;
            assert_eq!(markers(&events).last(), Some(&(P0, n)), "{p1_library}");
        }
    }
}

/// P0's precombat main with Time Sieve ("{T}, Sacrifice five artifacts: Take an extra turn after
/// this one.") and six Islands on P0's battlefield, Thopter Assembly ("At the beginning of your
/// upkeep, if you control no Thopters other than this creature, return this creature to its owner's
/// hand and create five 1/1 colorless Thopter artifact creature tokens with flying.") in hand,
/// `library` Islands in P0's library and `stage` applied; the runner, the Sieve and the Assembly.
fn sieve_board(
    library: usize,
    stage: impl FnOnce(&mut GameScenario),
    db: &CardDatabase,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Real Islands: a card still in the library is face down to its holder (CR 401.2).
    for _ in 0..library {
        scenario.add_real_card(P0, "Island", Zone::Library, db);
    }
    scenario.with_library_top(P1, &["Island"; 25]);
    let sieve = scenario.add_real_card(P0, "Time Sieve", Zone::Battlefield, db);
    for _ in 0..6 {
        scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    }
    let assembly = scenario.add_real_card(P0, "Thopter Assembly", Zone::Hand, db);
    stage(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    (runner, sieve, assembly)
}

/// Drives the Time Sieve board until turn `last` ends, casting Thopter Assembly and sacrificing
/// its Thopters to the Sieve each main phase, discarding an Island at cleanup and declining every
/// offer; each offer's turn and count, and each of P0's precombat-main windows (an offered one as
/// its offer stands).
fn sieve_windows(
    (mut runner, sieve, assembly): (GameRunner, ObjectId, ObjectId),
    last: u32,
) -> (Vec<(u32, u32)>, Vec<GameState>) {
    let (mut offers, mut windows) = (Vec::new(), Vec::<GameState>::new());
    for _ in 0..40_000 {
        let state = runner.state();
        if state.turn_number > last || matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return (offers, windows);
        }
        if is_offer(state) {
            assert_eq!(state.phase, Phase::PreCombatMain, "{}", state.turn_number);
            offers.push((state.turn_number, fixed(&offer_of(state).1)));
            windows.push(state.clone());
            act(&mut runner, GameAction::DeclineShortcut);
            continue;
        }
        let priority = matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
            && state.stack.is_empty();
        let main = priority && state.active_player == P0 && state.phase == Phase::PreCombatMain;
        if main
            && windows
                .last()
                .is_none_or(|window| window.turn_number != state.turn_number)
        {
            windows.push(state.clone());
        }
        let untapped_islands = state
            .battlefield
            .iter()
            .filter(|id| state.objects[id].name == "Island" && !state.objects[id].tapped)
            .count();
        let thopters = state
            .battlefield
            .iter()
            .filter(|id| state.objects[id].is_token)
            .count();
        let action = match &state.waiting_for {
            _ if main && state.players[0].hand.contains(&assembly) && untapped_islands >= 6 => {
                GameAction::CastSpell {
                    object_id: assembly,
                    card_id: state.objects[&assembly].card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                }
            }
            _ if main && thopters >= 5 && !state.objects[&sieve].tapped => {
                GameAction::ActivateAbility {
                    source_id: sieve,
                    ability_index: ability(state, sieve, false),
                    payment_mode: CastPaymentMode::Auto,
                }
            }
            WaitingFor::PayCost { choices, count, .. } => GameAction::SelectCards {
                cards: choices
                    .iter()
                    .copied()
                    .filter(|id| state.objects[id].is_token)
                    .take(*count)
                    .collect(),
            },
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
            _ => GameAction::PassPriority,
        };
        act(&mut runner, action);
    }
    panic!("the Time Sieve drive did not end");
}

/// The window of turn `turn` among `windows`.
fn window_of(windows: &[GameState], turn: u32) -> &GameState {
    windows
        .iter()
        .find(|window| window.turn_number == turn)
        .unwrap_or_else(|| panic!("no window at turn {turn}"))
}

/// CR 500.7 + CR 514.1 + CR 732.2a: the Sieve's extra-turn period repeats its whole turn, casting
/// Thopter Assembly and sacrificing its Thopters, and is offered at the next turn's precombat
/// main, counted to the first cleanup that would discard (CR 401.2: what that discard names is
/// not yet drawn). The first window holds two Islands and the Assembly; its turn ends holding two
/// cards and each repetition adds one, so the sixth would end holding eight: 6. Each declined
/// window is one repetition nearer: 5, 4, 3, 2, 1.
#[test]
fn a_time_sieve_thopter_assembly_period_is_offered_to_its_first_cleanup_discard() {
    let Some(db) = shared_card_db() else { return };
    let board = sieve_board(25, |_| {}, db);
    let start = board.0.state().turn_number;
    let (offers, windows) = sieve_windows(board, start + 9);
    assert_eq!(in_hand(window_of(&windows, start + 3), "Island"), 2);
    assert_eq!(
        offers,
        (3..9)
            .map(|turn| (start + turn, 9 - turn))
            .collect::<Vec<_>>()
    );
    assert!(
        windows.iter().any(|window| window.turn_number == start + 9),
        "reach: the drive passes the windows that offer nothing"
    );
}

/// The count offered at the first window whose span records a cleanup discard, with that window.
fn first_discard_offer(
    library: usize,
    stage: impl FnOnce(&mut GameScenario),
    db: &CardDatabase,
) -> (Option<u32>, GameState) {
    let board = sieve_board(library, stage, db);
    let start = board.0.state().turn_number;
    let (offers, windows) = sieve_windows(board, start + 10);
    assert!(
        offers.contains(&(start + 3, 6)),
        "reach: the first window is offered, {offers:?}"
    );
    let window = window_of(&windows, start + 10).clone();
    assert!(
        window.players[0]
            .graveyard
            .iter()
            .any(|id| window.objects[id].name == "Island"),
        "reach: an Island was discarded before the window"
    );
    let count = offers
        .iter()
        .find(|(turn, _)| *turn == start + 10)
        .map(|(_, count)| *count);
    (count, window)
}

/// CR 514.1 + CR 401.2 + CR 504.1 + CR 704.5b + CR 732.2a: once the span records a cleanup
/// discard, the count is the lesser of the cards in hand sharing the discarded card's name, which
/// the recorded discard can name, and the cards left to draw. With 25 cards the window holds eight
/// Islands and the Assembly over a 16-card library: 8, and a take of 8 performs eight turns, each
/// discarding an Island, while 9 is refused. With 14 cards the library holds 5: 5.
#[test]
fn a_recorded_cleanup_discard_window_is_offered_the_lesser_of_its_named_hand_and_library() {
    let Some(db) = shared_card_db() else { return };
    for (library, named, left, count) in [(25, 8, 16, 8), (14, 8, 5, 5)] {
        let (offered, window) = first_discard_offer(library, |_| {}, db);
        assert_eq!(
            (in_hand(&window, "Island"), library_term(&window)),
            (named, left),
            "{library}"
        );
        assert_eq!(offered, Some(count), "{library}");
        if library == 25 {
            let discarded = |state: &GameState| {
                state.players[0]
                    .graveyard
                    .iter()
                    .filter(|id| state.objects[id].name == "Island")
                    .count()
            };
            let after = taken(&window, 8);
            assert_eq!(
                (after.turn_number, library_term(&after), discarded(&after)),
                (window.turn_number + 8, left - 8, discarded(&window) + 8)
            );
            assert_declaration_refused(&window, 9);
        }
    }
}

/// CR 514.1 + CR 603.4: Mysterio's Mirage ("At the beginning of your end step, if you discarded a
/// card this turn, create a 3/3 blue Illusion Villain creature token.") reads the turn's discards,
/// and Boneyard Wurm ("Boneyard Wurm's power and toughness are each equal to the number of creature
/// cards in your graveyard.", Grizzly Bears in that graveyard) reads the graveyard's contents, so
/// beside either the discarded card is not put back and the recorded-discard window is not offered.
#[test]
fn a_discard_or_graveyard_reader_keeps_the_recorded_discard_window_unoffered() {
    let Some(db) = shared_card_db() else { return };
    let (mirage, _) = first_discard_offer(
        25,
        |scenario| {
            scenario.add_enchantment_from_oracle(
                P0,
                "Mysterio's Mirage",
                "At the beginning of your end step, if you discarded a card this turn, create a \
                 3/3 blue Illusion Villain creature token.",
            );
        },
        db,
    );
    assert_eq!(mirage, None);
    let (wurm, window) = first_discard_offer(
        25,
        |scenario| {
            scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
            scenario.add_creature_from_oracle(
                P0,
                "Boneyard Wurm",
                0,
                0,
                "Boneyard Wurm's power and toughness are each equal to the number of creature \
                 cards in your graveyard.",
            );
        },
        db,
    );
    assert!(
        window
            .battlefield
            .iter()
            .any(|id| window.objects[id].name == "Boneyard Wurm"),
        "reach: the Wurm survives to the window"
    );
    assert_eq!(wurm, None);
}

/// Cauldron Familiar ("When this creature enters, each opponent loses 1 life and you gain 1 life.
/// / Sacrifice a Food: Return this card from your graveyard to the battlefield.") cast beside
/// Carrion Feeder ("Sacrifice a creature: Put a +1/+1 counter on this creature.") and Samwise
/// Gamgee, then recurred until the loop is offered; P1 holds Relic of Progenitus ("{T}: Target
/// player exiles a card from their graveyard."). Returns the runner and the Relic.
fn familiar_offered(db: &CardDatabase) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 20);
    let feeder = scenario.add_real_card(P0, "Carrion Feeder", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Samwise Gamgee", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    let familiar = scenario.add_real_card(P0, "Cauldron Familiar", Zone::Hand, db);
    let relic = scenario.add_real_card(P1, "Relic of Progenitus", Zone::Battlefield, db);
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let named = [familiar];
    let score = names(&named);
    cast(&mut runner, familiar, vec![], CastPaymentMode::Auto);
    settle(&mut runner, &score);
    for _ in 0..4 {
        for source in [feeder, familiar] {
            let index = ability(runner.state(), source, false);
            activate(&mut runner, source, index);
            settle(&mut runner, &score);
            if is_offer(runner.state()) {
                return (runner, relic);
            }
        }
    }
    panic!(
        "no offer; latest verdict {:?}",
        latest_verdict(runner.state())
    );
}

/// CR 732.2c: "If the shortcut was shortened from the original proposal, the player who now has
/// priority must make a different game choice than what was originally proposed for that
/// player." A Familiar take P1 shortens ends at P1's priority, where P1's bare pass is refused and
/// activating the Relic is accepted; the same take accepted outright owes its seat nothing.
#[test]
fn a_shortened_familiar_take_refuses_the_shorteners_bare_pass() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, relic) = familiar_offered(db);
    act(
        &mut runner,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(6),
            template: None,
        },
    );
    let mut accepted = GameRunner::from_state(runner.state().clone());
    let before = lives(runner.state());
    act(
        &mut runner,
        GameAction::RespondToShortcut {
            response: ShortcutResponse::Shorten { at_iteration: 3 },
        },
    );
    let state = runner.state();
    assert_eq!(
        (lives(state)[1], state.waiting_for.clone()),
        (before[1] - 3, WaitingFor::Priority { player: P1 }),
        "reach: the take performed the three cycles P1 named and ended at P1's priority"
    );
    let pass = engine::game::engine::apply(&mut state.clone(), P1, GameAction::PassPriority);
    assert!(
        matches!(pass, Err(EngineError::ActionNotAllowed(_))),
        "the shortener's bare pass must be refused: {pass:?}"
    );
    let index = ability(runner.state(), relic, false);
    activate(&mut runner, relic, index);

    accept(&mut accepted);
    let WaitingFor::Priority { player } = accepted.state().waiting_for else {
        panic!("{}", accepted.state().waiting_for.variant_name());
    };
    assert!(
        lives(accepted.state())[1] < before[1],
        "reach: the accepted take performed its cycles"
    );
    let pass = engine::game::engine::apply(
        &mut accepted.state().clone(),
        player,
        GameAction::PassPriority,
    );
    assert!(pass.is_ok(), "an accepted take latches nothing: {pass:?}");
}

/// CR 104.2a + CR 732.2a: with P1 at 1 after one hand-made cycle, the Altar period's next
/// repetition wins, so P0 is offered that one repetition and the take ends the game.
#[test]
fn a_period_its_first_replayed_repetition_wins_is_offered_one_repetition() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board_at("Zulaport Cutthroat", &[20, 2], db);
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
    };
    cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
    settle(&mut runner, &score);
    let index = ability(runner.state(), altar, true);
    activate(&mut runner, altar, index);
    settle(&mut runner, &score);
    assert_eq!(lives(runner.state()), vec![21, 1]);
    let WaitingFor::LoopShortcut {
        schema,
        predicted_winner,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("no offer: {:?}", latest_verdict(runner.state()));
    };
    assert_eq!(
        (
            &schema.iteration_count,
            schema.deliverable_capacity,
            *predicted_winner
        ),
        (&IterationCount::Fixed(1), 1, Some(P0))
    );
    take(&mut runner, 1);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    );
    assert_eq!(lives(runner.state()), vec![22, 0]);
}

/// CR 705.1 + CR 732.2a: Goblin Bangchuckers ("{T}: Flip a coin. If you win the flip, this
/// creature deals 2 damage to any target. …") untapped by Freed from the Real ("{U}: Untap enchanted
/// creature.") with P1 at 2: its next repetition wins only on a won flip, so it is refused as
/// random, though this seed's next flip wins.
#[test]
fn a_repetition_that_wins_on_a_flip_is_refused_as_random() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new_n_player(2, 2);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 4);
    scenario.with_mana_pool(
        P0,
        (0..8)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    for seat in [P0, P1] {
        for _ in 0..10 {
            scenario.add_real_card(seat, "Island", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    let goblin = place(runner.state_mut(), P0, "Goblin Bangchuckers", db);
    let freed = place(runner.state_mut(), P0, "Freed from the Real", db);
    attach_to(runner.state_mut(), freed, goblin);
    runner.state_mut().layers_dirty.mark_full();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let tap = ability(runner.state(), goblin, false);
    let untap = runner.state().objects[&freed]
        .abilities
        .iter()
        .enumerate()
        .filter(|(_, a)| a.kind == AbilityKind::Activated)
        .map(|(index, _)| index)
        .nth(1)
        .expect("Freed from the Real's untap ability");
    let at_p1 = |action: &GameAction| {
        i32::from(matches!(
            action,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1))
            }
        ))
    };
    activate(&mut runner, goblin, tap);
    settle(&mut runner, &at_p1);
    assert_eq!(
        lives(runner.state()),
        vec![20, 2],
        "reach: the first flip won"
    );
    activate(&mut runner, freed, untap);
    settle(&mut runner, &at_p1);
    assert!(
        !is_offer(runner.state()),
        "a repetition winning on a flip was offered"
    );
    assert_eq!(
        latest_verdict(runner.state()),
        Err(OfferRefusal::Randomness)
    );
    activate(&mut runner, goblin, tap);
    settle(&mut runner, &at_p1);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "reach: the next flip wins"
    );
}

/// CR 800.4a + CR 104.2a + CR 732.2a: the Altar + Gravecrawler + Zulaport Cutthroat period on
/// three seats, P1 at 1 and P2 at 4 after one hand-made cycle: its next repetition eliminates P1
/// but wins no game.
#[test]
fn a_repetition_eliminating_one_of_two_opponents_is_not_offered_as_won() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, altar, gravecrawler) = altar_board_at("Zulaport Cutthroat", &[20, 2, 5], db);
    let score = |action: &GameAction| {
        2 * names(&[gravecrawler])(action) + chooses_color(ManaType::Black)(action)
    };
    cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
    settle(&mut runner, &score);
    let index = ability(runner.state(), altar, true);
    activate(&mut runner, altar, index);
    settle(&mut runner, &score);
    assert_eq!(lives(runner.state()), vec![21, 1, 4]);
    assert!(!is_offer(runner.state()));
    assert_eq!(
        latest_verdict(runner.state()),
        Err(OfferRefusal::NoRecurrence)
    );
    cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
    settle(&mut runner, &score);
    let index = ability(runner.state(), altar, true);
    activate(&mut runner, altar, index);
    settle(&mut runner, &score);
    assert_eq!(
        (
            eliminated(runner.state()),
            runner.state().game_end.is_none()
        ),
        (vec![P1], true),
        "reach: the next repetition eliminates P1 and ends no game"
    );
}

/// Kilo, Apogee Mind ("Whenever Kilo becomes tapped, proliferate.") tapped by Relic of Legends
/// and untapped by Freed from the Real, with Scheming Aspirant ("Whenever you proliferate, each
/// opponent loses 2 life and you gain 2 life."), P0's, on `scenario`'s board with P0 holding the
/// turn and priority: one live cycle, its proliferate naming `named`, leaves each seat at `offered`
/// (life, poison) under the offer.
fn kilo_offer(
    mut scenario: GameScenario,
    offered: &[(i32, u32)],
    named: &[PlayerId],
    db: &CardDatabase,
) -> GameRunner {
    use crate::kilo_live_offer_from_real_dump::{FREED_UNTAP, RELIC_TAP_MANA};
    scenario.at_phase(Phase::PreCombatMain);
    let kilo = scenario.add_real_card(P0, "Kilo, Apogee Mind", Zone::Battlefield, db);
    let relic = scenario.add_real_card(P0, "Relic of Legends", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Scheming Aspirant", Zone::Battlefield, db);
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.loop_detection = LoopDetectionMode::Interactive;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    // CR 102.3: the live cycle gains P0 2 life and costs each of its opponents 2.
    for (seat, (life, poison)) in offered.iter().enumerate() {
        let id = PlayerId(seat as u8);
        let start = if id == P0 {
            life - 2
        } else if engine::game::players::is_opponent(state, P0, id) {
            life + 2
        } else {
            *life
        };
        let gained = u32::from(named.contains(&id));
        state.players[seat].life = start;
        state.players[seat].poison_counters = poison - gained;
    }
    let freed = place(runner.state_mut(), P0, "Freed from the Real", db);
    attach_to(runner.state_mut(), freed, kilo);
    let wanted: Vec<TargetRef> = named.iter().copied().map(TargetRef::Player).collect();
    let score = |action: &GameAction| {
        names(&[kilo])(action)
            + chooses_color(ManaType::Blue)(action)
            + i32::from(matches!(action, GameAction::SelectTargets { targets }
                if targets == &wanted))
    };
    activate(&mut runner, relic, RELIC_TAP_MANA);
    settle(&mut runner, &score);
    activate(&mut runner, freed, FREED_UNTAP);
    settle(&mut runner, &score);
    assert_eq!(seat_totals(runner.state()), offered, "reach");
    runner
}

/// [`kilo_offer`] on a Two-Headed Giant board.
fn two_headed_giant_kilo_offer(
    offered: [(i32, u32); 4],
    named: &[PlayerId],
    db: &CardDatabase,
) -> GameRunner {
    kilo_offer(
        GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42),
        &offered,
        named,
        db,
    )
}

fn seat_totals(state: &GameState) -> Vec<(i32, u32)> {
    state
        .players
        .iter()
        .map(|p| (p.life, p.poison_counters))
        .collect()
}

fn predicted_winner(runner: &GameRunner) -> Option<PlayerId> {
    let WaitingFor::LoopShortcut {
        predicted_winner, ..
    } = runner.state().waiting_for
    else {
        panic!("an offer, got {:?}", runner.state().waiting_for);
    };
    predicted_winner
}

/// CR 704.6a + CR 704.6b + CR 104.2c: the opposing team's 5 + 5 shared poison plus 2 a repetition
/// reaches fifteen at the third, while its 3 + 30 shared life less 4 a repetition is still 25
/// after the second; the offer counts and names that poison loss, including when a proposer's
/// teammate is below 0 life but the team total is not.
#[test]
fn a_two_headed_giant_offer_counts_the_opposing_teams_shared_loss() {
    let Some(db) = shared_card_db() else { return };
    let opponents = [PlayerId(2), PlayerId(3)];
    for offered in [
        [(13, 0), (15, 0), (3, 5), (30, 5)],
        [(30, 0), (-3, 0), (3, 5), (30, 5)],
    ] {
        let mut runner = two_headed_giant_kilo_offer(offered, &opponents, db);
        let (count, capacity, certificate) = offered_parts(&runner);
        assert_eq!(
            (count, capacity),
            (IterationCount::Fixed(3), 3),
            "{offered:?}"
        );
        assert_eq!(
            certificate.win_kind,
            Some(WinKind::PoisonLoss),
            "{offered:?}"
        );
        assert_eq!(predicted_winner(&runner), Some(P0), "{offered:?}");
        let events = take(&mut runner, 3);
        assert_eq!(
            losses_in(&events),
            [
                (PlayerId(2), BTreeSet::from([LossReason::Poison])),
                (PlayerId(3), BTreeSet::from([LossReason::Poison])),
            ],
            "{offered:?}"
        );
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::GameOver { winner: Some(P0) }
            ),
            "{offered:?}: {:?}",
            runner.state().waiting_for
        );
    }
}

/// CR 704.6b + CR 732.2a: the proposer's team holds 12 shared poison plus 1 a repetition and
/// reaches fifteen at the third, before the opponents' 8 plus 2 does at the fourth, so the most
/// the offer counts is 2, and a declared 3 is refused.
#[test]
fn a_two_headed_giant_offer_never_eliminates_the_proposers_team() {
    let Some(db) = shared_card_db() else { return };
    let offered = [(13, 0), (15, 12), (30, 4), (30, 4)];
    let mut runner = two_headed_giant_kilo_offer(offered, &[P1, PlayerId(2), PlayerId(3)], db);
    let (count, capacity, certificate) = offered_parts(&runner);
    assert_eq!((count, capacity), (IterationCount::Fixed(2), 2));
    assert_eq!(certificate.win_kind_at, None);
    assert_eq!(predicted_winner(&runner), None);

    let mut over = GameRunner::from_state(runner.state().clone());
    over.act(GameAction::DeclareShortcut {
        count: IterationCount::Fixed(3),
        template: None,
    })
    .expect("an over-capacity declaration is answered, not an error");
    assert!(matches!(
        over.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(seat_totals(over.state()), offered);

    let events = take(&mut runner, 2);
    assert!(losses_in(&events).is_empty());
    assert!(eliminated(runner.state()).is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

/// CR 704.6b + CR 104.2c: the opposing team's 10 shared poison plus 2 a repetition reaches
/// fifteen at the third, ending the game while the proposer's team, at 10 plus 1, holds 13.
#[test]
fn a_two_headed_giant_offer_ends_at_the_opposing_teams_earlier_crossing() {
    let Some(db) = shared_card_db() else { return };
    let offered = [(13, 0), (15, 10), (30, 5), (30, 5)];
    let mut runner = two_headed_giant_kilo_offer(offered, &[P1, PlayerId(2), PlayerId(3)], db);
    let (count, _, certificate) = offered_parts(&runner);
    assert_eq!(count, IterationCount::Fixed(3));
    assert_eq!(certificate.win_kind, Some(WinKind::PoisonLoss));
    assert_eq!(predicted_winner(&runner), Some(P0));
    let events = take(&mut runner, 3);
    assert_eq!(
        losses_in(&events),
        [
            (PlayerId(2), BTreeSet::from([LossReason::Poison])),
            (PlayerId(3), BTreeSet::from([LossReason::Poison])),
        ]
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    ));
    assert!(!runner.state().players[1].is_eliminated);
}

/// Asserts `runner`'s offer counts `n`, names `kind` won at `n` by P0, and declares it so, then
/// takes it: the seats leave with `losses` and P0 wins.
fn assert_offered_win(
    runner: &mut GameRunner,
    n: u32,
    kind: WinKind,
    losses: &[(PlayerId, LossReason)],
) {
    let (count, capacity, certificate) = offered_parts(runner);
    assert_eq!((count, capacity), (IterationCount::Fixed(n), n));
    let named = (Some(kind), Some(n));
    assert_eq!((certificate.win_kind, certificate.win_kind_at), named);
    assert_eq!(predicted_winner(runner), Some(P0));
    let declared = proposed(runner.state().clone(), n);
    let WaitingFor::RespondToShortcut { proposal, .. } = &declared.waiting_for else {
        unreachable!("proposed")
    };
    assert_eq!((proposal.win_kind, proposal.win_kind_at), named);
    let events = take(runner, n);
    let expected: Vec<_> = losses
        .iter()
        .map(|&(seat, reason)| (seat, BTreeSet::from([reason])))
        .collect();
    assert_eq!(losses_in(&events), expected);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P0) }
        ),
        "{:?}",
        runner.state().waiting_for
    );
}

/// CR 704.6b + CR 810.8a: the opposing team's 9 + 5 shared poison reaches fifteen when the first
/// repetition's proliferate gives P3 a sixth, removing both opponents at 30 team life; the counted
/// win is named for poison, including when the proposer's teammate is below 0 life but the team
/// total is not.
#[test]
fn a_two_headed_giant_counted_win_is_named_by_the_teams_recorded_loss() {
    let Some(db) = shared_card_db() else { return };
    for offered in [
        [(13, 0), (15, 0), (15, 9), (15, 5)],
        [(32, 0), (-3, 0), (15, 9), (15, 5)],
    ] {
        let mut runner = two_headed_giant_kilo_offer(offered, &[P3], db);
        assert_offered_win(
            &mut runner,
            1,
            WinKind::PoisonLoss,
            &[(P2, LossReason::Poison), (P3, LossReason::Poison)],
        );
    }
}

/// CR 904.2b + CR 102.3 + CR 704.5c + CR 704.5a: facing the archenemy P1, P0's fellow hero P2
/// leaves to its tenth poison counter from the proliferate, then Scheming Aspirant drains the
/// archenemy, P0's one opponent, from 2 to 0; the teammate's loss names no part of the win.
#[test]
fn a_teammates_loss_in_the_winning_repetition_does_not_name_the_win() {
    let Some(db) = shared_card_db() else { return };
    let format = FormatConfig {
        archenemy_player: Some(P1),
        ..FormatConfig::archenemy()
    };
    let mut runner = kilo_offer(
        GameScenario::new_with_format(format, 3, 42),
        &[(20, 0), (2, 0), (20, 9)],
        &[P2],
        db,
    );
    assert_offered_win(
        &mut runner,
        1,
        WinKind::LethalDamage,
        &[(P2, LossReason::Poison), (P1, LossReason::LifeTotal)],
    );
}

/// CR 704.6a + CR 704.6b: the opposing team's 8 shared life loses 4 a repetition and reaches 0 at
/// the second, while the proposer's team poison, 12 plus 1 a repetition from proliferating P1, is
/// 14; the counted win is named for life.
#[test]
fn a_two_headed_giant_counted_win_reads_no_poison_on_the_proposers_team() {
    let Some(db) = shared_card_db() else { return };
    let mut runner = two_headed_giant_kilo_offer([(13, 0), (15, 12), (4, 0), (4, 0)], &[P1], db);
    assert_offered_win(
        &mut runner,
        2,
        WinKind::LethalDamage,
        &[(P2, LossReason::LifeTotal), (P3, LossReason::LifeTotal)],
    );
    assert!(!runner.state().players[1].is_eliminated);
}

/// CR 704.5c + CR 704.5a: P1 leaves to its tenth poison counter from the proliferate, then
/// Scheming Aspirant drains P2 from 2 to 0 in the same repetition; two reasons name no single win.
#[test]
fn a_counted_win_with_two_reasons_is_named_immediate() {
    let Some(db) = shared_card_db() else { return };
    let mut runner = kilo_offer(
        GameScenario::new_with_format(FormatConfig::free_for_all(), 3, 42),
        &[(20, 0), (20, 9), (2, 0)],
        &[P1],
        db,
    );
    assert_offered_win(
        &mut runner,
        1,
        WinKind::ImmediateWin,
        &[(P1, LossReason::Poison), (P2, LossReason::LifeTotal)],
    );
}

/// CR 704.6b + CR 732.2a: the opposing team's 7 shared poison plus 1 a repetition reaches fifteen
/// at the eighth repetition's proliferate, before its drain takes the team's last 2 life; the
/// threshold offer names the poison win its performance records, at eight.
#[test]
fn a_threshold_offers_performed_win_is_named_by_its_recorded_loss() {
    let Some(db) = shared_card_db() else { return };
    let mut runner = two_headed_giant_kilo_offer([(13, 0), (15, 0), (15, 7), (15, 0)], &[P2], db);
    assert_offered_win(
        &mut runner,
        8,
        WinKind::PoisonLoss,
        &[(P2, LossReason::Poison), (P3, LossReason::Poison)],
    );
}

/// CR 704.5a + CR 704.5c + CR 104.2a: P1's 6 life falls 2 a repetition and reaches 0 at the third;
/// P2's 6 poison plus 1 a repetition reaches ten at the fourth, leaving P0 alone. The threshold
/// offer names the win by the fourth repetition's poison, not P1's earlier departure.
#[test]
fn a_threshold_offers_performed_win_is_named_by_its_last_repetition() {
    let Some(db) = shared_card_db() else { return };
    let mut runner = kilo_offer(
        GameScenario::new_with_format(FormatConfig::free_for_all(), 3, 42),
        &[(20, 0), (6, 0), (20, 6)],
        &[P2],
        db,
    );
    assert_offered_win(
        &mut runner,
        4,
        WinKind::PoisonLoss,
        &[(P1, LossReason::LifeTotal), (P2, LossReason::Poison)],
    );
}

/// CR 704.5c at the host's poison-loss threshold + CR 704.5a + CR 732.2a: P1's 10 life falls 2 a
/// repetition and reaches 0 at the fifth; its 6 poison plus 1 a repetition reaches ten at the
/// fourth but twelve only at the sixth. The threshold offer counts and names whichever the host's
/// threshold makes first.
#[test]
fn a_threshold_offer_counts_and_names_the_hosts_poison_threshold() {
    let Some(db) = shared_card_db() else { return };
    for (threshold, n, kind, reason) in [
        (10, 4, WinKind::PoisonLoss, LossReason::Poison),
        (12, 5, WinKind::LethalDamage, LossReason::LifeTotal),
    ] {
        let format = FormatConfig {
            poison_counter_threshold: Some(threshold),
            ..FormatConfig::standard()
        };
        let mut runner = kilo_offer(
            GameScenario::new_with_format(format, 2, 42),
            &[(22, 0), (10, 6)],
            &[P1],
            db,
        );
        assert_offered_win(&mut runner, n, kind, &[(P1, reason)]);
    }
}
