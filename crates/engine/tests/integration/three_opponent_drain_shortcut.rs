//! Exquisite Blood ("Whenever an opponent loses life, you gain that much life.") with Enduring
//! Tenacity ("Whenever you gain life, target opponent loses that much life.") in a four-player
//! game. Each repetition costs its target 3, so the CR 732.2a count on any offer board is the sum
//! over the living opponents of ceil(life / 3), the trigger already on the stack (CR 603.3d)
//! being one of its seat's hits.
//!
//! S (`exquisite_blood_tenacity_three_opponents_4p.json.gz`) is a saved game whose offer
//! predates these readings. The `committed_dump_walk` S row is the one row that reads S's saved
//! offer; R0 and every other row here only decline it.
//!
//! F-C (`exquisite_blood_tenacity_alternating_offer_4p.json.gz`) is minted by this engine through
//! `apply()`: S, its offer declined, answering P1 then P2 up to the first offer. Regenerate it
//! (gzipped by the writer) with
//! `PHASE_REGENERATE_FIXTURES=1 cargo nextest run -p phase-engine --features test-support --test integration --run-ignored only -E 'test(/regenerate_alternating_offer_fixture/)'`.

use std::io::Write;

use engine::analysis::decision_template::{
    AnnouncementSubject, DecisionPointKind, DecisionSlot, DecisionTemplate, IterationCount,
    PinnedDecision, Ranking, ReplayMode, ShortcutDecisionSchema, TargetPin, TargetSchedule,
};
use engine::analysis::loop_check::ShortcutResponse;
use engine::game::combat::AttackTarget;
use engine::game::engine::apply;
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, resolve_interaction_response,
    InteractionSubmitError,
};
use engine::game::scenario::GameScenario;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, LoopDetectionMode, PersistedGameState, WaitingFor};
use engine::types::interaction::{
    AmountAssignment, InteractionChoiceId, InteractionId, InteractionOpportunityResponse,
    InteractionReasonCode, InteractionResponse, InteractionResponseSpec, InteractionSessionId,
    InteractionShortcutCountSpec, InteractionShortcutDecision, InteractionShortcutPin,
    InteractionShortcutPreview, InteractionSubmission,
};
use engine::types::phase::Phase;
use engine::types::PlayerId;

use crate::loop_shortcut::{gunzip_dump, restore_dump};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);
/// Life each repetition moves: the lifelinked drain on this board is 3.
const HIT: i32 = 3;

const FC_FILE: &str = "exquisite_blood_tenacity_alternating_offer_4p.json.gz";

pub(crate) fn load_s() -> GameState {
    restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/exquisite_blood_tenacity_three_opponents_4p.json.gz"
    )))
}

fn fc_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FC_FILE)
}

// Read at run time: the regenerator lives in this binary, so it must build without F-C.
pub(crate) fn load_fc() -> GameState {
    let gz = std::fs::read(fc_path()).expect("F-C is committed beside S");
    restore_dump(&gunzip_dump(&gz))
}

/// Decline S's offer, then play on: P0 passes, each Tenacity target is the next scripted seat
/// (the last one repeats), and an offer reached before `min_answers` answers is declined.
fn drive_to_offer(mut state: GameState, answers: &[PlayerId], min_answers: usize) -> GameState {
    apply(&mut state, P0, GameAction::DeclineShortcut).expect("S opens on a declinable offer");
    play_to_offer(state, answers, min_answers, false)
}

/// `drive_to_offer` without S's decline: every lifelinker attacks P1, who blocks each with one
/// of its creatures iff `blocked`, and simultaneous triggers keep their offered order.
fn play_to_offer(
    mut state: GameState,
    answers: &[PlayerId],
    min_answers: usize,
    blocked: bool,
) -> GameState {
    let mut answered = 0;
    for _ in 0..200 {
        match state.waiting_for.clone() {
            WaitingFor::LoopShortcut { proposer, .. } => {
                if answered >= min_answers {
                    return state;
                }
                apply(&mut state, proposer, GameAction::DeclineShortcut).expect("declinable");
            }
            WaitingFor::Priority { player } => {
                apply(&mut state, player, GameAction::PassPriority).expect("pass");
            }
            WaitingFor::DeclareAttackers { player, .. } => {
                let attacks = state
                    .battlefield
                    .iter()
                    .copied()
                    .filter(|id| state.objects[id].name.starts_with("Lifelinker"))
                    .map(|id| (id, AttackTarget::Player(P1)))
                    .collect();
                apply(
                    &mut state,
                    player,
                    GameAction::DeclareAttackers {
                        attacks,
                        bands: vec![],
                    },
                )
                .expect("the lifelinkers attack");
            }
            WaitingFor::DeclareBlockers { player, .. } => {
                let attackers: Vec<_> = state
                    .combat
                    .as_ref()
                    .map(|c| c.attackers.iter().map(|a| a.object_id).collect())
                    .unwrap_or_default();
                let assignments = if blocked {
                    state
                        .battlefield
                        .iter()
                        .copied()
                        .filter(|id| state.objects[id].controller == player)
                        .zip(attackers)
                        .collect()
                } else {
                    vec![]
                };
                apply(
                    &mut state,
                    player,
                    GameAction::DeclareBlockers { assignments },
                )
                .expect("legal blocks");
            }
            WaitingFor::OrderTriggers { player, triggers } => {
                apply(
                    &mut state,
                    player,
                    GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    },
                )
                .expect("the offered order");
            }
            WaitingFor::TriggerTargetSelection { player, .. } => {
                let seat = *answers.get(answered).or(answers.last()).expect("a script");
                answered += 1;
                apply(
                    &mut state,
                    player,
                    GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(seat)],
                    },
                )
                .expect("the scripted seat is a legal target");
            }
            other => panic!("unexpected stop before the offer: {}", other.variant_name()),
        }
    }
    panic!("no offer within 200 beats");
}

fn offer(state: &GameState) -> (&ShortcutDecisionSchema, Option<&DecisionTemplate>) {
    match &state.waiting_for {
        WaitingFor::LoopShortcut {
            schema,
            declaration,
            ..
        } => (schema, declaration.as_ref()),
        other => panic!("expected an offer, got {}", other.variant_name()),
    }
}

fn opponents_life(state: &GameState) -> [i32; 3] {
    [P1, P2, P3].map(|p| state.players[p.0 as usize].life)
}

/// The seat the Tenacity trigger already on the stack targets.
fn in_flight(state: &GameState) -> Option<PlayerId> {
    state
        .stack
        .iter()
        .filter_map(|entry| entry.ability())
        .find_map(|ability| match ability.targets.as_slice() {
            [TargetRef::Player(seat)] => Some(*seat),
            _ => None,
        })
}

/// CR 732.2a: the repetitions that eliminate every living opponent.
fn card_count(state: &GameState) -> u32 {
    state
        .players
        .iter()
        .filter(|p| p.id != P0 && !p.is_eliminated)
        .map(|p| {
            assert!(p.life > 0, "a living opponent has positive life");
            (p.life + HIT - 1) as u32 / HIT as u32
        })
        .sum()
}

/// The card count spread in candidate order: each seat's hits less the in-flight one, and one
/// more on the last seat for the final announcement, which never resolves.
fn candidate_order(state: &GameState) -> Vec<(PlayerId, u32)> {
    let lead = in_flight(state);
    let mut allocation: Vec<(PlayerId, u32)> = [P1, P2, P3]
        .into_iter()
        .filter(|seat| !state.players[seat.0 as usize].is_eliminated)
        .map(|seat| {
            let hits = (state.players[seat.0 as usize].life + HIT - 1) as u32 / HIT as u32;
            (seat, hits - u32::from(lead == Some(seat)))
        })
        .filter(|(_, n)| *n > 0)
        .collect();
    if lead.is_some() {
        allocation.last_mut().expect("a living opponent").1 += 1;
    }
    allocation
}

fn rank(seat: PlayerId) -> Ranking {
    Ranking::new(vec![AnnouncementSubject::Seat(seat)]).expect("one subject")
}

/// The template's single Tenacity schedule and the count it is stamped at.
fn schedule(template: &DecisionTemplate) -> (TargetSchedule, IterationCount) {
    let [PinnedDecision::Targets { targets, .. }] = template.decisions.as_slice() else {
        panic!("one Targets decision, got {:?}", template.decisions);
    };
    let [TargetPin::Scheduled(schedule)] = targets.as_slice() else {
        panic!("one scheduled pin, got {targets:?}");
    };
    let ReplayMode::Scheduled { count } = &template.replay else {
        panic!("a scheduled replay, got {:?}", template.replay);
    };
    (schedule.clone(), count.clone())
}

fn tenacity_slot(schema: &ShortcutDecisionSchema) -> DecisionSlot {
    let [point] = schema.points.as_slice() else {
        panic!("the offer publishes Tenacity's one point");
    };
    assert!(matches!(point.kind, DecisionPointKind::Targets { .. }));
    point.slot.clone()
}

struct Dialog {
    interaction_id: InteractionId,
    seats: Vec<PlayerId>,
    ids: Vec<InteractionChoiceId>,
    group: u32,
    count: InteractionShortcutCountSpec,
    preview: Vec<InteractionShortcutPreview>,
}

impl Dialog {
    fn id(&self, seat: PlayerId) -> InteractionChoiceId {
        let at = self
            .seats
            .iter()
            .position(|s| *s == seat)
            .expect("a candidate");
        self.ids[at].clone()
    }

    fn seat(&self, id: &InteractionChoiceId) -> PlayerId {
        self.seats[self.ids.iter().position(|i| i == id).expect("published id")]
    }
}

/// P0's published shortcut dialog, with candidate ids aligned to the point's legal seats.
fn dialog(state: &mut GameState) -> Dialog {
    let (schema, _) = offer(state);
    let DecisionPointKind::Targets { legal_targets, .. } = &schema.points[0].kind else {
        panic!("Tenacity's point is a Targets point");
    };
    let seats: Vec<PlayerId> = legal_targets
        .iter()
        .map(|t| match t {
            TargetRef::Player(seat) => *seat,
            other => panic!("the victims are seats, got {other:?}"),
        })
        .collect();
    bind_interaction_authority(state, InteractionSessionId("drain".to_string()))
        .expect("valid session");
    let filtered = filter_state_for_viewer(state, P0);
    let view = derive_viewer_interaction(state, &filtered, P0);
    let opportunity = view
        .opportunities
        .into_iter()
        .find(|o| {
            matches!(
                o.response,
                InteractionOpportunityResponse::Schema {
                    spec: InteractionResponseSpec::Shortcut { .. },
                    ..
                }
            )
        })
        .expect("the offer is published as a shortcut schema");
    let InteractionOpportunityResponse::Schema {
        spec:
            InteractionResponseSpec::Shortcut {
                count,
                points,
                preview,
                ..
            },
        ..
    } = opportunity.response
    else {
        unreachable!();
    };
    let [point] = points.as_slice() else {
        panic!("one published point");
    };
    assert_eq!(point.candidate_ids.len(), seats.len());
    Dialog {
        interaction_id: opportunity.interaction_id,
        seats,
        ids: point.candidate_ids.clone(),
        group: point.group,
        count,
        preview,
    }
}

fn submit(
    state: &GameState,
    dialog: &Dialog,
    decision: InteractionShortcutDecision,
    allocation: &[(PlayerId, u32)],
) -> Result<GameAction, InteractionSubmitError> {
    let choice_ids = allocation.iter().map(|(s, _)| dialog.id(*s)).collect();
    let amounts = allocation
        .iter()
        .map(|(s, amount)| AmountAssignment {
            choice_id: dialog.id(*s),
            amount: *amount,
        })
        .collect();
    resolve_interaction_response(
        state,
        P0,
        &InteractionSubmission {
            interaction_id: dialog.interaction_id.clone(),
            response: InteractionResponse::Shortcut {
                decision,
                pins: vec![InteractionShortcutPin {
                    group: dialog.group,
                    choice_ids,
                    amounts,
                }],
            },
        },
    )
}

/// Apply P0's declaration; every opponent's CR 732.2c window is answered by `respond`.
fn take(
    state: &mut GameState,
    action: GameAction,
    respond: impl Fn(PlayerId) -> ShortcutResponse,
) -> usize {
    apply(state, P0, action).expect("the declaration is admitted");
    let mut responders = 0;
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            state,
            player,
            GameAction::RespondToShortcut {
                response: respond(player),
            },
        )
        .expect("a shortcut response");
        responders += 1;
    }
    responders
}

fn accept(_: PlayerId) -> ShortcutResponse {
    ShortcutResponse::Accept
}

/// CR 704.5a + CR 104.2a: the last opponent leaves at its loss, so P0 never gains the final 3.
fn assert_game_over(state: &GameState, label: &str) {
    assert_eq!(
        state.waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "{label}: every opponent is eliminated"
    );
    let lives: Vec<i32> = state.players.iter().map(|p| p.life).collect();
    assert_eq!(lives, vec![143, -2, 0, -2], "{label}");
}

fn accept_suggested(state: &GameState) -> GameState {
    let mut state = state.clone();
    let dialog = dialog(&mut state);
    let action = submit(
        &state,
        &dialog,
        InteractionShortcutDecision::AcceptSuggested,
        &[],
    )
    .expect("AcceptSuggested with an empty pin is admitted");
    assert!(
        take(&mut state, action, accept) > 0,
        "the respond window opens"
    );
    state
}

fn declare_fixed(
    state: &GameState,
    allocation: &[(PlayerId, u32)],
) -> Result<GameState, InteractionReasonCode> {
    let mut state = state.clone();
    let dialog = dialog(&mut state);
    let count = allocation.iter().map(|(_, n)| n).sum();
    let action = submit(
        &state,
        &dialog,
        InteractionShortcutDecision::Fixed { iterations: count },
        allocation,
    )
    .map_err(|e| e.code)?;
    assert!(
        take(&mut state, action, accept) > 0,
        "the respond window opens"
    );
    Ok(state)
}

struct Row {
    label: &'static str,
    answers: &'static [PlayerId],
    min_answers: usize,
    board: [i32; 3],
    lead: Option<PlayerId>,
    /// The published declaration's own count, where the row states it.
    own: Option<u32>,
}

const ROWS: &[Row] = &[
    Row {
        label: "R0",
        answers: &[P1, P2, P1],
        min_answers: 0,
        board: [10, 9, 1],
        lead: Some(P2),
        own: Some(5),
    },
    Row {
        label: "T-ALT2",
        answers: &[P2, P1, P2],
        min_answers: 0,
        board: [13, 6, 1],
        lead: Some(P1),
        own: Some(3),
    },
    Row {
        label: "T-ALT3",
        answers: &[P1, P3, P1],
        min_answers: 0,
        board: [10, 9, 1],
        lead: Some(P3),
        own: None,
    },
    Row {
        label: "T-LEADKILL",
        answers: &[P2, P1, P2],
        min_answers: 3,
        board: [10, 3, 1],
        lead: Some(P2),
        own: Some(1),
    },
    Row {
        label: "T-UNI",
        answers: &[P1],
        min_answers: 0,
        board: [10, 9, 1],
        lead: Some(P1),
        own: Some(4),
    },
    Row {
        label: "T-OFF",
        answers: &[P1],
        min_answers: 3,
        board: [4, 9, 1],
        lead: None,
        own: None,
    },
];

/// CR 732.2a: on every answer pattern the offer states the card count exactly, refuses one more,
/// and the suggestion, the candidate-order declaration and the AI candidate each end the game.
#[test]
fn every_answer_pattern_offers_the_card_count_and_each_take_eliminates_every_opponent() {
    for row in ROWS {
        let label = row.label;
        let state = drive_to_offer(load_s(), row.answers, row.min_answers);
        assert_eq!(opponents_life(&state), row.board, "{label}: offer board");
        assert_eq!(in_flight(&state), row.lead, "{label}: trigger in flight");

        let n = card_count(&state);
        let (schema, declaration) = offer(&state);
        assert_eq!(schema.deliverable_capacity, n, "{label}: capacity");
        assert_eq!(
            schema.iteration_count,
            IterationCount::Fixed(n),
            "{label}: count"
        );
        let slot = tenacity_slot(schema);
        assert_eq!(
            schema.leading_aims,
            row.lead.map(|seat| vec![(slot, seat)]).unwrap_or_default(),
            "{label}: CR 603.3d leading aim"
        );
        let declaration = declaration.expect("the observed answers are published");
        assert!(
            matches!(schedule(declaration).0, TargetSchedule::Constant(_)),
            "{label}: the published declaration repeats the observed answer"
        );
        if let Some(own) = row.own {
            assert_eq!(
                schedule(declaration).1,
                IterationCount::Fixed(own),
                "{label}"
            );
        }
        let suggestion = schema
            .suggested_declaration
            .clone()
            .expect("an eliminating suggestion is published");
        assert_eq!(schedule(&suggestion).1, IterationCount::Fixed(n), "{label}");

        let mut probe = state.clone();
        assert_eq!(
            dialog(&mut probe).count,
            InteractionShortcutCountSpec::Fixed {
                min: 1,
                max: n,
                suggested: n
            },
            "{label}: projected window"
        );

        assert_game_over(
            &accept_suggested(&state),
            &format!("{label} AcceptSuggested"),
        );

        let order = candidate_order(&state);
        let taken = declare_fixed(&state, &order).expect("the candidate-order count is admitted");
        assert_game_over(&taken, &format!("{label} candidate order"));

        let mut over = order.clone();
        over[0].1 += 1;
        assert_eq!(
            declare_fixed(&state, &over).err(),
            Some(InteractionReasonCode::ConstraintUnsatisfied),
            "{label}: one repetition past the card count is refused"
        );

        let ai: Vec<GameAction> = engine::ai_support::legal_actions(&state)
            .into_iter()
            .filter(|a| matches!(a, GameAction::DeclareShortcut { .. }))
            .collect();
        let [candidate] = ai.as_slice() else {
            panic!("{label}: one AI shortcut candidate, got {}", ai.len());
        };
        assert_eq!(
            candidate,
            &GameAction::DeclareShortcut {
                count: IterationCount::Fixed(n),
                template: Some(suggestion),
            },
            "{label}: the AI declares the suggestion"
        );
        let mut by_ai = state.clone();
        assert!(take(&mut by_ai, candidate.clone(), accept) > 0);
        assert_game_over(&by_ai, &format!("{label} AI"));
    }
}

/// R0's offer, with the alternating history, publishes the observed answer at its own count and
/// the eliminating Piecewise suggestion at the card count; F-C snapshots exactly this offer.
#[test]
fn the_alternating_offer_publishes_its_observation_and_an_eliminating_suggestion() {
    let r0 = drive_to_offer(load_s(), &[P1, P2, P1], 0);
    let (schema, declaration) = offer(&r0);
    assert_eq!(schema.deliverable_capacity, 8);
    assert_eq!(
        schedule(declaration.expect("published")),
        (TargetSchedule::Constant(rank(P1)), IterationCount::Fixed(5)),
        "P2's in-flight hit, then P1's four, P1 crossing at the fifth"
    );
    assert_eq!(
        schedule(schema.suggested_declaration.as_ref().expect("suggested")),
        (
            TargetSchedule::Piecewise(vec![(0, rank(P1)), (4, rank(P2)), (6, rank(P3))]),
            IterationCount::Fixed(8)
        )
    );
    match &r0.waiting_for {
        WaitingFor::LoopShortcut { certificate, .. } => {
            let per_cycle = certificate
                .per_cycle
                .as_ref()
                .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
                .expect("a bounded period");
            assert_eq!(
                per_cycle.frames_per_period, 2,
                "CR 732.2a: one repetition, the target answers aside"
            );
        }
        _ => unreachable!(),
    }
    assert_eq!(load_fc().waiting_for, r0.waiting_for, "F-C is this offer");
}

/// The published declaration declared at its own count is taken and stops where P1 leaves.
#[test]
fn the_published_declaration_is_taken_at_its_own_count() {
    let mut state = load_fc();
    let (_, declaration) = offer(&state);
    let declaration = declaration.expect("published").clone();
    let (_, count) = schedule(&declaration);
    assert_eq!(count, IterationCount::Fixed(5));
    let action = GameAction::DeclareShortcut {
        count,
        template: Some(declaration),
    };
    assert!(take(&mut state, action, accept) > 0);
    assert!(state.players[1].is_eliminated, "CR 800.4a: P1 left");
    assert_eq!([state.players[2].life, state.players[3].life], [6, 1]);
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
}

#[test]
fn accepting_the_suggestion_eliminates_every_opponent() {
    assert_game_over(&accept_suggested(&load_fc()), "AcceptSuggested");
}

/// CR 732.2a + CR 608.2b: an authored order is admitted at the card count wherever each aim lands
/// on a seat still in the game, refused one repetition past it and wherever an aim outlives its
/// seat.
#[test]
fn authored_orders_are_admitted_exactly_where_every_aim_lands() {
    let state = load_fc();
    for (label, allocation) in [
        ("candidate order", [(P1, 4), (P2, 2), (P3, 2)]),
        ("opens away from the lead", [(P3, 1), (P1, 4), (P2, 3)]),
        ("opens on the lead", [(P2, 2), (P3, 1), (P1, 5)]),
    ] {
        let taken = declare_fixed(&state, &allocation).expect(label);
        assert_game_over(&taken, label);
    }
    assert_eq!(
        declare_fixed(&state, &[(P1, 5), (P2, 2), (P3, 2)]).err(),
        Some(InteractionReasonCode::ConstraintUnsatisfied),
        "nine repetitions"
    );
    assert_eq!(
        declare_fixed(&state, &[(P2, 3), (P1, 4), (P3, 1)]).err(),
        Some(InteractionReasonCode::ConstraintUnsatisfied),
        "the third P2 aim resolves after P2 has left"
    );
}

/// A preview element's count, allocation and per-seat life lines.
type ExpectedElement = (u32, &'static [(PlayerId, u32)], &'static [(u8, i32)]);

/// The opponents' life losses a preview element states, by seat.
fn drained(preview: &InteractionShortcutPreview) -> Vec<(u8, i32)> {
    let mut drained: Vec<(u8, i32)> = preview
        .entries
        .iter()
        .filter_map(|e| match e.player {
            Some(seat) if seat != P0.0 && e.amount < 0 => Some((seat, e.amount)),
            _ => None,
        })
        .collect();
    drained.sort();
    drained
}

/// Every published preview element states the suggestion's prefix, the victims that prefix
/// drains with the in-flight hit on P2 (CR 603.3d), and is itself admitted.
#[test]
fn each_preview_element_states_the_suggestions_prefix_and_its_victims() {
    let mut state = load_fc();
    let dialog = dialog(&mut state);
    let expected: [ExpectedElement; 8] = [
        (1, &[(P1, 1)], &[(2, -3)]),
        (2, &[(P1, 2)], &[(1, -3), (2, -3)]),
        (3, &[(P1, 3)], &[(1, -6), (2, -3)]),
        (4, &[(P1, 4)], &[(1, -9), (2, -3)]),
        (5, &[(P1, 4), (P2, 1)], &[(1, -12), (2, -3)]),
        (6, &[(P1, 4), (P2, 2)], &[(1, -12), (2, -6)]),
        (7, &[(P1, 4), (P2, 2), (P3, 1)], &[(1, -12), (2, -9)]),
        (
            8,
            &[(P1, 4), (P2, 2), (P3, 2)],
            &[(1, -12), (2, -9), (3, -3)],
        ),
    ];
    let counts: Vec<u32> = dialog.preview.iter().map(|p| p.count).collect();
    assert_eq!(counts, (1..=8).collect::<Vec<_>>(), "one element per count");
    for (preview, (count, allocation, victims)) in dialog.preview.iter().zip(expected) {
        assert_eq!(preview.count, count);
        let stated: Vec<(PlayerId, u32)> = preview
            .allocation
            .iter()
            .map(|a| (dialog.seat(&a.choice_id), a.amount))
            .collect();
        assert_eq!(stated, allocation, "count {count}: allocation");
        assert_eq!(drained(preview), victims, "count {count}: victims");
        submit(
            &state,
            &dialog,
            InteractionShortcutDecision::Fixed { iterations: count },
            allocation,
        )
        .unwrap_or_else(|e| panic!("count {count}: the stated allocation is admitted, {e:?}"));
    }
}

#[test]
fn the_ai_declares_the_suggestion_and_ends_the_game() {
    let mut state = load_fc();
    let suggestion = offer(&state).0.suggested_declaration.clone();
    let ai: Vec<GameAction> = engine::ai_support::legal_actions(&state)
        .into_iter()
        .filter(|a| matches!(a, GameAction::DeclareShortcut { .. }))
        .collect();
    assert_eq!(
        ai,
        vec![GameAction::DeclareShortcut {
            count: IterationCount::Fixed(8),
            template: suggestion,
        }]
    );
    assert!(take(&mut state, ai[0].clone(), accept) > 0);
    assert_game_over(&state, "AI");
}

/// CR 732.2b: P1 shortening to five ends the drive at its own departure.
#[test]
fn a_responder_shortening_the_eliminating_order_stops_at_its_iteration() {
    let mut state = load_fc();
    let dialog = dialog(&mut state);
    let action = submit(
        &state,
        &dialog,
        InteractionShortcutDecision::Fixed { iterations: 8 },
        &[(P1, 4), (P2, 2), (P3, 2)],
    )
    .expect("admitted");
    let shortened = std::cell::Cell::new(false);
    take(&mut state, action, |player| {
        if player == P1 {
            shortened.set(true);
            ShortcutResponse::Shorten { at_iteration: 5 }
        } else {
            ShortcutResponse::Accept
        }
    });
    assert!(shortened.get(), "P1 answered the window");
    assert!(state.players[1].is_eliminated);
    assert_eq!([state.players[2].life, state.players[3].life], [6, 1]);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// The drain with `lifelinkers` 2/2 lifelinkers, each with a 0/5 of P1's to block it; the
/// opponents start at 9/6/3 and each repetition moves 2.
fn combat_board(lifelinkers: usize) -> GameState {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    for (seat, life) in [(P0, 20), (P1, 9), (P2, 6), (P3, 3)] {
        scenario.with_life(seat, life);
    }
    scenario.add_enchantment_from_oracle(
        P0,
        "Exquisite Blood",
        "Whenever an opponent loses life, you gain that much life.",
    );
    scenario.add_creature_from_oracle(
        P0,
        "Enduring Tenacity",
        4,
        3,
        "Whenever you gain life, target opponent loses that much life.",
    );
    for i in 0..lifelinkers {
        scenario
            .add_creature(P0, &format!("Lifelinker {i}"), 2, 2)
            .lifelink();
        scenario.add_vanilla(P1, 0, 5);
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner.state().clone()
}

/// The seats the Tenacity triggers on the stack latched, bottom to top.
fn latched(state: &GameState) -> Vec<PlayerId> {
    state
        .stack
        .iter()
        .filter_map(|entry| match entry.ability()?.targets.as_slice() {
            [TargetRef::Player(seat)] => Some(*seat),
            _ => None,
        })
        .collect()
}

fn assert_every_opponent_eliminated(state: &GameState, label: &str) {
    assert_eq!(
        state.waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "{label}"
    );
    assert!(
        [P1, P2, P3]
            .iter()
            .all(|p| state.players[p.0 as usize].is_eliminated),
        "{label}: {:?}",
        opponents_life(state)
    );
}

/// The blocked board at its offer: the top Tenacity trigger on P1 leads, and the suggestion
/// eliminates every opponent. 9 = 4 hits on P1 (the lead's included), 3 on P2, 2 on P3.
fn assert_top_trigger_on_p1_leads(state: &GameState, label: &str) {
    assert_eq!(opponents_life(state), [7, 6, 3], "{label}: offer board");
    let (schema, _) = offer(state);
    assert_eq!(schema.iteration_count, IterationCount::Fixed(9), "{label}");
    assert_eq!(
        schema.leading_aims,
        vec![(tenacity_slot(schema), P1)],
        "{label}: the top entry is the lead"
    );
    let mut probe = state.clone();
    let preview = dialog(&mut probe).preview;
    let first = preview
        .iter()
        .find(|p| p.count == 1)
        .expect("a one-repetition element");
    assert_eq!(drained(first), vec![(1, -2)], "{label}: the lead hits P1");
    assert_every_opponent_eliminated(&accept_suggested(state), label);
}

/// CR 405.5 + CR 608.1: of two Tenacity triggers latched by simultaneous lifelink damage, only
/// the top one (on P1) resolves before the next announcement; the buried one (on P3) never leads.
#[test]
fn only_the_top_latched_trigger_leads() {
    let state = play_to_offer(combat_board(2), &[P3, P1, P1], 0, true);
    assert_eq!(latched(&state), vec![P3, P1]);
    assert_top_trigger_on_p1_leads(&state, "two latched");
}

#[test]
fn a_single_latched_trigger_on_top_leads() {
    let state = play_to_offer(combat_board(1), &[P1, P1], 0, true);
    assert_eq!(latched(&state), vec![P1]);
    assert_top_trigger_on_p1_leads(&state, "one latched");
}

/// CR 405.2 + CR 405.5: with a Blood trigger on top, each resolution puts the next Tenacity
/// trigger above the latched ones, so none of them resolves while the loop runs and none leads.
/// 7 = 2 hits on P1, 3 on P2, 2 on P3.
#[test]
fn latched_triggers_buried_under_the_chain_do_not_lead() {
    let state = play_to_offer(combat_board(2), &[P3, P1, P1], 0, false);
    assert_eq!(opponents_life(&state), [3, 6, 3]);
    assert_eq!(latched(&state), vec![P3, P1]);
    let top = state.stack.back().expect("the chain is on the stack");
    assert!(
        top.ability().expect("a trigger").targets.is_empty(),
        "a Blood trigger is on top"
    );
    let (schema, _) = offer(&state);
    tenacity_slot(schema);
    assert_eq!(schema.iteration_count, IterationCount::Fixed(7));
    assert_eq!(schema.leading_aims, vec![]);
    assert_eq!(
        schedule(schema.suggested_declaration.as_ref().expect("suggested")),
        (
            TargetSchedule::Piecewise(vec![(0, rank(P1)), (2, rank(P2)), (5, rank(P3))]),
            IterationCount::Fixed(7)
        )
    );
    let mut probe = state.clone();
    let preview = dialog(&mut probe).preview;
    let at = |count| {
        drained(
            preview
                .iter()
                .find(|p| p.count == count)
                .expect("a preview element"),
        )
    };
    assert_eq!(at(1), vec![(1, -2)], "the in-drive announcement hits P1");
    assert_eq!(at(7), vec![(1, -4), (2, -6), (3, -4)]);
    assert_every_opponent_eliminated(&accept_suggested(&state), "buried");
}

#[test]
#[ignore = "writes F-C; run with PHASE_REGENERATE_FIXTURES=1"]
fn regenerate_alternating_offer_fixture() {
    let state = drive_to_offer(load_s(), &[P1, P2], 0);
    if std::env::var("PHASE_REGENERATE_FIXTURES").as_deref() != Ok("1") {
        return;
    }
    let envelope = serde_json::json!({
        "gameState": serde_json::to_value(PersistedGameState::Raw(Box::new(state)))
            .expect("serializable"),
    });
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(&serde_json::to_vec(&envelope).expect("json"))
        .expect("gzip");
    std::fs::write(fc_path(), gz.finish().expect("gzip")).expect("write F-C");
}
